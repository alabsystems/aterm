// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::requests_host::{Made, flush_within, opened_for_test};
use crate::{WindowId, stub_session};
use aterm_requests::{EndWhy, Out, State};

const WID: WindowId = WindowId(0);
const NODE: &str = "87be5228c0ffee0087be5228c0ffee00";
const BY: &str = "fleet:87be5228:s-0123456789abcdef0123";
/// A hang detector for the requests' writer thread.
const MUST: Duration = Duration::from_secs(60);

/// This test thread's requests in a folder of its own until dropped.
struct Requests(aterm_tempfile::TempDir);

impl Requests {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().expect("a folder");
        crate::requests_host::set_dir_for_test(Some(dir.path().to_path_buf()));
        Self(dir)
    }

    fn store(&self) -> aterm_requests::Store {
        aterm_requests::Store::open(self.0.path()).expect("the store")
    }

    fn journal(&self) -> String {
        std::fs::read_to_string(self.0.path().join(aterm_requests::JOURNAL)).unwrap_or_default()
    }
}

impl Drop for Requests {
    fn drop(&mut self) {
        crate::requests_host::set_dir_for_test(None);
    }
}

/// A headless app holding a parked shell a fleet drive asked for, with its request: the app, the
/// shell's local id and the request's id.
fn app_with_a_drives_shell() -> (App, u64, String) {
    let mut app = App::headless_for_test();
    app.lease.alone = || false;
    let id = app.next_session_id;
    app.next_session_id += 1;
    app.install_tab_session_parked(WID, stub_session(id), None, true);
    app.sync_window(WID);
    let sid = app.session_id_text(id).expect("its sid");
    let request = opened_for_test(true, NODE, "m114", BY, Made::Shell, &sid);
    (app, id, request)
}

fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap_or(0) + 1000
}

/// An idle leased shell's request concludes on the minute's look, and its pane stays parked,
/// its row saying why. NEGATIVE CONTROL: a shell running a command is no idle lease.
#[test]
fn an_idle_fleet_shell_concludes_on_the_look_and_its_pane_stays() {
    let requests = Requests::new();
    let (mut app, shell, id) = app_with_a_drives_shell();
    app.lease.idle_after = Duration::ZERO;
    app.lease.runs = |_| true;
    app.look_at_requests(now_ms());
    assert!(flush_within(MUST), "the writer flushed");
    assert_eq!(
        requests
            .store()
            .get(&id)
            .map(aterm_requests::Request::state),
        Some(State::Running),
        "{}",
        requests.journal()
    );
    app.lease.runs = |_| false;
    app.look_at_requests(now_ms());
    assert!(flush_within(MUST), "the writer flushed");
    assert_eq!(
        requests.store().get(&id).and_then(|request| request.end),
        Some((Out::Abandoned, EndWhy::Idle)),
        "{}",
        requests.journal()
    );
    assert!(app.session_is_parked(shell), "its pane stays");
    let row = app
        .worker_rows(WID)
        .into_iter()
        .find(|row| row.session == shell)
        .expect("its row");
    assert_eq!(row.group, "From m114");
    assert_eq!(
        row.why.as_deref(),
        Some("request abandoned, undriven 30 min")
    );
}

/// The relaunch sweep waits for the restore, a handoff and every other window on the state home,
/// then runs once. NEGATIVE CONTROLS: each of those holds it.
#[test]
fn the_relaunch_sweep_waits_for_the_restore_and_runs_once() {
    let requests = Requests::new();
    let mut app = App::headless_for_test();
    // A request of the process before, whose tab this one does not host.
    let gone = crate::session_store::test_handle(99);
    let id = opened_for_test(true, NODE, "m114", BY, Made::Shell, gone.sid.as_str());
    app.requests_born_ms = now_ms();
    app.lease.alone = || true;
    let before = requests.journal();
    app.pending_restore = Some(crate::restore::RestoreManifest::new(Vec::new()));
    app.look_at_requests(now_ms());
    app.pending_restore = None;
    app.incoming_handoff_pending = true;
    app.look_at_requests(now_ms());
    app.incoming_handoff_pending = false;
    app.lease.alone = || false;
    app.look_at_requests(now_ms());
    assert!(flush_within(MUST), "the writer flushed");
    assert_eq!(requests.journal(), before, "held");
    assert!(!app.requests_swept.load(Ordering::Acquire));
    app.lease.alone = || true;
    app.look_at_requests(now_ms());
    assert!(flush_within(MUST), "the writer flushed");
    assert_eq!(
        requests.store().get(&id).and_then(|request| request.end),
        Some((Out::Abandoned, EndWhy::Relaunch)),
        "{}",
        requests.journal()
    );
    assert!(app.requests_swept.load(Ordering::Acquire));
    // Once: a request the sweep would conclude, opened before the process was born, is left by
    // a later look.
    let later = crate::session_store::test_handle(98);
    let other = opened_for_test(true, NODE, "m114", BY, Made::Shell, later.sid.as_str());
    app.requests_born_ms = now_ms() + 1000;
    app.look_at_requests(now_ms());
    assert!(flush_within(MUST), "the writer flushed");
    assert_eq!(
        requests
            .store()
            .get(&other)
            .map(aterm_requests::Request::state),
        Some(State::Running)
    );
}

/// The host's block read hands a block that ended once and a running one at every look until
/// it ends, then moves past it. NEGATIVE CONTROL: a prompt not yet run is no span.
#[test]
fn the_look_hands_each_ended_block_once_and_a_running_one_until_it_ends() {
    let session = stub_session(41);
    let feed = |bytes: &[u8]| crate::term_lock(&session.term).process(bytes);
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    feed(prompt);
    let idle = host_spans(&session, 0, 0);
    assert!(idle.spans.is_empty(), "{:?}", idle.spans);
    feed(b"\x1b]133;C\x1b\\");
    let running = host_spans(&session, idle.next, 0);
    assert_eq!(running.spans.len(), 1, "{:?}", running.spans);
    assert_eq!(running.spans[0].span.to_ms, None);
    let again = host_spans(&session, running.next, 0);
    assert_eq!(
        again.spans, running.spans,
        "a running block is handed until it ends"
    );
    feed(b"\x1b]133;D;0\x1b\\");
    feed(prompt);
    let ended = host_spans(&session, again.next, 0);
    assert_eq!(ended.spans.len(), 1, "{:?}", ended.spans);
    assert_eq!(ended.spans[0].blk, running.spans[0].blk);
    assert!(ended.spans[0].span.to_ms.is_some(), "{:?}", ended.spans);
    assert!(ended.next > running.spans[0].blk);
    let after = host_spans(&session, ended.next, 0);
    assert!(
        after.spans.is_empty(),
        "an ended block is handed once: {:?}",
        after.spans
    );
    // An ended block older than the ring's floor is no span, and its cursor still moves.
    let old = host_spans(&session, again.next, u64::MAX);
    assert!(old.spans.is_empty());
    assert_eq!(old.next, ended.next);
}

thread_local! {
    static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// While no request is open, or concluded within the look's reach, the look reads no
/// terminal. NEGATIVE CONTROL: a live request's look reads each tab.
#[test]
fn no_live_request_reads_no_terminal() {
    let _requests = Requests::new();
    let mut app = App::headless_for_test();
    let id = app.next_session_id;
    app.next_session_id += 1;
    app.install_tab_session_parked(WID, stub_session(id), None, true);
    app.sync_window(WID);
    app.lease.spans = |_, from, _| {
        READS.with(|reads| reads.set(reads.get() + 1));
        SpanRead {
            spans: Vec::new(),
            next: from,
        }
    };
    READS.with(|reads| reads.set(0));
    app.look_at_requests(now_ms());
    assert_eq!(READS.with(std::cell::Cell::get), 0);
    let sid = app.session_id_text(id).expect("its sid");
    opened_for_test(true, NODE, "m114", BY, Made::Shell, &sid);
    app.look_at_requests(now_ms());
    assert!(READS.with(std::cell::Cell::get) >= 1);
}
