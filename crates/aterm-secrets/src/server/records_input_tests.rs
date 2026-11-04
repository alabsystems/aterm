// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::binding::Request as BindingRequest;
use crate::membership_log::untaken::ReceivedFrames;
use crate::membership_log::{Body as RecordBody, Entrant, Log, Store, encode_records};
use std::cell::Cell;
use std::ops::Deref;

fn records() -> Vec<Record> {
    let identity = Identity::from_secret_bytes([33; 32]);
    let seed = [34; 32];
    let incarnation = [35; 16];
    let request =
        BindingRequest::create([36; 16], [37; 32], [38; 16], incarnation, &identity, &seed)
            .unwrap();
    let actor = request.binding().actor();
    let genesis = Record::found(
        &[39; 32],
        vec![Entrant {
            request,
            node_key: [40; 32],
            name: "reader".into(),
            label: "m1".into(),
        }],
        1,
    )
    .unwrap();
    let mut log = Log::new(genesis.clone()).unwrap();
    let mut records = vec![genesis];
    for name in ["first", "second"] {
        let record = log
            .draft(&actor, 2, RecordBody::Name { name: name.into() })
            .sign(&seed, incarnation)
            .unwrap();
        log.insert(record.clone()).unwrap();
        records.push(record);
    }
    records
}

struct Stop(Option<Running>);

impl Deref for Stop {
    type Target = Running;
    fn deref(&self) -> &Running {
        self.0.as_ref().unwrap()
    }
}

impl Drop for Stop {
    fn drop(&mut self) {
        if let Some(running) = self.0.take() {
            running.stop();
        }
    }
}

fn running(path: &Path) -> Stop {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let options = Options {
        listen: listener.local_addr().unwrap(),
        store: path.to_owned(),
        advertise: None,
        pins: Vec::new(),
        hold: Duration::from_secs(60),
        keep_awake: false,
    };
    Stop(Some(start(listener, &options).unwrap()))
}

fn post_status(running: &Running, body: &[u8]) -> u16 {
    let mut stream = TcpStream::connect(running.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    write!(
        stream,
        "POST /v1/team/records HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn invalid_http_tails_never_apply_the_valid_prefix() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let running = running(dir.path());
    let records = records();
    assert_eq!(
        post_records(&running.shared, "team", records[0].bytes(), true).status,
        204
    );
    let path = dir.path().join("team.records");
    let before = std::fs::read(&path).unwrap();
    let stamp = fsx::stamp(&path);
    let mut bad_signature = records[2].bytes().to_vec();
    *bad_signature.last_mut().unwrap() ^= 1;
    let unknown = crate::membership_log::with_kind_code(&records[2], 0xff);
    let truncated = records[2].bytes()[..records[2].bytes().len() - 1].to_vec();
    for tail in [bad_signature, unknown, truncated] {
        let mut body = records[1].bytes().to_vec();
        body.extend_from_slice(&tail);
        let status = post_status(&running, &body);
        let state = running.shared.lock_server_state();
        assert_eq!(
            state.records["team"].store.inventory(),
            [*records[0].id()].into()
        );
        drop(state);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(fsx::stamp(&path), stamp);
        assert_eq!(status, 400);
    }
}

#[test]
fn invalid_body_does_not_refresh_an_unreadable_records_file() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let running = running(dir.path());
    let records = records();
    assert_eq!(
        post_records(&running.shared, "team", records[0].bytes(), true).status,
        204
    );
    let path = dir.path().join("team.records");
    let held_stamp = running.shared.lock_server_state().records["team"].stamp;
    std::fs::rename(&path, dir.path().join("saved.records")).unwrap();
    std::fs::create_dir(&path).unwrap();
    let mut body = records[1].bytes().to_vec();
    let mut bad = records[2].bytes().to_vec();
    *bad.last_mut().unwrap() ^= 1;
    body.extend_from_slice(&bad);
    assert_eq!(post_status(&running, &body), 400);
    let state = running.shared.lock_server_state();
    assert_eq!(state.records["team"].stamp, held_stamp);
    assert_eq!(state.records["team"].store.len(), 1);
}

#[test]
fn a_pinned_known_owner_is_offered_again_when_the_current_store_lost_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let running = running(dir.path());
    let records = records();
    let first_two = encode_records(&records[..2]);
    assert_eq!(
        post_records(&running.shared, "team", &first_two, true).status,
        204
    );
    let verified = running.shared.post_verifies.load(Ordering::SeqCst);
    let staged = from_bytes(&running.shared, "team", records[1].bytes())
        .unwrap_or_else(|_| panic!("known record"));
    assert_eq!(
        running.shared.post_verifies.load(Ordering::SeqCst),
        verified
    );
    let path = dir.path().join("team.records");
    fsx::write_atomic(&path, records[0].bytes(), 0o600).unwrap();
    {
        let mut state = running.shared.lock_server_state();
        let slot = state.records.get_mut("team").unwrap();
        slot.store = Store::for_vault("team")
            .unwrap()
            .recovering(records[0].bytes());
        slot.stamp = fsx::stamp(&path);
        slot.on_file = Some(1);
    }
    assert_eq!(
        apply_records(&running.shared, "team", staged, true).status,
        204
    );
    assert_eq!(std::fs::read(&path).unwrap(), first_two);
    assert!(
        running.shared.lock_server_state().records["team"]
            .store
            .contains(records[1].id())
    );
}

fn received(shared: &Shared, bytes: &[u8]) -> ReceivedFrames {
    let mut input = bytes;
    ReceivedFrames::read_from_with_known(&mut input, |id| known_record(shared, "team", id)).unwrap()
}

#[test]
fn a_pull_reuses_only_the_owner_matching_the_complete_frame_hash() {
    let records = records();
    let known = &records[1];
    let mut damaged = known.bytes().to_vec();
    *damaged.last_mut().unwrap() ^= 1;
    let bytes = [known.bytes(), &damaged].concat();
    let mut looked_up = Vec::new();
    let received = ReceivedFrames::read_from_with_known(&mut bytes.as_slice(), |id| {
        looked_up.push(*id);
        (id == known.id()).then(|| known.clone())
    })
    .unwrap();
    assert_eq!(
        looked_up,
        [*known.id(), aterm_digest::Sha256::digest(&damaged)]
    );
    assert!(received.whole().is_none());
    let readable = received.readable();
    assert_eq!(readable.len(), 1);
    assert_eq!(readable[0].bytes(), known.bytes());
}

#[test]
fn a_streamed_pull_owner_survives_loss_from_the_current_store() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let running = running(dir.path());
    let records = records();
    let first_two = encode_records(&records[..2]);
    assert_eq!(
        post_records(&running.shared, "team", &first_two, true).status,
        204
    );
    let staged = received(&running.shared, records[1].bytes());
    assert_eq!(staged.whole().unwrap()[0].id(), records[1].id());

    // A file refresh alone is monotone. Deliberately replace the controlled memory store
    // too, after acquisition, to exercise the final application's owner reoffer.
    let path = dir.path().join("team.records");
    fsx::write_atomic(&path, records[0].bytes(), 0o600).unwrap();
    {
        let mut state = running.shared.lock_server_state();
        let slot = state.records.get_mut("team").unwrap();
        slot.store = Store::for_vault("team")
            .unwrap()
            .recovering(records[0].bytes());
        slot.stamp = fsx::stamp(&path);
        slot.on_file = Some(1);
    }
    assert_eq!(
        apply_pulled_records(&running.shared, "team", staged)
            .1
            .status,
        204
    );
    assert_eq!(std::fs::read(&path).unwrap(), first_two);
    let state = running.shared.lock_server_state();
    assert!(state.records["team"].store.holds(records[1].id()));
    assert_eq!(state.records["team"].on_file, Some(2));
}

#[test]
fn a_readable_pull_keeps_the_original_unreadable_frame_for_its_note() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let running = running(dir.path());
    let records = records();
    assert_eq!(
        post_records(&running.shared, "team", records[0].bytes(), true).status,
        204
    );
    let unknown = crate::membership_log::with_kind_code(&records[2], 0xee);
    let unread = aterm_digest::Sha256::digest(&unknown);
    let bytes = [records[1].bytes(), &unknown].concat();
    let (sent, reply) =
        apply_pulled_records(&running.shared, "team", received(&running.shared, &bytes));
    assert_eq!(reply.status, 204);
    let state = running.shared.lock_server_state();
    let store = &state.records["team"].store;
    assert!(store.holds(records[1].id()));
    assert!(!store.contains(&unread));
    assert!(
        sent.left_in(store),
        "the original unreadable frame was lost"
    );
    assert_eq!(
        std::fs::read(dir.path().join("team.records")).unwrap(),
        encode_records(&records[..2])
    );
}

#[test]
fn a_layout_invalid_pull_is_not_empty_and_applies_no_prefix() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let running = running(dir.path());
    let records = records();
    assert_eq!(
        post_records(&running.shared, "team", records[0].bytes(), true).status,
        204
    );
    let path = dir.path().join("team.records");
    let before = std::fs::read(&path).unwrap();
    let stamp = fsx::stamp(&path);
    let bytes = [records[1].bytes(), &[0; 32]].concat();
    let staged = received(&running.shared, &bytes);
    assert_eq!(staged.byte_len(), bytes.len());
    assert!(staged.whole().is_none());
    assert!(staged.readable().is_empty());
    let (_, reply) = apply_pulled_records(&running.shared, "team", staged);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(fsx::stamp(&path), stamp);
    assert!(
        !running.shared.lock_server_state().records["team"]
            .store
            .contains(records[1].id())
    );
    assert_eq!(reply.status, 400);
    let empty = received(&running.shared, &[]);
    assert_eq!(empty.byte_len(), 0);
    assert!(empty.whole().unwrap().is_empty());
}

#[test]
fn completed_pull_candidates_keep_admission_capacity_and_unrooted_verdicts() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let running = running(dir.path());
    let records = records();
    let (_, reply) = apply_pulled_records(
        &running.shared,
        "team",
        received(&running.shared, records[0].bytes()),
    );
    assert_eq!(reply.status, 403);
    assert!(running.shared.lock_server_state().records.is_empty());

    let (_, reply) = apply_pulled_records(
        &running.shared,
        crate::ALAB_VAULT,
        received(&running.shared, records[1].bytes()),
    );
    assert_eq!(reply.status, 422);
    assert!(running.shared.lock_server_state().records.is_empty());

    {
        let mut state = running.shared.lock_server_state();
        for i in 0..MAX_VAULTS {
            state.records.insert(
                format!("filled-{i}"),
                RecordSlot::new(
                    Store::for_vault("team")
                        .unwrap()
                        .recovering(records[0].bytes()),
                ),
            );
        }
    }
    let (_, reply) = apply_pulled_records(
        &running.shared,
        crate::ALAB_VAULT,
        received(&running.shared, records[0].bytes()),
    );
    assert_eq!(reply.status, 507);
    assert_eq!(running.shared.lock_server_state().records.len(), MAX_VAULTS);
}

#[test]
fn a_pull_write_failure_keeps_its_candidate_out_of_the_current_store() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let running = running(dir.path());
    let records = records();
    assert_eq!(
        post_records(&running.shared, "team", records[0].bytes(), true).status,
        204
    );
    let path = dir.path().join("team.records");
    let before = std::fs::read(&path).unwrap();
    running.shared.writes_fail.store(true, Ordering::SeqCst);
    let (_, reply) = apply_pulled_records(
        &running.shared,
        "team",
        received(&running.shared, records[1].bytes()),
    );
    running.shared.writes_fail.store(false, Ordering::SeqCst);
    assert_eq!(reply.status, 500);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        !running.shared.lock_server_state().records["team"]
            .store
            .contains(records[1].id())
    );
}

struct ClockRead {
    bytes: Cursor<Vec<u8>>,
    now: Cell<Instant>,
    step: Duration,
    max: usize,
    waits: Vec<Duration>,
}

impl Read for ClockRead {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let len = out.len().min(self.max);
        let read = self.bytes.read(&mut out[..len])?;
        self.now.set(self.now.get() + self.step);
        Ok(read)
    }
}

impl BodyStream for ClockRead {
    fn body_now(&self) -> Instant {
        self.now.get()
    }
    fn read_wait(&mut self, wait: Duration) -> io::Result<()> {
        self.waits.push(wait);
        Ok(())
    }
}

#[test]
fn the_body_deadline_includes_later_frames_and_invalid_tail_drain() {
    let records = records();
    let tuning = Tuning {
        body_deadline: Duration::from_millis(7),
        body_stall: Duration::from_secs(1),
        ..Tuning::default()
    };
    for invalid in [false, true] {
        let mut bytes = records[0].bytes().to_vec();
        if invalid {
            bytes.extend_from_slice(&crate::membership_log::with_kind_code(&records[1], 0xff));
            bytes.extend_from_slice(&[0; 32 * 1024]);
        } else {
            bytes.extend_from_slice(&encode_records(&records[1..]));
        }
        let length = bytes.len();
        let mut stream = ClockRead {
            bytes: Cursor::new(bytes),
            now: Cell::new(Instant::now()),
            step: Duration::from_millis(1),
            max: 1024,
            waits: Vec::new(),
        };
        let mut body = HttpBody::new(&mut stream, Vec::new(), length, &tuning);
        let reply = stage_with(&mut body, length, |_| None, || {}, || {})
            .err()
            .unwrap();
        assert_eq!(reply.status, 408);
        assert_eq!(
            stream.waits,
            [7, 6, 5, 4, 3, 2, 1].map(Duration::from_millis)
        );
        if invalid {
            assert!(
                stream.bytes.position() as usize
                    > records[0].bytes().len() + records[1].bytes().len(),
                "the semantic failure reaches the remainder drain before its deadline"
            );
        }
    }
}

#[test]
fn raw_frame_limit_counts_duplicates_and_early_bytes_end_at_content_length() {
    let record = records().remove(0);
    let repeated: Vec<u8> = std::iter::repeat_n(record.bytes(), MAX_RECORDS + 1)
        .flatten()
        .copied()
        .collect();
    let mut verified = 0;
    let reply = stage_with(
        &mut Cursor::new(&repeated),
        repeated.len(),
        |_| Some(record.clone()),
        || {},
        || {
            verified += 1;
        },
    )
    .err()
    .unwrap();
    assert_eq!(reply.status, 400);
    assert_eq!(verified, 0);

    let bytes = record.bytes().to_vec();
    let length = bytes.len();
    let mut stream = ClockRead {
        bytes: Cursor::new(Vec::new()),
        now: Cell::new(Instant::now()),
        step: Duration::ZERO,
        max: 1,
        waits: Vec::new(),
    };
    let mut body = HttpBody::new(&mut stream, bytes, length, &Tuning::default());
    let staged = stage_with(&mut body, length, |_| Some(record.clone()), || {}, || {})
        .unwrap_or_else(|_| panic!("early bytes form one exact record"));
    assert_eq!(staged.records().len(), 1);
    assert!(
        stream.waits.is_empty(),
        "no read for EOF after Content-Length"
    );
}

#[test]
fn an_incomplete_declared_body_overrides_an_earlier_semantic_error() {
    let record = records().remove(0);
    let mut invalid = record.bytes().to_vec();
    *invalid.last_mut().unwrap() ^= 1;
    let length = invalid.len() + 10;
    let mut stream = ClockRead {
        bytes: Cursor::new(invalid),
        now: Cell::new(Instant::now()),
        step: Duration::ZERO,
        max: 1024,
        waits: Vec::new(),
    };
    let mut body = HttpBody::new(&mut stream, Vec::new(), length, &Tuning::default());
    let reply = stage_with(&mut body, length, |_| None, || {}, || {})
        .err()
        .unwrap();
    assert_eq!(reply.status, 400);
    let Body::Text(reason) = reply.body else {
        panic!("body failure has a reason");
    };
    assert!(
        reason.contains("Content-Length"),
        "the short HTTP body, rather than the earlier signature failure, is the reported refusal"
    );
}

#[test]
fn known_reuse_hashes_every_signed_byte_before_skipping_verification() {
    let record = records().remove(0);
    let mut verified = 0;
    let accepted = stage_with(
        &mut Cursor::new(record.bytes()),
        record.bytes().len(),
        |id| (id == record.id()).then(|| record.clone()),
        || {},
        || verified += 1,
    )
    .unwrap_or_else(|_| panic!("the exact known record is accepted"));
    assert_eq!(accepted.records()[0].id(), record.id());
    assert_eq!(verified, 0);
    let mut changed = record.bytes().to_vec();
    *changed.last_mut().unwrap() ^= 1;
    let refusal = stage_with(
        &mut Cursor::new(&changed),
        changed.len(),
        |id| (id == record.id()).then(|| record.clone()),
        || {},
        || verified += 1,
    )
    .err()
    .unwrap();
    assert_eq!(refusal.status, 400);
    assert_eq!(verified, 1, "the altered signature is a new frame");
}

#[test]
fn records_expect_continue_precedes_the_body_and_commits_the_complete_batch() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let running = running(dir.path());
    let records = records();
    let body = encode_records(&records);
    let mut stream = TcpStream::connect(running.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    write!(stream, "POST /v1/team/records HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nExpect: 100-continue\r\n\r\n", body.len()).unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
        assert!(head.len() <= MAX_HEAD);
    }
    assert_eq!(
        std::str::from_utf8(&head)
            .unwrap()
            .split_whitespace()
            .nth(1),
        Some("100")
    );
    assert!(!dir.path().join("team.records").exists());
    stream.write_all(&body).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert_eq!(answer.split_whitespace().nth(1), Some("204"));
    assert_eq!(
        std::fs::read(dir.path().join("team.records")).unwrap(),
        body
    );
    let state = running.shared.lock_server_state();
    assert_eq!(
        state.records["team"].store.inventory(),
        records.iter().map(|r| *r.id()).collect()
    );
    assert_eq!(state.records["team"].on_file, Some(records.len()));
}
