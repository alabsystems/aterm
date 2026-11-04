// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::binding::Request;
use crate::membership_log::{Body as RecordBody, Draft, Entrant, Log, MemberId, Record, Store};
use std::collections::VecDeque;

const HANG: Duration = Duration::from_secs(60);

struct StopRunning(Option<Running>);

impl std::ops::Deref for StopRunning {
    type Target = Running;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}

impl Drop for StopRunning {
    fn drop(&mut self) {
        if let Some(running) = self.0.take() {
            running.stop();
        }
    }
}

fn founder() -> (Record, [u8; 32], [u8; 16], MemberId) {
    let seed = [41; 32];
    let incarnation = [42; 16];
    let identity = Identity::from_secret_bytes([43; 32]);
    let request =
        Request::create([44; 16], [45; 32], [46; 16], incarnation, &identity, &seed).unwrap();
    let actor = request.binding().actor();
    let genesis = Record::found(
        &[47; 32],
        vec![Entrant {
            request,
            node_key: [48; 32],
            name: "sender".into(),
            label: "m1".into(),
        }],
        1,
    )
    .unwrap();
    (genesis, seed, incarnation, actor)
}

fn chain(count: usize) -> Vec<Record> {
    let (genesis, seed, incarnation, _) = founder();
    let network = *genesis.id();
    let mut records = vec![genesis];
    for i in 0..count {
        let prior = *records.last().unwrap().id();
        records.push(
            Draft {
                network,
                sequence: i as u64 + 1,
                previous: (i != 0).then_some(prior),
                parents: vec![prior],
                authored_ms: i as u64 + 2,
                body: RecordBody::Name {
                    name: format!("sender-{i}"),
                },
            }
            .sign(&seed, incarnation)
            .unwrap(),
        );
    }
    records
}

fn large_record(size: usize) -> Record {
    let (genesis, seed, incarnation, actor) = founder();
    let mut log = Log::new(genesis).unwrap();
    let (draft, key) = log
        .draft_epoch(&actor, 2, crate::membership_log::EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = draft.sign(&seed, incarnation).unwrap();
    let epoch_id = *epoch.id();
    log.insert(epoch).unwrap();
    log.draft_put(
        &actor,
        3,
        epoch_id,
        &key,
        &crate::text::Entry {
            name: "payload".into(),
            value: crate::SecretBytes::new(vec![b'x'; size]),
            audience: crate::text::Audience::Everyone,
            if_absent: false,
            anchor: None,
        },
    )
    .unwrap()
    .sign(&seed, incarnation)
    .unwrap()
}

struct Sink {
    bytes: Vec<u8>,
    at: Instant,
    step: Duration,
    waits: Vec<Duration>,
    writes: VecDeque<Result<usize, io::ErrorKind>>,
    before: Option<Box<dyn FnOnce()>>,
}

impl Sink {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            at: Instant::now(),
            step: Duration::ZERO,
            waits: Vec::new(),
            writes: VecDeque::new(),
            before: None,
        }
    }
}

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(before) = self.before.take() {
            before();
        }
        self.at += self.step;
        let take = self
            .writes
            .pop_front()
            .unwrap_or(Ok(bytes.len()))
            .map_err(io::Error::from)?
            .min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..take]);
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ReplyWriter for Sink {
    fn set_wait(&mut self, wait: Duration) -> io::Result<()> {
        self.waits.push(wait);
        Ok(())
    }

    fn now(&self) -> Instant {
        self.at
    }
}

#[test]
fn tiny_and_large_record_parts_coalesce_without_changing_the_wire() {
    let mut records = chain(1024);
    records.insert(500, large_record(2 * WRITE_CHUNK + 123));
    let expected = crate::membership_log::encode_records(&records);
    let mut actual = Vec::new();
    let mut sizes = Vec::new();
    record_chunks(&records, |bytes| {
        sizes.push(bytes.len());
        actual.extend_from_slice(bytes);
        Ok(())
    })
    .unwrap();
    assert_eq!(actual, expected);
    assert!(sizes.iter().all(|&n| n > 0 && n <= WRITE_CHUNK));
    assert_eq!(sizes.len(), expected.len().div_ceil(WRITE_CHUNK));
    let mut empty_calls = 0;
    record_chunks(&[], |_| {
        empty_calls += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(empty_calls, 0);

    let overhead = large_record(1).bytes().len() - 1;
    for length in [WRITE_CHUNK - 1, WRITE_CHUNK, WRITE_CHUNK + 1] {
        let record = large_record(length - overhead);
        assert_eq!(record.bytes().len(), length);
        let mut seen = Vec::new();
        record_chunks(std::slice::from_ref(&record), |bytes| {
            seen.push(bytes.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.concat(), record.bytes());
        assert_eq!(seen.len(), length.div_ceil(WRITE_CHUNK));
        assert!(
            seen.iter()
                .all(|part| !part.is_empty() && part.len() <= WRITE_CHUNK)
        );
    }
}

#[test]
fn an_oversized_record_reply_is_rejected_before_any_header() {
    let large = large_record(crate::text::MAX_VALUE);
    let reply = records_reply(vec![large; 65], false);
    let mut sink = Sink::new();
    let error = write_reply_to(&mut sink, &reply, None, HANG, HANG).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(sink.bytes.is_empty());
    assert!(sink.waits.is_empty());
}

#[test]
fn the_header_and_every_record_chunk_share_one_absolute_deadline() {
    let reply = records_reply(vec![large_record(WRITE_CHUNK); 3], false);
    let mut sink = Sink::new();
    sink.step = Duration::from_millis(4);
    let error =
        write_reply_to(&mut sink, &reply, None, HANG, Duration::from_millis(10)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        sink.waits,
        [10, 6, 2].map(Duration::from_millis),
        "neither a record boundary nor a partial chunk renews the deadline"
    );
    let mut expired = Sink::new();
    assert_eq!(
        write_reply_to(&mut expired, &reply, None, HANG, Duration::ZERO)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(expired.bytes.is_empty());
}

#[test]
fn partial_and_interrupted_writes_keep_bytes_and_io_failures() {
    let mut sink = Sink::new();
    sink.step = Duration::from_millis(1);
    sink.writes = VecDeque::from([Err(io::ErrorKind::Interrupted), Ok(2), Ok(1)]);
    let deadline = sink.at + Duration::from_millis(100);
    send(&mut sink, b"abcdefgh", HANG, Some(deadline)).unwrap();
    assert_eq!(sink.bytes, b"abcdefgh");
    assert_eq!(sink.waits, [100, 99, 98, 97].map(Duration::from_millis));
    for (outcome, expected) in [
        (Ok(0), io::ErrorKind::WriteZero),
        (Err(io::ErrorKind::BrokenPipe), io::ErrorKind::BrokenPipe),
    ] {
        let mut sink = Sink::new();
        sink.writes.push_back(outcome);
        let deadline = sink.at + HANG;
        assert_eq!(
            send(&mut sink, b"x", HANG, Some(deadline))
                .unwrap_err()
                .kind(),
            expected
        );
    }
}

#[test]
fn a_selected_reply_survives_concurrent_store_replacement_without_holding_state() {
    let root = aterm_tempfile::tempdir().unwrap();
    let records = chain(3);
    let initial = crate::membership_log::encode_records(&records[..3]);
    let store = Store::decode("team", &initial).unwrap();
    let running = StopRunning(Some(
        start(
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
            &Options {
                listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                store: root.path().to_path_buf(),
                advertise: None,
                pins: Vec::new(),
                hold: HANG,
                keep_awake: false,
            },
        )
        .unwrap(),
    ));
    running
        .shared
        .lock_server_state()
        .records
        .insert("team".into(), RecordSlot::new(store));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let reply = get_records(&running.shared, &server, "team", None, None, true, true);
    let (change, requested) = std::sync::mpsc::channel();
    let (finished, done) = std::sync::mpsc::channel();
    let shared = Arc::clone(&running.shared);
    let replaced = Arc::new(AtomicBool::new(false));
    let changed = Arc::clone(&replaced);
    let all = crate::membership_log::encode_records(&records);
    let worker = std::thread::spawn(move || {
        requested.recv_timeout(HANG).unwrap();
        if let Ok(mut state) = shared.server_state.try_lock() {
            let slot = state.records.get_mut("team").unwrap();
            slot.store.insert(records.last().unwrap().clone()).unwrap();
            slot.store = Store::decode("team", &all).unwrap();
            changed.store(true, Ordering::SeqCst);
        }
        finished.send(()).unwrap();
    });
    let mut sink = Sink::new();
    sink.before = Some(Box::new(move || {
        change.send(()).unwrap();
        done.recv_timeout(HANG).unwrap();
    }));
    write_reply_to(&mut sink, &reply, None, HANG, HANG).unwrap();
    drop(reply);
    worker.join().unwrap();
    let current = get_records(&running.shared, &server, "team", None, None, true, true);
    drop(running);
    drop(client);
    assert!(
        replaced.load(Ordering::SeqCst),
        "the sender retained server_state"
    );
    let split = sink
        .bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap()
        + 4;
    assert!(
        std::str::from_utf8(&sink.bytes[..split])
            .unwrap()
            .contains(&format!("Content-Length: {}\r\n", initial.len()))
    );
    assert_eq!(&sink.bytes[split..], initial);
    let Body::Records(current) = current.body else {
        panic!("the next GET has current records");
    };
    assert_eq!(current.len(), 4);
}
