// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use super::*;

/// A wait for something that must happen: a hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

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
        let Ok((mut conn, _)) = listener.accept() else {
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
    let got = race(
        &[peer],
        &sha256_hex(&unit),
        unit.len() as u64,
        &dest,
        release_host(b"never written", until_stopped),
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
    assert!(
        !units_dir(&layout).join(&sha).exists(),
        "and gc's sweep its link under units/"
    );
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

/// Bind `share` for `layout` on a loopback port, admitting the addresses `members` names (read
/// at most every `refresh`), on a thread of its own: the address it listens on, its stop flag,
/// its notes and the thread.
fn sharing_on(
    layout: &Layout,
    members: Members,
    refresh: Duration,
) -> (
    String,
    Arc<AtomicBool>,
    mpsc::Receiver<String>,
    std::thread::JoinHandle<()>,
) {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listen = SocketAddr::from(([127, 0, 0, 1], port));
    let running = Arc::new(AtomicBool::new(true));
    let (said, notes) = mpsc::channel();
    let thread = std::thread::spawn({
        let (layout, running) = ((*layout).clone(), Arc::clone(&running));
        move || {
            share(
                &layout,
                &Share {
                    listen,
                    running: &running,
                    members: &*members,
                    refresh,
                    note: &|line| {
                        let _ = said.send(line.to_owned());
                    },
                },
            );
        }
    });
    (listen.to_string(), running, notes, thread)
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
    let (addr, running, notes, thread) = sharing_on(&layout, Arc::new(loopback), HANG);
    heard(&notes, "serving");
    assert!(units_dir(&layout).is_dir(), "the server made units/");
    assert_eq!(
        serving(&layout).map(|at| at.to_string()),
        Some(addr.clone()),
        "where it serves is recorded"
    );
    keep_with_free(&layout, &dl, &sha, &build, ROOM);

    let dest = layout.prefix.join("got.tar.zst");
    let got = race(
        std::slice::from_ref(&addr),
        &sha,
        unit.len() as u64,
        &dest,
        release_host(b"never written", until_stopped),
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

    running.store(false, Ordering::Release);
    thread.join().unwrap();
    assert_eq!(serving(&layout), None, "a stopped server's record goes");
    assert!(
        TcpStream::connect(&addr).is_err(),
        "and its listener is closed"
    );
}

#[test]
fn the_persons_off_closes_a_sharing_store_and_on_opens_it_again() {
    let layout = crate::testdir::layout("atpkg-peers-share-off-");
    let (addr, running, notes, thread) = sharing_on(&layout, Arc::new(loopback), HANG);
    heard(&notes, "serving");
    turn_off(&layout).unwrap();
    heard(&notes, "off");
    assert!(
        TcpStream::connect(&addr).is_err(),
        "the off closed its listener"
    );
    assert_eq!(serving(&layout), None, "and its record");
    turn_on(&layout).unwrap();
    heard(&notes, "serving");
    assert!(TcpStream::connect(&addr).is_ok(), "--on opened it again");
    running.store(false, Ordering::Release);
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
        held: Some("Local Network is off".to_owned()),
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
            read.held = Some("Local Network is off".to_owned());
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
        let Ok((mut conn, _)) = listener.accept() else {
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
    let (conn, _) = listener.accept().unwrap();
    let pace = Pace {
        head: Duration::from_millis(300),
        ..QUICK
    };
    let (done, ended) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(answer(&conn, &dir, &pace).map_err(|e| e.kind()));
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
        let (addr, running, notes, thread) = sharing_on(&layout, members, refresh);
        heard(&notes, "serving");
        for _ in 0..3 {
            assert_eq!(ask(&addr, "GET / HTTP/1.1\r\n\r\n"), "", "closed unread");
        }
        running.store(false, Ordering::Release);
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
    let (addr, running, notes, thread) = sharing_on(&layout, Arc::new(loopback), HANG);
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

    running.store(false, Ordering::Release);
    thread.join().unwrap();
}
