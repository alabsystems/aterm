// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-platform behavioral tests for the portable control-socket stream.
//!
//! These run on BOTH platforms: on Unix they exercise the std aliases (a
//! regression guard that the portable surface never drifts from std); on
//! Windows they are the LOAD-BEARING proof that afunix.sys semantics hold —
//! in particular that `shutdown` unblocks a blocked cross-thread read (relay
//! teardown depends on it) and that a stale/absent socket reads as
//! `ConnectionRefused`/`NotFound` (the `decide_bind` unlink-on-restart
//! contract depends on it).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aterm_uds::{CtlListener, CtlStream, latest, process, rand};

/// A fresh per-test directory under the OS temp dir (tests run in parallel in
/// one process, so the name carries the pid AND the test tag).
fn test_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-uds-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// The control channel's on-the-wire handshake over a real bound socket:
/// `AUTH <hex>` is consumed silently on success (the first reply a client
/// reads answers its first VERB), a bad token yields exactly `ERR auth\n` —
/// mirroring `control_auth`'s `run_auth_preamble` shape.
#[test]
fn roundtrip_auth_handshake_over_a_bound_socket() {
    let dir = test_dir("roundtrip");
    let path = dir.join("aterm-1.sock");
    let token = "a".repeat(64);

    let listener = CtlListener::bind(&path).expect("bind");
    let srv_token = token.clone();
    let server = std::thread::spawn(move || {
        // Exercise the `incoming()` iterator (the production accept loop).
        let stream = listener
            .incoming()
            .next()
            .expect("incoming never yields None")
            .expect("accept");
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut writer = stream;
        let mut first = String::new();
        reader.read_line(&mut first).expect("auth line");
        if first.trim_end() == format!("AUTH {srv_token}") {
            let mut verb = String::new();
            reader.read_line(&mut verb).expect("verb line");
            writer
                .write_all(format!("OK {}\n", verb.trim_end()).as_bytes())
                .expect("reply");
        } else {
            writer.write_all(b"ERR auth\n").expect("deny");
        }
        writer.flush().expect("flush");
    });

    let client = CtlStream::connect(&path).expect("connect");
    // `&CtlStream` must be Write (the aterm-ctl / proxy call-site shape).
    (&client)
        .write_all(format!("AUTH {token}\ntext\n").as_bytes())
        .unwrap();
    (&client).flush().unwrap();
    let mut reply = String::new();
    BufReader::new(&client).read_line(&mut reply).unwrap();
    assert_eq!(reply, "OK text\n", "AUTH consumed silently; verb answered");
    server.join().unwrap();

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `pair()` yields a connected duplex pair (the LoopbackTransport shape).
#[test]
fn pair_echo() {
    let (a, mut b) = CtlStream::pair().expect("pair");
    (&a).write_all(b"ping").unwrap();
    let mut buf = [0u8; 4];
    b.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"ping");
    b.write_all(b"pong").unwrap();
    let mut buf2 = [0u8; 4];
    (&a).read_exact(&mut buf2).unwrap();
    assert_eq!(&buf2, b"pong");
}

/// One relay direction: pump `r` into `w` until EOF/error (the proxy.rs
/// `copy_until_eof` shape).
fn pump(r: &mut CtlStream, w: &mut CtlStream) {
    let mut buf = [0u8; 1024];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if w.write_all(&buf[..n]).and_then(|()| w.flush()).is_err() {
                    return;
                }
            }
        }
    }
}

/// The proxy.rs `relay_bidirectional` shape: BOTH directions pumped
/// concurrently over CLONES of the two streams; when either direction ends,
/// that pump shuts BOTH sockets down both ways, which must unblock the OTHER
/// pump's parked read so everything joins.
#[test]
fn clone_bidirectional_relay() {
    let (app, relay_client) = CtlStream::pair().expect("client pair");
    let (relay_child, mut child) = CtlStream::pair().expect("child pair");

    let mut c2s_r = relay_client.try_clone().unwrap();
    let mut c2s_w = relay_child.try_clone().unwrap();
    let mut s2c_r = relay_child.try_clone().unwrap();
    let mut s2c_w = relay_client.try_clone().unwrap();
    let (down_client, down_child) = (
        relay_client.try_clone().unwrap(),
        relay_child.try_clone().unwrap(),
    );
    let (up_client, up_child) = (relay_client, relay_child);

    // child -> client pump.
    let down = std::thread::spawn(move || {
        pump(&mut s2c_r, &mut s2c_w);
        let _ = down_client.shutdown(std::net::Shutdown::Both);
        let _ = down_child.shutdown(std::net::Shutdown::Both);
    });
    // client -> child pump.
    let up = std::thread::spawn(move || {
        pump(&mut c2s_r, &mut c2s_w);
        let _ = up_client.shutdown(std::net::Shutdown::Both);
        let _ = up_child.shutdown(std::net::Shutdown::Both);
    });

    // Drive one round trip through the live relay: app -> child, child echoes.
    (&app).write_all(b"hello\n").unwrap();
    let mut got = [0u8; 6];
    child.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"hello\n");
    child.write_all(b"world\n").unwrap();
    let mut back = [0u8; 6];
    (&app).read_exact(&mut back).unwrap();
    assert_eq!(&back, b"world\n");

    // Teardown: the app hangs up; the client->child pump sees EOF and shuts
    // both relay sockets down, which must unblock the child->client pump's
    // parked read (the cross-thread-shutdown property) so both pumps join.
    app.shutdown(std::net::Shutdown::Both).unwrap();
    up.join().expect("client->child pump joins after teardown");
    down.join()
        .expect("child->client pump joins after teardown");
}

/// THE relay-teardown load-bearing property: `shutdown(Both)` on a CLONE must
/// unblock a read parked on another thread within a bounded window, returning
/// EOF or an error — never hanging.
#[test]
fn shutdown_unblocks_blocked_read() {
    let (a, _b) = CtlStream::pair().expect("pair");
    let clone = a.try_clone().expect("clone");
    let (tx, rx) = std::sync::mpsc::channel();
    // The sleep alone used to be the ONLY thing ordering "reader is parked" before
    // "shutdown fires". On a loaded box the thread may not be scheduled inside
    // 100ms, the shutdown lands FIRST, and the read then returns EOF for a reason
    // that has nothing to do with unblocking a parked read — the property under
    // test never runs and the test passes anyway. Publish entry, then keep the
    // sleep as slack for the gap between the flag and the actual park.
    let entering = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_entering = entering.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        reader_entering.store(true, std::sync::atomic::Ordering::SeqCst);
        // No timeout set: this read parks until the shutdown lands.
        let res = (&a).read(&mut buf);
        let _ = tx.send(res);
    });
    while !entering.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::yield_now();
    }
    std::thread::sleep(Duration::from_millis(100));
    clone.shutdown(std::net::Shutdown::Both).expect("shutdown");
    let res = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("shutdown must unblock the parked read within the deadline");
    // An `Err` also unblocks the relay loop — acceptable; EOF must be clean.
    if let Ok(n) = res {
        assert_eq!(n, 0, "post-shutdown read is EOF");
    }
    reader.join().unwrap();
}

/// A read timeout expires as `WouldBlock`/`TimedOut` (what the tls relay's
/// poll loop accepts), within a sane tolerance, and a timed stream still
/// delivers data that IS available.
#[test]
fn read_timeout_returns_would_block() {
    let (a, b) = CtlStream::pair().expect("pair");
    a.set_read_timeout(Some(Duration::from_millis(150)))
        .expect("set timeout");
    let start = Instant::now();
    let mut buf = [0u8; 8];
    let err = (&a).read(&mut buf).expect_err("no data -> timeout");
    assert!(
        matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "timeout kind must poll-loop-match, got {:?}",
        err.kind()
    );
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_millis(50) && waited < Duration::from_secs(5),
        "timeout must be roughly honored, waited {waited:?}"
    );
    // Data present before the deadline is delivered normally.
    (&b).write_all(b"late").unwrap();
    let n = (&a).read(&mut buf).expect("data available");
    assert_eq!(&buf[..n], b"late");
    // A zero timeout is rejected exactly like std.
    assert_eq!(
        a.set_read_timeout(Some(Duration::ZERO))
            .expect_err("zero timeout")
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
}

/// The `decide_bind` contract: a socket FILE with no listener (a crashed
/// instance's leftover) must read as stale — `ConnectionRefused`/`NotFound` —
/// and an absent path must read as `NotFound`. Anything else would make the
/// server refuse to rebind after every crash.
#[test]
fn stale_socket_reads_not_live() {
    let dir = test_dir("stale");
    let path = dir.join("aterm-2.sock");

    let listener = CtlListener::bind(&path).expect("bind");
    CtlStream::connect(&path).expect("live listener accepts a connect");
    drop(listener); // the file REMAINS; nobody listens -> stale

    // Socket teardown can be asynchronous: poll (bounded) until the stale
    // verdict lands; never reaching it is the real regression.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stale = false;
    while Instant::now() < deadline {
        match CtlStream::connect(&path) {
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                stale = true;
                break;
            }
            Err(_) => {} // transient teardown error: keep polling
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        stale,
        "a listener-less socket file must read ConnectionRefused/NotFound"
    );

    // Absent path: explicitly NotFound (normalized on Windows).
    let _ = std::fs::remove_file(&path);
    let err = CtlStream::connect(&path).expect_err("absent socket");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "absent => NotFound"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `std::fs::remove_file` deletes the socket file (on Windows an afunix
/// REPARSE POINT — DeleteFileW removes it without following), so the
/// `remove_file` + rebind flow works unchanged on both platforms.
#[test]
fn remove_file_deletes_socket_reparse_point() {
    let dir = test_dir("unlink");
    let path = dir.join("aterm-3.sock");
    let listener = CtlListener::bind(&path).expect("bind");
    assert!(
        std::fs::symlink_metadata(&path).is_ok(),
        "bind left a socket file on disk"
    );
    drop(listener);
    std::fs::remove_file(&path).expect("remove_file deletes the socket file");
    assert_eq!(
        std::fs::symlink_metadata(&path).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    // And the path is bindable again (the unlink-then-rebind flow).
    let again = CtlListener::bind(&path).expect("rebind after unlink");
    drop(again);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The `latest` alias publishes atomically and repoints (symlink on Unix,
/// pointer file on Windows — one portable surface), leaving no temp residue;
/// the client-side `resolve` follows it on Windows and is the identity on
/// Unix (where the kernel resolves the symlink during connect).
#[test]
fn latest_pointer_publishes_atomically_and_repoints() {
    let dir = test_dir("latest");
    let link = dir.join("aterm.sock");

    let first = dir.join("aterm-101.sock");
    latest::publish(&link, &first.to_string_lossy());
    assert_eq!(
        latest::target_name(&link).as_deref(),
        Some(std::ffi::OsStr::new("aterm-101.sock"))
    );

    // A newer instance wins the alias; no temp residue is left behind.
    let second = dir.join("aterm-202.sock");
    latest::publish(&link, &second.to_string_lossy());
    assert_eq!(
        latest::target_name(&link).as_deref(),
        Some(std::ffi::OsStr::new("aterm-202.sock"))
    );
    assert!(!dir.join("aterm-202.sock.lnk").exists(), "no .lnk residue");

    // Client-side redirect.
    let resolved = latest::resolve(&link.to_string_lossy());
    if cfg!(windows) {
        assert_eq!(
            std::path::Path::new(&resolved),
            second.as_path(),
            "resolve follows the pointer file to the instance socket"
        );
    } else {
        assert_eq!(
            resolved,
            link.to_string_lossy(),
            "resolve is the identity on Unix (kernel follows the symlink)"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// On Windows a planted alias with junk / path-escaping contents must NOT
/// resolve (contents are validated against the `aterm-<pid>.sock` shape);
/// the client then just dials the given path and fails cleanly.
#[cfg(windows)]
#[test]
fn latest_pointer_rejects_junk_and_escaping_contents() {
    let dir = test_dir("latest-junk");
    let link = dir.join("aterm.sock");
    for junk in [
        "not-a-socket-name",
        "..\\..\\evil.sock",
        "aterm-12x.sock",
        "sub\\aterm-12.sock",
        "C:\\evil\\aterm-12.sock",
        "",
    ] {
        std::fs::write(&link, junk).unwrap();
        assert_eq!(
            latest::target_name(&link),
            None,
            "junk contents {junk:?} must not validate"
        );
        assert_eq!(
            latest::resolve(&link.to_string_lossy()),
            link.to_string_lossy(),
            "junk alias resolves to the unchanged path"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An unencodable socket path (over sun_path's 107 usable bytes) fails with a
/// clean `InvalidInput` on both platforms — never a garbled bind.
#[test]
fn sun_path_too_long_errors_cleanly() {
    let long = std::env::temp_dir().join(format!("{}.sock", "x".repeat(200)));
    let err = CtlListener::bind(&long).expect_err("oversized path");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "bind: {err}");
    let err = CtlStream::connect(&long).expect_err("oversized path");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "connect: {err}"
    );
}

/// Pid liveness: we are alive; a reaped child and pid 0 are dead. This is the
/// predicate the stale-instance sweep keys on.
#[test]
fn pid_alive_self_true_reaped_child_false() {
    assert!(process::pid_alive(std::process::id()), "self is alive");
    assert!(!process::pid_alive(0), "pid 0 is never a real instance");

    #[cfg(unix)]
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("0")
        .spawn()
        .expect("spawn");
    #[cfg(windows)]
    let mut child = std::process::Command::new("cmd")
        .args(["/c", "exit"])
        .spawn()
        .expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
    drop(child);
    assert!(!process::pid_alive(dead), "a reaped child is dead");
}

/// The CSPRNG fills and two draws differ (astronomically unlikely to collide).
#[test]
fn rand_fill_produces_distinct_bytes() {
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    rand::fill(&mut a).expect("entropy available");
    rand::fill(&mut b).expect("entropy available");
    assert_ne!(a, b, "two 32-byte draws must differ");
    assert_ne!(a, [0u8; 32], "a draw is not all zeroes");
}
