// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use super::*;

/// A wait for something that must happen: a hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// A pace a peer the test serves keeps however loaded the machine: it always answers, so its
/// race waits the hang detector, never [`PEER_PACE`]'s 5 s head and 2 s idle.
const ANSWERS: Pace = Pace {
    head: HANG,
    window: HANG,
    step: 1024 * 1024,
    idle: HANG,
};

fn sha256_hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

/// A peer that answers one request with `body`, then waits for the client to close the
/// connection — once the client is done with the transfer — and says so on the channel.
fn one_shot_peer(body: Vec<u8>) -> (String, mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (done, judged) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut conn, _)) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        ) else {
            return;
        };
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && conn.read(&mut byte).is_ok_and(|n| n == 1) {
            head.push(byte[0]);
        }
        let _ = write!(
            conn,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let _ = conn.write_all(&body);
        let mut rest = Vec::new();
        let _ = conn.read_to_end(&mut rest);
        let _ = done.send(());
    });
    (addr, judged)
}

/// A release host that lands `bytes` at `dest` once `go` says so, or gives up when stopped.
fn release_host<'a>(
    bytes: &'a [u8],
    go: impl FnOnce(&AtomicBool) -> bool + 'a,
) -> impl FnOnce(&Path, &AtomicBool) -> Result<(), String> + 'a {
    move |dest: &Path, stop: &AtomicBool| {
        if !go(stop) {
            return Err(aterm_update_core::DOWNLOAD_STOPPED.to_string());
        }
        std::fs::write(dest, bytes).map_err(|e| e.to_string())
    }
}

/// Wait for `stop`, or the hang detector.
fn until_stopped(stop: &AtomicBool) -> bool {
    let began = std::time::Instant::now();
    while !stop.load(Ordering::Acquire) {
        assert!(began.elapsed() < HANG, "the release host was never stopped");
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

#[test]
fn a_peer_that_holds_the_unit_lands_it_and_stops_the_release_host() {
    let dir = crate::testdir::dir("atpkg-peers-wins-");
    let unit = b"the signed toolset archive".to_vec();
    let (peer, _judged) = one_shot_peer(unit.clone());
    let dest = dir.join("trust-1.tar.zst");
    let got = race_paced(
        &[peer],
        &sha256_hex(&unit),
        unit.len() as u64,
        &dest,
        release_host(b"never written", until_stopped),
        &ANSWERS,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        unit,
        "the peer's unit is the download"
    );
    assert!(!suffixed(&dest, PEER_PART_SUFFIX).exists());
}

#[test]
fn a_peer_unit_with_other_bytes_never_lands_even_when_the_release_host_fails() {
    let dir = crate::testdir::dir("atpkg-peers-corrupt-");
    let unit = b"the signed toolset archive".to_vec();
    let mut corrupt = unit.clone();
    corrupt[3] ^= 0x20;
    let (peer, judged) = one_shot_peer(corrupt);
    let dest = dir.join("trust-1.tar.zst");
    let got = race(
        &[peer],
        &sha256_hex(&unit),
        unit.len() as u64,
        &dest,
        |_: &Path, _: &AtomicBool| {
            judged.recv_timeout(HANG).expect("the peer was asked");
            Err("the release host is down".to_string())
        },
    );
    assert_eq!(got, Err("the release host is down".to_string()));
    assert!(
        !dest.exists(),
        "bytes that miss the signed digest never land"
    );
    assert!(
        !suffixed(&dest, PEER_PART_SUFFIX).exists(),
        "nor is their partial left"
    );
}

#[test]
fn a_rejected_peer_leaves_the_release_host_download_to_finish() {
    let dir = crate::testdir::dir("atpkg-peers-refetch-");
    let unit = b"the signed toolset archive".to_vec();
    let mut corrupt = unit.clone();
    corrupt[0] ^= 0x01;
    let (peer, judged) = one_shot_peer(corrupt);
    let dest = dir.join("trust-1.tar.zst");
    let got = race(
        &[peer],
        &sha256_hex(&unit),
        unit.len() as u64,
        &dest,
        release_host(&unit, |stop| {
            judged.recv_timeout(HANG).expect("the peer was asked");
            !stop.load(Ordering::Acquire)
        }),
    );
    assert_eq!(got, Ok(()), "the release host's download stands");
    assert_eq!(std::fs::read(&dest).unwrap(), unit);
}

#[test]
fn a_good_peer_lands_its_unit_when_the_release_host_fails() {
    // The negative control of the corrupt-peer row: the same race with the signed bytes.
    let dir = crate::testdir::dir("atpkg-peers-good-");
    let unit = b"the signed toolset archive".to_vec();
    let (peer, judged) = one_shot_peer(unit.clone());
    let dest = dir.join("trust-1.tar.zst");
    let got = race_paced(
        &[peer],
        &sha256_hex(&unit),
        unit.len() as u64,
        &dest,
        |_: &Path, _: &AtomicBool| {
            judged.recv_timeout(HANG).expect("the peer was asked");
            Err("the release host is down".to_string())
        },
        &ANSWERS,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(std::fs::read(&dest).unwrap(), unit);
}

#[test]
fn a_row_that_signs_no_size_is_never_taken_from_a_peer() {
    // A peer's body is bounded by the signed size; with none, the release host alone fetches.
    let dir = crate::testdir::dir("atpkg-peers-unsized-");
    let unit = b"the signed toolset archive".to_vec();
    let (peer, _judged) = one_shot_peer(unit.clone());
    let dest = dir.join("trust-1.tar.zst");
    let got = race(
        &[peer],
        &sha256_hex(&unit),
        0,
        &dest,
        |_: &Path, _: &AtomicBool| Err("the release host is down".to_string()),
    );
    assert_eq!(got, Err("the release host is down".to_string()));
    assert!(!dest.exists());
}

/// A build of `trust` at `build` with its download `dl` holding `unit`.
fn staged(layout: &Layout, build: u64, unit: &[u8]) -> (PathBuf, PathBuf) {
    let build_dir = layout.prefix.join(format!("store/trust/{build}"));
    std::fs::create_dir_all(&build_dir).unwrap();
    let dl = layout.prefix.join(format!("trust-{build}.tar.zst"));
    std::fs::write(&dl, unit).unwrap();
    (build_dir, dl)
}

/// Room to keep anything ([`keep_with_free`]).
const ROOM: Option<u64> = Some(u64::MAX);

/// `layout` as a live server of it leaves it: `units/` made and this process recorded as
/// serving ([`serving`]).
fn as_if_serving(layout: &Layout) {
    std::fs::create_dir_all(units_dir(layout)).unwrap();
    record_serving(layout, SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)));
}

#[test]
fn a_store_keeps_the_signed_bytes_of_each_unit_while_a_server_of_it_is_live() {
    let layout = crate::testdir::layout("atpkg-peers-keep-");
    let unit = b"the signed toolset archive";
    let (build, dl) = staged(&layout, 7, unit);
    let sha = sha256_hex(unit);
    let beside = build.with_file_name("7.unit");

    keep_with_free(&layout, &dl, &sha, &build, ROOM);
    assert!(!beside.exists(), "no server made units/, so none is kept");
    std::fs::create_dir_all(units_dir(&layout)).unwrap();
    keep_with_free(&layout, &dl, &sha, &build, ROOM);
    assert!(
        !beside.exists(),
        "nor while no server is live, though one made units/ (a Mac that left its fleet)"
    );

    as_if_serving(&layout);
    keep_with_free(&layout, &dl, &sha256_hex(b"other bytes"), &build, ROOM);
    assert!(
        !beside.exists(),
        "a file that misses its signed digest is not kept"
    );
    keep_with_free(&layout, &dl, &sha, &build, Some(KEEP_FLOOR - 1));
    assert!(!beside.exists(), "nor any unit while the disk is short");

    keep_with_free(&layout, &dl, &sha, &build, Some(KEEP_FLOOR));
    assert_eq!(
        std::fs::read(&beside).unwrap(),
        unit,
        "kept beside its build"
    );
    assert_eq!(std::fs::read(units_dir(&layout).join(&sha)).unwrap(), unit);
    // The pass removes its download once the build is live; the kept unit outlives it.
    std::fs::remove_file(&dl).unwrap();
    sweep_kept(&layout);
    assert!(
        units_dir(&layout).join(&sha).exists(),
        "a unit its build holds stays"
    );
    assert_eq!(
        sharing(&layout, None).kept,
        (1, unit.len() as u64),
        "the doctor counts it"
    );

    crate::store::discard_build(&build);
    assert!(!beside.exists(), "a discarded build takes its unit with it");
    sweep_kept(&layout);
    if cfg!(unix) {
        assert!(
            !units_dir(&layout).join(&sha).exists(),
            "and gc's sweep its link under units/"
        );
    } else {
        // Off Unix there is no link count to read: the unit stands until the person's off.
        assert!(units_dir(&layout).join(&sha).exists(), "kept until --off");
        turn_off(&layout).unwrap();
        assert!(!units_dir(&layout).exists(), "--off takes every unit");
    }
}

#[test]
fn the_persons_off_keeps_no_unit_and_stands_until_turned_back_on() {
    let layout = crate::testdir::layout("atpkg-peers-off-");
    let unit = b"the signed toolset archive";
    let (build, dl) = staged(&layout, 7, unit);
    let sha = sha256_hex(unit);
    let beside = build.with_file_name("7.unit");
    as_if_serving(&layout);
    keep_with_free(&layout, &dl, &sha, &build, ROOM);
    assert!(beside.exists());

    turn_off(&layout).unwrap();
    assert!(layout.units_off().is_file(), "the off is recorded");
    assert!(!beside.exists(), "every build's kept unit goes");
    assert!(!units_dir(&layout).exists(), "and units/ with them");
    assert!(sharing(&layout, None).off);

    // A server that made units/ just before the off was recorded keeps nothing after it.
    as_if_serving(&layout);
    keep_with_free(&layout, &dl, &sha, &build, ROOM);
    assert!(!beside.exists(), "nothing is kept while the off stands");
    turn_off(&layout).unwrap();
    assert!(layout.units_off().is_file(), "a second off keeps the first");

    turn_on(&layout).unwrap();
    assert!(!layout.units_off().exists(), "--on lifts it");
    turn_on(&layout).unwrap();
    as_if_serving(&layout);
    keep_with_free(&layout, &dl, &sha, &build, ROOM);
    assert!(beside.exists(), "and the store keeps units again");
}

#[test]
fn a_foreground_server_never_lifts_the_persons_off() {
    let layout = crate::testdir::layout("atpkg-peers-serve-off-");
    turn_off(&layout).unwrap();
    let refused = serve(&layout, SocketAddr::from(([127, 0, 0, 1], 0)), |_| {
        panic!("nothing is served while the person's off stands")
    });
    assert!(refused.is_err(), "{refused:?}");
    assert!(layout.units_off().is_file(), "the off still stands");
    assert!(
        !units_dir(&layout).exists(),
        "and nothing was made to keep units in"
    );
}

/// A private aterm (this Cargo test binary) neither listens on nor dials the person's toolset
/// port at any address, however a listen or a peer names it: each is refused before a socket is
/// made, and a port of the test's own is bound and dialed as before. A rule that let the port
/// through reaches no socket of the person's here either: each listen names a store not made
/// yet, which `open` turns away before its bind, and each dial goes through a `dial` that makes
/// no socket.
///
/// NEGATIVE CONTROL (2026-11-04): with `open` or `connect_by` asking no rule, the shape before, or
/// a rule that holds the fleet's port alone, a listen fails `NotFound` (no store), a dial reaches
/// the panicking `dial`, and this fails without a sandbox.
#[test]
fn a_test_binary_listens_on_and_dials_no_default_port_and_its_own_as_before() {
    let absent = crate::testdir::absent_layout("atpkg-peers-persons-port-");
    let refused = |e: io::Error| assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
    for at in ["127.0.0.1", "[::1]", "0.0.0.0"] {
        let at: SocketAddr = format!("{at}:{DEFAULT_PORT}").parse().unwrap();
        refused(open(&absent, at).expect_err("a listen on the person's port"));
        let served = serve(&absent, at, |bound| {
            panic!("a private aterm served {bound}")
        });
        refused(served.expect_err("a foreground server on the person's port"));
    }
    let named: Vec<String> = ["127.0.0.1", "[::1]", "localhost"]
        .iter()
        .map(ToString::to_string)
        .collect();
    let peers = configured(&named);
    assert_eq!(peers.len(), named.len(), "each peer is dialed: {peers:?}");
    for peer in peers {
        let dialed = connect_by(&peer, None, |addr| panic!("a private aterm dialed {addr}"));
        let Err(dialed) = dialed else {
            panic!("a private aterm reached {peer}")
        };
        refused(dialed);
    }

    let layout = crate::testdir::layout("atpkg-peers-own-port-");
    let own = open(&layout, SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("the control: a port of its own");
    let at = own.local_addr().unwrap();
    assert!(
        connect(&at.to_string(), None).is_ok(),
        "the control: {at} is dialed"
    );
}

#[test]
fn the_status_read_says_where_serving_stands_as_the_doctor_does_from_the_two_records() {
    let layout = crate::testdir::layout("atpkg-peers-status-");
    let doctor = |members: usize| {
        let fleet = Fleet {
            members,
            dial: Vec::new(),
            held: None,
        };
        with_fleet(fleet, || sharing(&layout, None).serves())
    };
    let at = SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT));
    let read = |members: usize| (serves(&layout, members), doctor(members));
    assert_eq!(
        read(0),
        (Serves::NoMember, Serves::NoMember),
        "on, no member"
    );
    assert_eq!(read(2), (Serves::Idle, Serves::Idle), "on, nothing serving");
    as_if_serving(&layout);
    assert_eq!(read(2), (Serves::Serving(at), Serves::Serving(at)));
    turn_off(&layout).unwrap();
    assert!(turned_off(&layout));
    assert_eq!(
        read(2),
        (Serves::Off, Serves::Off),
        "the off stands over a live server's record"
    );
    turn_on(&layout).unwrap();
    assert!(!turned_off(&layout), "--on lifts it");
    assert_eq!(read(2), (Serves::Serving(at), Serves::Serving(at)));
}

#[test]
fn an_off_written_before_the_store_is_made_stands() {
    let layout = crate::testdir::absent_layout("atpkg-peers-off-early-");
    turn_off(&layout).unwrap();
    assert!(layout.units_off().is_file(), "the off is recorded");
    assert!(sharing(&layout, None).off);
}

/// The fleet members a sharing store admits in a test, and how often it reads them.
type Members = Arc<dyn Fn() -> Vec<IpAddr> + Send + Sync>;

/// This host's loopback address, the one member a test's peer connects from.
fn loopback() -> Vec<IpAddr> {
    vec![IpAddr::from([127, 0, 0, 1])]
}

/// Run `share` for `layout` on a kernel-assigned loopback port, admitting the addresses
/// `members` names (read at most every `refresh`), on a thread of its own: the address of its
/// bound listener, its stop flag, its notes and the thread.
fn sharing_on(
    layout: &Layout,
    members: Members,
    refresh: Duration,
) -> (
    String,
    Arc<Stop>,
    mpsc::Receiver<String>,
    std::thread::JoinHandle<()>,
) {
    let listen = SocketAddr::from(([127, 0, 0, 1], 0));
    let stop = Arc::new(Stop::new());
    let (said, notes) = mpsc::channel();
    let (bound, ready) = mpsc::channel();
    let thread = std::thread::spawn({
        let (layout, stop) = ((*layout).clone(), Arc::clone(&stop));
        move || {
            share(
                &layout,
                &Share {
                    listen,
                    stop: &stop,
                    members: &*members,
                    refresh,
                    note: &|line| {
                        if let Some(at) = serving(&layout) {
                            let _ = bound.send(at);
                        }
                        let _ = said.send(line.to_owned());
                    },
                },
            );
        }
    });
    let at = ready
        .recv_timeout(HANG)
        .expect("the sharing store bound its listener");
    (at.to_string(), stop, notes, thread)
}

/// The note `notes` hears next that contains `word`, within the hang detector.
fn heard(notes: &mpsc::Receiver<String>, word: &str) -> String {
    let began = std::time::Instant::now();
    loop {
        let left = HANG.saturating_sub(began.elapsed());
        let note = notes.recv_timeout(left).expect("the server said so");
        if note.contains(word) {
            return note;
        }
    }
}

/// Send `request` to `addr` and read the whole answer.
fn ask(addr: &str, request: &str) -> String {
    let mut conn = TcpStream::connect(addr).unwrap();
    conn.set_read_timeout(Some(HANG)).unwrap();
    // A server that closes a source it does not admit may do so before the request is sent.
    let _ = conn.write_all(request.as_bytes());
    let mut answer = String::new();
    let _ = conn.read_to_string(&mut answer);
    answer
}

/// Send `request` to `addr`, close the sending half so the server reads EOF, and read the whole
/// answer.
fn ask_closed(addr: &str, request: &str) -> String {
    let mut conn = TcpStream::connect(addr).unwrap();
    conn.set_read_timeout(Some(HANG)).unwrap();
    conn.write_all(request.as_bytes()).unwrap();
    conn.shutdown(std::net::Shutdown::Write).unwrap();
    let mut answer = String::new();
    let _ = conn.read_to_string(&mut answer);
    answer
}

#[test]
fn a_sharing_store_serves_a_kept_unit_by_default_and_lists_nothing() {
    let layout = crate::testdir::layout("atpkg-peers-share-");
    let unit = b"the signed toolset archive".to_vec();
    let (build, dl) = staged(&layout, 7, &unit);
    let sha = sha256_hex(&unit);
    let (addr, stop, notes, thread) = sharing_on(&layout, Arc::new(loopback), HANG);
    heard(&notes, "serving");
    assert!(units_dir(&layout).is_dir(), "the server made units/");
    assert_eq!(
        serving(&layout).map(|at| at.to_string()),
        Some(addr.clone()),
        "where it serves is recorded"
    );
    keep_with_free(&layout, &dl, &sha, &build, ROOM);

    let dest = layout.prefix.join("got.tar.zst");
    let got = race_paced(
        std::slice::from_ref(&addr),
        &sha,
        unit.len() as u64,
        &dest,
        release_host(b"never written", until_stopped),
        &ANSWERS,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(std::fs::read(&dest).unwrap(), unit, "served from the peer");

    // No listing: only `GET /unit/<sha256>` of a kept unit is answered with bytes; a unit this
    // store does not hold is 404.
    for (request, status) in [
        ("GET / HTTP/1.1\r\n\r\n", "404"),
        ("GET /unit/ HTTP/1.1\r\n\r\n", "404"),
        ("GET /units HTTP/1.1\r\n\r\n", "404"),
        (&format!("GET /unit/{sha}/.. HTTP/1.1\r\n\r\n"), "404"),
        (
            &format!("GET /unit/{} HTTP/1.1\r\n\r\n", sha256_hex(b"another unit")),
            "404",
        ),
        (&format!("HEAD /unit/{sha} HTTP/1.1\r\n\r\n"), "405"),
    ] {
        let answer = ask(&addr, request);
        assert!(
            answer.starts_with(&format!("HTTP/1.1 {status}")) && answer.ends_with("\r\n\r\n"),
            "{request:?} → {answer:?}"
        );
    }

    stop.set();
    thread.join().unwrap();
    // Observe this listener's owner and record: its released port may already belong to
    // another parallel test, so a new connection cannot prove whether this owner closed it.
    assert_eq!(serving(&layout), None, "a stopped server's record goes");
}

#[test]
fn the_persons_off_closes_a_sharing_store_and_on_opens_it_again() {
    let layout = crate::testdir::layout("atpkg-peers-share-off-");
    let (_, stop, notes, thread) = sharing_on(&layout, Arc::new(loopback), HANG);
    heard(&notes, "serving");
    turn_off(&layout).unwrap();
    heard(&notes, "off");
    assert_eq!(
        serving(&layout),
        None,
        "the off removes its listener's record"
    );
    turn_on(&layout).unwrap();
    heard(&notes, "serving");
    let at = serving(&layout).expect("the resumed store records its listener");
    assert!(
        TcpStream::connect(at).is_ok(),
        "--on opened its recorded listener"
    );
    stop.set();
    thread.join().unwrap();
}

#[test]
fn a_server_makes_no_store_that_is_not_there_yet() {
    let layout = crate::testdir::absent_layout("atpkg-peers-nostore-");
    let refused = serve(&layout, SocketAddr::from(([127, 0, 0, 1], 0)), |_| {
        panic!("nothing is served from a store not made yet")
    });
    assert_eq!(refused.map_err(|e| e.kind()), Err(io::ErrorKind::NotFound));
    assert!(
        !layout.prefix.exists(),
        "the pass that installs makes the store"
    );
}

#[test]
fn a_download_races_the_fleet_unless_a_person_wrote_its_peers() {
    let named = vec!["mac-b.local".to_owned()];
    let none: &[String] = &[];
    let fleet = [
        "10.0.0.2:7487",
        "10.0.0.3:7487",
        "10.0.0.2:7487",
        "not a peer",
    ];
    with_fleet_peers(&fleet, || {
        assert_eq!(
            unit_peers(None),
            ["10.0.0.2:7487", "10.0.0.3:7487"],
            "unwritten: the fleet's members, each once"
        );
        assert_eq!(
            unit_peers(Some(named.as_slice())),
            ["mac-b.local:7487"],
            "a written list wins"
        );
        assert_eq!(
            unit_peers(Some(none)),
            Vec::<String>::new(),
            "and `peers = []` takes from none"
        );
    });
    let many: Vec<String> = (0..40).map(|n| format!("10.0.1.{n}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    with_fleet_peers(&many, || {
        assert_eq!(unit_peers(None).len(), FLEET_PEERS_MAX, "at most this many");
    });
    with_fleet_peers(&[], || {
        assert!(unit_peers(None).is_empty(), "no fleet, no peer")
    });
}

#[test]
fn the_doctor_counts_the_members_a_server_answers_apart_from_those_a_download_dials() {
    let layout = crate::testdir::layout("atpkg-peers-doctor-");
    let held = Fleet {
        members: 3,
        dial: Vec::new(),
        held: Some("sharing is off".to_owned()),
    };
    let read = with_fleet(held, || sharing(&layout, None));
    assert_eq!(
        (read.members, read.serves(), read.takes()),
        (3, Serves::Idle, Takes::Held),
        "a Mac whose downloads dial no member is still in its fleet, and says why: {read:?}"
    );
    let dialed = Fleet {
        members: 3,
        dial: vec!["10.0.0.2:7487".to_owned(), "10.0.0.3:7487".to_owned()],
        held: None,
    };
    let read = with_fleet(dialed, || sharing(&layout, None));
    assert_eq!((read.members, read.takes()), (3, Takes::Fleet(2)));
    let read = with_fleet(Fleet::default(), || sharing(&layout, None));
    assert_eq!(
        (read.serves(), read.takes()),
        (Serves::NoMember, Takes::ReleaseHost),
        "the control: no fleet, no member"
    );
}

#[test]
fn the_doctor_says_off_first_then_a_live_server_then_whether_there_is_a_member() {
    let at = SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT));
    let on = Sharing {
        off: false,
        serving: Some(at),
        kept: (2, 3 * 1024 * 1024),
        members: 3,
        dials: 3,
        held: None,
        written: None,
    };
    let with = |change: &dyn Fn(&mut Sharing)| {
        let mut read = on.clone();
        change(&mut read);
        (read.serves(), read.takes())
    };
    assert_eq!(
        with(&|_: &mut Sharing| {}),
        (Serves::Serving(at), Takes::Fleet(3))
    );
    assert_eq!(
        with(&|read: &mut Sharing| read.off = true),
        (Serves::Off, Takes::Fleet(3)),
        "the off stands over a server's stale record"
    );
    assert_eq!(
        with(&|read: &mut Sharing| read.serving = None).0,
        Serves::Idle
    );
    assert_eq!(
        with(&|read: &mut Sharing| {
            read.serving = None;
            read.members = 0;
            read.dials = 0;
        }),
        (Serves::NoMember, Takes::ReleaseHost)
    );
    assert_eq!(
        with(&|read: &mut Sharing| read.written = Some(0)).1,
        Takes::Written(0),
        "a written list wins over the fleet"
    );
    assert_eq!(
        with(&|read: &mut Sharing| {
            read.dials = 0;
            read.held = Some("sharing is off".to_owned());
        })
        .1,
        Takes::Held
    );
}

/// A peer that answers one request with a head declaring `size` bytes, then sends one byte
/// each `every` for as long as the client reads, at most [`HANG`].
fn trickling_peer(size: u64, every: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let Ok((mut conn, _)) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        ) else {
            return;
        };
        let _ = write!(conn, "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n\r\n");
        let began = std::time::Instant::now();
        while began.elapsed() < HANG && conn.write_all(b"x").is_ok() {
            std::thread::sleep(every);
        }
    });
    addr
}

/// A pace a trickle of a byte every 50 ms can never keep: 64 KiB within each 300 ms.
const QUICK: Pace = Pace {
    head: HANG,
    window: Duration::from_millis(300),
    step: 64 * 1024,
    idle: HANG,
};

#[test]
fn a_trickling_peer_cannot_hold_a_download_whose_release_host_failed() {
    let dir = crate::testdir::dir("atpkg-peers-trickle-");
    let unit = vec![7u8; 1024 * 1024];
    let peer = trickling_peer(unit.len() as u64, Duration::from_millis(50));
    let dest = dir.join("trust-1.tar.zst");
    let (done, ended) = mpsc::channel();
    std::thread::spawn({
        let dest = dest.clone();
        move || {
            let got = race_paced(
                &[peer],
                &sha256_hex(&unit),
                unit.len() as u64,
                &dest,
                |_: &Path, _: &AtomicBool| Err("the release host is down".to_string()),
                &QUICK,
            );
            let _ = done.send(got);
        }
    });
    assert_eq!(
        ended.recv_timeout(HANG).expect("the download ended"),
        Err("the release host is down".to_string()),
        "the peer that fell below the pace was passed over"
    );
    assert!(!dest.exists());
    assert!(!suffixed(&dest, PEER_PART_SUFFIX).exists());
}

#[test]
fn a_client_that_trickles_its_request_holds_no_server_slot() {
    let dir = crate::testdir::dir("atpkg-peers-slow-client-");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (conn, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    let pace = Pace {
        head: Duration::from_millis(300),
        ..QUICK
    };
    let (done, ended) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(answer(&conn, Some(&dir), &pace).map_err(|e| e.kind()));
    });
    let began = std::time::Instant::now();
    let request = format!("GET /unit/{} HTTP/1.1\r\n", "0".repeat(64)).into_bytes();
    let mut request = request.iter().copied().cycle();
    let answered = loop {
        if let Ok(answered) = ended.try_recv() {
            break answered;
        }
        assert!(
            began.elapsed() < HANG,
            "the server never gave up on its client"
        );
        let _ = client.write_all(&[request.next().unwrap_or(b' ')]);
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        answered,
        Err(io::ErrorKind::TimedOut),
        "a head that never ends within the pace ends the answer"
    );
}

#[test]
fn one_address_holds_at_most_its_share_of_a_servers_slots() {
    let slots = Slots::default();
    let (one, other) = (IpAddr::from([10, 0, 0, 2]), IpAddr::from([10, 0, 0, 3]));
    for _ in 0..SERVE_PER_SOURCE {
        assert!(slots.take(one));
    }
    assert!(!slots.take(one), "its share is held");
    assert!(slots.take(other), "another address still has slots");
    slots.give(one);
    assert!(slots.take(one), "a slot given back is taken again");
    let many = Slots::default();
    for n in 0..SERVE_CONNECTIONS {
        let ip = IpAddr::from([10, 0, 1, u8::try_from(n).unwrap()]);
        assert!(many.take(ip));
    }
    assert!(
        !many.take(IpAddr::from([10, 0, 2, 1])),
        "every slot is held"
    );
}

#[test]
fn a_sharing_store_reads_its_members_once_per_refresh_not_per_connection() {
    for (refresh, reads) in [(HANG, 1), (Duration::ZERO, 3)] {
        let layout = crate::testdir::layout("atpkg-peers-members-");
        let read = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let members: Members = Arc::new({
            let read = Arc::clone(&read);
            move || {
                read.fetch_add(1, Ordering::AcqRel);
                Vec::new()
            }
        });
        let (addr, stop, notes, thread) = sharing_on(&layout, members, refresh);
        heard(&notes, "serving");
        for _ in 0..3 {
            assert_eq!(ask(&addr, "GET / HTTP/1.1\r\n\r\n"), "", "closed unread");
        }
        stop.set();
        thread.join().unwrap();
        assert_eq!(
            read.load(Ordering::Acquire),
            reads,
            "refreshed every {refresh:?}"
        );
    }
}

#[test]
fn only_a_host_with_an_optional_port_is_a_peer() {
    let peers: Vec<String> = [
        "mac-b.local",
        "10.0.0.5:7000",
        "[fe80::1]:7001",
        "[::1]",
        "http://mac-b.local",
        "user@mac-b",
        "mac b",
        "mac-b:0",
        "",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        configured(&peers),
        [
            "mac-b.local:7487",
            "10.0.0.5:7000",
            "[fe80::1]:7001",
            "[::1]:7487"
        ]
    );
}

#[test]
fn an_oversized_or_incomplete_request_head_is_refused_without_answering() {
    let layout = crate::testdir::layout("atpkg-peers-oversized-head-");
    let unit = b"kept unit".to_vec();
    let (build, dl) = staged(&layout, 1, &unit);
    let sha = sha256_hex(&unit);
    let (addr, stop, notes, thread) = sharing_on(&layout, Arc::new(loopback), HANG);
    heard(&notes, "serving");
    keep_with_free(&layout, &dl, &sha, &build, ROOM);

    // 1. Oversized headers exceeding REQUEST_HEAD_MAX (8 KiB) without a terminating blank line.
    let oversized = format!(
        "GET /unit/{sha} HTTP/1.1\r\nX-Pad: {}\r\n\r\n",
        "a".repeat(10 * 1024)
    );
    assert_eq!(
        ask(&addr, &oversized),
        "",
        "oversized request head must be closed without serving the unit"
    );

    // 2. Incomplete request head without a terminating blank line before EOF.
    let incomplete = format!("GET /unit/{sha} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    assert_eq!(
        ask_closed(&addr, &incomplete),
        "",
        "incomplete request head must not be answered"
    );

    // Control: a well-formed request still receives 200 OK.
    assert!(
        ask(&addr, &format!("GET /unit/{sha} HTTP/1.1\r\n\r\n")).starts_with("HTTP/1.1 200 OK\r\n"),
        "valid request still served"
    );

    // Conflicting duplicate Content-Length headers are rejected; identical ones pass.
    let mut dup_conflict = io::Cursor::new(
        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nContent-Length: 4\r\n\r\n".as_slice(),
    );
    assert_eq!(response_length(&mut dup_conflict), None);
    let mut dup_same = io::Cursor::new(
        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nContent-Length: 9\r\n\r\n".as_slice(),
    );
    assert_eq!(response_length(&mut dup_same), Some(9));

    stop.set();
    thread.join().unwrap();
}

/// NEGATIVE CONTROL: `turn_off` never follows symlinked `store` or `store/<program>` directories
/// and never unlinks a symlinked `*.unit`, while unlinking real `<build>.unit` files in place.
#[cfg(unix)]
#[test]
fn turn_off_never_follows_symlinked_store_programs_or_units() {
    let layout = crate::testdir::layout("atpkg-peers-turn-off-symlink-");
    let outside = layout.prefix.parent().unwrap().join(format!(
        "atpkg-peers-outside-{}",
        layout.prefix.file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir_all(outside.join("victim")).unwrap();
    std::fs::write(outside.join("victim/1.unit"), b"keep").unwrap();

    let store = layout.prefix.join("store");
    std::fs::create_dir_all(store.join("real")).unwrap();
    std::fs::write(store.join("real/1.unit"), b"drop").unwrap();
    std::os::unix::fs::symlink(outside.join("victim/1.unit"), store.join("real/2.unit")).unwrap();
    std::os::unix::fs::symlink(outside.join("victim"), store.join("linked")).unwrap();

    turn_off(&layout).unwrap();
    assert!(!store.join("real/1.unit").exists(), "real unit removed");
    assert!(
        std::fs::symlink_metadata(store.join("real/2.unit")).is_ok(),
        "symlinked unit entry left untouched"
    );
    assert!(
        outside.join("victim/1.unit").exists(),
        "outside target untouched"
    );
    let _ = std::fs::remove_dir_all(&outside);
}

/// Where this test's mesh `Units` dialer opens its stream ([`set_units_dialer`]).
static MESH_UNITS: Mutex<Option<(IpAddr, SocketAddr)>> = Mutex::new(None);

/// A dialer that opens a `Units` stream to the member at `ip` where the test named one: a plain
/// socket stands for the mesh's stream, which atpkg sees only as a [`UnitConn`].
fn mesh_units(ip: IpAddr) -> Option<Box<dyn UnitConn>> {
    let (member, at) = (*MESH_UNITS.lock().unwrap())?;
    (member == ip).then(|| Box::new(TcpStream::connect(at).unwrap()) as Box<dyn UnitConn>)
}

#[test]
fn a_download_takes_a_unit_over_the_mesh_units_stream_first_and_port_7487_after() {
    let layout = crate::testdir::layout("atpkg-peers-units-op-");
    let unit = b"the signed toolset archive, over Units".to_vec();
    let (build, dl) = staged(&layout, 7, &unit);
    let sha = sha256_hex(&unit);
    as_if_serving(&layout);
    keep_with_free(&layout, &dl, &sha, &build, ROOM);
    // The member's `Units` op: each stream answered by `answer_unit`, as the node service does.
    let stream_end = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = stream_end.local_addr().unwrap();
    let served = std::thread::spawn({
        let layout = layout.clone();
        move || {
            let (conn, _) = aterm_uds::accept(
                &stream_end,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            answer_unit(&conn, &layout).unwrap();
        }
    });
    // The member's port 7487 takes no connection: only the stream can land the unit.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = closed.local_addr().unwrap().to_string();
    drop(closed);
    *MESH_UNITS.lock().unwrap() = Some((IpAddr::from([127, 0, 0, 1]), at));
    let dest = layout.prefix.join("got.tar.zst");
    let got = with_units_dialer(mesh_units, || {
        race_paced(
            std::slice::from_ref(&dead),
            &sha,
            unit.len() as u64,
            &dest,
            release_host(b"never written", until_stopped),
            &ANSWERS,
        )
    });
    assert_eq!(got, Ok(()));
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        unit,
        "served over the Units stream"
    );
    served.join().unwrap();

    // A member with no stream to give (`None`) is dialed at its port, the fallback.
    *MESH_UNITS.lock().unwrap() = None;
    let (peer, judged) = one_shot_peer(unit.clone());
    let fallback = layout.prefix.join("fallback.tar.zst");
    let got = with_units_dialer(mesh_units, || {
        race_paced(
            std::slice::from_ref(&peer),
            &sha,
            unit.len() as u64,
            &fallback,
            release_host(b"never written", until_stopped),
            &ANSWERS,
        )
    });
    assert_eq!(got, Ok(()));
    assert_eq!(
        std::fs::read(&fallback).unwrap(),
        unit,
        "served from port 7487's stand-in"
    );
    judged.recv_timeout(HANG).unwrap();
}

#[test]
fn a_store_whose_person_turned_sharing_off_answers_no_units_stream() {
    let layout = crate::testdir::layout("atpkg-peers-units-off-");
    as_if_serving(&layout);
    turn_off(&layout).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client.set_read_timeout(Some(HANG)).unwrap();
    let (conn, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    write!(client, "GET /unit/{} HTTP/1.1\r\n\r\n", "0".repeat(64)).unwrap();
    answer_unit(&conn, &layout).unwrap();
    drop(conn);
    let mut answer = String::new();
    client.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 404"), "{answer:?}");
}

/// On Unix, an idle `share` waits on its listener, its stop's bell and `FileWatch` on `units.off`
/// rather than waking every `FALLBACK_POLL`, both while serving and while `--off` stands; and an
/// `--off` noticed through a wake on the listener before `FileWatch` settles still hears `--on`
/// inside the same settle window.
#[cfg(unix)]
#[test]
fn an_idle_sharing_store_waits_without_polling_when_serving_and_when_off() {
    let layout = crate::testdir::layout("atpkg-peers-idle-wait-");
    let stop = Arc::new(Stop::new());
    let looks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (said, notes) = mpsc::channel();
    let thread = std::thread::spawn({
        let (layout, stop, looks) = (layout.clone(), Arc::clone(&stop), Arc::clone(&looks));
        move || {
            share_counting(
                &layout,
                &Share {
                    listen: SocketAddr::from(([127, 0, 0, 1], 0)),
                    stop: &stop,
                    members: &loopback,
                    refresh: HANG,
                    note: &|line| {
                        let _ = said.send(line.to_owned());
                    },
                },
                Some(&looks),
            );
        }
    });
    heard(&notes, "serving");
    let parked = Instant::now();
    while looks.load(Ordering::Relaxed) == 0 {
        assert!(parked.elapsed() < HANG, "share never entered its wait");
        std::thread::sleep(Duration::from_millis(5));
    }
    let at_rest = looks.load(Ordering::Relaxed);
    std::thread::sleep(FALLBACK_POLL * 3);
    assert_eq!(
        looks.load(Ordering::Relaxed),
        at_rest,
        "an idle sharing server looked again with no connection, off or stop"
    );

    // Rapid `--off` -> listener poke -> `--on` inside `FileWatch::QUIET` must still resume serving.
    let bound = serving(&layout).expect("recorded serving address");
    turn_off(&layout).unwrap();
    let _ = TcpStream::connect(bound);
    heard(&notes, "off");
    turn_on(&layout).unwrap();
    heard(&notes, "serving");

    // While `--off` stands, `wait_while_off` also sleeps without polling.
    turn_off(&layout).unwrap();
    heard(&notes, "off");
    std::thread::sleep(aterm_uds::file_watch::QUIET * 2);
    let off_at_rest = looks.load(Ordering::Relaxed);
    std::thread::sleep(FALLBACK_POLL * 3);
    assert_eq!(
        looks.load(Ordering::Relaxed),
        off_at_rest,
        "a turned-off sharing server polled while waiting for --on or stop"
    );

    stop.set();
    thread.join().unwrap();
}
