// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The format fence, shipped dark (`docs/DESIGN-vault-log-2026-11-14.md` step 3, tracker
//! FL-147; [`crate::crossing`]): a node states the version-1 records bound byte for byte until
//! its format mark is stored, nodes of two formats exchange no record, the mark is stored before
//! the first record of its format, a records GET answers one reply's causally closed prefix, and
//! what a format-2 node offers aterm v0.198.0 and v0.201.0 leaves their records as they were.
//!
//! The vault is [`fixture::Batch`]'s: a base log three records short of
//! [`crate::membership_log::LEGACY_RECORDS`] and a removal batch past it.

use super::*;
use crate::crossing::fixture::{self, Batch, VAULT};
use crate::crossing::{
    FORMAT_ONE, FORMAT_ONE_BYTES, FORMAT_POST_QUERY, FORMAT_TWO, Faults, FormatMark, Mark,
    RecordsBound, inject, legacy,
};
use crate::membership_log::{RecordId, Store, decode_records, encode_records};
use std::io::{Read, Write};

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

fn serve_tuned(store: &Path, tuning: Tuning) -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    launch(
        listener,
        &Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: store.to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
        tuning,
    )
    .expect("started")
}

fn url(running: &Running) -> String {
    format!("http://{}/v1/{VAULT}", running.addr)
}

/// A server under `store`, tuned by `tuning`, that holds the base log, posted as a build before
/// the fence posts it.
fn holding_base(store: &Path, tuning: Tuning) -> Running {
    let running = serve_tuned(store, tuning);
    crate::transport::post_records(&url(&running), &encode_records(&Batch::get().base))
        .expect("the base reaches the server");
    assert_eq!(held(&running), fixture::BASE);
    running
}

/// The held records of `running`'s log.
fn held(running: &Running) -> usize {
    running
        .stored_records(VAULT)
        .log()
        .map_or(0, crate::membership_log::Log::held_count)
}

/// The vault's format mark in `store`.
fn format_mark_in(store: &Path) -> FormatMark {
    FormatMark::at(store.join(format!("{VAULT}.format")))
}

/// SHA-256 of the bytes of the vault's records file in `store`.
fn records_file_digest(store: &Path) -> [u8; 32] {
    let path = crate::records_storage::reader_path(&store.join(format!("{VAULT}.records")))
        .expect("the records file");
    aterm_digest::Sha256::digest(&std::fs::read(path).expect("the records file reads"))
}

/// One answer, read whole off the wire.
struct Answer {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Answer {
    /// The value of its header `name`, if it sent one.
    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    /// The records bound its head states, read as this build reads it.
    fn bound(&self) -> Option<RecordsBound> {
        self.header(crate::transport::RECORDS_BOUND_HEADER)
            .and_then(RecordsBound::parse)
    }

    /// The records its body carries; none for a body that is not a records stream.
    fn records(&self) -> Vec<crate::membership_log::Record> {
        decode_records(&self.body).unwrap_or_default()
    }
}

/// Send `request` as is and read the whole answer.
fn exchange(running: &Running, request: &[u8]) -> Answer {
    let mut stream = TcpStream::connect(running.addr).expect("connect");
    stream.set_read_timeout(Some(HANG)).expect("timeout");
    stream.set_write_timeout(Some(HANG)).expect("timeout");
    stream.write_all(request).expect("write");
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("an answer's head");
    let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status");
    Answer {
        status,
        head,
        body: bytes[split + 4..].to_vec(),
    }
}

/// A records GET naming `have`, stating `bound` as is (`None`: none, as a build before the
/// fence sends it).
fn get(running: &Running, have: &[RecordId], bound: Option<&str>) -> Answer {
    let query = if have.is_empty() {
        String::new()
    } else {
        let ids: Vec<String> = have
            .iter()
            .map(|id| aterm_codec::hex::encode_lower(id))
            .collect();
        format!("?have={}", ids.join(","))
    };
    let bound = bound.map_or_else(String::new, |value| {
        format!("{}: {value}\r\n", crate::transport::RECORDS_BOUND_HEADER)
    });
    exchange(
        running,
        format!(
            "GET /v1/{VAULT}/records{query} HTTP/1.1\r\nHost: 127.0.0.1\r\n{}{bound}Connection: \
             close\r\n\r\n",
            format_args!("{}: 1\r\n", crate::transport::NO_WAIT_HEADER),
        )
        .as_bytes(),
    )
}

/// A records POST of `body`, with `query` and stating `bound` as is (`None`: none).
fn post(running: &Running, query: Option<&str>, bound: Option<&str>, body: &[u8]) -> Answer {
    let query = query.map_or_else(String::new, |query| format!("?{query}"));
    let bound = bound.map_or_else(String::new, |value| {
        format!("{}: {value}\r\n", crate::transport::RECORDS_BOUND_HEADER)
    });
    let mut request = format!(
        "POST /v1/{VAULT}/records{query} HTTP/1.1\r\nHost: 127.0.0.1\r\n{bound}Content-Length: \
         {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    exchange(running, &request)
}

/// The head of the records POST this build sends stating `bound`, read by a listener that
/// answers `400` without reading its body.
fn posted_head(bound: RecordsBound) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let reader = std::thread::spawn(move || {
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("a post");
        stream.set_read_timeout(Some(HANG)).expect("timeout");
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(stream.read(&mut byte).expect("read"), 1, "a whole head");
            head.push(byte[0]);
        }
        let _ = stream.write_all(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        String::from_utf8_lossy(&head).into_owned()
    });
    let _ = crate::transport::post_records_answered_until(
        &format!("http://{addr}/v1/{VAULT}"),
        Batch::get().base[0].bytes(),
        bound,
        None,
    );
    reader.join().expect("the reader")
}

/// A format-2 node and a format-1 node exchange no record, either way and in either role, and
/// byte capacities must each hold the other's history.
///
/// NEGATIVE CONTROL (in the test): two nodes of one format and capacity exchange; the same
/// format-2 server serves a format-2 GET its base.
#[test]
fn vault_fence_format_two_and_format_one_exchange_nothing() {
    let one = RecordsBound::uncrossed();
    let two = one.at_format(FORMAT_TWO);
    for crossed in [false, true] {
        let one = RecordsBound::here(crossed);
        let two = one.at_format(FORMAT_TWO);
        assert!(one.can_exchange(one) && two.can_exchange(two), "control");
        assert!(!one.can_exchange(two) && !two.can_exchange(one));
    }
    let smaller = RecordsBound {
        bytes: FORMAT_ONE_BYTES / 2,
        ..two
    };
    assert!(smaller.can_exchange(smaller), "control");
    assert!(!two.can_exchange(smaller) && !smaller.can_exchange(two));
    let none = RecordsBound { bytes: 0, ..two };
    assert!(!none.can_exchange(none));

    let batch = Batch::get();
    // A format-2 server: what every build before it asks, and what a format-1 build of this
    // one asks, is answered 409 with no record, and it takes nothing such a poster sends.
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(root.path(), Tuning::default());
    format_mark_in(root.path())
        .store(FORMAT_TWO)
        .expect("the mark");
    for asked in [
        None,
        Some(one.value()),
        Some(RecordsBound::here(true).value()),
    ] {
        let answer = get(&running, &[], asked.as_deref());
        assert_eq!(answer.status, 409, "{asked:?}: {}", answer.head);
        assert!(answer.records().is_empty(), "{asked:?}");
        assert_eq!(answer.bound(), Some(two), "{asked:?}: it states its format");
    }
    for (query, stated) in [
        (None, None),
        (None, Some(one.value())),
        (
            Some(crate::transport::CROSSED_POST_QUERY),
            Some(RecordsBound::here(true).value()),
        ),
    ] {
        let answer = post(
            &running,
            query,
            stated.as_deref(),
            &encode_records(&batch.removal),
        );
        assert_eq!(answer.status, 409, "{stated:?}: {}", answer.head);
    }
    assert!(
        crate::transport::fetch_records_sync(&url(&running), &[], None, HANG, None, one).is_err(),
        "a format-1 puller of this build takes nothing from it"
    );
    // A build that reads no format 2 serves no record of a marked store, whatever is asked
    // (`server::unread_format`); one that reads it serves a format-2 GET its base.
    let refused = get(&running, &[], Some(&two.value()));
    assert_eq!(refused.status, 500, "{}", refused.head);
    assert!(refused.records().is_empty());
    let mark = format_mark_in(root.path());
    inject(
        mark.path(),
        Faults {
            follows: true,
            ..Faults::default()
        },
    );
    let served = get(&running, &[], Some(&two.value()));
    inject(mark.path(), Faults::default());
    assert_eq!(served.status, 200, "control: {}", served.head);
    assert_eq!(served.records().len(), fixture::BASE, "control");
    assert_eq!(held(&running), fixture::BASE, "nothing posted was taken");
    running.stop();

    // A format-1 server: a format-2 node's GET and POST are answered 409, its POST before the
    // body is read, and it takes nothing from such a server's answer.
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(root.path(), Tuning::default());
    let refused = get(&running, &[], Some(&two.value()));
    assert_eq!(refused.status, 409, "{}", refused.head);
    assert!(refused.records().is_empty());
    assert_eq!(refused.bound(), Some(one), "it states version 1");
    let unread = exchange(
        &running,
        format!(
            "POST /v1/{VAULT}/records?{FORMAT_POST_QUERY} HTTP/1.1\r\nHost: 127.0.0.1\r\n{}: \
             {}\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n",
            crate::transport::RECORDS_BOUND_HEADER,
            two.value()
        )
        .as_bytes(),
    );
    assert_eq!(
        unread.status, 409,
        "refused with no body sent: {}",
        unread.head
    );
    let posted = crate::transport::post_records_answered_until(
        &url(&running),
        &encode_records(&batch.removal),
        two,
        None,
    );
    assert!(!matches!(posted, Ok(Ok(()))), "{posted:?}");
    assert!(
        crate::transport::fetch_records_sync(&url(&running), &[], None, HANG, None, two).is_err()
    );
    assert_eq!(held(&running), fixture::BASE);
    assert!(!format_mark_in(root.path()).path().exists());
    running.stop();
}

/// A build of this package stores no format mark, so it states the records bound aterm v0.198.0
/// and v0.201.0 state, byte for byte, and they read it as the bound it is: from a bound alone, a
/// node's own records ([`crate::crossing::stated`]) and a server's reply.
///
/// NEGATIVE CONTROL (in the test): once a format mark is stored the same node states version 2,
/// which their parse reads as no bound.
#[test]
fn vault_fence_a_dark_build_states_version_one_byte_for_byte() {
    for held in [1, 4_096, 12_288, 16_384, u32::MAX as usize] {
        for crossed in [false, true] {
            let now = RecordsBound::of(held, crossed);
            let then = legacy::Bound::of(held, crossed);
            assert_eq!(now.value(), then.value());
            assert_eq!(legacy::Bound::parse(&now.value()), Some(then));
            assert_eq!(RecordsBound::parse(&then.value()), Some(now));
            assert_eq!(
                (now.format, now.bytes),
                (FORMAT_ONE, FORMAT_ONE_BYTES),
                "format 1 states no capacity: it is the stream bound every build shares"
            );
        }
    }

    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let store =
        Store::decode(VAULT, &encode_records(&Batch::get().base)).expect("the base's store");
    let crossed = Mark::at(dir.path().join("records.crossed"));
    let format = FormatMark::at(dir.path().join("records.format"));
    let stated = crate::crossing::stated(&crossed, &format, &store).expect("stated");
    let then = legacy::Bound::of(store.budgets().held, false);
    assert_eq!(stated.value(), then.value());
    assert!(!format.path().exists(), "stating stores nothing");

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(root.path(), Tuning::default());
    let answer = get(&running, &[], Some(&RecordsBound::uncrossed().value()));
    assert_eq!(answer.status, 200);
    let header = answer
        .header(crate::transport::RECORDS_BOUND_HEADER)
        .expect("a bound");
    assert_eq!(
        header,
        legacy::Bound::of(crate::membership_log::RecordBudgets::current().held, false).value()
    );
    assert!(
        posted_head(RecordsBound::uncrossed()).starts_with(&format!("POST /v1/{VAULT}/records "))
    );

    format.store(FORMAT_TWO).expect("the mark");
    let marked = crate::crossing::stated(&crossed, &format, &store).expect("stated");
    assert_eq!(
        marked.value(),
        format!("2 {} {FORMAT_ONE_BYTES} 2 uncrossed", store.budgets().held),
        "control"
    );
    assert_eq!(legacy::Bound::parse(&marked.value()), None, "control");
    format_mark_in(root.path())
        .store(FORMAT_TWO)
        .expect("the server's mark");
    // Its build reads no format 2, so it serves no record of the marked store (a `500`), and
    // states the mark's format beside the refusal.
    let marked = get(&running, &[], Some(&marked.value()));
    assert_eq!(marked.status, 500, "{}", marked.head);
    assert!(
        marked
            .header(crate::transport::RECORDS_BOUND_HEADER)
            .is_some_and(|value| value.starts_with("2 ")),
        "control: {}",
        marked.head
    );
    running.stop();
}

/// A node stores its format mark before it holds its first record of that format: a mark it
/// cannot store leaves the write unrun, and one that dies between the two holds the mark and none
/// of the records. A mark is never lowered, and one this build cannot read states no bound and
/// serves nothing.
///
/// NEGATIVE CONTROL (in the test): with [`Faults::format_after_write`] (the records first, then
/// the mark) the node that dies between holds the record and states format 1 beside it.
#[test]
fn vault_fence_the_mark_precedes_the_first_format_two_record() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let records = dir.path().join("records");
    let mark = FormatMark::at(dir.path().join("records.format"));
    let write = |path: &Path| {
        let path = path.to_path_buf();
        move || crate::fsx::write_atomic(&path, b"a format-2 record", 0o600)
    };

    inject(
        mark.path(),
        Faults {
            fail_store: true,
            ..Faults::default()
        },
    );
    assert!(crate::crossing::after_format_mark(&mark, FORMAT_TWO, write(&records)).is_err());
    assert!(!records.exists(), "a refused mark: the write never ran");
    assert_eq!(mark.stored().expect("readable"), FORMAT_ONE);

    inject(
        mark.path(),
        Faults {
            dies_between: true,
            ..Faults::default()
        },
    );
    assert!(crate::crossing::after_format_mark(&mark, FORMAT_TWO, write(&records)).is_err());
    assert_eq!(mark.stored().expect("readable"), FORMAT_TWO);
    assert!(!records.exists(), "the mark and none of the records");

    inject(mark.path(), Faults::default());
    let seen = crate::crossing::after_format_mark(&mark, FORMAT_TWO, || {
        assert!(!records.exists());
        mark.stored()
    })
    .expect("the write ran");
    assert_eq!(
        seen, FORMAT_TWO,
        "the write ran with the mark already stored"
    );
    assert!(mark.store(FORMAT_ONE).is_err(), "no mark names format 1");
    mark.store(FORMAT_TWO).expect("stored again, as it was");
    assert_eq!(mark.stored().expect("readable"), FORMAT_TWO);

    // A mark this build cannot read: no bound, and a server serves nothing.
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(root.path(), Tuning::default());
    for garbled in [
        &b""[..],
        b"aterm-secrets records format 1\n",
        b"aterm-secrets records format 02\n",
        b"aterm-secrets records format 2",
        b"aterm-secrets records crossed 1\n",
    ] {
        std::fs::write(format_mark_in(root.path()).path(), garbled).expect("garbled");
        assert!(format_mark_in(root.path()).stored().is_err(), "{garbled:?}");
        let answer = get(&running, &[], Some(&RecordsBound::uncrossed().value()));
        assert_eq!(answer.status, 500, "{garbled:?}: {}", answer.head);
        assert!(answer.records().is_empty());
        assert_eq!(answer.bound(), None);
    }
    running.stop();

    // NEGATIVE CONTROL: the records first, then the mark.
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let records = dir.path().join("records");
    let mark = FormatMark::at(dir.path().join("records.format"));
    inject(
        mark.path(),
        Faults {
            dies_between: true,
            format_after_write: true,
            ..Faults::default()
        },
    );
    assert!(crate::crossing::after_format_mark(&mark, FORMAT_TWO, write(&records)).is_err());
    assert!(records.exists(), "the mutant held the record");
    assert_eq!(
        mark.stored().expect("readable"),
        FORMAT_ONE,
        "and states format 1 beside it"
    );
}

/// A server answers a records GET one reply's causally closed prefix, under its reply cap: a
/// prefix says more follow and vouches no heads digest, and a puller that takes each prefix and
/// asks again past it holds the whole log.
///
/// NEGATIVE CONTROL (in the test): the server at the production cap answers the same log whole,
/// with its heads digest.
#[test]
fn vault_fence_a_server_pages_its_reply_and_vouches_no_digest_for_a_prefix() {
    let batch = Batch::get();
    let cap = 64 * 1024;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(
        root.path(),
        Tuning {
            reply_cap: cap,
            ..Tuning::default()
        },
    );
    let whole: usize = batch.base.iter().map(|r| r.wire_len()).sum();
    assert!(whole > 4 * cap, "the base takes several replies");
    let stated = RecordsBound::uncrossed().value();
    let mut puller = Store::for_vault(VAULT).expect("an empty store");
    let mut replies = 0;
    loop {
        let answer = get(&running, &puller.have(), Some(&stated));
        assert_eq!(answer.status, 200, "{}", answer.head);
        replies += 1;
        let records = answer.records();
        assert!(records.iter().map(|r| r.wire_len()).sum::<usize>() <= cap);
        let before = puller
            .log()
            .map_or(0, crate::membership_log::Log::held_count);
        let count = records.len();
        puller.ingest_records(records).expect("a prefix ingests");
        let after = puller
            .log()
            .map_or(0, crate::membership_log::Log::held_count);
        assert_eq!(
            after - before,
            count,
            "closed under parents: every record completes"
        );
        let more = answer.header(crate::transport::RECORDS_MORE_HEADER) == Some("1");
        assert_eq!(
            answer
                .header(crate::transport::RECORDS_HEADS_HEADER)
                .is_some(),
            !more,
            "a prefix vouches no digest: {}",
            answer.head
        );
        if !more {
            break;
        }
        assert!(replies < fixture::BASE, "every reply moves");
    }
    assert!(replies > 4, "{replies}");
    assert_eq!(
        puller.records_digest(),
        running.stored_records(VAULT).records_digest()
    );
    running.stop();

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(root.path(), Tuning::default());
    let answer = get(&running, &[], Some(&stated));
    assert_eq!(answer.records().len(), fixture::BASE, "control");
    assert_eq!(answer.header(crate::transport::RECORDS_MORE_HEADER), None);
    assert!(
        answer
            .header(crate::transport::RECORDS_HEADS_HEADER)
            .is_some()
    );
    running.stop();
}

/// What a format-2 node offers aterm v0.198.0 and v0.201.0 leaves their records file as it was,
/// run on what those builds do ([`legacy`], captured from their source): their parse reads its
/// version-2 bound as no bound; their server refuses its records POST (`format=2`) unread; and
/// their record readers refuse a version-2 record, and with it the whole stream it is in. The
/// stream is offered to a server of this build on the path such a server takes for a POST that
/// states no bound (format 1, not crossed: `records_input`'s staging, the same text at both
/// tags as here), and its records file keeps its SHA-256. Their GET of a format-2 node is
/// answered 409 ([`vault_fence_format_two_and_format_one_exchange_nothing`], with the exact
/// requests they send).
///
/// NEGATIVE CONTROL (in the test): their parse and gate read this build's version-1 bound and a
/// crossed POST as before, and the same stream without the version-2 record is taken.
#[test]
fn vault_fence_v0_198_and_v0_201_offered_format_two_keep_their_file() {
    let batch = Batch::get();
    let two = RecordsBound::uncrossed().at_format(FORMAT_TWO);
    for bound in [two, RecordsBound::here(true).at_format(FORMAT_TWO)] {
        let read = legacy::Bound::parse(&bound.value());
        assert_eq!(read, None, "{}", bound.value());
        assert_eq!(
            legacy::records_fence(fixture::BASE, true, read),
            Some(409),
            "a crossed one refuses it"
        );
        assert_eq!(
            legacy::post_gate(Some(FORMAT_POST_QUERY), read),
            Some(400),
            "refused unread"
        );
        assert!(
            posted_head(bound)
                .starts_with(&format!("POST /v1/{VAULT}/records?{FORMAT_POST_QUERY} ")),
            "this build's format-2 POST carries the query"
        );
    }
    let one = RecordsBound::here(true);
    assert_eq!(
        legacy::Bound::parse(&one.value()),
        Some(legacy::Bound::of(one.held as usize, true)),
        "control"
    );
    assert_eq!(
        legacy::post_gate(
            Some(crate::transport::CROSSED_POST_QUERY),
            legacy::Bound::parse(&one.value())
        ),
        None,
        "control"
    );

    let mut version_two = batch.removal[0].bytes().to_vec();
    version_two[4] = 2;
    assert!(!legacy::frames_as_a_record(&version_two));
    assert!(
        legacy::frames_as_a_record(batch.removal[0].bytes()),
        "control"
    );
    let mut stream = encode_records(&batch.removal);
    stream.extend_from_slice(&version_two);
    assert!(decode_records(&stream).is_err());

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(root.path(), Tuning::default());
    let before = records_file_digest(root.path());
    let offered = post(&running, None, None, &stream);
    assert_eq!(offered.status, 400, "{}", offered.head);
    assert_eq!(records_file_digest(root.path()), before);
    assert_eq!(held(&running), fixture::BASE);
    let control = post(&running, None, None, &encode_records(&batch.removal[..1]));
    assert_eq!(control.status, 204, "control: {}", control.head);
    assert_ne!(records_file_digest(root.path()), before, "control");
    running.stop();
}
