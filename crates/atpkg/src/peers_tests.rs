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

#[test]
fn a_store_keeps_units_only_while_it_serves_and_only_with_their_signed_bytes() {
    let layout = crate::testdir::layout("atpkg-peers-keep-");
    let build = layout.prefix.join("store/trust/7");
    std::fs::create_dir_all(&build).unwrap();
    let dl = layout.prefix.join("trust-7.tar.zst");
    let unit = b"the signed toolset archive";
    std::fs::write(&dl, unit).unwrap();
    let sha = sha256_hex(unit);
    let beside = build.with_file_name("7.unit");

    keep(&layout, &dl, &sha, &build);
    assert!(
        !beside.exists(),
        "a store that does not serve keeps nothing"
    );

    std::fs::create_dir_all(units_dir(&layout)).unwrap();
    keep(&layout, &dl, &sha256_hex(b"other bytes"), &build);
    assert!(
        !beside.exists(),
        "a file that misses its signed digest is not kept"
    );

    keep(&layout, &dl, &sha, &build);
    assert_eq!(
        std::fs::read(&beside).unwrap(),
        unit,
        "kept beside its build"
    );
    assert_eq!(std::fs::read(units_dir(&layout).join(&sha)).unwrap(), unit);
    // The pass removes its download once the build is live; the kept unit outlives it.
    std::fs::remove_file(&dl).unwrap();
    sweep(&units_dir(&layout));
    assert!(
        units_dir(&layout).join(&sha).exists(),
        "a unit its build holds stays"
    );

    crate::store::discard_build(&build);
    assert!(!beside.exists(), "a discarded build takes its unit with it");
    sweep(&units_dir(&layout));
    assert!(
        !units_dir(&layout).join(&sha).exists(),
        "a unit no build holds is swept"
    );
}

#[test]
fn a_serving_store_answers_a_kept_unit_and_misses_everything_else() {
    let layout = crate::testdir::layout("atpkg-peers-serve-");
    let build = layout.prefix.join("store/trust/7");
    std::fs::create_dir_all(&build).unwrap();
    let dl = layout.prefix.join("trust-7.tar.zst");
    let unit = b"the signed toolset archive".to_vec();
    std::fs::write(&dl, &unit).unwrap();
    let sha = sha256_hex(&unit);
    let (bound_tx, bound) = mpsc::channel();
    let serving = (*layout).clone();
    std::thread::spawn(move || {
        let _ = serve(&serving, SocketAddr::from(([127, 0, 0, 1], 0)), |addr| {
            let _ = bound_tx.send(addr);
        });
    });
    let addr = bound
        .recv_timeout(HANG)
        .expect("the server bound")
        .to_string();
    keep(&layout, &dl, &sha, &build);

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

    // A unit this store does not hold is missed, and the release host's download stands.
    let other = b"another unit".to_vec();
    let elsewhere = layout.prefix.join("other.tar.zst");
    let got = race(
        &[addr],
        &sha256_hex(&other),
        other.len() as u64,
        &elsewhere,
        release_host(&other, |_| true),
    );
    assert_eq!(got, Ok(()));
    assert_eq!(std::fs::read(&elsewhere).unwrap(), other);
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
