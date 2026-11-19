// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A vault server following its vault into records format 2 (tracker FL-148E2,
//! `crate::membership_log::follow`): a poster's or a peer's statement of format 2 stores no
//! mark, a seat's signed version-2 record does, and the mark is durable before the records
//! file or the slot holds that record. Once its mark names format 2 the server reads its file,
//! a POST's body and a peer's frames at that format, and it keeps a whole version-2 record a
//! source sends over the shed frame it holds, in memory and on its file.

use super::*;
use crate::crossing::{FORMAT_ONE, FORMAT_TWO, Faults, FormatMark, RecordsBound, inject};
use crate::membership_log::{
    Body as RecordBody, Entrant, EpochAudience, Log, MemberId, Reading as RecordsReading, Record,
    RecordId, Statement, Store, encode_records, store::decode_records_in,
};
use std::io::{Read, Write};

pub(super) const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
pub(super) const VAULT: &str = "team";

/// The vault's one seat, and its log's records as it signs them: a genesis, the `[everyone]`
/// epoch its `Put`s name, and whatever a test has it sign after.
pub(super) struct Seat {
    writer: [u8; 32],
    incarnation: [u8; 16],
    member: MemberId,
    log: Log,
    epoch: RecordId,
    pub(super) held: Vec<Record>,
}

impl Seat {
    pub(super) fn found() -> Self {
        let writer = [1; 32];
        let incarnation = [1; 16];
        let identity = crate::age::Identity::from_secret_bytes([0x41; 32]);
        let request = crate::commons::binding::Request::create(
            [0x31; 16],
            [0x42; 32],
            [0x21; 16],
            incarnation,
            &identity,
            &writer,
        )
        .expect("request");
        let member = request.binding().actor();
        let genesis = Record::found(
            &[7; 32],
            vec![Entrant {
                request,
                node_key: [0x81; 32],
                name: "mac-1".to_owned(),
                label: "m1".to_owned(),
            }],
            1_000,
        )
        .expect("genesis");
        let mut log = Log::new(genesis.clone()).expect("log");
        let epoch = log
            .draft(
                &member,
                1_500,
                RecordBody::Epoch {
                    audience: EpochAudience::Everyone,
                    generation: 1,
                    commitment: [7; 32],
                },
            )
            .sign(&writer, incarnation)
            .expect("signed");
        log.insert(epoch.clone()).expect("held");
        Self {
            writer,
            incarnation,
            member,
            log,
            epoch: *epoch.id(),
            held: vec![genesis, epoch],
        }
    }

    /// One more record: a whole `Put` signed as version 2 where `two`, else a format-1 `Name`.
    pub(super) fn sign(&mut self, two: bool) -> Record {
        let n = self.held.len() as u64;
        let record = if two {
            self.log
                .draft(
                    &self.member,
                    2_000 + n,
                    RecordBody::Put {
                        audience: EpochAudience::Everyone,
                        epoch: self.epoch,
                        salt: [self.held.len() as u8; 32],
                        ciphertext: vec![0xc2; 64].into(),
                    },
                )
                .sign_two(
                    &self.writer,
                    self.incarnation,
                    Some(&Statement {
                        supersedes: Vec::new(),
                        carries: None,
                    }),
                )
        } else {
            self.log
                .draft(
                    &self.member,
                    2_000 + n,
                    RecordBody::Name {
                        name: format!("mac-one-{n}"),
                    },
                )
                .sign(&self.writer, self.incarnation)
        }
        .expect("signed");
        assert_eq!(record.version(), if two { 2 } else { 1 });
        self.log.insert(record.clone()).expect("held");
        self.held.push(record.clone());
        record
    }

    /// A format-1 record the seat signs on its log's first `held` records alone, which a store
    /// that holds only those takes complete. The seat's log does not take it.
    fn sign_on_prefix(&self, held: usize) -> Record {
        let mut log = Log::new(self.held[0].clone()).expect("log");
        for record in &self.held[1..held] {
            log.insert(record.clone()).expect("held");
        }
        log.draft(
            &self.member,
            9_000,
            RecordBody::Name {
                name: "on-the-prefix".to_owned(),
            },
        )
        .sign(&self.writer, self.incarnation)
        .expect("signed")
    }
}

/// `whole`'s shed frame: the same record with its ciphertext shed, under the same id.
pub(super) fn shed(whole: &Record) -> Record {
    let frame = Record::decode_in(
        &whole.shed_frame().expect("a version-2 record"),
        RecordsReading::Follow,
    )
    .expect("the frame verifies");
    assert!(frame.is_shed() && !whole.is_shed());
    assert_eq!(frame.id(), whole.id());
    frame
}

/// A node whose build reads format 2, and that suffers nothing.
pub(super) fn follows() -> Faults {
    Faults {
        follows: true,
        ..Faults::default()
    }
}

/// What a format-2 node states of its records.
pub(super) fn two() -> RecordsBound {
    RecordsBound::here(false).at_format(FORMAT_TWO)
}

/// One answer, read whole off the wire.
pub(super) struct Answer {
    pub(super) status: u16,
    pub(super) head: String,
    pub(super) body: Vec<u8>,
}

impl Answer {
    /// The records bound its head states, read as this build reads it.
    pub(super) fn bound(&self) -> Option<RecordsBound> {
        self.head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(crate::transport::RECORDS_BOUND_HEADER)
                .then(|| RecordsBound::parse(value.trim()))
                .flatten()
        })
    }
}

/// A vault server over a store folder of its own, whose node suffers what a test injects.
pub(super) struct Server {
    pub(super) root: aterm_tempfile::TempDir,
    running: Option<Running>,
}

impl Server {
    /// One whose node suffers `faults`, holding nothing.
    pub(super) fn new(faults: Faults) -> Self {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let server = Self {
            root,
            running: None,
        };
        server.suffers(faults);
        server.started()
    }

    /// One whose node suffers `faults`, holding `records`, posted as a format-1 node posts.
    pub(super) fn holding(faults: Faults, records: &[Record]) -> Self {
        let server = Self::new(faults);
        let posted = server.post(None, &RecordsBound::uncrossed(), &encode_records(records));
        assert_eq!(posted.status, 204, "{}", posted.head);
        server
    }

    pub(super) fn started(mut self) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        self.running = Some(
            launch(
                listener,
                &Options {
                    listen: "127.0.0.1:0".parse().expect("addr"),
                    store: self.root.path().to_path_buf(),
                    advertise: None,
                    pins: Vec::new(),
                    hold: Duration::from_millis(200),
                    keep_awake: false,
                },
                Tuning::default(),
            )
            .expect("started"),
        );
        self
    }

    /// The same store under a server started anew, as after the process ended.
    pub(super) fn restarted(mut self) -> Self {
        if let Some(running) = self.running.take() {
            running.stop();
        }
        self.started()
    }

    pub(super) fn running(&self) -> &Running {
        self.running.as_ref().expect("running")
    }

    pub(super) fn mark(&self) -> FormatMark {
        FormatMark::at(self.root.path().join(format!("{VAULT}.format")))
    }

    pub(super) fn suffers(&self, faults: Faults) {
        inject(self.mark().path(), faults);
    }

    pub(super) fn format(&self) -> u32 {
        self.mark().stored().expect("the mark")
    }

    /// The bytes of its records file; none where it has none.
    pub(super) fn file(&self) -> Vec<u8> {
        crate::records_storage::reader_path(&self.root.path().join(format!("{VAULT}.records")))
            .ok()
            .and_then(|path| std::fs::read(path).ok())
            .unwrap_or_default()
    }

    /// Whether its records file holds `record` in exactly the form given.
    pub(super) fn file_holds(&self, record: &Record) -> bool {
        let wire = record.wire_bytes().expect("the record's bytes");
        self.file()
            .windows(wire.len())
            .any(|window| window == &wire[..])
    }

    /// Its store, after a look at its file.
    pub(super) fn store(&self) -> Store {
        self.running().stored_records(VAULT)
    }

    pub(super) fn held(&self) -> usize {
        self.store().log().map_or(0, Log::held_count)
    }

    pub(super) fn exchange(&self, request: &[u8]) -> Answer {
        let mut stream = TcpStream::connect(self.running().addr).expect("connect");
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

    /// A records POST of `body`, with `query`, stating `bound`.
    pub(super) fn post(&self, query: Option<&str>, bound: &RecordsBound, body: &[u8]) -> Answer {
        let query = query.map_or_else(String::new, |query| format!("?{query}"));
        let mut request = format!(
            "POST /v1/{VAULT}/records{query} HTTP/1.1\r\nHost: 127.0.0.1\r\n{}: \
             {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            crate::transport::RECORDS_BOUND_HEADER,
            bound.value(),
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(body);
        self.exchange(&request)
    }

    /// A records POST of `records` as a format-2 node sends it.
    pub(super) fn post_two(&self, records: &[&Record]) -> Answer {
        self.post(
            Some(crate::crossing::FORMAT_POST_QUERY),
            &two(),
            &encode_records(records.iter().copied()),
        )
    }

    /// A records GET naming nothing, stating `bound`, of a server that does not wait.
    pub(super) fn get(&self, bound: &RecordsBound) -> Answer {
        self.exchange(
            format!(
                "GET /v1/{VAULT}/records HTTP/1.1\r\nHost: 127.0.0.1\r\n{}: 1\r\n{}: \
                 {}\r\nConnection: close\r\n\r\n",
                crate::transport::NO_WAIT_HEADER,
                crate::transport::RECORDS_BOUND_HEADER,
                bound.value()
            )
            .as_bytes(),
        )
    }

    /// This server's vault as a peer pull finds it.
    pub(super) fn as_peer(&self) -> crate::discover::DiscoveredVault {
        crate::discover::DiscoveredVault {
            url: format!("http://{}/v1/{VAULT}", self.running().addr),
            txt: Some(VaultTxt {
                vault: VAULT.to_owned(),
                signer: "0000-0000-0000-0000-0000".to_owned(),
                seq: 1,
                head: [0; 32],
                label: Some("m2".to_owned()),
                rec: None,
            }),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop();
        }
        inject(self.mark().path(), Faults::default());
    }
}

/// A server whose build reads format 2 follows its vault there on a seat's signed version-2
/// record a format-2 poster sends, mark first. A server whose build reads none (every build
/// while `READS_FORMAT` names no release) answers that POST the fence's `409`, stores no mark
/// and leaves its file as it was. On a build that reads format 2, a poster that states format
/// 2 and brings format-1 records alone, or a version-2 record that waits on a parent it did not
/// send, is answered the same `409` and nothing of it is taken. A server that dies between its
/// mark and the records holds the mark and the file it held, states format 2, and takes the
/// same POST again as any format-2 server does. A server that followed serves the record to a
/// puller that states format 2, refuses one that states format 1, takes the seat's next
/// version-2 record by an append, and reads its file at format 2 when started anew.
///
/// NEGATIVE CONTROLS, each run against this test and seen to fail:
/// - `Faults::format_after_write` on the server that dies (the records first, then the mark):
///   ``the mark is durable before the file holds a format-2 record``, `left: (1, true)`
///   against `right: (2, false)`.
/// - `Faults::mark_on_header` (the poster's statement is the evidence): ``a statement of
///   format 2 stores no mark``, `left: 2` against `right: 1`.
/// - `Faults::mark_on_waiting` (a version-2 record that only waits is the evidence): ``a
///   version-2 record that waits is no evidence``, `left: 2` against `right: 1`.
/// - [`FormatMark::follows`] answering yes for every node (no gate): ``a build that reads no
///   format 2 refuses a format-2 poster``, `left: 204` against `right: 409`.
#[test]
fn vault_checkpoint_a_server_follows_a_posted_format_two_record_mark_first() {
    let mut seat = Seat::found();
    let base = seat.held.clone();
    let n1 = seat.sign(false);
    let v2 = seat.sign(true);

    // A build that reads no format 2: the fence's refusal, and nothing else.
    let dark = Server::holding(Faults::default(), &base);
    let before = dark.file();
    let refused = dark.post_two(&[&n1, &v2]);
    assert_eq!(
        refused.status, 409,
        "a build that reads no format 2 refuses a format-2 poster: {}",
        refused.head
    );
    assert_eq!(
        refused.bound(),
        Some(RecordsBound::uncrossed()),
        "and states version 1"
    );
    assert!(!dark.mark().path().exists());
    assert_eq!((dark.file(), dark.held()), (before.clone(), 2));
    drop(dark);

    // A build that reads it: what the poster states is no evidence.
    let claims = Server::holding(follows(), &base);
    let refused = claims.post_two(&[&n1]);
    assert_eq!(
        claims.format(),
        FORMAT_ONE,
        "a statement of format 2 stores no mark"
    );
    assert_eq!(refused.status, 409, "{}", refused.head);
    let refused = claims.post_two(&[&v2]);
    assert_eq!(
        claims.format(),
        FORMAT_ONE,
        "a version-2 record that waits is no evidence"
    );
    assert_eq!(refused.status, 409, "{}", refused.head);
    assert!(!claims.mark().path().exists());
    assert_eq!((claims.file(), claims.held()), (before.clone(), 2));
    assert!(
        !claims.store().contains(n1.id()) && !claims.store().contains(v2.id()),
        "nothing a refused poster sent is kept"
    );
    drop(claims);

    // Mark first: a server that dies between the two.
    let dies = Server::holding(
        Faults {
            dies_between: true,
            ..follows()
        },
        &base,
    );
    let died = dies.post_two(&[&n1, &v2]);
    assert_eq!(died.status, 500, "{}", died.head);
    assert_eq!(
        (dies.format(), dies.file_holds(&v2)),
        (FORMAT_TWO, false),
        "the mark is durable before the file holds a format-2 record"
    );
    assert_eq!(dies.file(), before);
    dies.suffers(follows());
    let dies = dies.restarted();
    assert_eq!(
        dies.get(&RecordsBound::uncrossed()).status,
        409,
        "it states format 2 beside the store it held"
    );
    assert_eq!(dies.post_two(&[&n1, &v2]).status, 204);
    assert!(dies.file_holds(&v2) && dies.store().holds(v2.id()));
    drop(dies);

    // A server that follows.
    let server = Server::holding(follows(), &base);
    let followed = server.post_two(&[&n1, &v2]);
    assert_eq!(followed.status, 204, "{}", followed.head);
    assert_eq!(server.format(), FORMAT_TWO);
    assert!(server.file_holds(&v2), "the file holds the record, whole");
    let store = server.store();
    assert_eq!(store.reading(), RecordsReading::Format(FORMAT_TWO));
    assert!(store.holds(n1.id()) && store.holds(v2.id()));
    let served = server.get(&two());
    assert_eq!(served.status, 200, "{}", served.head);
    assert_eq!(served.bound(), Some(two()));
    let sent = decode_records_in(&served.body, 16, RecordsReading::Format(FORMAT_TWO))
        .expect("a format-2 reader reads the reply");
    assert!(sent.iter().any(|record| record.id() == v2.id()) && sent.len() == 4);
    assert_eq!(server.get(&RecordsBound::uncrossed()).status, 409);

    // The seat's next version-2 record is taken as any record, on the end of the file.
    let next = seat.sign(true);
    let held = server.file();
    assert_eq!(server.post_two(&[&next]).status, 204);
    let grown = server.file();
    assert!(grown.starts_with(&held) && server.file_holds(&next));
    assert_eq!(
        grown.len(),
        held.len() + next.wire_len(),
        "an append of the record alone"
    );

    // Started anew, it reads its file at the format its mark names.
    let server = server.restarted();
    let store = server.store();
    assert_eq!(store.reading(), RecordsReading::Format(FORMAT_TWO));
    assert!(store.holds(v2.id()) && store.holds(next.id()));
    assert_eq!(server.held(), 5);
}

/// A server that followed its vault into format 2 and then runs a build that reads none (one
/// before the reader, which every build is while `READS_FORMAT` names no release) takes no
/// record. That build reads the followed file as the records before its first version-2 one;
/// a write from that store would replace the file and lose every record past it. So a POST
/// from a format-2 poster is refused before its body is read, the file is byte for byte what
/// the build that read it left, and that build reads every record of it again.
///
/// NEGATIVE CONTROL: `Faults::writes_unread_format` (the records are taken), run against this
/// test and seen to fail with ``a server that does not read its marked format writes
/// nothing``, `left: (204, false)` against `right: (500, true)`: the file was replaced by the
/// three records that build read and the posted one.
#[test]
fn vault_checkpoint_a_server_that_reads_no_format_two_writes_no_followed_file() {
    let mut seat = Seat::found();
    let base = seat.held.clone();
    let n1 = seat.sign(false);
    let v2 = seat.sign(true);
    let after = seat.sign(false);
    let server = Server::holding(follows(), &base);
    assert_eq!(server.post_two(&[&n1, &v2, &after]).status, 204);
    let followed = server.file();
    assert!(server.file_holds(&v2) && server.file_holds(&after));

    // A build that reads no format 2, on the followed store.
    server.suffers(Faults::default());
    let server = server.restarted();
    let read = server.store();
    assert_eq!(read.reading(), RecordsReading::ONE);
    assert!(read.holds(n1.id()) && !read.holds(v2.id()) && !read.holds(after.id()));
    // A record that store takes complete: one the seat signed on the records before `v2`.
    let fork = seat.sign_on_prefix(3);
    let refused = server.post_two(&[&fork]);
    assert_eq!(
        (refused.status, server.file() == followed),
        (500, true),
        "a server that does not read its marked format writes nothing: {}",
        refused.head
    );
    assert_eq!(server.format(), FORMAT_TWO, "the mark is never lowered");

    // The build that reads format 2 again.
    server.suffers(follows());
    let server = server.restarted();
    assert!(server.store().holds(v2.id()) && server.store().holds(after.id()));
    assert_eq!(server.post_two(&[&fork]).status, 204);
    assert!(server.file_holds(&fork) && server.file().starts_with(&followed));
}

/// A format-2 server holding `seat`'s records so far, each in the form `seat` holds it but
/// `frame`, which the server holds shed. It followed on that frame: a shed version-2 record a
/// seat signed is evidence as the whole one is.
fn holding_frame(seat: &Seat, frame: &Record) -> Server {
    let server = Server::holding(follows(), &seat.held[..2]);
    let held: Vec<Record> = seat.held[2..]
        .iter()
        .map(|record| {
            if record.id() == frame.id() {
                shed(record)
            } else {
                record.clone()
            }
        })
        .collect();
    let posted = server.post_two(&held.iter().collect::<Vec<_>>());
    assert_eq!(posted.status, 204, "{}", posted.head);
    assert_eq!(server.format(), FORMAT_TWO);
    let store = server.store();
    assert!(
        store
            .record(frame.id())
            .is_some_and(crate::membership_log::Record::is_shed)
    );
    server
}

/// A server that holds a version-2 record's shed frame keeps the whole record a poster sends.
/// The two share one id, so a server that asks only whether it holds the id answers `204` and
/// drops the ciphertext. The server's store holds the record whole after the POST, and a POST
/// of the same whole record again changes nothing; a shed frame posted to a server that holds
/// the whole record leaves it whole.
///
/// NEGATIVE CONTROLS, each a change to `apply_records` run against this test and seen to fail
/// with ``the server keeps the whole record a poster sends over the frame it holds``:
/// - both early `204`s asking the store's ids alone (`brings_nothing` without
///   `Store::lacks_whole`, as before).
/// - `records.retain` dropping every record whose id the copy holds (as before).
#[test]
fn vault_checkpoint_a_server_keeps_a_posted_whole_record_over_the_frame_it_holds() {
    let mut seat = Seat::found();
    seat.sign(false);
    let v2 = seat.sign(true);
    let server = holding_frame(&seat, &v2);

    let posted = server.post_two(&[&v2]);
    assert_eq!(posted.status, 204, "{}", posted.head);
    let kept = server.store();
    assert!(
        kept.record(v2.id()).is_some_and(|held| !held.is_shed()),
        "the server keeps the whole record a poster sends over the frame it holds"
    );
    assert_eq!(server.held(), 4, "one record, in another form");

    // Again, and then its frame: neither changes what is held.
    let clones = server.running().shared.post_clones.load(Ordering::SeqCst);
    assert_eq!(server.post_two(&[&v2]).status, 204);
    assert_eq!(server.post_two(&[&shed(&v2)]).status, 204);
    assert_eq!(
        server.running().shared.post_clones.load(Ordering::SeqCst),
        clones,
        "a record held as it was sent, or better, copies no store"
    );
    assert!(
        server
            .store()
            .record(v2.id())
            .is_some_and(|held| !held.is_shed()),
        "a frame never takes a whole record's place"
    );
}

/// A whole version-2 record that took its shed frame's place is written to the server's file,
/// not kept in memory alone. The log holds as many complete records as before, so a server
/// that reads "no more complete records" as "only waiting records came" answers `204` with
/// the ciphertext on no disk, and loses it when it stops. The file holds the whole record
/// once the POST is answered, and a server started anew on that file holds it whole.
///
/// NEGATIVE CONTROL: `take_copy` comparing the copy's count of complete records alone (`held
/// == complete_before.0`, as before), run against this test and seen to fail with ``a 204
/// means the whole record is on the file``.
#[test]
fn vault_checkpoint_a_whole_record_that_replaced_a_frame_is_written_to_the_file() {
    let mut seat = Seat::found();
    seat.sign(false);
    let v2 = seat.sign(true);
    let last = seat.sign(false);
    let server = holding_frame(&seat, &v2);
    assert!(server.file_holds(&shed(&v2)) && !server.file_holds(&v2));

    assert_eq!(server.post_two(&[&v2]).status, 204);
    assert!(
        server.file_holds(&v2),
        "a 204 means the whole record is on the file"
    );
    assert_eq!(
        server.file(),
        encode_records(&seat.held),
        "the file holds exactly the log, the record after the frame's place too"
    );
    let server = server.restarted();
    let store = server.store();
    assert!(store.record(v2.id()).is_some_and(|held| !held.is_shed()));
    assert!(store.holds(last.id()));
    assert_eq!(server.held(), 5);
}

/// A server plans an append by bytes, not by a count of records. Its slot knows the file holds
/// the log's first records; once a whole version-2 record takes its shed frame's place among
/// them, the file is shorter than those records and holds the frame. A write that put only
/// the records past that count on the file's end would leave the frame there under a store
/// that holds the record whole. So such a write replaces the file, which then holds exactly
/// the store's records. A store whose first records are still the file's is appended to, on
/// the same file.
///
/// NEGATIVE CONTROL: `persist_records` planning its append by `on_file` alone (the
/// `held_len_to` guard dropped, as before), run against this test and seen to fail with ``the
/// file holds the whole record``: the file kept the frame and gained the new record alone.
#[test]
fn vault_checkpoint_a_server_plans_its_append_by_bytes() {
    let mut seat = Seat::found();
    seat.sign(false);
    let v2 = seat.sign(true);
    seat.sign(false);
    let server = holding_frame(&seat, &v2);
    let shared = &server.running().shared;
    let path = crate::records_storage::reader_path(&shared.records_path(VAULT)).expect("the file");
    let slot = |shared: &Shared| {
        let state = shared.lock_server_state();
        let slot = &state.records[VAULT];
        (slot.store.clone(), slot.stamp, slot.on_file)
    };
    let persist = |store: &Store, stamp: Option<fsx::FileStamp>, on_file: Option<usize>| {
        let _writes = shared
            .record_writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _writing = Writing::mark(shared, VAULT);
        persist_records(shared, VAULT, store, stamp, on_file)
            .map_err(|failure| failure.error)
            .expect("written")
    };

    // The store's first records are the file's: one more record goes on the file's end.
    let (mut store, stamp, on_file) = slot(shared);
    assert_eq!(on_file, Some(5));
    let more = seat.sign(false);
    assert_eq!(store.ingest_records(vec![more.clone()]).expect("held"), 1);
    let before = std::fs::read(&path).expect("the file");
    let written = persist(&store, stamp, on_file).expect("a stamp");
    let stamp = stamp.expect("the slot's stamp");
    assert_eq!(
        (written.dev, written.ino),
        (stamp.dev, stamp.ino),
        "an append: the same file"
    );
    let appended = std::fs::read(&path).expect("the file");
    assert!(appended.starts_with(&before) && appended.len() == before.len() + more.wire_len());

    // The whole record takes the frame's place, and one more record comes with it.
    let last = seat.sign(false);
    assert_eq!(
        store
            .ingest_records(vec![v2.clone(), last.clone()])
            .expect("held"),
        2
    );
    assert_eq!(store.held_len_to(6), Some(appended.len() + 64));
    persist(&store, Some(written), Some(6));
    let file = std::fs::read(&path).expect("the file");
    let whole = v2.wire_bytes().expect("the record's bytes");
    assert!(
        file.windows(whole.len()).any(|window| window == &whole[..]),
        "the file holds the whole record"
    );
    assert_eq!(file, store.read_held().expect("the store's records"));
}

/// A server whose build reads format 2 asks a peer that refused its records pull for a later
/// format again, stating format 2, once the pass's pulls have ended, and follows on a seat's
/// signed version-2 record in the answer, mark first, as it does a poster's. A server whose
/// build reads none asks that peer once, stores no mark and takes nothing.
///
/// NEGATIVE CONTROL: [`FormatMark::follows`] answering yes for every node (no gate), run
/// against this test and seen to fail with ``a build that reads no format 2 asks a peer
/// once``, `left: 2` against `right: 1`.
#[test]
fn vault_checkpoint_a_server_asks_a_peer_that_refused_for_a_later_format_again() {
    let mut seat = Seat::found();
    let base = seat.held.clone();
    let n1 = seat.sign(false);
    let v2 = seat.sign(true);
    let source = Server::holding(follows(), &base);
    assert_eq!(source.post_two(&[&n1, &v2]).status, 204);
    assert_eq!(source.format(), FORMAT_TWO);
    let asked = || source.running().records_gets();

    let dark = Server::holding(Faults::default(), &base);
    let before = (dark.file(), asked());
    let _ = dark.running().pull_from_peers(&[source.as_peer()]);
    assert_eq!(
        asked() - before.1,
        1,
        "a build that reads no format 2 asks a peer once"
    );
    assert!(!dark.mark().path().exists());
    assert_eq!((dark.file(), dark.held()), (before.0, 2));
    drop(dark);

    let puller = Server::holding(follows(), &base);
    let before = asked();
    assert_eq!(puller.running().pull_from_peers(&[source.as_peer()]), 1);
    assert_eq!(asked() - before, 2, "refused at format 1, asked again");
    assert_eq!(puller.format(), FORMAT_TWO);
    assert!(puller.store().holds(v2.id()) && puller.file_holds(&v2));
    // Its next pull states format 2 and brings what the source holds since.
    let next = seat.sign(true);
    assert_eq!(source.post_two(&[&next]).status, 204);
    assert_eq!(puller.running().pull_from_peers(&[source.as_peer()]), 1);
    assert!(puller.store().holds(next.id()) && puller.file_holds(&next));
}

/// A seat of a vault two Macs found: its entrant, its member id, and its signing key.
struct Founder {
    entrant: Entrant,
    member: MemberId,
    writer: [u8; 32],
    incarnation: [u8; 16],
}

impl Founder {
    fn new(n: u8) -> Self {
        let writer = [n; 32];
        let incarnation = [n; 16];
        let identity = crate::age::Identity::from_secret_bytes([0x40 + n; 32]);
        let request = crate::commons::binding::Request::create(
            [0x31; 16],
            [0x42; 32],
            [0x20 + n; 16],
            incarnation,
            &identity,
            &writer,
        )
        .expect("request");
        Self {
            member: request.binding().actor(),
            entrant: Entrant {
                request,
                node_key: [0x80 + n; 32],
                name: format!("mac-{n}"),
                label: format!("m{n}"),
            },
            writer,
            incarnation,
        }
    }
}

/// A seat another server of the pass holds removed moves no server across the format line. A
/// server whose build reads format 2 pulls from two peers: one states format 2 and holds a
/// version-2 record a seat signed while it held its seat, the other states format 1 and holds
/// that seat's `Remove`. The pull of the first follows nothing itself: it takes nothing, stores
/// no mark, and names the peer for after the pass. Once the other pull has taken the removal,
/// the first peer is asked again stating format 2, its record is no seat's, no mark is stored
/// and the record is not kept; a whole pass over both peers ends the same way, whichever
/// answers first. A server none of whose peers holds the removal follows that same peer on
/// that same record, so it is the removal that holds the first one back.
///
/// NEGATIVE CONTROL, a change to the code run against this test and seen to fail:
/// `pull_from_peer` asking the refusing peer again at once (`follow_later_peers` called where
/// it pushes the peer to `later`): ``a pull follows nothing itself``, `left: (2, 2)` against
/// `right: (1, 1)`.
#[test]
fn vault_checkpoint_a_server_follows_only_after_the_pass_took_its_other_pulls() {
    let (one, other) = (Founder::new(1), Founder::new(2));
    let genesis = Record::found(
        &[7; 32],
        vec![one.entrant.clone(), other.entrant.clone()],
        1_000,
    )
    .expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let epoch = log
        .draft(
            &one.member,
            1_500,
            RecordBody::Epoch {
                audience: EpochAudience::Everyone,
                generation: 1,
                commitment: [7; 32],
            },
        )
        .sign(&one.writer, one.incarnation)
        .expect("signed");
    log.insert(epoch.clone()).expect("held");
    // The seat the vault is about to remove signs a version-2 record.
    let v2 = log
        .draft(
            &other.member,
            2_000,
            RecordBody::Put {
                audience: EpochAudience::Everyone,
                epoch: *epoch.id(),
                salt: [3; 32],
                ciphertext: vec![0xc2; 64].into(),
            },
        )
        .sign_two(
            &other.writer,
            other.incarnation,
            Some(&Statement {
                supersedes: Vec::new(),
                carries: None,
            }),
        )
        .expect("signed");
    let removal = log
        .draft(
            &one.member,
            2_000,
            RecordBody::Remove {
                member: other.member,
                cutoff: *genesis.id(),
            },
        )
        .sign(&one.writer, one.incarnation)
        .expect("signed");
    let base = [genesis.clone(), epoch.clone()];

    let later = Server::holding(follows(), &base);
    assert_eq!(later.post_two(&[&v2]).status, 204);
    assert_eq!(later.format(), FORMAT_TWO);
    let removed = Server::holding(Faults::default(), &base);
    assert_eq!(
        removed
            .post(
                None,
                &RecordsBound::uncrossed(),
                &encode_records([&removal])
            )
            .status,
        204
    );
    let asked = || later.running().records_gets();
    let url_of = |server: &Server| format!("http://{}/v1/{VAULT}", server.running().addr);
    let wants = [Want {
        vault: VAULT.to_owned(),
        due: Due {
            envelope: false,
            records: true,
        },
        named_seq: None,
    }];

    // Pull by pull: the pull of the format-2 peer follows nothing itself.
    let puller = Server::holding(follows(), &base);
    let shared = &puller.running().shared;
    let before = (puller.file(), asked());
    let pulled = pull_from_peer(shared, later.running().addr, &url_of(&later), &wants);
    assert_eq!(
        (asked() - before.1, puller.format()),
        (1, FORMAT_ONE),
        "a pull follows nothing itself"
    );
    assert_eq!((pulled.stored, pulled.later.len()), (0, 1));
    assert_eq!(puller.file(), before.0);
    let other_pull = pull_from_peer(shared, removed.running().addr, &url_of(&removed), &wants);
    assert_eq!((other_pull.stored, other_pull.later.len()), (1, 0));
    assert!(puller.store().holds(removal.id()));
    assert_eq!(follow_later_peers(shared, &pulled.later), 0);
    assert_eq!(asked() - before.1, 2, "asked again, stating format 2");
    assert_eq!(
        puller.format(),
        FORMAT_ONE,
        "a seat another peer of the pass removed moves no server across the line"
    );
    assert!(!puller.store().contains(v2.id()) && !puller.file_holds(&v2));
    assert!(!puller.mark().path().exists());
    drop(puller);

    // A whole pass over both peers, whichever answers first.
    let puller = Server::holding(follows(), &base);
    let before = asked();
    assert_eq!(
        puller
            .running()
            .pull_from_peers(&[later.as_peer(), removed.as_peer()]),
        1
    );
    assert_eq!(asked() - before, 2);
    assert!(puller.store().holds(removal.id()));
    assert_eq!(puller.format(), FORMAT_ONE);
    assert!(!puller.store().contains(v2.id()) && !puller.file_holds(&v2));
    drop(puller);

    // The same peer, beside one that holds no removal of the seat.
    let holds_none = Server::holding(Faults::default(), &base);
    let stale = Server::holding(follows(), &base);
    assert_eq!(
        stale
            .running()
            .pull_from_peers(&[later.as_peer(), holds_none.as_peer()]),
        1
    );
    assert_eq!(
        (stale.format(), stale.store().holds(v2.id())),
        (FORMAT_TWO, true),
        "a seat's version-2 record is the evidence while its log holds the seat"
    );
}
