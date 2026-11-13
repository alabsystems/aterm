// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The trustd the keeper holds a grant on goes away (TP-4): the keeper counts the grant lost
/// once trustd's end of the lease is closed. NEGATIVE CONTROL: while trustd keeps the lease
/// nothing is lost, and the keeper's own drop of a grant it holds is no loss.
#[test]
fn the_keeper_counts_a_grant_trustd_ended_and_not_its_own_drop() {
    use std::io::BufRead as _;

    for trustd_ends in [true, false] {
        let private = aterm_tempfile::Builder::new()
            .prefix("tl-g-")
            .tempdir_in("/tmp")
            .unwrap();
        let d = private.path();
        let sock = d.join("g.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let (end_tx, end_rx) = std::sync::mpsc::channel::<()>();
        let (ended_tx, ended_rx) = std::sync::mpsc::channel::<()>();
        let trustd = std::thread::spawn(move || {
            let (conn, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            let mut w = conn.try_clone().unwrap();
            let mut lines = BufReader::new(conn).lines();
            for reply in [
                "{\"executable_sha256\":\"aa\",\"release\":\"1.99\",\"commit\":\"abc\"}",
                "GRANTED 1",
            ] {
                lines.next().expect("a request").unwrap();
                writeln!(w, "{reply}").unwrap();
            }
            // Gone at the test's word: the lease's connection closes, both ends of it.
            let _ = end_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
            drop((lines, w, listener));
            let _ = ended_tx.send(());
        });
        let keeper = Keeper::start(sock, "aa".into(), d.join("grants"), d.join("seen"));
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !keeper.held() {
            assert!(Instant::now() < deadline, "the keeper took no grant");
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(keeper.lost(), 0, "a held grant is not lost");
        if trustd_ends {
            end_tx.send(()).unwrap();
            ended_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the stand-in closed the lease");
            // Once the lease's last copy closes: a child another test forks may hold
            // one until it execs.
            while keeper.lost() == 0 {
                assert!(
                    Instant::now() < deadline,
                    "the grant trustd ended is not lost"
                );
                aterm_uds::deadline::poll_yield();
            }
            trustd.join().unwrap();
            drop(keeper);
        } else {
            let lost = Arc::clone(&keeper.lost);
            drop(keeper);
            assert_eq!(lost.load(std::sync::atomic::Ordering::SeqCst), 0);
            drop(end_tx);
            trustd.join().unwrap();
        }
    }
}

#[test]
fn a_failed_connection_clone_refuses_an_uncancellable_wait() {
    let k = Kept {
        stop: Arc::new(AtomicBool::new(false)),
        held: Arc::new(AtomicBool::new(false)),
        lost: Arc::new(AtomicU32::new(0)),
        conn: Arc::new(Mutex::new(None)),
    };
    assert!(!k.name(Err(std::io::Error::other("injected clone failure"))));
    assert!(k.conn.lock().unwrap().is_none());

    let (stream, _peer) = UnixStream::pair().unwrap();
    assert!(k.name(stream.try_clone()));
    assert!(k.conn.lock().unwrap().is_some());

    k.stop.store(true, Ordering::SeqCst);
    k.conn.lock().unwrap().take();
    assert!(!k.name(Ok(stream)));
    assert!(k.conn.lock().unwrap().is_none());
}

#[test]
fn dropping_before_the_worker_starts_sends_no_request() {
    let d = aterm_tempfile::Builder::new()
        .prefix("tl-start-")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = d.path().join("k.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let k = Kept {
        stop: Arc::new(AtomicBool::new(false)),
        held: Arc::new(AtomicBool::new(false)),
        lost: Arc::new(AtomicU32::new(0)),
        conn: Arc::new(Mutex::new(None)),
    };
    let stop = Arc::clone(&k.stop);
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let mut keeper = Keeper {
        stop: Arc::clone(&k.stop),
        held: Arc::clone(&k.held),
        lost: Arc::clone(&k.lost),
        conn: Arc::clone(&k.conn),
        thread: None,
    };
    keeper.thread = Some(std::thread::spawn(move || {
        start_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the worker was never released");
        keep(
            &sock,
            "aa",
            &d.path().join("grants"),
            &d.path().join("seen"),
            &k,
        );
    }));
    let (ended_tx, ended_rx) = std::sync::mpsc::channel();
    let ending = std::thread::spawn(move || {
        drop(keeper);
        ended_tx.send(()).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    while !stop.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "drop never stopped the worker");
        aterm_uds::deadline::poll_yield();
    }
    start_tx.send(()).unwrap();
    ended_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the stopped worker never ended");
    ending.join().unwrap();
    assert_eq!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::WouldBlock,
        "a worker stopped before startup must not connect"
    );
}
