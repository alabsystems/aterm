// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-platform behavioral tests for the portable control-socket stream.
//!
//! On Unix they check the std aliases; on Windows they check that afunix.sys
//! semantics hold, in particular that `shutdown` unblocks a blocked
//! cross-thread read (relay teardown) and that a stale or absent socket reads
//! as `ConnectionRefused`/`NotFound` (unlink-on-restart in `decide_bind`).
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::{BufRead, BufReader, IoSlice, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aterm_uds::{CtlListener, CtlStream, latest, process};

/// A fresh per-test directory under the OS temp dir (tests run in parallel in
/// one process, so the name carries the pid and the test tag).
fn test_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-uds-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// The control channel's handshake over a real bound socket: `AUTH <hex>` is
/// consumed silently on success (the first reply answers the first verb), and
/// a bad token yields exactly `ERR auth\n`.
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

/// A gather write is one call that carries every slice in order: `writev` on
/// Unix and `WSASend` on Windows, where std's default would send the first
/// slice alone. `fdpass::send_vectored`, the push stream's gather, likewise.
#[test]
fn a_gather_write_carries_every_slice_in_one_call() {
    let (a, mut b) = CtlStream::pair().expect("pair");
    let bufs = [IoSlice::new(b"ab"), IoSlice::new(b""), IoSlice::new(b"cde")];
    assert_eq!((&a).write_vectored(&bufs).unwrap(), 5, "write_vectored");
    assert_eq!(
        aterm_uds::fdpass::send_vectored(&a, &bufs).unwrap(),
        5,
        "send_vectored"
    );
    let mut buf = [0u8; 10];
    b.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"abcdeabcde");
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

/// The proxy's `relay_bidirectional` shape: both directions pumped
/// concurrently over clones of the two streams; when either direction ends,
/// its pump shuts both sockets down, which must unblock the other pump's
/// parked read so everything joins.
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

/// A read timeout expires as `WouldBlock`/`TimedOut` (what the tls relay's
/// poll loop accepts), and a timed stream still delivers available data.
///
/// A signal handler that runs during the wait ends it early with `EINTR`
/// (`Interrupted`), which no `SA_RESTART` restarts on a socket with a timeout
/// (signal(7)): that is no timeout, and a poll loop reads again, as this one
/// does. This process installs no handler, but a translator may run its own:
/// Rosetta signals every thread of a process it translates with an internal
/// `SIGTRAP` whenever it syncs them (measured 2026-10-17, strace: si_code
/// 0xffefdbe9, sent by one of the process's own threads).
#[test]
fn read_timeout_returns_would_block() {
    let (a, b) = CtlStream::pair().expect("pair");
    a.set_read_timeout(Some(Duration::from_millis(50)))
        .expect("set timeout");
    let mut buf = [0u8; 8];
    // The hang detector: only a wait that never ends, or ends by signal for a
    // whole minute, reaches it.
    let hang = Instant::now() + Duration::from_secs(60);
    let err = loop {
        match (&a).read(&mut buf) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                assert!(Instant::now() < hang, "a minute of interrupted waits");
            }
            other => break other.expect_err("no data -> timeout"),
        }
    };
    assert!(
        matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "timeout kind must poll-loop-match, got {:?}",
        err.kind()
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

/// A socket file with no listener (a crashed instance's leftover) reads as
/// stale (`ConnectionRefused`/`NotFound`) and an absent path as `NotFound`, so
/// the server can rebind after a crash.
#[test]
fn stale_socket_reads_not_live() {
    let dir = test_dir("stale");
    let path = dir.join("aterm-2.sock");

    let listener = CtlListener::bind(&path).expect("bind");
    CtlStream::connect(&path).expect("live listener accepts a connect");
    drop(listener); // the file remains; nobody listens -> stale

    // Socket teardown can be asynchronous: poll (bounded) until the stale
    // verdict lands.
    let deadline = Instant::now() + Duration::from_secs(60);
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
/// reparse point, which DeleteFileW removes without following), so
/// `remove_file` + rebind works on both platforms.
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
/// pointer file on Windows), leaving no temp residue; `resolve` follows it on
/// Windows and is the identity on Unix.
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

/// On Windows a planted alias with junk or path-escaping contents does not
/// resolve; the client then dials the given path and fails cleanly.
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

/// An unencodable socket path (over sun_path's 107 usable bytes) fails with
/// `InvalidInput` on both platforms, never a garbled bind.
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

/// Pid liveness, the predicate the stale-instance sweep keys on: we are alive;
/// a reaped child and pid 0 are dead.
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
