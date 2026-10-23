// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests control-socket retirement before cold `exec` and reopening on failure.

use std::io::{BufRead as _, BufReader, ErrorKind, Read as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use aterm_uds::{CtlListener, CtlStream};

use super::control_lanes::{LaneLimits, LaneReader, LaneService, Lanes, Outbox};
use super::{Request, Scope, ServeDisposition, read_request_line};
use crate::control_listener::{Endpoint, Supervisor, rebind_socket, socket_file_id};

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
    ) -> Option<(Scope, crate::control::Principal, Option<String>)> {
        (read_request_line(reader)? == "AUTH ok").then_some((
            Scope::Owner,
            crate::control::Principal::Instance,
            None,
        ))
    }

    fn serve_line(
        &self,
        request: Request,
        _scope: Scope,
        _stream: &CtlStream,
        _reader: &mut LaneReader<'_>,
        out: &mut Outbox,
    ) -> Option<ServeDisposition> {
        let line = request.line();
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

    fn is_wait(&self, _request: &Request, _scope: Scope) -> bool {
        false
    }

    fn subscribe(&self, _request: Request, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}

    fn has_live_claim(&self, _id: u64) -> bool {
        false
    }
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

/// `prepare_cold_exec` keeps the socket open, `run_cold_exec` retires it (held connections
/// read EOF and new connects are refused), and `resume_after_cold_exec` reopens it.
///
/// Runs alone because a sibling test's forked child can inherit the listening fd before exec.
#[test]
fn a_cold_exec_retires_the_control_socket_and_a_failed_one_reopens_it() {
    if !crate::test_alone::alone() {
        return;
    }
    let dir = crate::test_scratch::short_dir("the retired socket");
    let path = dir.path().join("c.sock");
    let published = path.to_str().expect("utf8").to_string();
    let listener = CtlListener::bind(&path).expect("bind");
    // Three request lanes: the held verb keeps one to the end, and the lane that answered the
    // `ping` before the exec frees only once its thread reads the hang-up, which on a loaded
    // machine can come after the reopened socket's client arrives; a third serves that client.
    let lanes = Lanes::new(LaneLimits {
        rpc: 3,
        wait: 1,
        inline: 1,
        open: 8,
        subscriptions: 1,
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
                // Refused as the instance refuses one: with the busy line, not a bare close.
                if let Err(stream) = lanes.admit(stream) {
                    let _ = (&stream)
                        .write_all(aterm_types::control_verbs::CONTROL_BUSY_LINE.as_bytes());
                }
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

    assert_eq!(
        held_read(),
        Err(ErrorKind::WouldBlock),
        "the steps before the exec leave the held connection open"
    );
    assert_answers(
        &path,
        "the steps before the exec leave the socket answering",
    );

    let mut exec_ran = false;
    let returned = app.run_cold_exec(|| {
        exec_ran = true;
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
    assert!(exec_ran, "the exec ran");
    assert_eq!(
        returned.kind(),
        ErrorKind::NotFound,
        "its error is returned"
    );

    app.resume_after_cold_exec();
    assert_answers(&path, "the reopened socket answers");

    verbs.release();
    stop.store(true, Ordering::Release);
}
