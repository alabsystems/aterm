// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Mac's records pass meeting a vault server that states records format 2 (tracker FL-148e,
//! `crate::membership_log::follow`): what the server states stores no mark, a seat's signed
//! version-2 record does, and the mark is durable before the records file holds that record.
//! The pass takes what its other sources sent before it follows, so a seat its source just
//! removed moves it nowhere; it asks again only a source that stated a later format; and a
//! build that reads no format 2 writes nothing over a file a build that does left it.

use super::*;
use crate::crossing::{FORMAT_ONE, FORMAT_TWO, Faults, RecordsBound, inject};
use crate::membership_log::untaken::chain::have_named;
use crate::membership_log::{
    Body, Draft, Entrant, EpochAudience, Log, MemberId, Reading, Record, RecordId, Statement,
    Store, encode_records,
};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};

const HANG_DETECTOR: Duration = aterm_spec::wait::MUST_HAPPEN;
const VAULT_SEED: [u8; 32] = [7; 32];

/// What a [`Host`] holds and says.
struct Holding {
    /// Its records.
    store: Store,
    /// The records it holds and never sends.
    withheld: BTreeSet<RecordId>,
    /// What each records GET stated of its puller's records
    /// ([`transport::RECORDS_BOUND_HEADER`]), in order; `None` for one that stated nothing.
    asked: Vec<Option<String>>,
    /// Whether each records GET asked for a served frame
    /// ([`transport::RECORDS_SERVED_HEADER`]), in order.
    asked_served: Vec<bool>,
    /// What it states of its own records, on every reply; `None` for a server of a build
    /// before the fence, which states nothing and answers every GET.
    bound: Option<RecordsBound>,
    /// It answers every records GET `500`, stating nothing, as a server that failed does.
    failing: bool,
}

/// A records server on loopback that states a bound (format 2's, unless a test says another):
/// it answers a records GET whose puller states a bound it can exchange with
/// ([`RecordsBound::can_exchange`]) what it holds past the ids the GET names, and any other
/// `409`, each beside its own bound, as this build's server does (`server::records_fence`). One
/// that states none answers every GET. A GET that asks for a served frame is answered as a
/// rooted store answers it ([`Store::missing_served`]): its records, then its roots and stubs
/// where the GET does not name its cut. It answers a POST `400`.
struct Host {
    addr: SocketAddr,
    holding: Arc<Mutex<Holding>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

fn head_of(stream: &mut TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(HANG_DETECTOR)).ok()?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    Some(String::from_utf8_lossy(&head).into_owned())
}

/// What `holding` answers the records GET whose head is `head`: its status and body.
fn answer(holding: &mut Holding, head: &str) -> (&'static str, Vec<u8>) {
    let line = head.lines().next().unwrap_or("");
    let stated = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(transport::RECORDS_BOUND_HEADER)
            .then(|| value.trim().to_owned())
    });
    holding.asked.push(stated.clone());
    let served = head.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case(transport::RECORDS_SERVED_HEADER) && value.trim() == "1"
        })
    });
    holding.asked_served.push(served);
    if holding.failing {
        return ("500 Internal Server Error", Vec::new());
    }
    let peer = stated.as_deref().and_then(RecordsBound::parse);
    if let Some(own) = holding.bound
        && !peer.is_some_and(|peer| own.can_exchange(peer))
    {
        return (
            "409 Conflict",
            b"incompatible membership records capacities".to_vec(),
        );
    }
    let body = if served {
        let mut missing = holding
            .store
            .missing_served(&have_named(line), crate::membership_log::REPLY_CAP);
        missing
            .records
            .retain(|record| !holding.withheld.contains(record.id()));
        crate::membership_log::store::read_reply(&missing).expect("the reply")
    } else {
        let records: Vec<Record> = holding
            .store
            .missing(&have_named(line))
            .into_iter()
            .filter(|record| !holding.withheld.contains(record.id()))
            .collect();
        encode_records(&records)
    };
    if body.is_empty() {
        ("204 No Content", body)
    } else {
        ("200 OK", body)
    }
}

impl Host {
    /// One that states format 2.
    fn start(store: Store) -> Self {
        Self::stating(store, Some(RecordsBound::here(false).at_format(FORMAT_TWO)))
    }

    /// One that states `bound`, or nothing.
    fn stating(store: Store, bound: Option<RecordsBound>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let holding = Arc::new(Mutex::new(Holding {
            store,
            withheld: BTreeSet::new(),
            asked: Vec::new(),
            asked_served: Vec::new(),
            bound,
            failing: false,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (held, stopping) = (Arc::clone(&holding), Arc::clone(&stop));
        let worker = std::thread::spawn(move || {
            loop {
                let accepted = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                );
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok((mut stream, _)) = accepted else {
                    continue;
                };
                let Some(head) = head_of(&mut stream) else {
                    continue;
                };
                let line = head.lines().next().unwrap_or("");
                let reply = if line.starts_with("GET ") && line.contains("/records") {
                    let mut holding = held.lock().unwrap_or_else(PoisonError::into_inner);
                    let (status, body) = answer(&mut holding, &head);
                    let bound = holding.bound.filter(|_| !holding.failing);
                    let states = bound.map_or_else(String::new, |bound| {
                        format!("{}: {}\r\n", transport::RECORDS_BOUND_HEADER, bound.value())
                    });
                    let mut reply = format!(
                        "HTTP/1.1 {status}\r\n{states}Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    reply.extend_from_slice(&body);
                    reply
                } else {
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                };
                let _ = stream.write_all(&reply);
            }
        });
        Self {
            addr,
            holding,
            stop,
            worker: Some(worker),
        }
    }

    fn holding(&self) -> std::sync::MutexGuard<'_, Holding> {
        self.holding.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The format each records GET so far stated.
    fn asked_formats(&self) -> Vec<Option<u32>> {
        self.holding()
            .asked
            .iter()
            .map(|stated| {
                stated
                    .as_deref()
                    .and_then(RecordsBound::parse)
                    .map(|bound| bound.format)
            })
            .collect()
    }
}

impl Host {
    /// Whether each records GET so far asked for a served frame.
    fn asked_served(&self) -> Vec<bool> {
        self.holding().asked_served.clone()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The vault `team`'s one seat, and its log's records as it signs them.
struct Seat {
    writer: [u8; 32],
    incarnation: [u8; 16],
    member: MemberId,
    log: Log,
    /// Its `[everyone]` epoch, which its `Put`s name.
    epoch: RecordId,
    held: Vec<Record>,
}

impl Seat {
    fn found() -> Self {
        Self::founded_by(&VAULT_SEED)
    }

    /// The one seat of a log whose genesis the key of `seed` signed: the vault's
    /// ([`VAULT_SEED`]), or a host's own.
    fn founded_by(seed: &[u8; 32]) -> Self {
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
            seed,
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
                Body::Epoch {
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

    /// One more record: a `Put` signed as version 2 where `two`, else a format-1 `Name`.
    fn sign(&mut self, two: bool) -> Record {
        let n = self.held.len() as u64;
        let record = if two {
            self.log
                .draft(
                    &self.member,
                    2_000 + n,
                    Body::Put {
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
                    Body::Name {
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

    /// A format-2 server's store of what the seat signed so far.
    fn store(&self) -> Store {
        let mut store = Store::for_vault("team")
            .expect("store")
            .read_at(Reading::Format(FORMAT_TWO));
        assert_eq!(
            store.ingest_records(self.held.clone()).expect("ingested"),
            self.held.len()
        );
        store
    }
}

/// A Mac configured to read vault `team` at a [`Host`], holding the vault's genesis alone.
struct Mac {
    paths: Paths,
}

impl Mac {
    /// One whose node suffers `faults`.
    fn new(root: &Path, name: &str, host: &Host, seat: &Seat, faults: Faults) -> Self {
        Self::holding(root, name, host, &seat.held[0], faults)
    }

    /// One that holds `genesis` alone, whose node suffers `faults`.
    fn holding(root: &Path, name: &str, host: &Host, genesis: &Record, faults: Faults) -> Self {
        let mac = Self::unrooted(root, name, host, faults);
        save_records(&mac.paths, &encode_records([genesis])).expect("the genesis");
        mac
    }

    /// One of the vault whose genesis the key of `founder` signed, that holds `records`, whose
    /// node suffers `faults`.
    fn of_vault(
        root: &Path,
        name: &str,
        host: &Host,
        founder: &[u8; 32],
        records: &[Record],
        faults: Faults,
    ) -> Self {
        let mac = Self::pinned(root, name, host, founder, faults);
        save_records(&mac.paths, &encode_records(records)).expect("the records");
        mac
    }

    /// One that holds no record, pinned to the vault's key as every one is, whose node suffers
    /// `faults`.
    fn unrooted(root: &Path, name: &str, host: &Host, faults: Faults) -> Self {
        Self::pinned(root, name, host, &VAULT_SEED, faults)
    }

    /// [`Self::unrooted`], pinned to the key of `founder`.
    fn pinned(root: &Path, name: &str, host: &Host, founder: &[u8; 32], faults: Faults) -> Self {
        let paths = Paths::for_home(&root.join(name));
        let key = envelope::signer_public(founder).expect("the vault key");
        let pin = crate::b64::encode_padded(&key);
        let url = format!("http://{}/v1/team#{pin}", host.addr);
        configure_log_member(&paths, &url, name).expect("configured");
        inject(format_mark(&paths).path(), faults);
        Self { paths }
    }

    fn suffers(&self, faults: Faults) {
        inject(format_mark(&self.paths).path(), faults);
    }

    fn pass(&self) -> crate::Result<RecordsPass> {
        self.pass_beside(&[])
    }

    /// A pass that asks `peers` beside its source, as a browse that found them does.
    fn pass_beside(&self, peers: &[&Host]) -> crate::Result<RecordsPass> {
        let peers: Vec<String> = peers
            .iter()
            .map(|peer| format!("http://{}/v1/team", peer.addr))
            .collect();
        let found = PeerDiscovery::selected(&peers);
        sync_records_until(&self.paths, None, &[], Some(&found), None, None)
    }

    fn holds(&self, record: &Record) -> bool {
        load_records(&self.paths)
            .expect("the store")
            .holds(record.id())
    }

    fn format(&self) -> u32 {
        format_mark(&self.paths).stored().expect("the mark")
    }

    /// The bytes of its records file.
    fn file(&self) -> Vec<u8> {
        std::fs::read(self.paths.records_data_file().expect("the layout")).expect("the file")
    }

    /// The bytes of its records file as a reader of format 2 selects it, with the rewrite
    /// generation of its entry.
    fn stored_file(&self) -> (u64, Vec<u8>) {
        let custody = crate::records_storage::rewrite::data_path(&self.paths.records_file())
            .expect("the layout");
        (
            custody.generation,
            std::fs::read(custody.path).expect("the file"),
        )
    }

    fn file_holds(&self, record: &Record) -> bool {
        let wire = record.wire_bytes().expect("the record's bytes");
        self.file()
            .windows(wire.len())
            .any(|window| window == &wire[..])
    }
}

impl Drop for Mac {
    fn drop(&mut self) {
        inject(format_mark(&self.paths).path(), Faults::default());
    }
}

/// A node whose build reads format 2, and that suffers nothing.
fn follows() -> Faults {
    Faults {
        follows: true,
        ..Faults::default()
    }
}

/// A source that only states format 2 moves no Mac across the line. A Mac whose build reads
/// format 2 asks such a server again, stating format 2 with no mark stored; where the answer
/// brings format-1 records alone, or a version-2 record that waits on a parent the server
/// withheld, the Mac stores no mark and its records file is what it was. A Mac whose build
/// reads no format 2 (every build while `READS_FORMAT` names none) asks once, stating the
/// version-1 bound byte for byte, and never again.
///
/// NEGATIVE CONTROLS, each run against this test and seen to fail:
/// - `Faults::mark_on_header` (the server's statement is the evidence): ``a header alone
///   stores no mark``, `left: 2` against `right: 1`.
/// - `Faults::mark_on_waiting` (a version-2 record that only waits is the evidence): ``a
///   version-2 record that waits is no evidence``, `left: 2` against `right: 1`.
/// - [`crate::crossing::FormatMark::follows`] answering yes for every node (no gate): ``a
///   build that reads no format 2 asks once, stating version 1``.
#[test]
fn vault_checkpoint_a_header_alone_stores_no_mark() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut seat = Seat::found();
    seat.sign(false);
    seat.sign(false);

    // A server that states format 2 and holds format-1 records alone.
    let host = Host::start(seat.store());
    let dark = Mac::new(root.path(), "dark", &host, &seat, Faults::default());
    let before = dark.file();
    let pass = dark.pass();
    assert_eq!(
        host.holding().asked,
        vec![Some(RecordsBound::uncrossed().value())],
        "a build that reads no format 2 asks once, stating version 1"
    );
    assert!(pass.is_err(), "format 1 and format 2 exchange nothing");
    assert_eq!((dark.format(), dark.file()), (FORMAT_ONE, before.clone()));
    host.holding().asked.clear();

    let mac = Mac::new(root.path(), "mac", &host, &seat, follows());
    let pass = mac.pass();
    assert_eq!(mac.format(), FORMAT_ONE, "a header alone stores no mark");
    assert!(pass.is_err(), "nothing was followed");
    assert_eq!(
        host.asked_formats(),
        vec![Some(FORMAT_ONE), Some(FORMAT_TWO)],
        "asked again, stating format 2"
    );
    assert!(!format_mark(&mac.paths).path().exists());
    assert_eq!(mac.file(), before, "the records file is what it was");
    drop(host);

    // One whose version-2 record waits on a parent it never sends.
    let parent = seat.sign(false);
    let two = seat.sign(true);
    let host = Host::start(seat.store());
    host.holding().withheld.insert(*parent.id());
    let waits = Mac::new(root.path(), "waits", &host, &seat, follows());
    let pass = waits.pass();
    assert_eq!(
        waits.format(),
        FORMAT_ONE,
        "a version-2 record that waits is no evidence"
    );
    assert!(pass.is_err(), "nothing was followed");
    assert_eq!(
        host.asked_formats(),
        vec![Some(FORMAT_ONE), Some(FORMAT_TWO)]
    );
    assert_eq!(waits.file(), before);
    assert!(!waits.file_holds(&two));
}

/// A Mac whose build reads format 2 follows a vault there on a seat's signed version-2 record,
/// mark first. Its pass is refused at format 1, asks again stating format 2, takes the answer
/// into a copy, and on the evidence stores its mark and then writes the copy; it then asks
/// again at format 2 and is fresh, and a later pass takes the seat's next version-2 record as
/// any record. A Mac that dies between its mark and its records holds the mark and the file it
/// held, and its next pass takes the records at format 2. The same file read by a build that
/// reads no format 2 gives no version-2 record.
///
/// NEGATIVE CONTROL: `Faults::format_after_write` (the records first, then the mark), run
/// against this test and seen to fail with ``the mark is durable before the records file
/// holds a format-2 record``, `left: (1, true)` against `right: (2, false)`.
#[test]
fn vault_checkpoint_a_reader_follows_a_signed_format_two_record_mark_first() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut seat = Seat::found();
    seat.sign(false);
    let two = seat.sign(true);
    let host = Host::start(seat.store());

    // Mark first: a Mac that dies between the two.
    let dies = Mac::new(
        root.path(),
        "dies",
        &host,
        &seat,
        Faults {
            dies_between: true,
            ..follows()
        },
    );
    let before = dies.file();
    assert!(dies.pass().is_err(), "the Mac died");
    assert_eq!(
        (dies.format(), dies.file_holds(&two)),
        (FORMAT_TWO, false),
        "the mark is durable before the records file holds a format-2 record"
    );
    assert_eq!(dies.file(), before);
    dies.suffers(follows());
    let pass = dies.pass().expect("a pass at format 2");
    assert_eq!(pass.added, 3, "{pass:?}");
    assert!(dies.file_holds(&two));

    host.holding().asked.clear();
    let mac = Mac::new(root.path(), "mac", &host, &seat, follows());
    let pass = mac.pass().expect("a pass that follows");
    assert_eq!(pass.added, 3, "{pass:?}");
    assert!(pass.fresh() && !pass.more, "{pass:?}");
    assert_eq!(
        host.asked_formats(),
        vec![Some(FORMAT_ONE), Some(FORMAT_TWO), Some(FORMAT_TWO)],
        "refused at format 1, asked again stating format 2, then a pass at format 2"
    );
    assert_eq!(mac.format(), FORMAT_TWO);
    assert!(mac.file_holds(&two));
    let store = load_records(&mac.paths).expect("the store");
    assert_eq!(store.reading(), Reading::Format(FORMAT_TWO));
    assert!(store.holds(two.id()));
    assert_eq!(
        records_bound(&mac.paths, &store).expect("the bound").format,
        FORMAT_TWO
    );

    // The seat's next version-2 record comes by an ordinary pass.
    let next = seat.sign(true);
    host.holding().store = seat.store();
    let pass = mac.pass().expect("a pass at format 2");
    assert_eq!(pass.added, 1, "{pass:?}");
    assert!(
        load_records(&mac.paths)
            .expect("the store")
            .holds(next.id())
    );

    // A build that reads no format 2 reads no version-2 record from that file.
    mac.suffers(Faults::default());
    let read = load_records(&mac.paths).expect("the store");
    assert_eq!(read.reading(), Reading::ONE);
    assert!(!read.holds(two.id()) && !read.holds(next.id()));
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

    fn sign(&self, draft: Draft) -> Record {
        draft.sign(&self.writer, self.incarnation).expect("signed")
    }
}

/// A format-1 server's store of `records`, or a format-2 server's.
fn serving(records: &[&Record], format: u32) -> Store {
    let mut store = Store::for_vault("team")
        .expect("store")
        .read_at(Reading::Format(format));
    let records: Vec<Record> = records.iter().copied().cloned().collect();
    let held = records.len();
    assert_eq!(store.ingest_records(records).expect("ingested"), held);
    store
}

/// A seat the vault removed moves no Mac across the format line in the pass whose source
/// answered with its `Remove`. The Mac's source is an honest format-1 server that holds the
/// removal; a host on the LAN states format 2 and holds a version-2 record the removed seat
/// signed while it held its seat. The pass takes the source's records first, so the log the
/// evidence is judged on holds the removal: the LAN host is asked again stating format 2, its
/// record is no seat's, no mark is stored and the record is not kept. The source, which stated
/// no later format, is asked once. A Mac whose source holds no removal follows that same LAN
/// host on that same record, so it is the removal that holds the first one back.
///
/// NEGATIVE CONTROL: `Faults::follow_before_take` (the follow before the pass takes what its
/// source sent, as this slice's first build ordered it), run against this test and seen to
/// fail with ``a seat the source just removed moves no Mac across the line``, `left: 2`
/// against `right: 1`.
#[test]
fn vault_checkpoint_a_removed_seats_record_moves_no_mac_past_its_removal() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (one, other) = (Founder::new(1), Founder::new(2));
    let genesis = Record::found(
        &VAULT_SEED,
        vec![one.entrant.clone(), other.entrant.clone()],
        1_000,
    )
    .expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let epoch = one.sign(log.draft(
        &one.member,
        1_500,
        Body::Epoch {
            audience: EpochAudience::Everyone,
            generation: 1,
            commitment: [7; 32],
        },
    ));
    log.insert(epoch.clone()).expect("held");
    // The seat the vault is about to remove signs a version-2 record.
    let two = log
        .draft(
            &other.member,
            2_000,
            Body::Put {
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
    assert_eq!(two.version(), 2);
    let removal = one.sign(log.draft(
        &one.member,
        2_000,
        Body::Remove {
            member: other.member,
            cutoff: *genesis.id(),
        },
    ));
    let one_bound = RecordsBound::here(false);
    let lan = Host::start(serving(&[&genesis, &epoch, &two], FORMAT_TWO));

    let source = Host::stating(
        serving(&[&genesis, &epoch, &removal], FORMAT_ONE),
        Some(one_bound),
    );
    let mac = Mac::holding(root.path(), "mac", &source, &genesis, follows());
    let pass = mac.pass_beside(&[&lan]).expect("the source answered");
    assert_eq!(
        mac.format(),
        FORMAT_ONE,
        "a seat the source just removed moves no Mac across the line"
    );
    assert!(mac.holds(&removal), "the pass took its source's records");
    assert!(!mac.holds(&two) && !mac.file_holds(&two));
    assert_eq!(pass.added, 2, "{pass:?}");
    assert_eq!(
        lan.asked_formats(),
        vec![Some(FORMAT_ONE), Some(FORMAT_TWO)],
        "the host that stated format 2 was asked again, stating it"
    );
    assert_eq!(
        source.holding().asked,
        vec![Some(one_bound.value())],
        "a source that stated no later format is asked once"
    );
    drop(source);
    lan.holding().asked.clear();

    // The same LAN host, beside a source that holds no removal of the seat.
    let source = Host::stating(serving(&[&genesis, &epoch], FORMAT_ONE), Some(one_bound));
    let stale = Mac::holding(root.path(), "stale", &source, &genesis, follows());
    stale.pass_beside(&[&lan]).expect("a pass that follows");
    assert_eq!(
        (stale.format(), stale.holds(&two)),
        (FORMAT_TWO, true),
        "a seat's version-2 record is the evidence while its log holds the seat"
    );
}

/// Only a source that refused a pull beside a later format is asked again. A crossed Mac whose
/// build reads format 2 pulls from a server of a build before the fence, which states no
/// bound: the Mac takes nothing of the answer (`transport::check_records_reply`), and that
/// refusal names no later format, so the server is asked once, stating the version-1 bound,
/// and never told of format 2.
///
/// NEGATIVE CONTROL: `Faults::later_on_any_refusal` (every refused source asked again, as this
/// slice's first build did), run against this test and seen to fail with ``a refusal that
/// states no later format is not asked again``, the server asked a second time stating
/// `2 16384 67108864 2 crossed`.
#[test]
fn vault_checkpoint_only_a_later_formats_refusal_is_asked_again() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut seat = Seat::found();
    seat.sign(false);
    let before_the_fence = Host::stating(serving(&seat.held.iter().collect::<Vec<_>>(), 1), None);
    let mac = Mac::new(root.path(), "mac", &before_the_fence, &seat, follows());
    crossing_mark(&mac.paths).store().expect("crossed");
    let before = mac.file();
    let pass = mac.pass();
    assert_eq!(
        before_the_fence.holding().asked,
        vec![Some(RecordsBound::here(true).value())],
        "a refusal that states no later format is not asked again"
    );
    assert!(pass.is_err(), "a crossed Mac takes nothing unstated");
    assert_eq!((mac.format(), mac.file()), (FORMAT_ONE, before));
}

/// A Mac that holds no genesis follows no log but its vault's. A host on the LAN states format
/// 2 and serves a log of its own: a genesis its own key signed, and a version-2 record that
/// log's one seat signed, which is signed evidence of that log and none of the vault. The Mac,
/// asked again stating format 2, takes none of it: no mark is stored and it holds no record,
/// because the copy it follows in is a copy of its own store, which takes a genesis only of
/// the key line its configuration pins (`records_store_selected`). The same Mac behind the
/// vault's own format-2 server, whose genesis the key it pins signed, follows it from nothing.
///
/// NEGATIVE CONTROL: `records_store_selected` leaving the store under no key line (its
/// `store.under(..)` dropped), run against this test and seen to fail with ``asked again,
/// stating format 2``, `left: [Some(1), Some(2), Some(2)]`: the Mac followed the host's log
/// and asked it a third time, at format 2.
#[test]
fn vault_checkpoint_a_mac_with_no_genesis_follows_only_its_vaults_log() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut own = Seat::founded_by(&[9; 32]);
    let forged = own.sign(true);
    let host = Host::start(own.store());
    let mac = Mac::unrooted(root.path(), "mac", &host, follows());
    let pass = mac.pass();
    assert_eq!(
        host.asked_formats(),
        vec![Some(FORMAT_ONE), Some(FORMAT_TWO)],
        "asked again, stating format 2"
    );
    assert_eq!(
        mac.format(),
        FORMAT_ONE,
        "a log the vault's key did not sign moves no Mac across the line"
    );
    assert!(pass.is_err(), "nothing was followed");
    assert!(!format_mark(&mac.paths).path().exists());
    assert!(
        load_records(&mac.paths).expect("the store").log().is_none() && !mac.holds(&forged),
        "and roots it in nothing"
    );
    drop(host);

    let mut seat = Seat::found();
    let two = seat.sign(true);
    let host = Host::start(seat.store());
    let joins = Mac::unrooted(root.path(), "joins", &host, follows());
    joins.pass().expect("a pass that follows");
    assert_eq!(joins.format(), FORMAT_TWO);
    assert!(joins.holds(&seat.held[0]) && joins.holds(&two) && joins.file_holds(&two));
}

/// A build that reads no format 2 writes nothing over a records file a build that does left
/// it. A Mac follows its vault into format 2, and its file holds a version-2 record and a
/// format-1 record after it; the same Mac then runs a build that reads none (every build while
/// `READS_FORMAT` names no release, which is what a followed Mac is rolled back to). That build
/// reads the file as the records before the version-2 one, and a write of what it read is
/// refused, as is a pass: the file is byte for byte what the reader left, and a build that
/// reads format 2 reads every record of it again.
///
/// NEGATIVE CONTROL: `Faults::writes_unread_format` (the write goes on), run against this test
/// and seen to fail with ``a store that does not read the marked format is not written``.
#[test]
fn vault_checkpoint_a_build_that_reads_no_format_two_writes_no_followed_file() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut seat = Seat::found();
    seat.sign(false);
    let two = seat.sign(true);
    let after = seat.sign(false);
    let host = Host::start(seat.store());
    let mac = Mac::new(root.path(), "mac", &host, &seat, follows());
    mac.pass().expect("a pass that follows");
    assert!(mac.file_holds(&two) && mac.file_holds(&after));
    let followed = mac.file();

    // A build that reads no format 2.
    let dark = Faults::default();
    mac.suffers(dark);
    let read = load_records(&mac.paths).expect("the store");
    assert_eq!(read.reading(), Reading::ONE);
    assert!(!read.holds(two.id()) && !read.holds(after.id()));
    let lock = StateLock::take(&mac.paths).expect("the state lock");
    let wrote = write_records(&mac.paths, &read);
    drop(lock);
    assert!(
        wrote.is_err(),
        "a store that does not read the marked format is not written"
    );
    assert_eq!(mac.file(), followed, "the followed file is as it was");
    let _ = mac.pass();
    assert_eq!(mac.file(), followed, "a pass of that build leaves it too");

    // The build that reads format 2 again.
    mac.suffers(follows());
    assert!(mac.holds(&two) && mac.holds(&after));
}

/// Run `run` and count the calls its records-file writes on this thread made to the write
/// hook, which feeds the mesh's members lane.
fn fed<T>(run: impl FnOnce() -> T) -> (T, usize) {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let seen = calls.clone();
    let out = crate::membership_log::store::with_written(move |_| seen.set(seen.get() + 1), run);
    (out, calls.get())
}

/// A Mac whose format mark names format 2 neither feeds nor takes the mesh's members lane
/// (ATERM-KI-1116-1471), which states no records bound and so passes the HTTP fence by. A Mac
/// with no mark feeds it at each write of its records file, takes what it brings, and is read
/// by it whole, as ever. Once the Mac follows its vault into format 2, the write that follows
/// its mark and every write after it reach no hook, the lane reads it an empty log, and each
/// record the lane brings is refused for good and leaves the file as it was; a build that
/// reads no format 2 on that Mac keeps the lane shut too, since the mark alone shuts it.
///
/// NEGATIVE CONTROL: `Faults::lane_past_mark` (the lane open whatever the mark names), run
/// against this test and seen to fail with ``the write that follows the mark feeds the lane
/// nothing``, `left: 1` against `right: 0`.
#[test]
fn vault_checkpoint_a_marked_mac_neither_feeds_nor_takes_the_members_lane() {
    use crate::membership_log::store::MemberVerdict;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut seat = Seat::found();
    let first = seat.sign(false);
    let two = seat.sign(true);
    let host = Host::start(seat.store());

    // No mark: the lane carries this Mac's records both ways.
    let plain = Mac::new(root.path(), "plain", &host, &seat, Faults::default());
    let (added, calls) =
        fed(|| save_records(&plain.paths, &encode_records([&seat.held[1]])).expect("saved"));
    assert_eq!(
        (added, calls),
        (1, 1),
        "an unmarked Mac's write feeds the lane"
    );
    let (took, calls) =
        fed(|| ingest_mesh_records(&plain.paths, vec![first.clone()]).expect("ingested"));
    assert_eq!(took, vec![(*first.id(), MemberVerdict::Applied)]);
    assert_eq!(calls, 1);
    assert!(plain.holds(&first));
    assert!(
        mesh_members(&plain.paths)
            .expect("the lane's read")
            .holds(first.id())
    );

    // The Mac follows its vault into format 2: the lane is shut from the mark on.
    let mac = Mac::new(root.path(), "mac", &host, &seat, follows());
    let (pass, calls) = fed(|| mac.pass());
    pass.expect("a pass that follows");
    assert_eq!(mac.format(), FORMAT_TWO);
    assert!(mac.holds(&two));
    assert_eq!(
        calls, 0,
        "the write that follows the mark feeds the lane nothing"
    );
    let lane = mesh_members(&mac.paths).expect("the lane's read");
    assert!(
        lane.log().is_none() && lane.complete_inventory().is_empty(),
        "the lane reads a marked Mac an empty log"
    );
    let next = seat.sign(false);
    let held = mac.file();
    let (took, calls) =
        fed(|| ingest_mesh_records(&mac.paths, vec![next.clone()]).expect("judged"));
    assert!(
        matches!(took.as_slice(), [(id, MemberVerdict::Refused(_))] if id == next.id()),
        "a marked Mac takes nothing the lane brings: {took:?}"
    );
    assert_eq!((calls, mac.file()), (0, held), "and its file is as it was");
    // Its next record comes over HTTP, and that write feeds nothing either.
    host.holding().store = seat.store();
    let (pass, calls) = fed(|| mac.pass());
    assert_eq!(pass.expect("a pass at format 2").added, 1);
    assert!(mac.holds(&next));
    assert_eq!(calls, 0);

    // A build that reads no format 2 on the marked Mac: the mark alone shuts the lane.
    mac.suffers(Faults::default());
    let last = seat.sign(false);
    let took = ingest_mesh_records(&mac.paths, vec![last.clone()]).expect("judged");
    assert!(matches!(took.as_slice(), [(_, MemberVerdict::Refused(_))]));
    assert!(
        mesh_members(&mac.paths)
            .expect("the lane's read")
            .log()
            .is_none()
    );
}

/// A MAC REFUSED BELOW THE FENCE SAYS IT NEEDS AN UPDATE (tracker FL-148E3). A Mac whose build
/// reads no format 2 pulls from its vault's source, which states format 2 and refuses it
/// ([`crate::ErrorKind::LaterFormat`]). The pass keeps a note of that refusal beside the
/// records ([`told_later_format`]), which `aterm fleet` reads as this Mac's own `needs update`
/// (`aterm_agent::fleet_view::format_floor`); the note is the only file it writes: no mark is
/// stored and the records file is what it was. A pass that the source answers at format 1
/// again removes it, so a source's unsigned word moves the display only while the source says
/// it; a pass in which the source failed and a peer answered leaves it. Once the Mac runs a
/// build that reads format 2, the note an older build left reads as nothing, and the pass, which
/// asks the source again stating format 2 (`follow_later_format`), removes it.
///
/// What an older reader sees: the sync's diagnostic line is the one this refusal always gave
/// (`aterm-secrets-sync-v1 failed other`, which every reader parses as a failure of no named
/// kind), and a successful sync's line is untouched, since the refusal reaches no sync's
/// outcome; an older `aterm fleet` reads this Mac's format mark alone, which is absent, so it
/// shows what it showed.
///
/// NEGATIVE CONTROLS, each run against this test and seen to fail:
/// - `note_later_format` writing no note: ``a Mac refused below the fence says it needs an
///   update``.
/// - `records_pass` passing `pass.is_ok()` as whether the source was heard: ``a pass the source
///   never answered keeps the note``.
/// - `told_later_format` without its `!reads_later_format` guard: ``a build that reads format 2
///   is told nothing by an older build's note``.
/// - `note_later_format` removing the note only where the source was heard (no `|| follows`):
///   ``a build that follows removes the note``.
#[test]
fn vault_checkpoint_a_mac_refused_below_the_fence_says_it_needs_an_update() {
    use crate::sync_diagnostic::SyncDiagnostic;

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut seat = Seat::found();
    seat.sign(false);
    let host = Host::start(seat.store());
    let dark = Mac::new(root.path(), "dark", &host, &seat, Faults::default());
    let before = dark.file();
    assert!(!told_later_format(&dark.paths));

    let refused = dark
        .pass()
        .expect_err("format 1 and format 2 exchange nothing");
    assert_eq!(refused.kind(), crate::ErrorKind::LaterFormat);
    assert!(
        told_later_format(&dark.paths),
        "a Mac refused below the fence says it needs an update"
    );
    assert_eq!((dark.format(), dark.file()), (FORMAT_ONE, before.clone()));
    assert!(!format_mark(&dark.paths).path().exists());
    assert_eq!(
        records_marked_format(&dark.paths).expect("the mark"),
        FORMAT_ONE
    );
    let line = SyncDiagnostic::Failed(refused.kind()).line();
    assert_eq!(line, "aterm-secrets-sync-v1 failed other");
    assert_eq!(
        SyncDiagnostic::parse(&line),
        Some(SyncDiagnostic::Failed(crate::ErrorKind::Other))
    );
    let again = dark.pass().expect_err("refused again");
    assert_eq!(again.kind(), crate::ErrorKind::LaterFormat);
    assert!(
        told_later_format(&dark.paths),
        "the note stays while refused"
    );

    // The source fails while a peer answers: the pass succeeds, the source was not heard, and
    // the note stays.
    host.holding().failing = true;
    let peer = Host::stating(seat.store(), Some(RecordsBound::uncrossed()));
    let pass = dark.pass_beside(&[&peer]).expect("a peer answered");
    assert!(pass.source_short, "{pass:?}");
    assert!(
        told_later_format(&dark.paths),
        "a pass the source never answered keeps the note"
    );
    drop(peer);

    // The source answers at format 1 again: the note goes.
    host.holding().failing = false;
    host.holding().bound = Some(RecordsBound::uncrossed());
    dark.pass().expect("a pass at format 1");
    assert!(
        !told_later_format(&dark.paths),
        "a source that answers takes the note back"
    );
    assert!(!dark.paths.later_format_file().exists());

    // The source states format 2 again, and the note is back; then the Mac runs a build that
    // reads format 2: the note an older build left says nothing from then on, and the pass that
    // follows the source removes it.
    host.holding().bound = Some(RecordsBound::here(false).at_format(FORMAT_TWO));
    dark.pass().expect_err("refused again");
    assert!(told_later_format(&dark.paths));
    dark.suffers(follows());
    assert!(
        dark.paths.later_format_file().exists() && !told_later_format(&dark.paths),
        "a build that reads format 2 is told nothing by an older build's note"
    );
    let asked = host.asked_formats().len();
    let _ = dark.pass();
    assert_eq!(
        host.asked_formats()[asked..],
        [Some(FORMAT_ONE), Some(FORMAT_TWO)],
        "it follows the source"
    );
    assert!(
        !dark.paths.later_format_file().exists(),
        "a build that follows removes the note"
    );
}

/// A Mac behind a format-2 vault's final checkpoint catches up from a pruned server, and its
/// records file becomes a rewritten one. Its pull at format 1 is refused; it asks again
/// stating format 2 and asking for a served frame, takes the server's records and the root it
/// serves into a copy, and on a seat's version-2 record there stores its mark and then
/// rewrites its records entry to the copy: the root and the stub of the record the prune
/// dropped in the file's frame, then the records. It reads the file back as the pruned log
/// (the record that follows only the dropped one held, the dropped one known and not held),
/// asks again naming the cut and is served nothing, and takes the vault's next record by an
/// append to the same generation's file. That write writes the pin again with its own bytes, so
/// a watcher that took its files once hears it, however many rewrites since. A Mac whose build reads no format 2 asks for no
/// served frame, and its file and entry are what they were; the same build on the followed
/// Mac reads no rewritten layout and writes nothing.
///
/// NEGATIVE CONTROLS, each a change to the code run against this test and seen to fail:
/// - `transport::Pull::takes_served` answering no (no puller asks for a served frame): ``a
///   pass that follows``, the pass ending in the server's `409` as a later format's: records
///   alone leave the seat's version-2 record waiting on the dropped one, which is no evidence.
/// - `client::persist_stored` answering `false` for every store (the ordinary writer writes
///   a rooted store): ``the entry is a rewritten layout``, `left: 0` against `right: 1`.
/// - `transport::Pull::takes_served` answering as its caller says, whatever the pull reads:
///   ``a build that reads no format 2 asks for no served frame``.
/// - A write to a rewritten generation writing no pin again (`rewrite::Mutant::Unheard`):
///   ``the write moves the pin the watcher names``, its stamps equal.
#[test]
fn vault_checkpoint_a_puller_takes_a_served_root_into_a_rewritten_file() {
    use crate::membership_log::stored::KEPT_MAGIC;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let vault = crate::membership_log::pruned_vault();
    let host = Host::start(vault.peer.clone());

    let dark = Mac::of_vault(
        root.path(),
        "dark",
        &host,
        &vault.founder,
        &vault.behind,
        Faults::default(),
    );
    let before = dark.file();
    assert!(
        dark.pass().is_err(),
        "format 1 and format 2 exchange nothing"
    );
    let asked = host.holding().asked.clone();
    assert_eq!(
        (asked, host.asked_served()),
        (vec![Some(RecordsBound::uncrossed().value())], vec![false]),
        "a build that reads no format 2 asks for no served frame"
    );
    assert_eq!((dark.format(), dark.file()), (FORMAT_ONE, before.clone()));
    assert_eq!(dark.stored_file(), (0, before));
    host.holding().asked.clear();
    host.holding().asked_served.clear();

    let mac = Mac::of_vault(
        root.path(),
        "mac",
        &host,
        &vault.founder,
        &vault.behind,
        follows(),
    );
    let pass = mac.pass().expect("a pass that follows");
    assert!(pass.fresh() && !pass.more, "{pass:?}");
    assert_eq!(
        (host.asked_formats(), host.asked_served()),
        (
            vec![Some(FORMAT_ONE), Some(FORMAT_TWO), Some(FORMAT_TWO)],
            vec![false, true, true]
        ),
        "refused at format 1, then asked for a served frame at format 2"
    );
    assert_eq!(mac.format(), FORMAT_TWO);
    let (generation, file) = mac.stored_file();
    assert_eq!(generation, 1, "the entry is a rewritten layout");
    assert!(
        file.starts_with(KEPT_MAGIC),
        "its file begins with the frame"
    );
    let named = mac.paths.records_generation().expect("the generation");
    assert_eq!((named.number, named.rewrite.is_some()), (1, true));

    let store = load_records(&mac.paths).expect("the store");
    let log = store.log().expect("a log");
    assert_eq!(log.newest_root().map(|root| root.id), Some(vault.root));
    assert!(
        log.get(&vault.slept).is_some(),
        "the sleeper's record is held"
    );
    assert!(log.is_dropped(&vault.dropped) && log.get(&vault.dropped).is_none());
    assert_eq!(store.heads_digest(), vault.peer.heads_digest());
    assert_eq!(store.inventory(), vault.peer.inventory());

    // Asked again, it names the cut and is served nothing: its file is as it was.
    let pass = mac.pass().expect("a pass at format 2");
    assert_eq!(pass.added, 0, "{pass:?}");
    assert_eq!(mac.stored_file(), (1, file.clone()));

    // The vault's next record is written to the same generation's file, and the pin, which a
    // watcher of this Mac's standing names, is written again, so the write wakes it.
    let watched = mac.paths.records_watch_files();
    let current = crate::records_storage::rewrite::data_path(&mac.paths.records_file())
        .expect("the pinned generation")
        .path;
    assert!(
        !watched.contains(&current),
        "no watcher names a file whose name a rewrite changes"
    );
    let pin = crate::records_storage::standing_files(&mac.paths.records_file()).expect("its files")
        [1]
    .clone();
    assert!(watched.contains(&pin));
    let seen = fsx::stamp_if_present(&pin).expect("the pin's stamp");
    let mut peer = vault.peer.clone();
    assert_eq!(
        peer.ingest_records(vec![vault.next.clone()]).expect("held"),
        1
    );
    host.holding().store = peer;
    let pass = mac.pass().expect("a pass at format 2");
    assert_eq!(pass.added, 1, "{pass:?}");
    let (generation, grown) = mac.stored_file();
    let wire = vault.next.wire_bytes().expect("the record's bytes");
    assert_eq!(generation, 1);
    assert_eq!(grown, [&file[..], &wire[..]].concat(), "an append");
    assert_ne!(
        fsx::stamp_if_present(&pin).expect("the pin's stamp"),
        seen,
        "the write moves the pin the watcher names"
    );
    assert_eq!(
        mac.paths.records_generation().expect("the generation"),
        named,
        "the pin selects what it selected"
    );
    assert!(
        load_records(&mac.paths)
            .expect("the store")
            .holds(vault.next.id())
    );

    // A build that reads no format 2 reads no rewritten layout, and writes nothing.
    mac.suffers(Faults::default());
    assert!(load_records(&mac.paths).is_err());
    assert!(mac.pass().is_err());
    mac.suffers(follows());
    assert_eq!(mac.stored_file(), (1, grown));
}

/// A lab member that followed its vault into format 2: its build reads format 2, and its
/// records file holds, mark first, a version-2 `Put` it signed under the vault's `[everyone]`
/// epoch. Returns that record.
pub(super) fn follows_on_its_own_put(member: &Paths) -> Record {
    let mark = format_mark(member);
    inject(mark.path(), follows());
    let store = load_records(member).expect("the member's store");
    let log = store.log().expect("a log");
    let identity = read_identity(member).expect("read").expect("an identity");
    let keys = log.unwrap_epochs(&identity).expect("its epoch keys");
    let (epoch, key) = keys
        .iter()
        .find(|(id, _)| {
            matches!(
                log.get(id).map(Record::body),
                Some(Body::Epoch {
                    audience: EpochAudience::Everyone,
                    ..
                })
            )
        })
        .expect("the everyone epoch");
    let (bootstrap, actor, incarnation) = super::log_entries_tests::member_key(member);
    let entry = crate::text::Entry {
        name: "format-two-value".into(),
        value: SecretBytes::new(b"synthetic-two".to_vec()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let draft = log
        .draft_put(
            &actor,
            aterm_time::unix_millis().expect("the clock"),
            *epoch,
            key,
            &entry,
        )
        .expect("a put");
    let two = bootstrap
        .device()
        .with_seed(|seed| {
            draft.sign_two(
                seed,
                incarnation,
                Some(&Statement {
                    supersedes: Vec::new(),
                    carries: None,
                }),
            )
        })
        .expect("signed");
    crate::membership_log::follow::follow_records(&mark, &store, vec![two.clone()], None)
        .expect("followed")
        .expect("a seat's version-2 record is the evidence")
        .take(&mark, |store| write_records(member, store))
        .expect("mark first, then the records");
    assert_eq!(mark.stored().expect("the mark"), FORMAT_TWO);
    two
}

/// A new Mac joins a format-2 vault by a person's Allow and the phrase, asking twice. The
/// member, whose format mark is stored, refuses the run that states format 1 before it touches
/// the phrase, naming its format (`NO later=2`); a new Mac whose build reads format 2 runs the
/// same words again stating format 2, is handed the member's log, and on the member's signed
/// version-2 record in it stores its own mark before its configuration and records. A new Mac
/// whose build reads no format 2 asks once, fails with the member's reason, and holds no mark,
/// no configuration and no records.
///
/// NEGATIVE CONTROLS, each a change to the code run against this test and seen to fail:
/// - `client::later_records_bound` answering `None` for every Mac (nobody asks again): ``the
///   phrase lets in a Mac that reads format 2``, the member's format reason.
/// - `client::later_records_bound` not asking [`crate::crossing::FormatMark::follows`] (every
///   Mac asks again): ``a build that reads no format 2 asks once``, `left: 2` against
///   `right: 1`.
/// - the member answering [`crate::members::bundle::Reader::later_format`] `None` (a plain
///   `NO`, as before this slice): ``the phrase lets in a Mac that reads format 2``.
#[test]
fn vault_checkpoint_a_joining_mac_asks_again_at_format_two() {
    use crate::members::desk::{self, Desk, Local};
    use crate::members::join::{self, Stream};
    use crate::members::joiner::Joining;
    use std::sync::atomic::AtomicUsize;

    let _answering = ServersAnswering::set();
    let lab = super::log_entries_tests::two_macs(&[(
        Audience::Everyone,
        "shared",
        b"everyone-v1".as_slice(),
    )]);
    let m22 = &lab.m22;
    let two = follows_on_its_own_put(m22);

    let desk = Mutex::new(Desk::new(
        Local::of(m22)
            .expect("m22's desk")
            .expect("m22 is a member")
            .with_home(Some(crate::home_lan::HomeNet::test())),
    ));
    let hand = |handing: &crate::members::desk::Handing<'_>| hand_over(m22, handing);
    let runs = AtomicUsize::new(0);
    // A new Mac's run, with m22, of the phrase a person's Allow of its row gives, counting the
    // run's connections.
    let run = |new: &Paths, node: u8, name: &str| {
        let device = crate::commons::bootstrap::Device::open(new)
            .expect("device")
            .expect("an identity");
        let store = load_records(new).expect("its store");
        let joining = Joining {
            records_bound: records_bound(new, &store).expect("its bound"),
            device: &device,
            node: [node; 32],
            name: name.to_owned(),
            model: "Mac15,3".to_owned(),
        };
        std::thread::scope(|scope| {
            let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
                let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
                for end in [&ours, &theirs] {
                    end.set_read_timeout(Some(HANG_DETECTOR))?;
                }
                let (desk, hand) = (&desk, &hand);
                scope.spawn(move || {
                    drop(desk::serve(
                        desk,
                        &mut ours,
                        std::net::IpAddr::from([203, 0, 113, 9]),
                        hand,
                    ));
                });
                Ok(Box::new(theirs))
            };
            let (_, missed) = join::ask_all(&dial, &joining, &["m22:7100".to_owned()]);
            assert!(missed.is_empty(), "{missed:?}");
            let words = {
                let mut desk = desk.lock().unwrap_or_else(PoisonError::into_inner);
                let now = aterm_time::Instant::now();
                let rows = desk.rows(now);
                let row = rows
                    .iter()
                    .find(|row| row.name == name)
                    .expect("the new Mac's ask shows a row");
                let phrase = desk.allow(row.id, now).expect("a person allows it");
                desk.words(phrase, now).expect("live").join(" ")
            };
            let heard = join::ask_all(&dial, &joining, &["m22:7100".to_owned()]).0;
            assert_eq!(heard.len(), 1);
            let phrase = crate::phrase::Phrase::parse(&words).expect("three words");
            let counting = |at: &str| {
                runs.fetch_add(1, Ordering::SeqCst);
                dial(at)
            };
            join::run_phrase(new, &counting, &joining, &heard, &phrase, None, &mut |_| {})
                .map(|_| ())
        })
    };

    // A build that reads no format 2: one run, the member's reason, nothing on disk.
    let dark = Paths::for_home(&lab.root.path().join("dark"));
    machine_identity(&dark).expect("identity");
    let refused = run(&dark, 0x51, "dark").expect_err("format 1 and format 2 exchange no log");
    assert!(refused.to_string().contains("format 2"), "{refused}");
    assert_eq!(
        runs.swap(0, Ordering::SeqCst),
        1,
        "a build that reads no format 2 asks once"
    );
    assert!(!format_mark(&dark).path().exists());
    assert!(config(&dark).expect("read").is_none());
    assert!(crate::records_storage::absent(&dark.records_file()).expect("looked"));

    let new = Paths::for_home(&lab.root.path().join("new"));
    machine_identity(&new).expect("identity");
    inject(format_mark(&new).path(), follows());
    run(&new, 0x52, "new").expect("the phrase lets in a Mac that reads format 2");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "refused at format 1, then run again at format 2"
    );
    assert_eq!(format_mark(&new).stored().expect("the mark"), FORMAT_TWO);
    let store = load_records(&new).expect("its store");
    assert_eq!(store.reading(), Reading::Format(FORMAT_TWO));
    assert!(store.holds(two.id()));
    assert_eq!(
        crate::client::stored(&new, "format-two-value").expect("read"),
        b"synthetic-two"
    );
    for mac in [m22, &dark, &new] {
        inject(format_mark(mac).path(), Faults::default());
    }
    lab.server.stop();
}

/// A member whose log is rooted at a final checkpoint hands its roots and stubs after its
/// records, and a Mac that reads format 2 takes them: the handed bytes read as records and a
/// served frame, a store behind the cut that takes both holds what the member holds, and a
/// marked Mac that merges them ([`save_records`]) holds them in a rewritten file. A reader of
/// format 1 refuses the handed bytes whole and its file is what it was, and a member whose log
/// is whole hands its records' stream alone, byte for byte.
///
/// NEGATIVE CONTROLS, each a change to the code run against this test and seen to fail:
/// - `admit::held_records` handing records alone: ``a rooted member hands its roots``.
/// - `client::ingest_as_read` reading every store's bytes at format 1: ``a marked Mac takes
///   the handed log``.
#[test]
fn vault_checkpoint_a_rooted_members_bundle_carries_its_roots() {
    use crate::membership_log::MAX_HELD_RECORDS;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let vault = crate::membership_log::pruned_vault();
    let handed = admit::held_records(&vault.peer).expect("the member's records");

    let (records, served) =
        admit::bundle_records(&handed, MAX_HELD_RECORDS, Reading::Follow).expect("read");
    let served = served.expect("a rooted member hands its roots");
    assert_eq!(served.roots, vec![vault.root]);
    let mut behind = Store::with_compiled(None).read_at(Reading::Follow);
    behind
        .ingest_served(records, Some(&served))
        .expect("taken with its roots");
    assert_eq!(behind.inventory(), vault.peer.inventory());
    assert!(behind.holds(&vault.slept));
    assert!(
        admit::bundle_records(&handed, MAX_HELD_RECORDS, Reading::ONE).is_err(),
        "a reader of format 1 refuses a rooted member's bytes"
    );

    // A whole log's bundle is its records' stream alone.
    let mut seat = Seat::found();
    seat.sign(true);
    assert_eq!(
        admit::held_records(&seat.store()).expect("the member's records"),
        encode_records(&seat.held)
    );

    let host = Host::start(vault.peer.clone());
    let dark = Mac::of_vault(
        root.path(),
        "dark",
        &host,
        &vault.founder,
        &vault.behind,
        Faults::default(),
    );
    let before = dark.file();
    assert!(save_records(&dark.paths, &handed).is_err());
    assert_eq!(dark.stored_file(), (0, before));

    let mac = Mac::of_vault(
        root.path(),
        "mac",
        &host,
        &vault.founder,
        &vault.behind,
        follows(),
    );
    crate::crossing::after_format_mark(&format_mark(&mac.paths), FORMAT_TWO, || Ok(()))
        .expect("the mark");
    save_records(&mac.paths, &handed).expect("a marked Mac takes the handed log");
    let (generation, file) = mac.stored_file();
    assert_eq!(generation, 1);
    assert!(file.starts_with(crate::membership_log::stored::KEPT_MAGIC));
    let store = load_records(&mac.paths).expect("the store");
    assert_eq!(store.inventory(), vault.peer.inventory());
    assert!(store.holds(&vault.slept));
    assert!(host.holding().asked.is_empty(), "no server was asked");
}

/// A rewrite never drops a record its store was read without. A marked Mac reads its store,
/// another writer then appends a record to its records file, and the first store takes a
/// served root: its write of the entry (`persist_stored`, the rewrite at that root) is
/// refused, since the file holds a record the store neither holds nor dropped, and the entry
/// and its file are what they were. A store read again holds that record, takes the same
/// root, and the rewritten file holds the record.
///
/// NEGATIVE CONTROL, a change to the code run against this test and seen to fail:
/// `client::persist_stored` rewriting whatever the file holds (its check that the store knows
/// every record on file skipped): ``a store read before another writer's record does not
/// replace the file``, the rewrite answering `Ok(true)`.
#[test]
fn vault_checkpoint_a_rewrite_drops_no_record_its_store_was_read_without() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let vault = crate::membership_log::pruned_vault();
    let host = Host::start(vault.peer.clone());
    let mac = Mac::of_vault(
        root.path(),
        "mac",
        &host,
        &vault.founder,
        &vault.behind,
        follows(),
    );
    crate::crossing::after_format_mark(&format_mark(&mac.paths), FORMAT_TWO, || Ok(()))
        .expect("the mark");
    let handed = admit::held_records(&vault.peer).expect("the member's records");

    let mut stale = load_records(&mac.paths).expect("the store");
    assert_eq!(
        save_records(&mac.paths, &encode_records([&vault.stray])).expect("another writer"),
        1
    );
    let before = mac.stored_file();
    assert!(
        before
            .1
            .ends_with(&vault.stray.wire_bytes().expect("the record's bytes"))
    );
    assert!(ingest_as_read(&mut stale, &handed).expect("the served root") > 0);
    assert!(stale.kept_frame().expect("a frame").is_some() && !stale.contains(vault.stray.id()));
    let refused = {
        let _lock = StateLock::take(&mac.paths).expect("the state");
        persist_stored(&mac.paths, &stale)
            .expect_err("a store read before another writer's record does not replace the file")
    };
    assert!(
        refused
            .to_string()
            .contains("holds a record its store was read without"),
        "{refused}"
    );
    assert_eq!(mac.stored_file(), before, "the entry is what it was");

    // Read again, the store holds the record, and the rewrite keeps it.
    save_records(&mac.paths, &handed).expect("a store read again takes the root");
    let (generation, file) = mac.stored_file();
    assert_eq!(generation, 1);
    assert!(file.starts_with(crate::membership_log::stored::KEPT_MAGIC));
    let store = load_records(&mac.paths).expect("the store");
    assert!(store.holds(vault.stray.id()) && store.holds(&vault.slept));
    assert!(host.holding().asked.is_empty(), "no server was asked");
}

/// Every reader of a Mac's records entry that names its data file or judges its selection reads
/// a rewritten entry where the Mac reads format 2 (its format mark names it, on a build that
/// reads it), and selects as it always did everywhere else. A Mac takes a served root into a
/// rewritten file; then each reader is walked over it:
/// - `Paths::records_data_file` names the pinned generation's file. aterm-db's moved-home check
///   (`runtime/moved.rs`, the digest of that file), aterm-agent's fleet log (`fleet_log.rs`,
///   its stamp) and census (`fleet_census.rs`, `is_file`) and `client::remove`'s look
///   ([`remove::records_stamp`], and the node loop's watch) name it there: their crates' tests
///   cannot build a rewritten entry (the gate's seam, [`Faults::follows`], is this crate's
///   tests'), so this test makes each of their calls on what it names.
/// - The vault memos' stamps (`vault_memo::stamp_inputs`) and the seat ask's memo
///   ([`seat_ask::mark`]) stamp it, and an append to the generation's file moves both.
/// - `client::genesis`'s private-realm read reads it, frame and records, strictly
///   ([`crate::membership_log::Store::framed_strictly`]).
///
/// The same build on the Mac with the gate shut refuses each, as it did before, and a Mac that
/// never followed selects and stamps exactly as format 1 does.
///
/// NEGATIVE CONTROLS, each a change to the code run against this test and seen to fail:
/// - `Paths::records_data_file` selecting as format 1 alone: ``the data file is the pinned
///   generation's``, a refused layout.
/// - `vault_memo`'s `records_standing` calling `records_storage::standing`: ``the vault memo
///   stamps a rewritten entry``.
/// - `seat_ask::mark` calling `records_storage::standing`: ``the seat ask stamps a rewritten
///   entry``.
/// - `records_storage::rewrite::standing` judging the files named `layout` and `current`:
///   ``an append moves the entry's standing``.
/// - `client::genesis::private_realm` selecting as format 1 (`records_storage::selected`):
///   ``the private-realm read reads a rewritten entry``, a refused layout.
/// - `Store::framed_strictly` decoding the file whole, frame included: ``the private-realm
///   read reads a rewritten entry``.
#[test]
fn vault_checkpoint_every_reader_of_a_rewritten_entry_reads_it() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let vault = crate::membership_log::pruned_vault();
    let host = Host::start(vault.peer.clone());

    // A Mac that never followed selects and stamps as format 1 does.
    let dark = Mac::of_vault(
        root.path(),
        "dark",
        &host,
        &vault.founder,
        &vault.behind,
        Faults::default(),
    );
    let entry = dark.paths.records_file();
    assert_eq!(
        dark.paths.records_data_file().expect("the records file"),
        crate::records_storage::reader_path(&entry).expect("the format-1 selection")
    );
    assert!(
        records_standing(&dark.paths, None) == crate::records_storage::standing(&entry, None),
        "a Mac that never followed stamps its entry as format 1 does"
    );

    let mac = Mac::of_vault(
        root.path(),
        "mac",
        &host,
        &vault.founder,
        &vault.behind,
        follows(),
    );
    mac.pass().expect("a pass that follows");
    let entry = mac.paths.records_file();
    let custody = crate::records_storage::rewrite::data_path(&entry).expect("the layout");
    assert_eq!(custody.generation, 1, "the entry is a rewritten layout");
    let file = std::fs::read(&custody.path).expect("the file");

    // The readers that name the data file.
    let data = mac.paths.records_data_file();
    assert_eq!(
        data.as_ref().ok(),
        Some(&custody.path),
        "the data file is the pinned generation's: {data:?}"
    );
    let data = data.expect("named");
    assert!(data.is_file(), "the census finds a records file");
    assert_eq!(
        std::fs::read(&data).expect("read"),
        file,
        "its digest is the file's"
    );
    let stamp = remove::records_stamp(&mac.paths).expect("the node loop's look stamps it");
    assert_eq!(stamp.len, file.len() as u64);

    // The readers that stamp its selection.
    let memo = || vault_memo::stamp_inputs(&mac.paths, vault_memo::inputs);
    let memo_before = memo().expect("the vault memo stamps a rewritten entry");
    let seat_before = seat_ask::mark(&mac.paths).expect("the seat ask stamps a rewritten entry");
    let records_before = records_standing(&mac.paths, None).expect("the entry stands");
    assert!(
        memo().expect("stamped") == memo_before,
        "an unchanged entry stamps the same"
    );

    // The private-realm read: the vault is no ALab one, and every record of the file places.
    let config = config(&mac.paths).expect("read").expect("a vault");
    let realm = genesis::private_realm(&mac.paths, &config);
    assert!(
        matches!(realm, Ok(true)),
        "the private-realm read reads a rewritten entry: {realm:?}"
    );

    // The vault's next record is appended to the generation's file in place, as the records
    // writer appends (nothing else in the folder changes), which moves every stamp.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&custody.path)
        .and_then(|mut file| file.write_all(&vault.next.wire_bytes().expect("the record's bytes")))
        .expect("an append");
    assert!(
        load_records(&mac.paths)
            .expect("the store")
            .holds(vault.next.id()),
        "the appended record reads"
    );
    assert_eq!(
        mac.paths.records_data_file().expect("named"),
        custody.path,
        "an append keeps the generation"
    );
    assert!(
        records_standing(&mac.paths, None).expect("the entry stands") != records_before,
        "an append moves the entry's standing"
    );
    assert!(
        memo().expect("stamped") != memo_before,
        "an append moves the memo's stamps"
    );
    assert!(
        seat_ask::mark(&mac.paths).expect("stamped") != seat_before,
        "an append moves the seat ask's stamp"
    );
    assert!(
        remove::records_stamp(&mac.paths).expect("stamped") != stamp,
        "an append moves the node loop's look"
    );

    // The same build with the gate shut reads no rewritten layout: each reader refuses.
    mac.suffers(Faults::default());
    assert!(mac.paths.records_data_file().is_err());
    assert!(remove::records_stamp(&mac.paths).is_none());
    assert!(memo().is_none() && seat_ask::mark(&mac.paths).is_none());
    assert!(genesis::private_realm(&mac.paths, &config).is_err());
    mac.suffers(follows());
}
