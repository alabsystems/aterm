// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public server health requires causal completeness of the authenticated frontier.

#![cfg(unix)]

use aterm_secrets::{
    age::Identity,
    commons::binding::Request,
    envelope::{self, Pin, Unsigned},
    membership_log::{Body, Entrant, Inserted, Log, Record, Store},
    service_health,
    service_repair::HealthRequirement,
};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const NAME: &str = "health-completeness";
const WRITER: [u8; 32] = [91; 32];
const INCARNATION: [u8; 16] = [92; 16];
const OLD_SIGNER: [u8; 32] = [93; 32];
const NEW_SIGNER: [u8; 32] = [94; 32];
const HANG: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Endpoint {
    Envelope,
    Records,
}

#[derive(Clone)]
struct Response {
    status: u16,
    bytes: Vec<u8>,
    short: bool,
}

impl Response {
    fn ok(bytes: Vec<u8>) -> Self {
        Self {
            status: 200,
            bytes,
            short: false,
        }
    }
}

#[derive(Default)]
struct Traffic {
    requests: Vec<Endpoint>,
    errors: Vec<String>,
}

struct Peer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    traffic: Arc<Mutex<Traffic>>,
    worker: Option<JoinHandle<()>>,
}

impl Peer {
    fn start(envelope: Response, records: Response) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let traffic = Arc::new(Mutex::new(Traffic::default()));
        let (stopping, observed) = (Arc::clone(&stop), Arc::clone(&traffic));
        let worker = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            ) {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                if let Err(error) = serve(&mut stream, &envelope, &records, &observed) {
                    observed.lock().unwrap().errors.push(error.to_string());
                }
            }
        });
        Self {
            addr,
            stop,
            traffic,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/v1/{NAME}", self.addr)
    }

    fn finish(mut self) -> Traffic {
        self.shutdown().expect("bounded health peer must finish");
        let mut traffic = self.traffic.lock().unwrap();
        std::mem::take(&mut *traffic)
    }

    fn shutdown(&mut self) -> std::thread::Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = TcpStream::connect(self.addr);
            worker.join()
        } else {
            Ok(())
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        // Join before any response/fixture state is dropped, including on panic.
        let _ = self.shutdown();
    }
}

fn serve(
    stream: &mut TcpStream,
    envelope: &Response,
    records: &Response,
    traffic: &Mutex<Traffic>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(HANG))?;
    stream.set_write_timeout(Some(HANG))?;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "health request head too long",
            ));
        }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        head.push(byte[0]);
    }
    let head = std::str::from_utf8(&head).map_err(io::Error::other)?;
    let line = head.split("\r\n").next().unwrap();
    let target = line.split_whitespace().nth(1).unwrap_or("");
    let path = target.split('?').next().unwrap();
    if !line.starts_with("GET ") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected health GET",
        ));
    }
    let (endpoint, response) = if path == format!("/v1/{NAME}") {
        (Endpoint::Envelope, envelope)
    } else if path == format!("/v1/{NAME}/records") {
        (Endpoint::Records, records)
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected health path",
        ));
    };
    traffic.lock().unwrap().requests.push(endpoint);
    let phrase = match response.status {
        200 => "OK",
        403 => "Forbidden",
        _ => unreachable!(),
    };
    let declared = response.bytes.len() + usize::from(response.short) * 19;
    write!(
        stream,
        "HTTP/1.1 {} {phrase}\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n",
        response.status
    )?;
    stream.write_all(&response.bytes)
}

struct Observation {
    name: &'static str,
    accepted: bool,
    error: Option<String>,
}

fn probe(
    name: &'static str,
    envelope: Response,
    records: Response,
    requirement: &HealthRequirement,
    expected_requests: &[Endpoint],
) -> Observation {
    let peer = Peer::start(envelope, records);
    // The peer is this test's own, which answers: each read waits the hang detector.
    let result = service_health::server_within(&peer.url(), requirement, HANG);
    // The actual network worker is joined before any result assertion.
    let traffic = peer.finish();
    assert!(
        traffic.errors.is_empty(),
        "{name}: fixture errors {:?}",
        traffic.errors
    );
    assert_eq!(
        traffic.requests, expected_requests,
        "{name}: actual health route"
    );
    let accepted = result.is_ok();
    let error = result.err().map(|error| error.to_string());
    println!("health {name}: accepted={accepted} error={error:?}");
    Observation {
        name,
        accepted,
        error,
    }
}

struct Fixture {
    records: Vec<Record>,
    envelope: Vec<u8>,
    requirement: HealthRequirement,
}

impl Fixture {
    fn new() -> Self {
        let identity = Identity::from_secret_bytes([95; 32]);
        let request = Request::create(
            [96; 16],
            [97; 32],
            [98; 16],
            INCARNATION,
            &identity,
            &WRITER,
        )
        .unwrap();
        let actor = request.binding().actor();
        let genesis = Record::found(
            &OLD_SIGNER,
            vec![Entrant {
                request,
                node_key: [99; 32],
                name: "member".into(),
                label: "m1".into(),
            }],
            1,
        )
        .unwrap();
        let mut log = Log::new(genesis.clone()).unwrap();
        let mut records = vec![genesis];
        for (at, name) in [(2, "A"), (3, "B")] {
            let record = log
                .draft(&actor, at, Body::Name { name: name.into() })
                .sign(&WRITER, INCARNATION)
                .unwrap();
            assert_eq!(log.insert(record.clone()).unwrap(), Inserted::Held);
            records.push(record);
        }
        assert!(records[1].parents().contains(records[0].id()));
        assert_eq!(records[2].previous(), Some(records[1].id()));
        assert!(records[2].parents().contains(records[1].id()));

        let previous = envelope::sign(
            &Unsigned {
                name: NAME.into(),
                seq: 1,
                time: 1,
                handoff: Vec::new(),
                parents: Vec::new(),
                lockers: Vec::new(),
            },
            &OLD_SIGNER,
        )
        .unwrap();
        let previous = envelope::parse(&previous).unwrap();
        previous.verify_signature().unwrap();
        let current_signer = envelope::signer_public(&NEW_SIGNER).unwrap();
        let old_signer = envelope::signer_public(&OLD_SIGNER).unwrap();
        assert_ne!(current_signer, old_signer);
        let envelope = envelope::sign(
            &Unsigned {
                name: NAME.into(),
                seq: 2,
                time: 2,
                handoff: vec![(
                    old_signer,
                    envelope::endorse(&OLD_SIGNER, NAME, &current_signer).unwrap(),
                )],
                parents: vec![previous.digest()],
                lockers: Vec::new(),
            },
            &NEW_SIGNER,
        )
        .unwrap();
        let current = envelope::parse(&envelope).unwrap();
        current.verify_signature().unwrap();
        assert_eq!(
            current.check_pin(&old_signer, &[]).unwrap(),
            Pin::Rotated(current_signer)
        );

        let complete = read(&wire(records.iter()));
        let frontier = complete.frontier();
        assert_eq!(frontier, vec![*records[2].id()]);
        assert_eq!(complete.inventory(), complete.complete_inventory());
        assert_eq!(complete.complete_inventory().len(), 3);
        let requirement = HealthRequirement {
            vault: NAME.into(),
            signer: hex(&current_signer),
            frontier: current.seq,
            records: frontier.iter().map(hex).collect(),
        };
        assert_eq!(requirement.records, vec![hex(records[2].id())]);
        Self {
            records,
            envelope,
            requirement,
        }
    }

    fn probe(&self, name: &'static str, bytes: Vec<u8>) -> Observation {
        probe(
            name,
            Response::ok(self.envelope.clone()),
            Response::ok(bytes),
            &self.requirement,
            &[Endpoint::Envelope, Endpoint::Records],
        )
    }
}

fn wire<'a>(records: impl IntoIterator<Item = &'a Record>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend_from_slice(record.bytes());
    }
    bytes
}

fn read(bytes: &[u8]) -> Store {
    let mut store = Store::for_vault(NAME).unwrap();
    store.ingest(bytes).unwrap();
    store
}

fn hex(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for byte in bytes {
        write!(&mut out, "{byte:02x}").unwrap();
    }
    out
}

#[test]
fn server_health_requires_the_complete_authenticated_frontier() {
    let fixture = Fixture::new();
    let [g, a, b] = fixture.records.as_slice() else {
        unreachable!()
    };
    let full = wire([g, a, b]);
    let waiting_bytes = wire([g, b]);
    let waiting = read(&waiting_bytes);
    assert!(waiting.contains(b.id()));
    assert!(waiting.complete_bytes(g.id()).is_some());
    assert!(waiting.complete_bytes(b.id()).is_none());
    assert_eq!(waiting.inventory().len(), 2);
    assert_eq!(waiting.complete_inventory().len(), 1);
    let unrooted_bytes = b.bytes().to_vec();
    let unrooted = read(&unrooted_bytes);
    assert!(unrooted.contains(b.id()));
    assert!(unrooted.log().is_none());
    assert_eq!(unrooted.inventory().len(), 1);
    assert!(unrooted.complete_inventory().is_empty());

    // Gather both original false positives before either is asserted.
    let waiting_result = fixture.probe("waiting G+B", waiting_bytes);
    let unrooted_result = fixture.probe("unrooted B", unrooted_bytes);
    let complete_result = fixture.probe("complete rotated-signer G+A+B", full.clone());
    let mut duplicated = full.clone();
    duplicated.extend_from_slice(b.bytes());
    let duplicate_result = fixture.probe("complete duplicate frames", duplicated);

    let mut refusals = Vec::new();
    for (name, requirement) in [
        (
            "wrong vault",
            HealthRequirement {
                vault: "another-vault".into(),
                ..fixture.requirement.clone()
            },
        ),
        (
            "wrong signer",
            HealthRequirement {
                signer: "0".repeat(64),
                ..fixture.requirement.clone()
            },
        ),
        (
            "stale envelope",
            HealthRequirement {
                frontier: fixture.requirement.frontier + 1,
                ..fixture.requirement.clone()
            },
        ),
    ] {
        refusals.push(probe(
            name,
            Response::ok(fixture.envelope.clone()),
            Response::ok(full.clone()),
            &requirement,
            &[Endpoint::Envelope],
        ));
    }
    for (name, requirement) in [
        (
            "missing required id",
            HealthRequirement {
                records: vec!["0".repeat(64)],
                ..fixture.requirement.clone()
            },
        ),
        (
            "empty requirement",
            HealthRequirement {
                records: Vec::new(),
                ..fixture.requirement.clone()
            },
        ),
    ] {
        refusals.push(probe(
            name,
            Response::ok(fixture.envelope.clone()),
            Response::ok(full.clone()),
            &requirement,
            &[Endpoint::Envelope, Endpoint::Records],
        ));
    }

    let mut bad_envelope = fixture.envelope.clone();
    let marker = b"signature: ";
    let at = bad_envelope
        .windows(marker.len())
        .position(|bytes| bytes == marker)
        .unwrap()
        + marker.len();
    bad_envelope[at] = if bad_envelope[at] == b'A' { b'B' } else { b'A' };
    let parsed = envelope::parse(&bad_envelope).unwrap();
    assert!(parsed.verify_signature().is_err());
    refusals.push(probe(
        "bad envelope signature",
        Response::ok(bad_envelope),
        Response::ok(full.clone()),
        &fixture.requirement,
        &[Endpoint::Envelope],
    ));

    let mut bad_record = b.bytes().to_vec();
    *bad_record.last_mut().unwrap() ^= 1;
    assert!(Record::decode(&bad_record).is_err());
    let mut corrupt = wire([g, a]);
    corrupt.extend_from_slice(&bad_record);
    let mut strict = Store::for_vault(NAME).unwrap();
    assert!(strict.ingest(&corrupt).is_err());
    refusals.push(fixture.probe("bad record signature", corrupt));
    let mut torn = full.clone();
    torn.extend_from_slice(b"ALM");
    let mut strict = Store::for_vault(NAME).unwrap();
    assert!(strict.ingest(&torn).is_err());
    refusals.push(fixture.probe("malformed record tail", torn));

    let mut short_envelope = Response::ok(fixture.envelope.clone());
    short_envelope.short = true;
    refusals.push(probe(
        "truncated envelope HTTP",
        short_envelope,
        Response::ok(full.clone()),
        &fixture.requirement,
        &[Endpoint::Envelope],
    ));
    let mut short_records = Response::ok(full.clone());
    short_records.short = true;
    refusals.push(probe(
        "truncated records HTTP",
        Response::ok(fixture.envelope.clone()),
        short_records,
        &fixture.requirement,
        &[Endpoint::Envelope, Endpoint::Records],
    ));
    refusals.push(probe(
        "envelope HTTP refusal",
        Response {
            status: 403,
            bytes: b"refused".to_vec(),
            short: false,
        },
        Response::ok(full.clone()),
        &fixture.requirement,
        &[Endpoint::Envelope],
    ));
    refusals.push(probe(
        "records HTTP refusal",
        Response::ok(fixture.envelope.clone()),
        Response {
            status: 403,
            bytes: b"refused".to_vec(),
            short: false,
        },
        &fixture.requirement,
        &[Endpoint::Envelope, Endpoint::Records],
    ));

    assert!(
        complete_result.accepted,
        "rotated-signer complete history: {:?}",
        complete_result.error
    );
    assert!(
        duplicate_result.accepted,
        "valid duplicate frames: {:?}",
        duplicate_result.error
    );
    for result in refusals {
        assert!(!result.accepted, "{} must refuse", result.name);
        assert!(result.error.is_some());
    }
    println!(
        "all server-health identity, signature, strict-ingest and transport guards passed; waiting_accepted={} unrooted_accepted={}",
        waiting_result.accepted, unrooted_result.accepted
    );
    assert!(
        !waiting_result.accepted,
        "health accepted required B while its parent A was absent"
    );
    assert!(
        !unrooted_result.accepted,
        "health accepted required B without Genesis or its parent"
    );
}
