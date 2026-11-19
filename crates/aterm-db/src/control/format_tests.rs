// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The records format on the frontier wires (tracker FL-148E): a Mac with no format mark writes
//! and reads every frame as each release does; a Mac whose mark names format 2, on a build that
//! reads it, states its format and byte capacity, and only to a peer known to read them; a peer
//! of another format is refused at its bound; and a record a root dropped reads as one held.

use super::*;
use crate::control::faults;
use crate::control::frontier::{
    FORMAT_TRAILER, FORMATTED, SERVED, held_and_known, most_known, overlay, receive_batch, snapshot,
};
use aterm_secrets::{
    commons::hearing::{self, LIFTED, Reports, Version},
    crossing::{FORMAT_ONE, FORMAT_TWO, capacity},
    membership_log::{EpochAudience, Reading, Statement, Store, checkpoint},
};

/// The seed and incarnation Lab Mac `member` signs its records with.
fn signer(member: usize) -> ([u8; 32], [u8; 16]) {
    ([member as u8 + 31; 32], [member as u8 + 3; 16])
}

/// Where the format mark of the custody at `paths` is.
fn mark_of(paths: &client::Paths) -> PathBuf {
    paths.records_file().with_extension("format")
}

/// Store the format mark of the custody at `paths` at format 2, as a Mac that followed its vault
/// holds it.
fn mark_two(paths: &client::Paths) {
    aterm_secrets::fsx::write_atomic(&mark_of(paths), b"aterm-secrets records format 2\n", 0o600)
        .unwrap();
}

/// Make `log` the records the custody at `paths` holds, a custody whose mark names format 2:
/// the file a Mac that followed its vault holds, which no writer of this build makes yet (a
/// member's first version-2 write is tracker FL-148F's).
fn keep_two(paths: &client::Paths, log: &Log) {
    let wire = read_records(log.records()).unwrap();
    aterm_secrets::fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
}

/// The custody an endpoint keeps its records in.
fn custody_of(endpoint: &Endpoint) -> &client::Paths {
    endpoint.paths.as_ref().unwrap()
}

/// A store of version reports for Lab Mac `member` that keeps Mac `peer`'s time-signed report
/// of `version`, verified under that Mac's binding.
fn hearing_of(lab: &Lab, member: usize, peer: usize, version: Version) -> Arc<Mutex<Reports>> {
    let now_ms = aterm_time::unix_millis().unwrap();
    let own = lab.requests[member].binding();
    let theirs = lab.requests[peer].binding();
    let mut reports = Reports::new(lab.trust, own);
    reports.set_roster([own, theirs], now_ms);
    let wire = hearing::report_at(
        &signer(peer).0,
        *theirs.network(),
        &theirs.actor(),
        version,
        now_ms,
    )
    .unwrap();
    assert!(reports.accept(hearing::relayed(&wire, theirs, now_ms).unwrap(), now_ms));
    Arc::new(Mutex::new(reports))
}

/// Give `endpoint` everything that lets a Mac state a later format but the mark itself: a build
/// that reads format 2, the kept report of Mac `peer` at this build's release, and a floor that
/// report meets.
fn all_but_the_mark(lab: &Lab, endpoint: &mut Endpoint, member: usize, peer: usize) {
    client::test_reads_format_two(custody_of(endpoint));
    endpoint.reports = Some(hearing_of(lab, member, peer, Version::own()));
    endpoint.format_floor = Some(LIFTED);
}

/// One end of a socket that keeps the bytes it writes.
struct Taped {
    stream: TcpStream,
    written: Vec<u8>,
}

impl Read for Taped {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(out)
    }
}

impl Write for Taped {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.stream.write(bytes)?;
        self.written.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

/// One session on `wire` over loopback, each side with the repair state it keeps of the other:
/// what each side returned and the bytes it wrote, the initiator's first.
fn session(
    initiator: (&Endpoint, &mut Repair),
    responder: (&Endpoint, &mut Repair),
    wire: Wire,
) -> [(Result<Report>, Vec<u8>); 2] {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let run = |endpoint: &Endpoint,
               peer: &Endpoint,
               repair: &mut Repair,
               stream: TcpStream,
               side: Side| {
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let mut stream = Taped {
            stream,
            written: Vec::new(),
        };
        let result = endpoint.connected_until(
            &mut stream,
            Some(&proof_of(peer)),
            side,
            repair,
            EXCHANGE.horizon(Instant::now()),
            Protocol::Frontier(wire),
        );
        (result, stream.written)
    };
    thread::scope(|scope| {
        let answered = scope.spawn(|| {
            let stream = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap()
            .0;
            run(
                responder.0,
                initiator.0,
                responder.1,
                stream,
                Side::Responder,
            )
        });
        let dialed = run(
            initiator.0,
            responder.0,
            initiator.1,
            TcpStream::connect(address).unwrap(),
            Side::Initiator,
        );
        [dialed, answered.join().unwrap()]
    })
}

/// The first `count` frames of `tape`: each a four-byte little-endian length and its body.
fn frames(tape: &[u8], count: usize) -> Vec<&[u8]> {
    let mut rest = tape;
    (0..count)
        .map(|n| {
            let (length, after) = rest.split_at(4);
            let length = u32::from_le_bytes(length.try_into().unwrap()) as usize;
            assert!(length <= after.len(), "frame {n} is cut short");
            let (frame, after) = after.split_at(length);
            rest = after;
            frame
        })
        .collect()
}

/// The metadata frame every release since 0.153 writes, built here from its fields: the magic,
/// the trust context, the request, the held bound, the crossed byte and the flags.
fn released_metadata(
    trust: Trust,
    request: &Request,
    held: u32,
    crossed: bool,
    flags: u8,
) -> Vec<u8> {
    let mut wire = b"ACX4".to_vec();
    wire.extend_from_slice(&trust.network);
    wire.extend_from_slice(&trust.authority);
    wire.extend_from_slice(&trust.genesis);
    wire.extend_from_slice(request.bytes());
    wire.extend_from_slice(&held.to_be_bytes());
    wire.push(u8::from(crossed));
    wire.push(flags);
    wire
}

/// The statement of what a Mac holds every release since 0.154 writes, built here from its
/// fields: the magic, the held bound, the crossed byte, the count and the ids.
fn released_frontier(held: u32, crossed: bool, ids: &[Digest]) -> Vec<u8> {
    let mut wire = b"AFR4".to_vec();
    wire.extend_from_slice(&held.to_be_bytes());
    wire.push(u8::from(crossed));
    wire.extend_from_slice(&(ids.len() as u32).to_be_bytes());
    for id in ids {
        wire.extend_from_slice(id);
    }
    wire
}

/// Whether a released Mac (0.153 through 0.206) reads `frame` as a peer's metadata: it takes a
/// frame of exactly its own metadata's length whose crossed byte is 0 or 1.
fn released_reads_metadata(frame: &[u8]) -> bool {
    frame.len() == HELLO_FIXED + 6 && frame[HELLO_FIXED + 4] <= 1
}

/// What a bound of format 2 adds to the end of each frame that states it: its byte capacity and
/// its format, big-endian.
fn trailer_two() -> Vec<u8> {
    let mut trailer = capacity(FORMAT_TWO).to_be_bytes().to_vec();
    trailer.extend_from_slice(&FORMAT_TWO.to_be_bytes());
    assert_eq!(trailer.len(), FORMAT_TRAILER);
    trailer
}

/// What a peer whose request is `request` sends before its evidence on a delta-frontier wire,
/// stating `bound` in the form a Mac past format 1 writes: its metadata, then a statement that
/// it holds nothing.
fn opening_at(trust: Trust, request: &Request, bound: RecordsBound) -> Vec<u8> {
    let mut input = Vec::new();
    let metadata = Metadata {
        request: request.clone(),
        bound,
        flags: 0,
    };
    send_frame(&mut input, &metadata.frame(trust)).unwrap();
    send_frame(&mut input, &encode_frontier(bound, &[])).unwrap();
    input
}

/// `endpoint`'s part of a session on `wire` in `side`'s seat against a peer that sent `input`
/// over a channel that proved `proof`: what it returned, the bytes it wrote, how many of the
/// peer's it read, and its repair state.
fn scripted(
    endpoint: &Endpoint,
    proof: Option<&NodeProof>,
    side: Side,
    wire: Wire,
    input: &[u8],
) -> (Result<Report>, Vec<u8>, u64, Repair) {
    let mut stream = Script {
        input: Cursor::new(input.to_vec()),
        output: Vec::new(),
    };
    let mut repair = Repair::default();
    let result = endpoint.connected_until(
        &mut stream,
        proof,
        side,
        &mut repair,
        EXCHANGE.horizon(Instant::now()),
        Protocol::Frontier(wire),
    );
    (result, stream.output, stream.input.position(), repair)
}

/// Whether `repair` keeps anything of a peer's refused records for a later round: a run of
/// refused rounds, a hold on what the peer is offered, or a place its selection resumes.
fn struck(repair: &Repair) -> bool {
    repair.membership_hint.refused.is_some() || repair.membership_hint.cursor.is_some()
}

/// An `[everyone]` epoch by Lab Mac 0 at `at`, wrapped to both Macs and held in `log`, as
/// version-1 records: its id and its key.
fn epoch_in(lab: &Lab, log: &mut Log, at: u64) -> (Digest, [u8; 32]) {
    let actor = lab.requests[0].binding().actor();
    let (seed, incarnation) = signer(0);
    let (draft, key) = log
        .draft_epoch(&actor, at, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = draft.sign(&seed, incarnation).unwrap();
    let id = *epoch.id();
    log.insert(epoch).unwrap();
    for identity in &lab.identities {
        let wrap = log
            .draft_wrap(&actor, at, id, &key, identity.recipient())
            .unwrap()
            .sign(&seed, incarnation)
            .unwrap();
        log.insert(wrap).unwrap();
    }
    (id, *key)
}

/// Lab Mac `member`'s draft of a `Put` of `value` to `name` under `epoch`, following every head
/// of `log`.
fn put(
    lab: &Lab,
    log: &Log,
    member: usize,
    authored_ms: u64,
    epoch: &(Digest, [u8; 32]),
    name: &str,
    value: &[u8],
) -> Draft {
    use aterm_secrets::text::{Audience, Entry};
    log.draft_put(
        &lab.requests[member].binding().actor(),
        authored_ms,
        epoch.0,
        &epoch.1,
        &Entry {
            name: name.into(),
            value: aterm_secrets::SecretBytes::new(value.to_vec()),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        },
    )
    .unwrap()
}

/// A version-1 `Name` record by Lab Mac `member` that follows every head of `log`.
fn named(lab: &Lab, log: &Log, member: usize, authored_ms: u64, name: &str) -> Record {
    let (seed, incarnation) = signer(member);
    log.draft(
        &lab.requests[member].binding().actor(),
        authored_ms,
        Body::Name { name: name.into() },
    )
    .sign(&seed, incarnation)
    .unwrap()
}

/// [`put`] signed as record version 2, stating that it supersedes `supersedes`.
fn put_two(
    lab: &Lab,
    log: &Log,
    member: usize,
    authored_ms: u64,
    epoch: &(Digest, [u8; 32]),
    name: &str,
    supersedes: &[Digest],
) -> Record {
    let (seed, incarnation) = signer(member);
    let record = put(lab, log, member, authored_ms, epoch, name, b"two")
        .sign_two(
            &seed,
            incarnation,
            Some(&Statement {
                supersedes: supersedes.to_vec(),
                carries: None,
            }),
        )
        .unwrap();
    assert_eq!(record.version(), 2);
    record
}

/// Two Macs whose format marks name format 2, each on a build that reads it, complete an
/// `atdb/5` session in which every frame that states a bound states the format and the byte
/// capacity: the metadata (`ACX4`), what each holds (`AFR4`), the evidence header (`AEV4`) and
/// each batch header (`ABT4`) set bit 2 of the state byte and end with the capacity (`u64`) and
/// the format (`u32`), big-endian. The metadata is otherwise the released frame, and no released
/// Mac reads it, by its length or by its state byte. The session moves a version-2 record,
/// which the taking Mac stores as one. The dialling Mac knew its peer by the time-signed report
/// it keeps of it; in the session after, on `atdb/8` over native TLS, the other Mac dials on the
/// version-2 record of its peer it now holds, with no report, and its own version-2 record
/// moves.
///
/// NEGATIVE CONTROL: with `put_trailer` writing nothing, the peer refuses the dialler's metadata
/// (a state byte that says a trailer ends the frame, and none there) and closes: the test fails
/// where it asks that the dialler's session completed, with `Err(Error("failed to fill whole
/// buffer"))`.
#[test]
fn atdb_five_states_the_records_format_and_bytes() {
    let mut fixture = RecordsLab::new();
    for paths in &fixture.paths {
        client::test_reads_format_two(paths);
        mark_two(paths);
    }
    let mut log = fixture.log.clone();
    let epoch = epoch_in(&fixture.lab, &mut log, 2);
    let first = put_two(&fixture.lab, &log, 0, 3, &epoch, "format-two", &[]);
    log.insert(first.clone()).unwrap();
    keep_two(&fixture.paths[0], &log);
    fixture.endpoints[0].reports = Some(hearing_of(&fixture.lab, 0, 1, Version::own()));
    fixture.endpoints[0].format_floor = Some(LIFTED);

    let mut repairs = [Repair::default(), Repair::default()];
    let [dialing, answering] = &mut repairs;
    let [(dialed, dialed_tape), (answered, answered_tape)] = session(
        (&fixture.endpoints[0], dialing),
        (&fixture.endpoints[1], answering),
        Wire::Delta,
    );
    assert!(dialed.is_ok(), "{dialed:?}");
    assert!(answered.is_ok(), "{answered:?}");
    let held = RecordBudgets::current().held as u32;
    let trailer = trailer_two();
    for (member, tape) in [(0, &dialed_tape), (1, &answered_tape)] {
        let frames = frames(tape, 3);
        // The metadata: the released frame, its state byte's bit 2 set, then the trailer.
        let mut metadata = released_metadata(
            fixture.lab.trust,
            &fixture.lab.requests[member],
            held,
            false,
            1,
        );
        metadata[HELLO_FIXED + 4] = 2;
        metadata.extend_from_slice(&trailer);
        assert_eq!(frames[0], metadata, "Mac {member}'s metadata");
        assert!(!released_reads_metadata(frames[0]));
        assert!(!released_reads_metadata(&frames[0][..HELLO_FIXED + 6]));
        let read = Metadata::decode_as(fixture.lab.trust, frames[0], true).unwrap();
        assert_eq!(
            read.bound,
            RecordsBound::of(held as usize, false).at_format(FORMAT_TWO)
        );
        assert_eq!(read.bound.bytes, capacity(FORMAT_TWO));
        assert!(Metadata::decode_as(fixture.lab.trust, frames[0], false).is_err());
        assert!(Metadata::decode(fixture.lab.trust, frames[0]).is_err());
        // What it holds, then its evidence header.
        for (frame, magic, fixed) in [(frames[1], b"AFR4", 13), (frames[2], b"AEV4", 22)] {
            assert_eq!(&frame[..4], magic, "Mac {member}");
            assert_eq!(frame[4..8], held.to_be_bytes(), "Mac {member}");
            assert_eq!(frame[8], FORMATTED, "Mac {member}");
            assert!(frame.len() >= fixed + FORMAT_TRAILER, "Mac {member}");
            assert_eq!(
                frame[frame.len() - FORMAT_TRAILER..],
                trailer,
                "Mac {member}"
            );
        }
        assert_eq!(frames[2].len(), 22 + FORMAT_TRAILER);
        // Each batch header: a frame of 21 bytes and the trailer.
        let framed = [&[33, 0, 0, 0][..], b"ABT4"].concat();
        let batches: Vec<&[u8]> = tape
            .windows(8)
            .enumerate()
            .filter(|(_, window)| *window == framed)
            .map(|(at, _)| &tape[at + 4..at + 4 + 21 + FORMAT_TRAILER])
            .collect();
        assert!(!batches.is_empty(), "Mac {member} sent no batch");
        for header in batches {
            assert_eq!(header[4..8], held.to_be_bytes(), "Mac {member}");
            assert_eq!(header[8], FORMATTED, "Mac {member}");
            assert_eq!(header[21..], trailer, "Mac {member}");
        }
    }
    let taken = client::load_records(&fixture.paths[1]).unwrap();
    assert_eq!(
        taken.log().unwrap().get(first.id()).map(Record::version),
        Some(2),
        "the version-2 record moved"
    );
    assert_eq!(
        repairs[1].membership().map(|report| report.received),
        Some(log.held_count() - 1)
    );

    // Mac 1 keeps no report of Mac 0: it dials on the version-2 record Mac 0 signed.
    assert!(fixture.endpoints[1].reports.is_none());
    let mut log = log_of(&taken.log().unwrap().records().cloned().collect::<Vec<_>>());
    let second = put_two(&fixture.lab, &log, 1, 4, &epoch, "format-two-back", &[]);
    log.insert(second.clone()).unwrap();
    keep_two(&fixture.paths[1], &log);
    let (answered, dialed, repair) = dial(&fixture.endpoints[0], &fixture.endpoints[1]);
    assert!(dialed.is_ok(), "{dialed:?}");
    assert!(answered.is_ok(), "{answered:?}");
    assert!(repair.membership_error().is_none(), "{repair:?}");
    for paths in &fixture.paths {
        let store = client::load_records(paths).unwrap();
        assert_eq!(
            store.inventory(),
            log.records().map(|record| *record.id()).collect()
        );
        assert_eq!(
            store.log().unwrap().get(second.id()).map(Record::version),
            Some(2)
        );
        assert_eq!(client::records_marked_format(paths).unwrap(), FORMAT_TWO);
    }
}

/// A Mac that states a later format writes its first frame only to a peer known to read it.
/// Dialling, it writes nothing and reads nothing where it keeps no report of the peer, where the
/// floor names no release (as [`hearing::READS_FORMAT`] does, with the peer's report kept),
/// where the kept report names a release below the floor, where the channel proved no seat or
/// its own, and on `atdb/4`; with the peer's report at the floor it writes its metadata.
/// Answering, it writes nothing to a peer whose first frame states format 1 (a released
/// dialler), states another format, came over a channel that proved another seat or none, or
/// came on `atdb/4`; to a peer whose first frame states its own format over a channel that
/// proved that peer's key, it answers with its metadata.
///
/// NEGATIVE CONTROL: `Mutant::StatesFormatBlind` (a later format is stated to any peer, on any
/// wire), on in every thread of the test, seen to fail with `no report kept, Whole: 1074
/// bytes written` with `left: (1074, 347)`, `right: (0, 0)`.
#[test]
fn atdb_five_states_the_records_format_and_bytes_only_to_a_peer_known_to_read_them() {
    let mut fixture = RecordsLab::new();
    for paths in &fixture.paths {
        client::test_reads_format_two(paths);
        mark_two(paths);
    }
    let trust = fixture.lab.trust;
    let two = RecordsBound::uncrossed().at_format(FORMAT_TWO);
    let stated = 4 + HELLO_FIXED + 6 + FORMAT_TRAILER;
    let wires = [
        Wire::Whole,
        Wire::Delta,
        Wire::Duplex,
        Wire::Reconcile,
        Wire::Ranges,
    ];
    let proofs = [
        proof_of(&fixture.endpoints[0]),
        proof_of(&fixture.endpoints[1]),
    ];
    let answer = opening_at(trust, &fixture.lab.requests[1], two);
    let below = Version(LIFTED.0, LIFTED.1 - 1, 0);
    assert!(hearing::READS_FORMAT.is_none());

    // Dialling.
    let silent = |fixture: &RecordsLab, proof: Option<&NodeProof>, wire: Wire, why: &str| {
        let (result, written, read, _) =
            scripted(&fixture.endpoints[0], proof, Side::Initiator, wire, &answer);
        assert!(result.is_err(), "{why}, {wire:?}");
        assert_eq!(
            (written.len(), read),
            (0, 0),
            "{why}, {wire:?}: {} bytes written",
            written.len()
        );
    };
    for wire in wires {
        silent(&fixture, Some(&proofs[1]), wire, "no report kept");
    }
    fixture.endpoints[0].reports = Some(hearing_of(&fixture.lab, 0, 1, Version::own()));
    assert_eq!(fixture.endpoints[0].format_floor, hearing::READS_FORMAT);
    for wire in wires {
        silent(&fixture, Some(&proofs[1]), wire, "no release named");
    }
    fixture.endpoints[0].format_floor = Some(LIFTED);
    fixture.endpoints[0].reports = Some(hearing_of(&fixture.lab, 0, 1, below));
    for wire in wires {
        silent(&fixture, Some(&proofs[1]), wire, "a report below the floor");
    }
    fixture.endpoints[0].reports = Some(hearing_of(&fixture.lab, 0, 1, LIFTED));
    for wire in wires {
        silent(&fixture, None, wire, "no seat proved");
        silent(&fixture, Some(&proofs[0]), wire, "its own seat proved");
    }
    silent(&fixture, Some(&proofs[1]), Wire::Whole, "atdb/4");
    for wire in &wires[1..] {
        let (_, written, _, _) = scripted(
            &fixture.endpoints[0],
            Some(&proofs[1]),
            Side::Initiator,
            *wire,
            &answer,
        );
        assert!(written.len() >= stated, "{wire:?}");
        let metadata = frames(&written, 1)[0];
        assert_eq!(metadata.len(), HELLO_FIXED + 6 + FORMAT_TRAILER, "{wire:?}");
        assert_eq!(
            Metadata::decode_as(trust, metadata, true).unwrap().bound,
            two,
            "{wire:?}"
        );
    }

    // Answering: Mac 1 keeps no report.
    let unanswered = |proof: Option<&NodeProof>, wire: Wire, input: &[u8], why: &str| {
        let (result, written, _, _) =
            scripted(&fixture.endpoints[1], proof, Side::Responder, wire, input);
        assert!(result.is_err(), "{why}, {wire:?}");
        assert_eq!(
            written.len(),
            0,
            "{why}, {wire:?}: {} bytes written",
            written.len()
        );
    };
    let dialled = opening_at(trust, &fixture.lab.requests[0], two);
    for wire in wires {
        let released = opening(
            trust,
            &fixture.lab.requests[0],
            RecordsBound::uncrossed(),
            wire,
        );
        unanswered(Some(&proofs[0]), wire, &released, "a format-1 dialler");
    }
    for wire in &wires[1..] {
        let three = opening_at(
            trust,
            &fixture.lab.requests[0],
            RecordsBound::uncrossed().at_format(3),
        );
        unanswered(Some(&proofs[0]), *wire, &three, "a format-3 dialler");
        unanswered(None, *wire, &dialled, "no seat proved");
        unanswered(Some(&proofs[1]), *wire, &dialled, "another seat proved");
    }
    unanswered(Some(&proofs[0]), Wire::Whole, &dialled, "atdb/4");
    for wire in &wires[1..] {
        let (_, written, _, _) = scripted(
            &fixture.endpoints[1],
            Some(&proofs[0]),
            Side::Responder,
            *wire,
            &dialled,
        );
        assert!(written.len() >= stated, "{wire:?}");
        assert_eq!(
            Metadata::decode_as(trust, frames(&written, 1)[0], true)
                .unwrap()
                .bound,
            two,
            "{wire:?}"
        );
    }
}

/// A Mac with no format mark writes and reads what every release does, though its build reads
/// format 2, it keeps its peer's report at the floor, and the floor names a release: everything
/// that lets a Mac state a later format but the mark.
///
/// On each frontier wire, two such Macs' session is byte for byte the session of two Macs of
/// the same seats, logs and records with none of that, in both seats. Its first frames are the
/// released ones, built here from their fields: the metadata (`ACX4`, the held bound and the
/// crossed byte, 0 or 1), then on `atdb/5` and later what the Mac holds (`AFR4`), and on `atdb/4`
/// an evidence header of 22 bytes. Afterwards neither Mac holds a format mark, and each one's
/// records file is its twin's, byte for byte.
///
/// Such a Mac refuses a peer's metadata that states a later format as a release does: answering,
/// at the frame's length, after four bytes read and none written; dialling, having written its
/// released metadata and nothing more; and a frame of the released length whose state byte says
/// a later format, as a bound that is none.
///
/// NEGATIVE CONTROLS, each on in every thread of the test, seen to fail:
/// - `Mutant::StatesFormatUnmarked` (a build that reads format 2 states it with no mark stored):
///   the dialler of the two Macs with everything but the mark states format 2 and so speaks no
///   `atdb/4`, `Whole: Err(Error("a membership log past records format 1 speaks no atdb/4
///   frontier protocol"))`.
/// - `Mutant::ReadsFormatUngated` (a Mac that states format 1 reads a later format in a peer's
///   frame): `Delta: an answering Mac wrote 306 bytes` with `left: 306`, `right: 0`.
#[test]
fn atdb_five_states_the_records_format_and_bytes_of_an_unmarked_mac_as_every_release_does() {
    let lab = Lab::new();
    let genesis = Record::found(&ROOT, vec![entrant(&lab, 0), entrant(&lab, 1)], 1).unwrap();
    let base = log_of(std::slice::from_ref(&genesis));
    let network = *base.network();
    let logs: [Log; 2] = std::array::from_fn(|member| {
        let mut records = vec![genesis.clone()];
        records.extend(chain(network, member, *genesis.id(), None, 5 + member));
        log_of(&records)
    });
    let held = RecordBudgets::current().held as u32;
    for (n, wire) in [
        Wire::Whole,
        Wire::Delta,
        Wire::Duplex,
        Wire::Reconcile,
        Wire::Ranges,
    ]
    .into_iter()
    .enumerate()
    {
        let pair = |name: &str| -> [Endpoint; 2] {
            std::array::from_fn(|member| {
                custodied(
                    &lab,
                    member,
                    &format!("{name}-{n}-{member}"),
                    base.clone(),
                    &logs[member],
                )
            })
        };
        let plain = pair("plain");
        let mut unmarked = pair("unmarked");
        for member in 0..2 {
            all_but_the_mark(&lab, &mut unmarked[member], member, 1 - member);
        }
        let have: [Vec<Digest>; 2] =
            std::array::from_fn(|member| View::of(&unmarked[member]).unwrap().have());

        let run = |[dialer, answerer]: &[Endpoint; 2]| {
            let mut repairs = [Repair::default(), Repair::default()];
            let [dialing, answering] = &mut repairs;
            let [(dialed, dialed_tape), (answered, answered_tape)] =
                session((dialer, dialing), (answerer, answering), wire);
            assert!(dialed.is_ok(), "{wire:?}: {dialed:?}");
            assert!(answered.is_ok(), "{wire:?}: {answered:?}");
            [dialed_tape, answered_tape]
        };
        let released = run(&plain);
        let tapes = run(&unmarked);
        for member in 0..2 {
            assert!(
                tapes[member] == released[member],
                "{wire:?}: Mac {member} wrote {} bytes, its twin {}",
                tapes[member].len(),
                released[member].len()
            );
            let frames = frames(&tapes[member], 2);
            assert_eq!(
                frames[0],
                released_metadata(lab.trust, &lab.requests[member], held, false, 1),
                "{wire:?}: Mac {member}'s metadata"
            );
            assert!(released_reads_metadata(frames[0]));
            if wire == Wire::Whole {
                assert_eq!(frames[1].len(), 22, "{wire:?}: Mac {member}");
                assert_eq!(&frames[1][..4], b"AEV4", "{wire:?}: Mac {member}");
                assert_eq!(frames[1][4..9], released_frontier(held, false, &[])[4..9]);
            } else {
                assert_eq!(
                    frames[1],
                    released_frontier(held, false, &have[member]),
                    "{wire:?}: Mac {member}'s statement of what it holds"
                );
            }
            let (paths, twin) = (custody_of(&unmarked[member]), custody_of(&plain[member]));
            assert!(!mark_of(paths).exists(), "{wire:?}: Mac {member}");
            assert_eq!(
                client::records_marked_format(paths).unwrap(),
                FORMAT_ONE,
                "{wire:?}: Mac {member}"
            );
            assert_eq!(
                std::fs::read(paths.records_file()).unwrap(),
                std::fs::read(twin.records_file()).unwrap(),
                "{wire:?}: Mac {member}'s records file"
            );
            assert_eq!(
                client::load_records(paths).unwrap().inventory().len(),
                1 + 5 + 6,
                "{wire:?}: Mac {member}"
            );
        }

        // A peer's metadata that states a later format.
        if wire == Wire::Whole {
            continue;
        }
        let two = RecordsBound::uncrossed().at_format(FORMAT_TWO);
        let later = opening_at(lab.trust, &lab.requests[1], two);
        let proof = proof_of(&unmarked[1]);
        let (result, written, read, repair) =
            scripted(&unmarked[0], Some(&proof), Side::Responder, wire, &later);
        assert!(result.is_err(), "{wire:?}");
        assert_eq!(
            written.len(),
            0,
            "{wire:?}: an answering Mac wrote {} bytes",
            written.len()
        );
        assert_eq!(read, 4, "{wire:?}");
        assert!(!struck(&repair), "{wire:?}");
        let (result, written, _, repair) =
            scripted(&unmarked[0], Some(&proof), Side::Initiator, wire, &later);
        assert!(result.is_err(), "{wire:?}");
        assert_eq!(
            frames(&written, 1)[0],
            released_metadata(lab.trust, &lab.requests[0], held, false, 1),
            "{wire:?}"
        );
        assert_eq!(written.len(), 4 + HELLO_FIXED + 6, "{wire:?}");
        assert!(!struck(&repair), "{wire:?}");
        // The released length, and a state byte that says a later format.
        for state in [2, 3] {
            let mut frame = released_metadata(lab.trust, &lab.requests[1], held, false, 1);
            frame[HELLO_FIXED + 4] = state;
            let mut input = Vec::new();
            send_frame(&mut input, &frame).unwrap();
            let (result, written, _, _) =
                scripted(&unmarked[0], Some(&proof), Side::Initiator, wire, &input);
            assert_eq!(
                result.unwrap_err().to_string(),
                "invalid Commons records bound",
                "{wire:?}"
            );
            assert_eq!(written.len(), 4 + HELLO_FIXED + 6, "{wire:?}");
        }
    }
}

/// A peer that states a format this Mac does not read is refused at its bound, before anything
/// of the session is offered, taken or acknowledged, and nothing of the refusal outlives the
/// round: the repair state the two Macs keep of each other holds no run of refused records, no
/// hold on what the peer is offered and no place to resume ([`struck`]), and the next round
/// between them, once their formats agree, moves the record at once.
///
/// A Mac at format 2 and a peer whose metadata states format 3: dialling, the Mac writes its
/// metadata and nothing after the peer's; answering, it writes nothing.
///
/// A Mac at format 2 that dials a Mac with no format mark, on a report that the peer's release
/// reads format 2: the peer refuses the metadata at its length, as a release does, and writes
/// nothing; the dialler wrote its metadata alone. The peer's records and its marks are as they
/// were. Once the peer's mark names format 2, the same two repair states' next round completes
/// and moves the dialler's version-2 record.
///
/// NEGATIVE CONTROL: `Mutant::FormatUnjudgedAtBound` (two bounds that differ only in format
/// exchange), on in every thread of the test, seen to fail where the dialler went on into
/// the session past the format-3 peer's metadata: `Delta` with `left: "failed to fill whole
/// buffer"`, `right: "Commons frontier record capacities are incompatible"`.
#[test]
fn atdb_five_states_the_records_format_and_bytes_and_refuses_another_format_with_no_strike() {
    let mut fixture = RecordsLab::new();
    client::test_reads_format_two(&fixture.paths[0]);
    mark_two(&fixture.paths[0]);
    fixture.endpoints[0].reports = Some(hearing_of(&fixture.lab, 0, 1, Version::own()));
    fixture.endpoints[0].format_floor = Some(LIFTED);
    let trust = fixture.lab.trust;
    let stated = 4 + HELLO_FIXED + 6 + FORMAT_TRAILER;
    let proofs = [
        proof_of(&fixture.endpoints[0]),
        proof_of(&fixture.endpoints[1]),
    ];

    // A peer at format 3.
    let three = opening_at(
        trust,
        &fixture.lab.requests[1],
        RecordsBound::uncrossed().at_format(3),
    );
    for wire in [Wire::Delta, Wire::Duplex, Wire::Reconcile, Wire::Ranges] {
        let (result, written, _, repair) = scripted(
            &fixture.endpoints[0],
            Some(&proofs[1]),
            Side::Initiator,
            wire,
            &three,
        );
        assert_eq!(
            result.unwrap_err().to_string(),
            "Commons frontier record capacities are incompatible",
            "{wire:?}"
        );
        assert_eq!(
            written.len(),
            stated,
            "{wire:?}: a dialler wrote {} bytes after a format-3 peer's metadata",
            written.len()
        );
        assert!(!struck(&repair), "{wire:?}: {repair:?}");
        let (result, written, _, repair) = scripted(
            &fixture.endpoints[0],
            Some(&proofs[1]),
            Side::Responder,
            wire,
            &three,
        );
        assert!(result.is_err(), "{wire:?}");
        assert_eq!(written.len(), 0, "{wire:?}");
        assert!(!struck(&repair), "{wire:?}: {repair:?}");
    }

    // A peer with no format mark, dialled by a Mac at format 2.
    let mut log = fixture.log.clone();
    let epoch = epoch_in(&fixture.lab, &mut log, 2);
    let record = put_two(&fixture.lab, &log, 0, 3, &epoch, "format-two", &[]);
    log.insert(record.clone()).unwrap();
    keep_two(&fixture.paths[0], &log);
    let before = std::fs::read(fixture.paths[1].records_file()).unwrap();
    let mut repairs = [Repair::default(), Repair::default()];
    for wire in [Wire::Delta, Wire::Ranges] {
        let [dialing, answering] = &mut repairs;
        let [(dialed, dialed_tape), (answered, answered_tape)] = session(
            (&fixture.endpoints[0], dialing),
            (&fixture.endpoints[1], answering),
            wire,
        );
        assert!(dialed.is_err(), "{wire:?}");
        assert_eq!(
            answered.unwrap_err().to_string(),
            "Commons frame exceeds its bound",
            "{wire:?}"
        );
        assert_eq!(answered_tape.len(), 0, "{wire:?}");
        assert_eq!(dialed_tape.len(), stated, "{wire:?}");
        for repair in &repairs {
            assert!(!struck(repair), "{wire:?}: {repair:?}");
            assert!(repair.authenticated_actor().is_none(), "{wire:?}");
            assert!(repair.membership().is_none(), "{wire:?}");
        }
        assert_eq!(
            std::fs::read(fixture.paths[1].records_file()).unwrap(),
            before,
            "{wire:?}"
        );
        assert!(!mark_of(&fixture.paths[1]).exists(), "{wire:?}");
        assert!(!client::records_marked_crossed(&fixture.paths[1]).unwrap());
    }

    // The peer follows its vault into format 2: the next round of the same two states.
    client::test_reads_format_two(&fixture.paths[1]);
    mark_two(&fixture.paths[1]);
    let [dialing, answering] = &mut repairs;
    let [(dialed, _), (answered, _)] = session(
        (&fixture.endpoints[0], dialing),
        (&fixture.endpoints[1], answering),
        Wire::Delta,
    );
    assert!(dialed.is_ok(), "{dialed:?}");
    assert!(answered.is_ok(), "{answered:?}");
    assert_eq!(
        repairs[0].membership().map(|report| report.sent),
        Some(log.held_count() - 1)
    );
    assert_eq!(
        client::load_records(&fixture.paths[1])
            .unwrap()
            .log()
            .unwrap()
            .get(record.id())
            .map(Record::version),
        Some(2)
    );
    for repair in &repairs {
        assert!(repair.membership_hint.refused.is_none(), "{repair:?}");
    }
}

/// The start of day 100, a cut's day in [`rooted`].
const CUT: u64 = 100 * checkpoint::DAY_MS;

/// A log of the fixture's two Macs rooted at a final checkpoint, and the whole log it was pruned
/// from: Mac 1's `Put` of a name, which its version-2 `Put` of the same name superseded, is the
/// record the root dropped.
struct Rooted {
    whole: Log,
    rooted: Log,
    dropped: Record,
}

fn rooted(fixture: &RecordsLab) -> Rooted {
    let lab = &fixture.lab;
    let mut log = fixture.log.clone();
    let at = CUT - 5 * checkpoint::DAY_MS;
    let hold = |log: &mut Log, record: Record| {
        log.insert(record.clone()).unwrap();
        record
    };
    let epoch = epoch_in(lab, &mut log, at);
    let (seed, incarnation) = signer(1);
    let record = put(lab, &log, 1, at + 1, &epoch, "k", b"one")
        .sign(&seed, incarnation)
        .unwrap();
    let dropped = hold(&mut log, record);
    let record = named(lab, &log, 0, at + 2, "a1");
    hold(&mut log, record);
    let record = put_two(lab, &log, 1, at + 3, &epoch, "k", &[*dropped.id()]);
    hold(&mut log, record);
    let record = named(lab, &log, 0, at + 4, "a2");
    hold(&mut log, record);
    let record = named(lab, &log, 1, at + 5, "b3");
    hold(&mut log, record);
    let actors: [Actor; 2] = std::array::from_fn(|member| lab.requests[member].binding().actor());
    let hour = 60 * 60 * 1000;
    let now = CUT
        + checkpoint::MIN_AGE_MS
        + aterm_secrets::membership_log::budgets::WINDOW_SKEW_MS
        + hour;
    let plan = checkpoint::propose(&log, now).expect("a proposal");
    assert_eq!(plan.dropped, vec![*dropped.id()]);
    for member in 0..2 {
        let (seed, incarnation) = signer(member);
        let share = log
            .draft(
                &actors[member],
                CUT + checkpoint::MIN_AGE_MS + hour,
                plan.body.clone(),
            )
            .sign_two(&seed, incarnation, None)
            .unwrap();
        hold(&mut log, share);
    }
    let rooted = log.adopt(&plan.body).unwrap();
    assert!(rooted.is_dropped(dropped.id()) && rooted.get(dropped.id()).is_none());
    assert!(rooted.knows(dropped.id()));
    Rooted {
        whole: log,
        rooted,
        dropped,
    }
}

/// What each of two Macs learns of the other from `atdb/8`'s reconciliation of their records,
/// the first Mac dialling: the records it owes the peer (those it holds that the peer's set does
/// not name), and how many ids the peer named that it lacks.
fn reconciled(stores: [&Store; 2]) -> [(Vec<Digest>, usize); 2] {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let bound = RecordsBound::uncrossed().at_format(FORMAT_TWO);
    let run = |store: &Store, mut stream: TcpStream, side: Side| {
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let (records, ours) = held_and_known(store);
        let theirs = crate::control::reconcile(
            &mut stream,
            side,
            snapshot(&records, ours.clone()),
            ours.len(),
            most_known(bound, bound),
        )
        .unwrap();
        let owed = records
            .iter()
            .map(|record| *record.id())
            .filter(|id| !theirs.contains(id))
            .collect();
        (owed, theirs.difference(&ours).count())
    };
    thread::scope(|scope| {
        let answered = scope.spawn(|| {
            let stream = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap()
            .0;
            run(stores[1], stream, Side::Responder)
        });
        let dialed = run(
            stores[0],
            TcpStream::connect(address).unwrap(),
            Side::Initiator,
        );
        [dialed, answered.join().unwrap()]
    })
}

/// A push that brings a record a root of this Mac's log dropped is judged complete, not
/// refused, and leaves the record dropped: a peer that holds the whole log sends such a record
/// with its seat evidence where this Mac's stated heads do not reach it. A record this Mac
/// lacks, pushed with it, is taken.
///
/// NEGATIVE CONTROL: `Mutant::DroppedUnheld` (a dropped record is not held), on in every thread
/// of the test, seen to fail with `a push of a dropped record: "Commons evidence is not causally
/// complete"`.
#[test]
fn atdb_five_states_the_records_format_and_bytes_and_takes_a_push_of_a_dropped_record() {
    let fixture = RecordsLab::new();
    let Rooted {
        whole,
        rooted,
        dropped,
    } = rooted(&fixture);
    let later = named(
        &fixture.lab,
        &whole,
        0,
        CUT + 20 * checkpoint::DAY_MS,
        "later",
    );
    let view =
        || View::holding(Store::test_holding(rooted.clone(), Reading::Format(FORMAT_TWO)).unwrap());
    let pushed = overlay(&fixture.endpoints[0], &view(), vec![dropped.clone()]);
    let (log, taken) = pushed
        .map_err(|error| error.to_string())
        .expect("a push of a dropped record");
    assert!(log.is_dropped(dropped.id()) && log.get(dropped.id()).is_none());
    assert_eq!(log.held_count(), rooted.held_count());
    assert_eq!(taken.len(), 1);
    let (log, taken) = overlay(
        &fixture.endpoints[0],
        &view(),
        vec![dropped.clone(), later.clone()],
    )
    .unwrap();
    assert!(log.is_dropped(dropped.id()) && log.get(later.id()).is_some());
    assert_eq!(log.held_count(), rooted.held_count() + 1);
    assert_eq!(taken.len(), 2);
}

/// `atdb/8`'s reconciliation reads a record a root of this Mac's log dropped as dropped: it
/// names the record among what this Mac has, though it holds no such record, so a peer that
/// holds the whole log owes it nothing for that id and this Mac lacks nothing of the peer's for
/// it. Of a whole log one record longer, the peer owes that one record alone and this Mac lacks
/// one id; this Mac, which offers only records it holds, owes the peer none. The count of ids a
/// reconciliation admits is the lesser held bound at format 1, as it was, and at a later format
/// that and the ids a store's bytes hold as stubs.
///
/// NEGATIVE CONTROL: `Mutant::DroppedUnheld` (the reconciled set is the held records' ids
/// alone), on in every thread of the test, seen to fail with `this Mac names the record its root
/// dropped` with `left: 10`, `right: 11`; with that assertion removed, with `a whole peer owes
/// the dropped record: true` with `left: 2`, `right: 1`.
#[test]
fn atdb_five_states_the_records_format_and_bytes_and_reads_a_dropped_record_as_dropped() {
    let fixture = RecordsLab::new();
    let Rooted {
        whole,
        rooted,
        dropped,
    } = rooted(&fixture);
    let reading = Reading::Format(FORMAT_TWO);
    let later = named(
        &fixture.lab,
        &whole,
        0,
        CUT + 20 * checkpoint::DAY_MS,
        "later",
    );
    let mut longer = whole;
    longer.insert(later.clone()).unwrap();

    let own = Store::test_holding(rooted, reading).unwrap();
    let (records, known) = held_and_known(&own);
    assert!(records.iter().all(|record| record.id() != dropped.id()));
    assert_eq!(
        known.len(),
        records.len() + 1,
        "this Mac names the record its root dropped"
    );
    assert!(known.contains(dropped.id()));
    let peer = Store::test_holding(longer, reading).unwrap();
    let [(owed, lacked), (peer_owed, peer_lacked)] = reconciled([&own, &peer]);
    assert_eq!(
        peer_owed.len(),
        1,
        "a whole peer owes the dropped record: {}",
        peer_owed.contains(dropped.id())
    );
    assert_eq!(peer_owed, vec![*later.id()]);
    assert_eq!((lacked, peer_lacked), (1, 0));
    assert!(owed.is_empty());

    // How many ids a reconciliation admits.
    let (small, large) = (RecordsBound::of(7, true), RecordsBound::of(10, false));
    assert_eq!(most_known(large, small), 7);
    let stubs = usize::try_from(capacity(FORMAT_TWO) / 32).unwrap();
    assert_eq!(
        most_known(large.at_format(FORMAT_TWO), small.at_format(FORMAT_TWO)),
        7 + stubs
    );
}

/// Make the records of the custody at `paths`, whose mark names format 2, rooted at `rooted`'s
/// final checkpoint, as a Mac takes a served root from its vault's server: a reply of every
/// record and the served frame ([`Store::missing_served`]), taken and written as the client
/// writes it (`client::save_records`, a rewritten records entry).
fn take_root(paths: &client::Paths, rooted: &Log) {
    let store = Store::test_holding(rooted.clone(), Reading::Format(FORMAT_TWO)).unwrap();
    let reply = store.missing_served(&[], aterm_secrets::membership_log::REPLY_CAP);
    assert!(reply.served.is_some());
    let bytes = aterm_secrets::membership_log::read_reply(&reply).unwrap();
    client::save_records(paths, &bytes).unwrap();
    let taken = client::load_records(paths).unwrap();
    assert!(taken.log().unwrap().rooting().is_some());
}

/// Two Macs whose marks name format 2 on a build that reads it: Mac 0's records are rooted at
/// [`rooted`]'s final checkpoint, taken from its vault, and Mac 1 holds the genesis alone, behind
/// the cut. Neither keeps evidence that the other reads format 2.
fn behind_a_cut() -> (RecordsLab, Rooted) {
    let fixture = RecordsLab::new();
    for paths in &fixture.paths {
        client::test_reads_format_two(paths);
        mark_two(paths);
    }
    let rooted = rooted(&fixture);
    take_root(&fixture.paths[0], &rooted.rooted);
    (fixture, rooted)
}

/// How many times `magic` is in `tape`.
fn begun(tape: &[u8], magic: &[u8; 4]) -> usize {
    tape.windows(4).filter(|window| window == magic).count()
}

/// The batch headers (`ABT4`) of `tape`, each as its frame holds it: the length its frame
/// states, then the frame.
fn batch_headers(tape: &[u8]) -> Vec<(usize, &[u8])> {
    tape.windows(8)
        .enumerate()
        .filter(|(_, window)| window[4..] == b"ABT4"[..])
        .map(|(at, window)| {
            let length = u32::from_le_bytes(window[..4].try_into().unwrap()) as usize;
            (length, &tape[at + 4..(at + 4 + length).min(tape.len())])
        })
        .collect()
}

/// A peer behind a cut takes the served root over `atdb/8` and completes its session.
///
/// Mac 0's records are rooted at a final checkpoint that dropped a record; Mac 1 holds the
/// genesis alone. Mac 1 holds no evidence that Mac 0 reads format 2, so dialling it writes
/// nothing; once it keeps Mac 0's time-signed version report, relayed, at the floor, it dials.
/// Mac 0 serves its root in its first batch (a header whose state byte sets the served bit,
/// the frame, then every record Mac 1 lacks), once. Mac 1 takes them together, writes its store
/// rooted at the same checkpoint (read back from its records entry, the dropped record known
/// and not held), and both sides end the session complete: nothing deferred, no refusal kept,
/// no membership error, every record Mac 1 lacked sent and acknowledged.
///
/// NEGATIVE CONTROL: `Mutant::ServesNoRoot` (no root served), on in every thread of the test,
/// seen to fail where it asks that Mac 1's session deferred nothing: `Mac 1: Some(Report {
/// sent: 0, received: 3, deferred: 7, added: 0 })` with `left: Some(7)`, `right: Some(0)`
/// (records alone: Mac 1 takes the epoch and its wraps, and every record past the cut waits on
/// the record the root dropped, which it is never offered).
#[test]
fn a_peer_behind_a_cut_takes_the_served_root_and_completes_its_session() {
    let (
        mut fixture,
        Rooted {
            rooted, dropped, ..
        },
    ) = behind_a_cut();
    assert!(hearing::READS_FORMAT.is_none());
    let mut repairs = [Repair::default(), Repair::default()];

    // Mac 1 holds no evidence of Mac 0.
    let [dialing, answering] = &mut repairs;
    let [(dialed, dialed_tape), (answered, _)] = session(
        (&fixture.endpoints[1], dialing),
        (&fixture.endpoints[0], answering),
        Wire::Ranges,
    );
    assert!(dialed.is_err() && answered.is_err());
    assert_eq!(dialed_tape.len(), 0);
    assert!(repairs.iter().all(|repair| !struck(repair)));

    // Mac 1 keeps Mac 0's report, relayed.
    fixture.endpoints[1].reports = Some(hearing_of(&fixture.lab, 1, 0, Version::own()));
    fixture.endpoints[1].format_floor = Some(LIFTED);
    let [dialing, answering] = &mut repairs;
    let [(dialed, dialed_tape), (answered, answered_tape)] = session(
        (&fixture.endpoints[1], dialing),
        (&fixture.endpoints[0], answering),
        Wire::Ranges,
    );
    assert!(dialed.is_ok(), "{dialed:?}");
    assert!(answered.is_ok(), "{answered:?}");
    let lacked = rooted.held_count() - 1;
    for (member, repair) in [(1, &repairs[0]), (0, &repairs[1])] {
        assert!(
            repair.membership_error().is_none(),
            "Mac {member}: {repair:?}"
        );
        assert_eq!(
            repair.membership().map(|report| report.deferred),
            Some(0),
            "Mac {member}: {:?}",
            repair.membership()
        );
        assert!(!struck(repair), "Mac {member}: {repair:?}");
    }
    assert_eq!(repairs[0].membership().map(|r| r.received), Some(lacked));
    assert_eq!(repairs[1].membership().map(|r| r.sent), Some(lacked));
    assert_eq!(
        begun(&answered_tape, b"ALS1"),
        1,
        "Mac 0 served its root once"
    );
    assert_eq!(begun(&dialed_tape, b"ALS1"), 0, "Mac 1 served nothing");
    let served: Vec<_> = batch_headers(&answered_tape)
        .into_iter()
        .filter(|(_, header)| header[8] & SERVED != 0)
        .collect();
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].0, 21 + FORMAT_TRAILER);
    assert_eq!(served[0].1[8], FORMATTED | SERVED);
    assert_eq!(
        u32::from_be_bytes(served[0].1[9..13].try_into().unwrap()) as usize,
        lacked
    );

    let [own, peer] = [0, 1].map(|member| client::load_records(&fixture.paths[member]).unwrap());
    let log = peer.log().unwrap();
    assert!(log.rooting().is_some(), "Mac 1's records are rooted");
    assert!(log.is_dropped(dropped.id()) && log.get(dropped.id()).is_none());
    assert_eq!(
        log.newest_root().map(|root| root.id),
        rooted.newest_root().map(|root| root.id)
    );
    assert_eq!(peer.complete_inventory(), own.complete_inventory());
    assert_eq!(peer.inventory(), own.inventory());
    for paths in &fixture.paths {
        assert_eq!(client::records_marked_format(paths).unwrap(), FORMAT_TWO);
    }
}

/// Two Macs at the same cut send each other no served frame: every batch header either writes
/// in their session states the format alone (no served bit), names no record and no byte, and
/// no frame begins with the served magic (`ALS1`). The session completes with nothing sent,
/// received or deferred.
///
/// NEGATIVE CONTROL: `Mutant::ServesAtTheCut` (a rooted log's roots served whatever the peer
/// holds), on in every thread of the test, seen to fail with `Ranges: Mac 1 sent a served
/// frame` with `left: 1`, `right: 0`.
#[test]
fn a_peer_behind_a_cut_is_sent_no_frame_once_it_holds_the_cut() {
    let (mut fixture, Rooted { rooted, .. }) = behind_a_cut();
    take_root(&fixture.paths[1], &rooted);
    fixture.endpoints[1].reports = Some(hearing_of(&fixture.lab, 1, 0, Version::own()));
    fixture.endpoints[1].format_floor = Some(LIFTED);
    for wire in [Wire::Ranges, Wire::Proofs] {
        let mut repairs = [Repair::default(), Repair::default()];
        let [dialing, answering] = &mut repairs;
        let [(dialed, dialed_tape), (answered, answered_tape)] = session(
            (&fixture.endpoints[1], dialing),
            (&fixture.endpoints[0], answering),
            wire,
        );
        assert!(dialed.is_ok(), "{wire:?}: {dialed:?}");
        assert!(answered.is_ok(), "{wire:?}: {answered:?}");
        for (member, tape) in [(1, &dialed_tape), (0, &answered_tape)] {
            assert_eq!(
                begun(tape, b"ALS1"),
                0,
                "{wire:?}: Mac {member} sent a served frame"
            );
            let headers = batch_headers(tape);
            assert!(!headers.is_empty(), "{wire:?}: Mac {member} sent no batch");
            for (length, header) in headers {
                assert_eq!(length, 21 + FORMAT_TRAILER, "{wire:?}: Mac {member}");
                assert_eq!(header[8], FORMATTED, "{wire:?}: Mac {member}");
                assert_eq!(header[9..21], [0; 12], "{wire:?}: Mac {member}");
            }
        }
        for repair in &repairs {
            let report = repair.membership().unwrap();
            assert_eq!(
                (report.sent, report.received, report.deferred),
                (0, 0, 0),
                "{wire:?}"
            );
        }
    }
}

/// A served root its peer refuses takes nothing, and the session still ends with no strike
/// either Mac keeps past it: a frame whose stub the root's digest does not bind (it does not
/// authenticate: the body Mac 1 computes from it is not the one the shares sign), one sent
/// without a share its root needs to be final by the rule both Macs judge it by, and one that
/// names another vault's log. Seen refused, in that order, as `a served root is not final by
/// its shares` (the error `Log::take_served` gives a body its shares do not make final, a
/// computed body that is not theirs among them), the same, and `a served frame is of another
/// network`. Each time Mac 1's records are as they
/// were, both sessions end without a membership error, Mac 1's deferred (it still lacks the
/// cut), and neither Mac's repair state holds a refusal or a place to resume. Mac 0 holds the
/// frame back under the same views for [`REFUSAL_HOLD`] ([`MembershipHint::serves`]), so each
/// session here runs that much later on Mac 0's clock than the one before
/// ([`faults::later`]). The next session of the same two repair states, serving the root as
/// it is, completes and roots Mac 1.
///
/// NEGATIVE CONTROL: `Mutant::RefusedRootStrikes` (a served batch the peer took none of settles
/// as a refused round), on in every thread of the test, seen to fail with `Forged: Mac 0 keeps
/// a strike`.
#[test]
fn a_peer_behind_a_cut_refuses_a_root_that_does_not_hold_with_no_strike() {
    let (
        mut fixture,
        Rooted {
            rooted, dropped, ..
        },
    ) = behind_a_cut();
    fixture.endpoints[0].reports = Some(hearing_of(&fixture.lab, 0, 1, Version::own()));
    fixture.endpoints[0].format_floor = Some(LIFTED);
    let before = client::load_records(&fixture.paths[1]).unwrap().inventory();
    let mut repairs = [Repair::default(), Repair::default()];
    let faults = [
        faults::Served::Forged,
        faults::Served::Unfinal,
        faults::Served::Foreign,
    ];
    for (n, fault) in (0u32..).zip(faults) {
        let [dialing, answering] = &mut repairs;
        let [(dialed, dialed_tape), (answered, _)] = faults::later(REFUSAL_HOLD * n, || {
            faults::serving(fault, || {
                session(
                    (&fixture.endpoints[0], dialing),
                    (&fixture.endpoints[1], answering),
                    Wire::Ranges,
                )
            })
        });
        assert!(dialed.is_ok(), "{fault:?}: {dialed:?}");
        assert!(answered.is_ok(), "{fault:?}: {answered:?}");
        assert_eq!(begun(&dialed_tape, b"ALS1"), 1, "{fault:?}");
        for (member, repair) in [(0, &repairs[0]), (1, &repairs[1])] {
            assert!(!struck(repair), "{fault:?}: Mac {member} keeps a strike");
            assert!(
                repair.membership_error().is_none(),
                "{fault:?}: Mac {member}: {repair:?}"
            );
        }
        assert_eq!(
            repairs[0].membership().map(|r| r.sent),
            Some(0),
            "{fault:?}"
        );
        assert!(
            repairs[1].membership().is_some_and(|r| r.deferred > 0),
            "{fault:?}: {:?}",
            repairs[1].membership()
        );
        let peer = client::load_records(&fixture.paths[1]).unwrap();
        assert!(peer.log().unwrap().rooting().is_none(), "{fault:?}");
        assert_eq!(peer.inventory(), before, "{fault:?}");
    }

    let [dialing, answering] = &mut repairs;
    let [(dialed, _), (answered, _)] = faults::later(REFUSAL_HOLD * 3, || {
        session(
            (&fixture.endpoints[0], dialing),
            (&fixture.endpoints[1], answering),
            Wire::Ranges,
        )
    });
    assert!(dialed.is_ok(), "{dialed:?}");
    assert!(answered.is_ok(), "{answered:?}");
    for repair in &repairs {
        assert_eq!(
            repair.membership().map(|r| r.deferred),
            Some(0),
            "{repair:?}"
        );
    }
    let log = client::load_records(&fixture.paths[1]).unwrap();
    assert!(log.log().unwrap().is_dropped(dropped.id()));
    assert_eq!(
        log.complete_inventory(),
        Store::test_holding(rooted, Reading::Format(FORMAT_TWO))
            .unwrap()
            .complete_inventory()
    );
}

/// A Mac with no format mark sends, on `atdb/8` and `atdb/9`, the bytes main sends, though its
/// build reads format 2, it keeps its peer's report at the floor and the floor names a release:
/// its session with a peer that lacks records it holds, and holds records it lacks, is byte for
/// byte the session of two Macs of the same seats, logs and records with none of that, and
/// every batch header in it is the released one, built here from its fields (`ABT4`, the held
/// bound, the crossed byte 0 or 1, the count and the bytes, 21 bytes), with records in batches
/// both ways and no served frame (`ALS1`).
///
/// NEGATIVE CONTROL: `Mutant::ServedBitUngated` (every batch header sets the served bit), on in
/// every thread of the test, seen to fail with `Ranges: Mac 0's batch header` with `left: 4`,
/// `right: 0`.
#[test]
fn a_peer_behind_a_cut_is_sent_by_an_unmarked_mac_the_bytes_main_sends() {
    let lab = Lab::new();
    let genesis = Record::found(&ROOT, vec![entrant(&lab, 0), entrant(&lab, 1)], 1).unwrap();
    let base = log_of(std::slice::from_ref(&genesis));
    let network = *base.network();
    let logs: [Log; 2] = std::array::from_fn(|member| {
        let mut records = vec![genesis.clone()];
        records.extend(chain(network, member, *genesis.id(), None, 3 + member));
        log_of(&records)
    });
    let held = RecordBudgets::current().held as u32;
    for wire in [Wire::Ranges, Wire::Proofs] {
        let pair = |name: &str| -> [Endpoint; 2] {
            std::array::from_fn(|member| {
                custodied(
                    &lab,
                    member,
                    &format!("{name}-{wire:?}-{member}"),
                    base.clone(),
                    &logs[member],
                )
            })
        };
        let plain = pair("plain");
        let mut unmarked = pair("unmarked");
        for member in 0..2 {
            all_but_the_mark(&lab, &mut unmarked[member], member, 1 - member);
        }
        let run = |[dialer, answerer]: &[Endpoint; 2]| {
            let mut repairs = [Repair::default(), Repair::default()];
            let [dialing, answering] = &mut repairs;
            let [(dialed, dialed_tape), (answered, answered_tape)] =
                session((dialer, dialing), (answerer, answering), wire);
            assert!(dialed.is_ok(), "{wire:?}: {dialed:?}");
            assert!(answered.is_ok(), "{wire:?}: {answered:?}");
            [dialed_tape, answered_tape]
        };
        let released = run(&plain);
        let tapes = run(&unmarked);
        for member in 0..2 {
            assert!(
                tapes[member] == released[member],
                "{wire:?}: Mac {member} wrote {} bytes, its twin {}",
                tapes[member].len(),
                released[member].len()
            );
            assert_eq!(begun(&tapes[member], b"ALS1"), 0, "{wire:?}: Mac {member}");
            let headers = batch_headers(&tapes[member]);
            assert!(
                headers.iter().any(|(_, header)| header[9..13] != [0; 4]),
                "{wire:?}: Mac {member} sent no record"
            );
            for (length, header) in headers {
                assert_eq!(length, 21, "{wire:?}: Mac {member}'s batch header");
                assert_eq!(header[4..8], held.to_be_bytes(), "{wire:?}: Mac {member}");
                assert_eq!(header[8], 0, "{wire:?}: Mac {member}'s batch header");
            }
            assert!(!mark_of(custody_of(&unmarked[member])).exists());
        }
    }
}

/// A peer behind a cut that refuses the served root is not served it again under the same
/// views: the session after, with the refusal still there (a stub the root's digest does not
/// bind), Mac 0 writes no served frame (`ALS1`) and no served bit, and offers Mac 1 the records
/// it lacks alone, of which Mac 1 takes the three that need no record the root dropped (the
/// epoch and its two wraps). Neither session leaves either Mac a strike. Mac 1 now holds what
/// it did not, so the views changed and the hold ends: the session after that serves the root
/// as it is, and Mac 1 takes it and completes.
///
/// NEGATIVE CONTROL: `Mutant::ServesRefusedRoot` (a refused frame is served again at once), on
/// in every thread of the test, seen to fail with `the session after a refusal served the frame
/// again` with `left: 1`, `right: 0`.
#[test]
fn a_peer_behind_a_cut_that_refuses_the_root_is_sent_records_alone_under_the_same_views() {
    let (mut fixture, Rooted { rooted, .. }) = behind_a_cut();
    fixture.endpoints[0].reports = Some(hearing_of(&fixture.lab, 0, 1, Version::own()));
    fixture.endpoints[0].format_floor = Some(LIFTED);
    let before = client::load_records(&fixture.paths[1]).unwrap().inventory();
    let mut repairs = [Repair::default(), Repair::default()];
    let run = |repairs: &mut [Repair; 2]| {
        let [dialing, answering] = repairs;
        let [(dialed, dialed_tape), (answered, _)] = session(
            (&fixture.endpoints[0], dialing),
            (&fixture.endpoints[1], answering),
            Wire::Ranges,
        );
        assert!(dialed.is_ok(), "{dialed:?}");
        assert!(answered.is_ok(), "{answered:?}");
        dialed_tape
    };

    let refused = faults::serving(faults::Served::Forged, || run(&mut repairs));
    assert_eq!(begun(&refused, b"ALS1"), 1);
    assert!(repairs[0].membership_hint.unserved.is_some());
    assert_eq!(
        client::load_records(&fixture.paths[1]).unwrap().inventory(),
        before
    );

    let again = faults::serving(faults::Served::Forged, || run(&mut repairs));
    assert_eq!(
        begun(&again, b"ALS1"),
        0,
        "the session after a refusal served the frame again"
    );
    assert!(
        batch_headers(&again)
            .iter()
            .all(|(_, header)| header[8] & SERVED == 0)
    );
    assert_eq!(repairs[0].membership().map(|r| r.sent), Some(3));
    assert_eq!(repairs[1].membership().map(|r| r.received), Some(3));
    for (member, repair) in repairs.iter().enumerate() {
        assert!(!struck(repair), "Mac {member} keeps a strike: {repair:?}");
    }

    let taken = run(&mut repairs);
    assert_eq!(begun(&taken, b"ALS1"), 1);
    assert!(repairs[0].membership_hint.unserved.is_none());
    for repair in &repairs {
        assert_eq!(
            repair.membership().map(|r| r.deferred),
            Some(0),
            "{repair:?}"
        );
    }
    assert_eq!(
        client::load_records(&fixture.paths[1])
            .unwrap()
            .complete_inventory(),
        Store::test_holding(rooted, Reading::Format(FORMAT_TWO))
            .unwrap()
            .complete_inventory()
    );
}

/// A batch header whose state byte sets the served bit and whose frame ends with no format
/// trailer states no bound, on `atdb/8` and `atdb/9`, to a Mac with no mark and to one whose
/// mark names format 2 alike: the batch is refused as `invalid Commons records bound`, as every
/// release refuses a state byte above 1, once its header is read and before anything after it
/// (here a frame and a count of records past one batch) is.
///
/// NEGATIVE CONTROL: `Mutant::ServedBitUnread` (the served bit is read in a header with no
/// trailer), on in every thread of the test, seen to fail with `Ranges: Mac 0: Err(Error("failed
/// to fill whole buffer"))` (it took the bit, read past the header for the frame, and found the
/// tape's end).
#[test]
fn a_peer_behind_a_cut_is_refused_a_served_bit_with_no_format_trailer() {
    let fixture = RecordsLab::new();
    client::test_reads_format_two(&fixture.paths[1]);
    mark_two(&fixture.paths[1]);
    let held = RecordBudgets::current().held as u32;
    let mut header = b"ABT4".to_vec();
    header.extend_from_slice(&held.to_be_bytes());
    header.push(SERVED);
    header.extend_from_slice(&(BATCH_RECORDS as u32 + 1).to_be_bytes());
    header.extend_from_slice(&1u64.to_be_bytes());
    let mut input = Vec::new();
    send_frame(&mut input, &header).unwrap();
    let sent = input.len() as u64;
    for wire in [Wire::Ranges, Wire::Proofs] {
        for member in 0..2 {
            let mut stream = Script {
                input: Cursor::new(input.clone()),
                output: Vec::new(),
            };
            let mut peer = Metadata {
                request: fixture.lab.requests[1 - member].clone(),
                bound: RecordsBound::of(held as usize, false),
                flags: 0,
            };
            let got = receive_batch(
                &fixture.endpoints[member],
                &mut stream,
                &mut peer,
                wire,
                usize::MAX,
            );
            assert_eq!(
                got.as_ref().map_err(ToString::to_string).err().as_deref(),
                Some("invalid Commons records bound"),
                "{wire:?}: Mac {member}: {got:?}"
            );
            assert_eq!(stream.input.position(), sent, "{wire:?}: Mac {member}");
            assert!(stream.output.is_empty(), "{wire:?}: Mac {member}");
        }
    }
}
