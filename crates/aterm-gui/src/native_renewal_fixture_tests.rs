// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control::control_media::{Hops, MainPost, recv_main};

/// A stand-in PTY whose drop, the hang-up, sets a flag.
struct Hangup(Arc<AtomicBool>);

impl Drop for Hangup {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// A connection worker posts a session-status hop once the host has seen `stop`, after the live
/// reader's last pump, and waits for it as `call_main` does. The teardown answers it before it
/// hangs up the PTY, pumps nothing after that, and returns.
#[test]
fn codex_renewal_fixture_teardown_answers_a_late_status_hop_before_pty_hangup() {
    static HOPS: Hops = Hops::new();
    const SESSION: u64 = 8880;
    let root = crate::test_scratch::short_dir("renewal-teardown");
    let handle = crate::session_store::test_handle_on(SESSION, -1);
    let mut dispatcher = Pump::source_fixture(handle.clone(), Arc::clone(&handle.term));
    let (post_hop, hops) = std::sync::mpsc::channel::<Wake>();
    let (accepted, worker_started) = std::sync::mpsc::channel();
    let (answered, answers) = std::sync::mpsc::channel();
    let server_stop = Arc::new(AtomicBool::new(false));
    let hung_up = Arc::new(AtomicBool::new(false));
    let socket = root.path().join("c.sock");
    let listener = CtlListener::bind(&socket).unwrap();
    let worker_stop = Arc::clone(&server_stop);
    let worker_hung_up = Arc::clone(&hung_up);
    let server = spawn_fixture_server(listener, Arc::clone(&server_stop), move |_stream| {
        accepted.send(()).unwrap();
        let until = Instant::now() + MUST_HAPPEN;
        while !worker_stop.load(Ordering::Acquire) {
            assert!(
                Instant::now() < until,
                "the teardown never stopped the server"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut post = MainPost::injected(|wake| post_hop.send(wake).is_ok(), &HOPS);
        let (reply, _hop) = post
            .ask(|reply| Wake::ReadSessionStatus {
                session: SESSION,
                reply,
            })
            .map_err(|error| io::Error::other(error.to_string()))?;
        let answer = recv_main(&reply).map_err(|error| error.to_string());
        answered
            .send((answer, worker_hung_up.load(Ordering::Acquire)))
            .unwrap();
        Ok(true)
    });
    let reader_hung_up = Arc::clone(&hung_up);
    let reader = std::thread::spawn(move || {
        // The real reader's read returns only once the shell is hung up.
        let until = Instant::now() + MUST_HAPPEN;
        while !reader_hung_up.load(Ordering::Acquire) {
            if Instant::now() >= until {
                return Err(io::Error::other("the PTY was never hung up"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    });
    let client = CtlStream::connect(&socket).unwrap();
    worker_started
        .recv_timeout(MUST_HAPPEN)
        .expect("the server served the client");
    let opts = crate::harness_host::window_drive_opts(
        root.path().join("home"),
        root.path().join("state"),
        None,
    );
    let live_reader =
        live_snapshot::Reader::start(Arc::clone(&handle.ctx), -1, opts, root.path().to_path_buf())
            .unwrap();
    let serving = Serving {
        server_stop,
        server,
        pty_stop: Arc::new(AtomicBool::new(false)),
        pty: Hangup(Arc::clone(&hung_up)),
        reader,
    };
    serving
        .tear_down(Some(live_reader), || {
            assert!(
                !hung_up.load(Ordering::Acquire),
                "the host pumped after it hung up the PTY"
            );
            if let Ok(wake) = hops.recv_timeout(Duration::from_millis(10)) {
                dispatcher.dispatch(wake);
            }
        })
        .unwrap();
    let (answer, after_hangup) = answers
        .try_recv()
        .expect("the server joined its worker before the teardown returned");
    assert!(answer.unwrap().unwrap().contains("schema=1"));
    assert!(!after_hangup, "the hop was answered after the PTY hung up");
    drop(client);
}
