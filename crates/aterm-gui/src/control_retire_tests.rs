// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cold update lane retires the control socket right before its `exec`, inside the
//! resident operator's commit permit (`App::run_cold_exec`, `control_listener::Endpoint`), and
//! reopens it when the `exec` returns (`App::resume_after_cold_exec`). Driven over the real
//! supervised accept loop and request lanes on a temporary socket; only the request handling is
//! a stand-in, whose `hold` keeps its lane the way a verb waiting on the main thread does.

use std::io::{BufRead as _, BufReader, ErrorKind, Read as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use aterm_uds::{CtlListener, CtlStream};

use super::control_lanes::{LaneLimits, LaneReader, LaneService, Lanes};
use super::{Scope, ServeDisposition, read_request_line};
use crate::control_listener::{Endpoint, Supervisor, rebind_socket, socket_file_id};
use crate::operator_host::ControlHandle as OperatorControl;

/// How long a wait for something that must happen may take before the test
/// calls it a hang.
const HANG: Duration = Duration::from_secs(60);

/// `AUTH ok` authenticates; `hold` is answered only once the test releases it (a verb whose
/// answer the main thread owes); any other line is answered at once.
#[derive(Default)]
struct MainThreadVerbs {
    hold: Mutex<Hold>,
    moved: Condvar,
}

#[derive(Default)]
struct Hold {
    holding: bool,
    released: bool,
}

impl MainThreadVerbs {
    fn wait_until_holding(&self) {
        let hold = self.hold.lock().unwrap_or_else(PoisonError::into_inner);
        let (hold, _) = self
            .moved
            .wait_timeout_while(hold, HANG, |hold| !hold.holding)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(hold.holding, "the held verb never reached its lane");
    }

    fn release(&self) {
        self.hold
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .released = true;
        self.moved.notify_all();
    }
}

impl LaneService for MainThreadVerbs {
    fn authenticate(
        &self,
        _stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, Option<String>)> {
        (read_request_line(reader)? == "AUTH ok").then_some((Scope::Owner, None))
    }

    fn serve_line(
        &self,
        line: String,
        _scope: Scope,
        _stream: &CtlStream,
        _reader: &mut LaneReader<'_>,
        out: &mut Vec<u8>,
    ) -> Option<ServeDisposition> {
        if line == "hold" {
            let mut hold = self.hold.lock().unwrap_or_else(PoisonError::into_inner);
            hold.holding = true;
            self.moved.notify_all();
            while !hold.released {
                hold = self
                    .moved
                    .wait(hold)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
        out.extend_from_slice(format!("OK {line}\n").as_bytes());
        None
    }

    fn is_wait(&self, _line: &str, _scope: Scope) -> bool {
        false
    }

    fn subscribe(&self, _line: String, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}
}

/// A fresh connection is served: it authenticates and `ping` is answered.
fn assert_answers(path: &std::path::Path, what: &str) {
    let client = CtlStream::connect(path).unwrap_or_else(|error| panic!("{what}: {error}"));
    client.set_read_timeout(Some(HANG)).expect("hang detector");
    (&client)
        .write_all(b"AUTH ok\nping\n")
        .unwrap_or_else(|error| panic!("{what}: {error}"));
    let mut reply = String::new();
    BufReader::new(&client)
        .read_line(&mut reply)
        .unwrap_or_else(|error| panic!("{what}: {error}"));
    assert_eq!(reply, "OK ping\n", "{what}");
}

/// A client holds an authenticated connection whose verb the main thread owes while the main
/// thread prepares the `exec`. While the operator's commit permit refuses, no `exec` runs: the
/// held connection stays open (WouldBlock) and the socket keeps answering. Once it is granted,
/// by the time the `exec` runs the held connection reads EOF and a new client is refused; when
/// the `exec` returns the same path serves again.
///
/// Runs alone in its own process: a forked child holds a copy of the listening socket until it
/// execs, so a sibling test's not-yet-exec'd child keeps a retired listener accepting. macOS has
/// no working close-on-fork (`F_SETFD` drops `FD_CLOFORK` on 26.6).
#[test]
fn a_cold_exec_retires_the_control_socket_and_a_failed_one_reopens_it() {
    if !crate::test_alone::alone() {
        return;
    }
    let dir = aterm_tempfile::TempDir::new_in("/tmp").expect("scratch dir");
    let path = dir.path().join("c.sock");
    let published = path.to_str().expect("utf8").to_string();
    let listener = CtlListener::bind(&path).expect("bind");
    let lanes = Lanes::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 8,
    });
    lanes.start();
    let verbs = Arc::new(MainThreadVerbs::default());
    lanes.publish(verbs.clone());
    let endpoint = Endpoint::default();
    let mut expected = socket_file_id(&published);
    let supervisor = Supervisor::new(
        listener,
        move || rebind_socket(&published, &mut expected),
        endpoint.clone(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    std::thread::spawn(move || {
        supervisor.run(
            move |stream| {
                let _ = lanes.admit(stream);
            },
            move || stopping.load(Ordering::Acquire),
        );
    });

    let held = CtlStream::connect(&path).expect("connect");
    (&held)
        .write_all(b"AUTH ok\nhold\n")
        .expect("send the held verb");
    verbs.wait_until_holding();
    held.set_nonblocking(true).expect("non-blocking");
    let held_read = || (&held).read(&mut [0u8; 64]).map_err(|error| error.kind());

    let mut app = crate::App::headless_for_test();
    app.control_endpoint = endpoint;
    app.prepare_cold_exec(7, true, false);

    let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
    let operator = OperatorControl::new("test-cold-exec-refused".to_string(), notify_tx.clone());
    let quiesce = operator.try_begin_update_quiesce().expect("quiesce");
    operator.begin_shutdown();
    let refused = app.run_cold_exec(Some(&quiesce), || unreachable!("a refused permit"));
    assert!(refused.is_err(), "the shutting-down operator refuses");
    assert_eq!(
        held_read(),
        Err(ErrorKind::WouldBlock),
        "a refused permit leaves the held connection open"
    );
    assert_answers(&path, "a refused permit leaves the socket answering");
    app.resume_after_cold_exec();

    let operator = OperatorControl::new("test-cold-exec-granted".to_string(), notify_tx);
    let granted = operator.try_begin_update_quiesce().expect("quiesce");
    let exec_ran = app.run_cold_exec(Some(&granted), || {
        assert_eq!(
            held_read(),
            Ok(0),
            "the held connection reads EOF by the time the exec runs"
        );
        assert_eq!(
            CtlStream::connect(&path)
                .map(drop)
                .map_err(|error| error.kind()),
            Err(ErrorKind::ConnectionRefused),
            "a new client is refused by the time the exec runs"
        );
        std::io::Error::from(ErrorKind::NotFound)
    });
    assert!(exec_ran.is_ok(), "the granted permit ran the exec");

    app.resume_after_cold_exec();
    assert_answers(&path, "the reopened socket answers");

    verbs.release();
    stop.store(true, Ordering::Release);
}
