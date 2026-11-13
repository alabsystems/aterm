// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{Read as _, Write as _};
use std::time::Instant;

use aterm_metrics::row::{HeldEntry, ManagerEntry, NeedEntry, Sid, Word};
use aterm_types::needs::{HoldWord, NeedWord};

use crate::fleet_listen::node_serve::read_needs_frame;
use crate::needs::{HarnessAsk, Need, NeedTail, Verdict};
use crate::session_store::{SessionHandle, SessionState, new_store, test_handle};
use crate::task_record::{TaskMark, TaskState};

/// When the tasks below began, Unix milliseconds.
const TASKS_AT_MS: u64 = 1_793_160_000_000;

/// A verdict whose need is `word`, a harness's own ask where `harness`.
fn needing(word: NeedWord, harness: bool) -> Verdict {
    Verdict {
        need: Some(Need {
            word,
            key: 1,
            what: "the row's words, which never leave the Mac".to_owned(),
            harness: harness.then(|| HarnessAsk {
                first: "pick the release name".to_owned(),
                more: 0,
                others: Vec::new(),
                program: aterm_phase::Program::Claude,
                tail: NeedTail::CarryingOn,
            }),
            report: None,
        }),
        ..Verdict::default()
    }
}

fn set(h: &SessionHandle, verdict: Verdict) {
    h.ctx.timeline.lock().unwrap().set_need(verdict);
}

/// `worker`'s task, in `state`, came from `manager` at [`TASKS_AT_MS`].
fn works_for(worker: &SessionHandle, manager: &SessionHandle, state: TaskState) {
    let mark = TaskMark {
        from: Some(manager.sid.as_str().to_owned()),
        ..TaskMark::new(state)
    };
    let _ = worker.ctx.timeline.lock().unwrap().mark_task(
        &mark,
        &Default::default(),
        Instant::now(),
        TASKS_AT_MS,
    );
}

fn sid_of(h: &SessionHandle) -> Sid {
    Sid::parse(h.sid.as_str()).expect("a minted sid")
}

/// A window with a session that asks for a person, two workers of it (one stalled, one at a
/// harness's own ask), a held session and an exited worker yields those needs, that hold and
/// that manager with its two live workers, each list by sid, with the ages the Fleet menu
/// reads; the exited session is in none of them.
#[test]
fn a_windows_needs_row_names_its_needs_its_holds_and_its_managers() {
    let manager = test_handle(1);
    let stalled = test_handle(2);
    let approving = test_handle(3);
    let held = test_handle(4);
    let gone = test_handle(5);
    set(&manager, needing(NeedWord::Ask, false));
    works_for(&stalled, &manager, TaskState::Stalled);
    set(&stalled, needing(NeedWord::Stalled, false));
    works_for(&approving, &manager, TaskState::Working);
    set(&approving, needing(NeedWord::Approval, true));
    set(
        &held,
        Verdict {
            held: Some(HoldWord::Person),
            ..Verdict::default()
        },
    );
    works_for(&gone, &manager, TaskState::Working);
    set(&gone, needing(NeedWord::Question, false));
    let ids = [&manager, &stalled, &approving, &held].map(sid_of);
    let store = new_store();
    for h in [&manager, &stalled, &approving, &held, &gone] {
        store.write().unwrap().register(h.clone());
    }
    store.write().unwrap().set_state(5, SessionState::Exited);

    let now_ms = TASKS_AT_MS + 600_000;
    let row = super::needs_row(&store, now_ms);
    assert_eq!(row.at, now_ms / 1000);
    assert_eq!(
        row.managers,
        [ManagerEntry {
            sid: ids[0],
            workers: 2
        }],
        "the exited worker is not counted"
    );
    assert_eq!(
        row.held,
        [HeldEntry {
            sid: ids[3],
            hold: Word::new("person").unwrap(),
            by: None,
            since: None,
        }]
    );
    let approval_since = row
        .needs
        .iter()
        .find(|need| need.sid == ids[2])
        .and_then(|need| need.since);
    assert!(
        approval_since.is_some_and(|since| (TASKS_AT_MS / 1000..=now_ms / 1000).contains(&since)),
        "an approval is aged by its agent verdict, taken from now: {approval_since:?}"
    );
    let mut want = vec![
        NeedEntry {
            sid: ids[0],
            word: Word::new("ask").unwrap(),
            harness: false,
            worker_of: None,
            since: None,
        },
        NeedEntry {
            sid: ids[1],
            word: Word::new("stalled").unwrap(),
            harness: false,
            worker_of: Some(ids[0]),
            since: Some(TASKS_AT_MS / 1000),
        },
        NeedEntry {
            sid: ids[2],
            word: Word::new("approval").unwrap(),
            harness: true,
            worker_of: Some(ids[0]),
            since: approval_since,
        },
    ];
    want.sort_by_key(|need| need.sid);
    assert_eq!(row.needs, want);
}

/// A program's own report is no harness's ask, whatever the need carries beside it.
#[test]
fn a_programs_reported_need_is_not_the_harnesss() {
    let reported = test_handle(1);
    let mut verdict = needing(NeedWord::Approval, true);
    if let Some(need) = verdict.need.as_mut() {
        need.report = Some(7);
    }
    set(&reported, verdict);
    let store = new_store();
    store.write().unwrap().register(reported);
    let row = super::needs_row(&store, TASKS_AT_MS);
    assert_eq!(row.needs.len(), 1);
    assert!(!row.needs[0].harness);
}

/// A row is written where it says other than the one last written, and not where it says the
/// same, whenever it was built; a row changed back is written again.
#[test]
fn only_a_changed_row_is_written_again_whenever_it_was_built() {
    let asking = test_handle(1);
    let ctx = std::sync::Arc::clone(&asking.ctx);
    let store = new_store();
    store.write().unwrap().register(asking);
    let mut sent = None;
    let first = super::changed_needs(&store, TASKS_AT_MS, &mut sent).expect("the first row");
    assert!(first.needs.is_empty());
    assert!(
        super::changed_needs(&store, TASKS_AT_MS + 60_000, &mut sent).is_none(),
        "the same row a minute later is no change"
    );
    ctx.timeline
        .lock()
        .unwrap()
        .set_need(needing(NeedWord::Ask, false));
    let asked = super::changed_needs(&store, TASKS_AT_MS + 61_000, &mut sent)
        .expect("the row once its session asks");
    assert_eq!(asked.needs.len(), 1);
    assert!(super::changed_needs(&store, TASKS_AT_MS + 62_000, &mut sent).is_none());
    ctx.timeline.lock().unwrap().set_need(Verdict::default());
    assert_eq!(
        super::changed_needs(&store, TASKS_AT_MS + 63_000, &mut sent)
            .expect("the row changed back")
            .needs,
        []
    );
}

/// A standing need whose since a rebuild reads a second off keeps the since last sent, so it
/// is no change; two seconds off, or another need of the session, is one.
#[test]
fn a_since_a_second_off_is_the_since_last_sent() {
    let need = |word: &str, since: u64| aterm_metrics::row::NeedsRow {
        at: 0,
        needs: vec![NeedEntry {
            sid: Sid::parse("s-00000000000000000001").expect("a sid"),
            word: Word::new(word).expect("a word"),
            harness: false,
            worker_of: None,
            since: Some(since),
        }],
        held: Vec::new(),
        managers: Vec::new(),
    };
    let sent = need("stalled", 1_000);
    for (word, since, kept) in [
        ("stalled", 1_001, 1_000),
        ("stalled", 999, 1_000),
        ("stalled", 1_002, 1_002),
        ("frozen", 1_001, 1_001),
    ] {
        let mut row = need(word, since);
        super::steady_since(&mut row, &sent);
        assert_eq!(row.needs[0].since, Some(kept), "{word} at {since}");
    }
}

/// The pusher writes nothing before the node job's `go`, then the window's row, then the row
/// again once it changed and the generation moved. Once the node job hangs up, the pusher's
/// next wake ends it.
#[test]
fn the_pusher_writes_the_row_after_go_and_again_once_it_changed() {
    let asking = test_handle(1);
    let ctx = std::sync::Arc::clone(&asking.ctx);
    let store = new_store();
    store.write().unwrap().register(asking);
    let (window, mut node) = aterm_uds::CtlStream::pair().expect("a stream pair");
    node.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    let (ended, pusher_ended) = std::sync::mpsc::channel();
    let pusher = std::thread::spawn({
        let store = store.clone();
        move || {
            super::push_needs(&store, window);
            let _ = ended.send(());
        }
    });

    node.write_all(b"go\n").unwrap();
    let first = read_needs_frame(&mut node).expect("the row once go is written");
    assert!(first.needs.is_empty() && first.held.is_empty() && first.managers.is_empty());

    ctx.timeline
        .lock()
        .unwrap()
        .set_need(needing(NeedWord::Ask, false));
    super::needs_moved();
    let second = read_needs_frame(&mut node).expect("the changed row");
    assert_eq!(second.needs.len(), 1);
    assert_eq!(second.needs[0].word.as_str(), "ask");

    aterm_uds::hangup::hang_up(&node);
    super::needs_moved();
    pusher_ended
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the pusher ended once the node job hung up");
    pusher.join().unwrap();
}

/// A node job that writes anything but `go` is hung up on before any row is written.
#[test]
fn the_pusher_writes_nothing_to_a_node_job_that_did_not_say_go() {
    let store = new_store();
    store.write().unwrap().register(test_handle(1));
    let (window, mut node) = aterm_uds::CtlStream::pair().expect("a stream pair");
    node.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    node.write_all(b"no\n").unwrap();
    super::push_needs(&store, window);
    let mut rest = Vec::new();
    node.read_to_end(&mut rest).expect("the pusher hung up");
    assert!(rest.is_empty(), "no row: {rest:?}");
}
