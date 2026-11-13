// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

use amacs::protocol::edit::EditRequest;

use super::*;
use crate::app_documents::{DocumentAdmissionOutcome, DocumentAdmissionRejected};
use crate::native_app::{AppEvent, AppViewState, TextInputEvent};

/// Scratch file fixture for `app_edit` tests.
pub(super) struct Fixture {
    _dir: aterm_tempfile::TempDir,
    pub(super) path: PathBuf,
    pub(super) uri: String,
}

pub(super) fn fixture(name: &str, bytes: &[u8]) -> Fixture {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-edit-{name}-"))
        .tempdir()
        .unwrap();
    let path = dir.path().join("COMMIT_EDITMSG");
    std::fs::write(&path, bytes).unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    Fixture {
        _dir: dir,
        path,
        uri,
    }
}

/// Creates a headless test `App` with `edit` enabled.
pub(super) fn app() -> App {
    let mut app = App::headless_for_test();
    app.serve_edits_headless_for_test();
    app
}

/// Creates a front terminal tab in `wid` with a unique session ID.
pub(super) fn caller(app: &mut App, wid: WindowId) -> u64 {
    let sid = fresh_session(app);
    app.push_stub_tab(wid, crate::stub_session(sid));
    app.next_session_id = sid + 1;
    sid
}

/// Reserves a unique session ID block on `app`.
pub(super) fn fresh_session(app: &mut App) -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1 << 40);
    let sid = NEXT.fetch_add(1 << 12, Ordering::Relaxed);
    app.next_session_id = sid;
    sid
}

pub(super) fn request(uri: &str, line: Option<(u32, Option<u32>)>) -> EditRequest {
    EditRequest {
        uri: uri.to_string(),
        line,
        create: false,
        for_word: None,
        wait: true,
        read_only: false,
        secret: None,
    }
}

/// Executes `@<session> edit` inline and returns its reply line.
pub(super) fn edit(app: &mut App, session: u64, request: EditRequest) -> (Ticket, String) {
    let ticket = crate::control_edit::mint_ticket();
    let (reply, answer) = std::sync::mpsc::channel();
    app.begin_edit(call(ticket, session, request, reply));
    (
        ticket,
        answer.try_recv().expect("an inline edit answers at once"),
    )
}

/// Builds an [`EditCall`] with resolved realpath URI and reply receiver.
pub(super) fn call(
    ticket: Ticket,
    session: u64,
    request: EditRequest,
    reply: std::sync::mpsc::Sender<String>,
) -> EditCall {
    EditCall {
        ticket,
        session,
        real_uri: crate::control_edit::real_file_uri(&request.uri),
        request,
        reply,
    }
}

/// Parks an `await edit` request for `key` on `app`.
pub(super) fn wait(app: &mut App, key: AwaitKey) -> (WaiterId, Receiver<String>) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let waiter = WaiterId(NEXT.fetch_add(1, Ordering::Relaxed));
    let (reply, answer) = std::sync::mpsc::channel();
    let real_uri = match &key {
        AwaitKey::Uri(uri) => crate::control_edit::real_file_uri(uri),
        AwaitKey::Ticket(_) => None,
    };
    app.await_edit(AwaitCall {
        waiter,
        key,
        real_uri,
        reply,
    });
    (waiter, answer)
}

pub(super) fn tab_ids(app: &App, wid: WindowId) -> Vec<TabId> {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .map(|tab| tab.id)
        .collect()
}

pub(super) fn active_tab(app: &App, wid: WindowId) -> TabId {
    app.windows[&wid]
        .tab_set
        .active_id()
        .expect("an active tab")
}

/// Returns the [`TabId`] in `wid` that contains `view`.
pub(super) fn tab_of(app: &App, wid: WindowId, view: ViewId) -> TabId {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .find(|tab| tab.root.contains(view))
        .map(|tab| tab.id)
        .expect("the view's tab")
}

pub(super) fn select(app: &mut App, wid: WindowId, tab: TabId) {
    assert!(app.windows.get_mut(&wid).unwrap().tab_set.switch_to(tab));
    app.sync_window(wid);
}

/// Focuses split leaf `view` in `wid`'s active tab.
pub(super) fn focus_leaf(app: &mut App, wid: WindowId, view: ViewId) {
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .active_mut()
        .unwrap()
        .set_focus(view);
    app.sync_window(wid);
}

fn attention(app: &App, wid: WindowId, tab: TabId) -> bool {
    app.windows[&wid]
        .tab_set
        .get(tab)
        .is_some_and(|tab| tab.presentation.indicators.attention)
}

pub(super) fn type_text(app: &mut App, wid: WindowId, text: &str) {
    app.dispatch_native_event(
        wid,
        AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
    )
    .unwrap();
}

fn ok(line: &str) -> EditOk {
    EditOk::parse(line).unwrap_or_else(|error| panic!("{line:?}: {error}"))
}

fn view(ok: &EditOk) -> ViewId {
    ViewId::from_stored(ok.view)
}

fn document_of(app: &App, view: ViewId) -> DocumentId {
    let Some(crate::tab_model::View::Native(native)) = app.view_store.get(view).copied() else {
        panic!("view {view:?} is not a native view");
    };
    app.native_runtime
        .document_id(native.instance)
        .expect("a document view")
}

fn orphaned(app: &App, ticket: Ticket) -> bool {
    app.native_documents.edits.open[&ticket].orphaned
}

#[test]
fn edit_opens_fresh_view_beside_caller_even_when_document_already_open() {
    let text = "one\ntwo\nthree\nfour\n";
    let f = fixture("fresh", text.as_bytes());
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);
    let neighbour = caller(&mut app, wid);
    let _ = neighbour;
    app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
    let (_, person_view) = app.active_native_view(wid).unwrap();
    let person_tab = active_tab(&app, wid);
    select(&mut app, wid, caller_tab);

    let before = Instant::now();
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, Some((3, None))));
    let ok = ok(&line);
    assert_eq!(ok.ticket, ticket);
    let canonical =
        crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&f.path).unwrap())
            .unwrap();
    assert_eq!(ok.uri, canonical);
    let tabs = tab_ids(&app, wid);
    let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
    let edit_tab = tabs[at + 1];
    assert_ne!(edit_tab, person_tab, "a new tab, right of the caller's");
    assert_eq!(active_tab(&app, wid), edit_tab, "it takes focus");
    let (_, front_view) = app.active_native_view(wid).unwrap();
    assert_eq!(front_view, view(&ok));
    assert_ne!(view(&ok), person_view, "the person's view is never reused");
    assert_eq!(document_of(&app, view(&ok)), document_of(&app, person_view));
    assert_eq!(ok.doc, document_of(&app, person_view).get());
    assert!(!attention(&app, wid, person_tab), "nor marked");
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view(&ok)) else {
        panic!("an Editor view");
    };
    let caret = crate::path_link::line_col_offset(text, 3, None);
    assert_eq!(
        state.buffer.as_ref().unwrap().selections,
        vec![crate::native_editor::Selection::caret(caret)]
    );
    assert_eq!(
        hold::get_at(sid, before),
        Some(hold::Hold {
            ticket,
            view: ok.view
        })
    );
}

#[test]
fn edit_admission_never_coalesces_with_a_gesture_open() {
    let f = fixture("coalesce", b"draft\n");
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let gesture = DocumentAdmissionKey {
        uri: f.uri.clone(),
        kind: AppKind::Editor,
        edit: None,
    };
    let pending = app.mint_document_admission(wid, gesture.clone());

    let (first, line) = edit(&mut app, sid, request(&f.uri, None));
    let first_ok = ok(&line);
    assert_eq!(
        app.document_admission_pending(wid, &gesture),
        Some(pending),
        "the edit read on its own; the gesture's read is untouched"
    );
    let (second, line) = edit(&mut app, sid, request(&f.uri, None));
    let second_ok = ok(&line);
    assert_ne!(first, second);
    assert_ne!(
        first_ok.view, second_ok.view,
        "each ticket owns its own view"
    );
    assert_eq!(first_ok.doc, second_ok.doc);

    let posted = |request| {
        let (reply, _answer) = std::sync::mpsc::channel();
        call(crate::control_edit::mint_ticket(), sid, request, reply)
    };
    let (_, a, a_ticket) = app
        .begin_edit_in_flight_for_test(posted(request(&f.uri, None)))
        .unwrap();
    let (_, b, b_ticket) = app
        .begin_edit_in_flight_for_test(posted(request(&f.uri, None)))
        .unwrap();
    assert_ne!(a, b);
    assert_ne!(a, gesture);
    assert_ne!(a_ticket, b_ticket);
    assert_eq!(app.document_admission_pending(wid, &a), Some(a_ticket));
    assert_eq!(app.document_admission_pending(wid, &b), Some(b_ticket));
}

#[test]
fn edit_uses_the_callers_window_not_the_frontmost() {
    let f = fixture("window", b"x\n");
    let mut app = app();
    let front = WindowId(0);
    let sid = fresh_session(&mut app);
    let other = app.insert_logical_window(crate::stub_session(sid), 24, 80);
    app.frontmost_window = Some(front);
    let front_tabs = tab_ids(&app, front);
    let caller_tab = active_tab(&app, other);

    let (_, line) = edit(&mut app, sid, request(&f.uri, None));
    let ok = ok(&line);
    assert_eq!(
        tab_ids(&app, front),
        front_tabs,
        "nothing lands in the front window"
    );
    let tabs = tab_ids(&app, other);
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[0], caller_tab);
    assert!(
        app.windows[&other]
            .tab_set
            .get(tabs[1])
            .unwrap()
            .root
            .contains(view(&ok)),
        "the view is in the caller's window"
    );
    assert_eq!(active_tab(&app, other), caller_tab);
    assert!(attention(&app, other, tabs[1]));
}

#[test]
fn focus_follows_the_askpass_rule() {
    let f = fixture("focus", b"x\n");
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);

    let (_, line) = edit(&mut app, sid, request(&f.uri, None));
    let first = ok(&line);
    let tabs = tab_ids(&app, wid);
    let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
    assert_eq!(active_tab(&app, wid), tabs[at + 1]);
    assert!(!attention(&app, wid, tabs[at + 1]));
    assert!(
        app.windows[&wid]
            .tab_set
            .get(tabs[at + 1])
            .unwrap()
            .root
            .contains(view(&first))
    );

    let _other = caller(&mut app, wid);
    let in_front = active_tab(&app, wid);
    let (_, line) = edit(&mut app, sid, request(&f.uri, None));
    let second = ok(&line);
    assert_eq!(active_tab(&app, wid), in_front);
    let tabs = tab_ids(&app, wid);
    let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
    let placed = tabs[at + 1];
    assert!(
        app.windows[&wid]
            .tab_set
            .get(placed)
            .unwrap()
            .root
            .contains(view(&second))
    );
    assert!(attention(&app, wid, placed));
}

#[cfg(unix)]
#[test]
fn an_edit_that_opens_behind_never_tells_the_persons_program_it_lost_focus() {
    use crate::focus_report_tests::{FOCUS_OUT, drain, observer};
    let f = fixture("behind-focus", b"x\n");
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let (pipe, sink) = observer();
    let person = fresh_session(&mut app);
    app.push_stub_tab(wid, crate::stub_session_with_sink(person, sink));
    app.next_session_id = person + 1;
    let person_tab = active_tab(&app, wid);
    crate::feed_session_for_test(&app, person, b"\x1b[?1004h");
    let _ = drain(pipe);

    let (_, line) = edit(&mut app, sid, request(&f.uri, None));
    let placed = tab_of(&app, wid, view(&ok(&line)));
    assert_eq!(active_tab(&app, wid), person_tab, "the person's tab stays");
    assert!(attention(&app, wid, placed), "opened behind, marked");
    assert_eq!(
        drain(pipe),
        Vec::<u8>::new(),
        "the person's program was never told it lost focus"
    );

    select(&mut app, wid, placed);
    assert_eq!(
        drain(pipe),
        FOCUS_OUT.to_vec(),
        "control: choosing the edit tab does blur it"
    );
}

#[test]
fn an_edit_that_opens_behind_leaves_the_persons_selection_drag_going() {
    let f = fixture("behind-drag", b"x\n");
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let person = fresh_session(&mut app);
    app.push_stub_tab(wid, crate::stub_session(person));
    app.next_session_id = person + 1;
    let person_tab = active_tab(&app, wid);
    crate::multi_window_tests::start_selection_drag(&mut app, wid);

    let (_, line) = edit(&mut app, sid, request(&f.uri, None));
    let placed = tab_of(&app, wid, view(&ok(&line)));
    assert_eq!(active_tab(&app, wid), person_tab, "the person's tab stays");
    assert!(
        app.windows[&wid].selecting,
        "the drag the person is making goes on"
    );

    select(&mut app, wid, placed);
    assert!(
        !app.windows[&wid].selecting,
        "control: choosing the edit tab ends the drag"
    );
}

#[test]
fn dirty_close_answers_closed_only_after_checkpoint_ack() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);

    let f = fixture("dirty-leaf", b"base\n");
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
    let ok = ok(&line);
    let edit_tab = active_tab(&app, wid);
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    let (_, terminal) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    focus_leaf(&mut app, wid, view(&ok));
    type_text(&mut app, wid, "edited ");
    let document = document_of(&app, view(&ok));
    let held = app.hold_document_save_for_test(document);
    assert!(
        app.close_focused_mixed_leaf(wid).is_err(),
        "the close waits"
    );
    assert!(app.document_close_pending_for_test(document));
    assert_eq!(
        answer.try_recv(),
        Err(TryRecvError::Empty),
        "no answer before the ack"
    );
    assert!(app.view_store.get(view(&ok)).is_some());

    app.land_held_document_save_for_test(document, view(&ok), held)
        .unwrap();
    let line = answer
        .try_recv()
        .expect("answered once the save is durable");
    let outcome = EditOutcome::parse(&line).unwrap();
    let durable = app.document_store.checkpoint_seq(document).unwrap();
    assert_eq!(
        outcome,
        EditOutcome {
            result: EditResult::Closed,
            saved: true,
            seq: durable.0
        }
    );
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "edited base\n");
    assert!(app.view_store.get(view(&ok)).is_none());
    assert!(
        app.view_store.get(terminal).is_some(),
        "the rest of the tab stays"
    );
    assert_eq!(active_tab(&app, wid), edit_tab, "its tab is still there");
    assert_ne!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));
    let (_, late) = wait(&mut app, AwaitKey::Ticket(ticket));
    assert_eq!(late.try_recv().unwrap(), line);

    select(&mut app, wid, caller_tab);
    let g = fixture("dirty-tab", b"base\n");
    let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
    let tab_view = view(&self::ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    type_text(&mut app, wid, "again ");
    let document = document_of(&app, tab_view);
    let held = app.hold_document_save_for_test(document);
    assert!(
        app.close_active_native_tab(wid).is_err(),
        "refused while saving"
    );
    assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
    assert!(app.view_store.get(tab_view).is_some());
    assert!(app.document_close_pending_for_test(document));
    assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "base\n");
    let requested = app.document_store.snapshot(document).unwrap().seq;
    assert!(app.document_store.checkpoint_seq(document).unwrap() < requested);
    app.land_held_document_save_for_test(document, tab_view, held)
        .unwrap();
    let line = answer
        .try_recv()
        .expect("the checkpoint acknowledgement completes the original close");
    let durable = app.document_store.checkpoint_seq(document).unwrap();
    assert_eq!(durable, requested);
    assert_eq!(
        EditOutcome::parse(&line).unwrap(),
        EditOutcome {
            result: EditResult::Closed,
            saved: true,
            seq: durable.0,
        }
    );
    assert_eq!(answer.try_recv(), Err(TryRecvError::Disconnected));
    assert!(app.view_store.get(tab_view).is_none());
    assert!(!app.document_close_pending_for_test(document));
    assert_ne!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));
    let (_, late) = wait(&mut app, AwaitKey::Ticket(ticket));
    assert_eq!(late.try_recv().unwrap(), line);
    assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "again base\n");
    assert_eq!(
        active_tab(&app, wid),
        caller_tab,
        "handed back to the caller"
    );
}

#[test]
fn hangup_withdraws_and_orphans_after_5s_and_keeps_the_view() {
    let f = fixture("orphan", b"keep\n");
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
    let ok = ok(&line);
    let (waiter, _answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    let held = Some(hold::Hold {
        ticket,
        view: ok.view,
    });
    assert_eq!(hold::get(sid), held);

    let left = Instant::now();
    app.withdraw_edit_waiter_at(waiter, left);
    app.sweep_orphaned_edits(left);
    assert!(!orphaned(&app, ticket));
    assert_eq!(hold::get_at(sid, left), held);
    let later = left + ORPHAN_AFTER;
    app.sweep_orphaned_edits(later);
    assert!(orphaned(&app, ticket));
    assert_eq!(hold::get(sid), None);
    assert!(
        app.view_store.get(view(&ok)).is_some(),
        "an orphan keeps its view"
    );
    assert_eq!(app.active_native_view(wid).map(|(_, v)| v), Some(view(&ok)));

    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    assert!(!orphaned(&app, ticket));
    assert_eq!(hold::get(sid), held);
    app.close_active_native_tab(wid).unwrap();
    let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
    assert_eq!(outcome.result, EditResult::Closed);
    assert!(!outcome.saved, "nothing was saved");
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "keep\n");
}

#[test]
fn await_by_uri_ignores_views_the_person_opened() {
    let f = fixture("uri", b"shared\n");
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);
    app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
    let person_tab = active_tab(&app, wid);
    select(&mut app, wid, caller_tab);
    let (_, line) = edit(&mut app, sid, request(&f.uri, None));
    let edit_tab = active_tab(&app, wid);
    let ok = ok(&line);
    let (_, by_request) = wait(&mut app, AwaitKey::Uri(f.uri.clone()));
    let (_, by_canonical) = wait(&mut app, AwaitKey::Uri(ok.uri.clone()));

    select(&mut app, wid, person_tab);
    app.close_active_native_tab(wid).unwrap();
    assert_eq!(by_request.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(by_canonical.try_recv(), Err(TryRecvError::Empty));

    select(&mut app, wid, edit_tab);
    app.close_active_native_tab(wid).unwrap();
    for answer in [by_request, by_canonical] {
        let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
        assert_eq!(outcome.result, EditResult::Closed);
    }

    let g = fixture("uri-successor", b"theirs\n");
    app.open_document_tab(AppKind::Editor, &g.uri).unwrap();
    let canonical =
        crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&g.path).unwrap())
            .unwrap();
    let (_, successor) = wait(&mut app, AwaitKey::Uri(canonical));
    assert_eq!(successor.try_recv(), Err(TryRecvError::Empty));
    app.close_active_native_tab(wid).unwrap();
    let outcome = EditOutcome::parse(&successor.try_recv().unwrap()).unwrap();
    assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, false));
}

#[test]
fn edit_on_headless_answers_unsupported() {
    let f = fixture("headless", b"x\n");
    let mut app = App::headless_for_test();
    let (_, line) = edit(&mut app, 0, request(&f.uri, None));
    assert_eq!(line, "ERR unsupported headless\n");
    assert!(app.native_documents.edits.open.is_empty());

    let mut app = self::app();
    let sid = caller(&mut app, WindowId(0));
    assert_eq!(
        edit(&mut app, sid + 1_000, request(&f.uri, None)).1,
        "ERR no-window\n"
    );
    assert!(app.native_documents.edits.open.is_empty());
}

#[test]
fn edit_refusals_name_the_reason() {
    let f = fixture("refusals", b"\xff\xfe not utf-8\n");
    let mut app = app();
    let sid = caller(&mut app, WindowId(0));
    let tabs = tab_ids(&app, WindowId(0));
    let missing =
        crate::native_document_host::path_to_file_uri(&f.path.with_extension("gone")).unwrap();
    for (uri, refusal) in [
        (f.uri.as_str(), "ERR refused invalid-utf8\n"),
        (missing.as_str(), "ERR refused missing\n"),
    ] {
        assert_eq!(edit(&mut app, sid, request(uri, None)).1, refusal, "{uri}");
    }
    assert_eq!(tab_ids(&app, WindowId(0)), tabs, "nothing opened");
    assert!(app.native_documents.edits.open.is_empty());
}

#[test]
fn timeout_cancels_and_a_late_admission_is_dropped() {
    let f = fixture("late", b"late\n");
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let tabs = tab_ids(&app, wid);
    let ticket = crate::control_edit::mint_ticket();
    let (reply, answer) = std::sync::mpsc::channel();
    let (window, key, admission) = app
        .begin_edit_in_flight_for_test(call(ticket, sid, request(&f.uri, None), reply))
        .unwrap();
    app.cancel_edit(ticket);
    assert_eq!(answer.try_recv(), Err(TryRecvError::Disconnected));

    let result = crate::native_document_host::admit_local_file(
        &key.uri,
        crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
        false,
    )
    .map(Box::new)
    .map_err(DocumentOpenFailure);
    let rejected = app
        .complete_document_admission(DocumentAdmissionOutcome {
            ticket: admission,
            wid: window,
            key,
            result,
        })
        .unwrap_err();
    assert!(
        matches!(rejected, DocumentAdmissionRejected::Dropped(_)),
        "{rejected:?}"
    );
    assert_eq!(tab_ids(&app, wid), tabs, "a late admission opens nothing");
    let (_, unknown) = wait(&mut app, AwaitKey::Ticket(ticket));
    assert_eq!(unknown.try_recv().unwrap(), "ERR unknown ticket\n");
}

#[test]
fn a_background_edit_keeps_its_mark_until_the_person_sees_it() {
    let text = "one\ntwo\nthree\n";
    let f = fixture("mark", text.as_bytes());
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);
    app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
    let (_, person_view) = app.active_native_view(wid).unwrap();
    let person_tab = active_tab(&app, wid);

    let (_, line) = edit(&mut app, sid, request(&f.uri, Some((2, None))));
    let ok = ok(&line);
    let placed = tab_of(&app, wid, view(&ok));
    let tabs = tab_ids(&app, wid);
    let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
    assert_eq!(tabs[at + 1], placed, "right of the caller's tab");
    assert_eq!(active_tab(&app, wid), person_tab, "the person's tab stays");
    assert!(attention(&app, wid, placed), "opened behind, marked");
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view(&ok)) else {
        panic!("an Editor view");
    };
    assert_eq!(
        state.buffer.as_ref().unwrap().selections,
        vec![crate::native_editor::Selection::caret(
            crate::path_link::line_col_offset(text, 2, None)
        )],
        "the caret is on line 2 behind the person's tab"
    );

    app.refresh_disambiguated_document_titles();
    app.refresh_aggregate_tab_presentation(wid, placed);
    assert!(attention(&app, wid, placed), "a refresh keeps the mark");
    type_text(&mut app, wid, "saved ");
    let document = document_of(&app, person_view);
    let held = app.hold_document_save_for_test(document);
    app.land_held_document_save_for_test(document, person_view, held)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&f.path).unwrap(),
        "saved one\ntwo\nthree\n"
    );
    assert!(attention(&app, wid, placed), "a save elsewhere keeps it");

    select(&mut app, wid, placed);
    assert!(!attention(&app, wid, placed));
    select(&mut app, wid, person_tab);
    app.refresh_aggregate_tab_presentation(wid, placed);
    assert!(!attention(&app, wid, placed), "seen once, spent");
}

#[test]
fn a_cancel_after_the_view_opened_orphans_it_unless_a_waiter_holds_it() {
    let f = fixture("cancel-open", b"x\n");
    let g = fixture("cancel-held", b"y\n");
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
    let first = view(&ok(&line));
    app.cancel_edit(ticket);
    assert!(orphaned(&app, ticket));
    assert_ne!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));
    assert!(
        app.view_store.get(first).is_some(),
        "an orphan keeps its view"
    );

    let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
    let second = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Uri(g.uri.clone()));
    app.cancel_edit(ticket);
    assert!(!orphaned(&app, ticket), "the waiter holds it");
    assert_eq!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));
    assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
    let tab = tab_of(&app, wid, second);
    select(&mut app, wid, tab);
    app.close_active_native_tab(wid).unwrap();
    let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
    assert_eq!(outcome.result, EditResult::Closed);
}

#[test]
fn a_refused_or_cancelled_admission_settles_the_waiters_it_covered() {
    let f = fixture("settle", b"x\n");
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);

    let ticket = crate::control_edit::mint_ticket();
    let (reply, _cancelled) = std::sync::mpsc::channel();
    app.begin_edit_in_flight_for_test(call(ticket, sid, request(&f.uri, None), reply))
        .unwrap();
    let (_, by_uri) = wait(&mut app, AwaitKey::Uri(f.uri.clone()));
    let (_, by_ticket) = wait(&mut app, AwaitKey::Ticket(ticket));
    assert_eq!(by_uri.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(by_ticket.try_recv(), Err(TryRecvError::Empty));
    app.cancel_edit(ticket);
    let outcome = EditOutcome::parse(&by_uri.try_recv().expect("settled at the cancel")).unwrap();
    assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, false));
    assert_eq!(by_ticket.try_recv().unwrap(), "ERR unknown ticket\n");

    let missing =
        crate::native_document_host::path_to_file_uri(&f.path.with_extension("gone")).unwrap();
    let ticket = crate::control_edit::mint_ticket();
    let (reply, refused) = std::sync::mpsc::channel();
    let (window, key, admission) = app
        .begin_edit_in_flight_for_test(call(ticket, sid, request(&missing, None), reply))
        .unwrap();
    let (_, by_uri) = wait(&mut app, AwaitKey::Uri(missing));
    let result = crate::native_document_host::admit_local_file(
        &key.uri,
        crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
        false,
    )
    .map(Box::new)
    .map_err(DocumentOpenFailure);
    let _ = app.complete_document_admission(DocumentAdmissionOutcome {
        ticket: admission,
        wid: window,
        key,
        result,
    });
    assert_eq!(refused.try_recv().unwrap(), "ERR refused missing\n");
    let outcome = EditOutcome::parse(&by_uri.try_recv().expect("settled at the refusal")).unwrap();
    assert_eq!(
        outcome,
        EditOutcome {
            result: EditResult::Closed,
            saved: false,
            seq: 0
        }
    );
    assert!(app.native_documents.edits.waiters.is_empty());
}

#[cfg(unix)]
#[test]
fn a_uri_wait_by_another_spelling_of_the_file_finds_it() {
    let alias = |fixture: &Fixture| {
        let dir = fixture.path.parent().unwrap();
        std::os::unix::fs::symlink(dir, dir.join("alias")).unwrap();
        crate::native_document_host::path_to_file_uri(&dir.join("alias/COMMIT_EDITMSG")).unwrap()
    };
    let f = fixture("spelling-ticket", b"x\n");
    let g = fixture("spelling-views", b"y\n");
    let (f_alias, g_alias) = (alias(&f), alias(&g));
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);

    let (_, line) = edit(&mut app, sid, request(&f.uri, None));
    let edit_view = view(&ok(&line));
    let (_, by_alias) = wait(&mut app, AwaitKey::Uri(f_alias));
    assert_eq!(by_alias.try_recv(), Err(TryRecvError::Empty));
    let tab = tab_of(&app, wid, edit_view);
    select(&mut app, wid, tab);
    app.close_active_native_tab(wid).unwrap();
    let outcome = EditOutcome::parse(&by_alias.try_recv().unwrap()).unwrap();
    assert_eq!(outcome.result, EditResult::Closed);

    app.open_document_tab(AppKind::Editor, &g.uri).unwrap();
    let (_, successor) = wait(&mut app, AwaitKey::Uri(g_alias));
    assert_eq!(
        successor.try_recv(),
        Err(TryRecvError::Empty),
        "the view is still open"
    );
    app.close_active_native_tab(wid).unwrap();
    let outcome = EditOutcome::parse(&successor.try_recv().unwrap()).unwrap();
    assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, false));
}

#[test]
fn edit_and_await_edit_answer_on_the_wire() {
    use crate::control::control_media::{Hops, MainPost, take_hop};
    fn slice(left: Duration) -> Duration {
        left.min(Duration::from_millis(5))
    }
    const HANG: Duration = Duration::from_secs(60);
    let text = "one\ntwo\nthree\nfour\n";
    let f = fixture("wire", text.as_bytes());
    let mut app = app();
    let wid = WindowId(0);
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);

    let hops: &'static Hops = Box::leak(Box::new(Hops::new()));
    let line = crate::control_edit::serve_edit(
        crate::control::Scope::Owner,
        Some(sid),
        &format!("{} line=3", f.uri),
        &mut MainPost::injected(
            |wake| match take_hop(wake) {
                crate::Wake::EditRequest(call) => {
                    app.begin_edit(*call);
                    true
                }
                other => panic!("unexpected {other:?}"),
            },
            hops,
        ),
        &mut || false,
        HANG,
        slice,
    );
    assert!(line.starts_with("OK edit ticket="), "{line}");
    let ok = ok(&line);
    let tabs = tab_ids(&app, wid);
    let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
    assert_eq!(tab_of(&app, wid, view(&ok)), tabs[at + 1]);
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view(&ok)) else {
        panic!("an Editor view");
    };
    assert_eq!(
        state.buffer.as_ref().unwrap().selections,
        vec![crate::native_editor::Selection::caret(
            crate::path_link::line_col_offset(text, 3, None)
        )]
    );

    let (wakes, posted) = std::sync::mpsc::channel::<crate::Wake>();
    let rest = format!("edit ticket={} timeout=60000", ok.ticket);
    let lane = std::thread::spawn(move || {
        crate::control_edit::serve_await_edit(
            crate::control::Scope::Owner,
            &rest,
            &mut MainPost::injected(|wake| wakes.send(wake).is_ok(), hops),
            &mut || false,
            slice,
        )
    });
    let deliver = |app: &mut App, wake| match take_hop(wake) {
        crate::Wake::EditAwait(call) => app.await_edit(*call),
        crate::Wake::EditAwaitWithdraw { waiter } => app.withdraw_edit_waiter(waiter),
        other => panic!("unexpected {other:?}"),
    };
    deliver(&mut app, posted.recv_timeout(HANG).expect("the wait"));
    type_text(&mut app, wid, "edited ");
    let document = document_of(&app, view(&ok));
    let held = app.hold_document_save_for_test(document);
    app.land_held_document_save_for_test(document, view(&ok), held)
        .unwrap();
    let durable = app.document_store.checkpoint_seq(document).unwrap();
    app.close_active_native_tab(wid).unwrap();
    deliver(&mut app, posted.recv_timeout(HANG).expect("the withdrawal"));
    assert_eq!(
        lane.join().unwrap(),
        format!("OK edit done result=closed saved=1 seq={}\n", durable.0)
    );
    assert_eq!(
        std::fs::read_to_string(&f.path).unwrap(),
        "one\ntwo\nedited three\nfour\n"
    );
    assert!(app.native_documents.edits.waiters.is_empty());
}

fn press(app: &mut App, wid: WindowId, chord: &str) {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
    for key in chord.split(' ') {
        let letter = key
            .strip_prefix("C-")
            .and_then(|rest| rest.chars().next())
            .expect("a C- key");
        let event = crate::input::InputEvent::Key {
            key: Key::Character(letter),
            mods: Modifiers::CTRL,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        };
        assert!(app.native_input_event(wid, &event), "{chord}");
    }
}

fn status(app: &App, view: ViewId) -> Option<String> {
    match app.native_runtime.view_state(view) {
        Some(AppViewState::Editor(state)) => state.status.clone(),
        _ => panic!("an Editor view"),
    }
}

fn buffer_text(app: &App, view: ViewId) -> String {
    let document = document_of(app, view);
    app.document_store
        .snapshot(document)
        .unwrap()
        .text
        .as_ref()
        .to_string()
}

fn header_row(app: &App, wid: WindowId) -> Option<(String, bool, bool)> {
    use crate::native_ui::UiKey;
    let compiled = app.compiled_native_ui(wid).expect("the view compiles");
    let row = compiled.semantic(&UiKey::new("editor/edit-header"))?;
    let part = |key: &str| {
        compiled.semantic(&UiKey::new(key)).is_some_and(|node| {
            node.parent.as_ref() == Some(&row.key)
                && node.action.as_ref().map(|action| action.as_str()) == Some(key)
        })
    };
    Some((
        row.label.clone(),
        part("editor/finish"),
        part("editor/abort"),
    ))
}

fn edit_action_gates(app: &App, wid: WindowId, action: &str) -> (bool, bool) {
    let (instance, view) = app.active_native_view(wid).unwrap();
    let commands = app.native_runtime.commands(instance, view).unwrap();
    let command = commands
        .iter()
        .find(|command| command.id.as_str() == action)
        .unwrap();
    let compiled = app.compiled_native_ui(wid).unwrap();
    let control = compiled
        .semantic(&crate::native_ui::UiKey::new(action))
        .unwrap();
    (command.enabled, control.state.unwrap().enabled)
}

fn edit_buffer(app: &App, view: ViewId) -> &crate::native_editor::EditorBufferView {
    let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
        panic!("an Editor view")
    };
    state.buffer.as_ref().unwrap()
}

fn outcome(answer: &Receiver<String>) -> EditOutcome {
    EditOutcome::parse(&answer.try_recv().expect("answered")).unwrap()
}

#[test]
fn finish_saves_then_answers_finished_saved_1_after_ack() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);
    let f = fixture("finish", b"base\n");
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    type_text(&mut app, wid, "fix: ");
    let document = document_of(&app, edit_view);
    let held = app.hold_document_save_for_test(document);
    press(&mut app, wid, "C-c C-c");
    assert_eq!(
        status(&app, edit_view).as_deref(),
        Some("Saving, then closing")
    );
    assert_eq!(answer.try_recv(), Err(TryRecvError::Empty), "not yet acked");
    assert!(app.view_store.get(edit_view).is_some());
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "base\n");

    app.land_held_document_save_for_test(document, edit_view, held)
        .unwrap();
    let durable = app.document_store.checkpoint_seq(document).unwrap();
    assert_eq!(
        outcome(&answer),
        EditOutcome {
            result: EditResult::Finished,
            saved: true,
            seq: durable.0,
        }
    );
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "fix: base\n");
    assert!(
        app.view_store.get(edit_view).is_none(),
        "done closes the view"
    );
    assert_eq!(
        active_tab(&app, wid),
        caller_tab,
        "handed back to the caller"
    );
    assert_ne!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));

    let g = fixture("finish-clean", b"as is\n");
    let (ticket, _) = edit(&mut app, sid, request(&g.uri, None));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    press(&mut app, wid, "C-c C-c");
    let done = outcome(&answer);
    assert_eq!((done.result, done.saved), (EditResult::Finished, false));
}

#[test]
fn abort_discards_and_answers_aborted() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);
    let f = fixture("abort", b"base\n");
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    type_text(&mut app, wid, "never mind ");
    press(&mut app, wid, "C-c C-k");
    let aborted = outcome(&answer);
    assert_eq!(
        (aborted.result, aborted.saved),
        (EditResult::Aborted, false)
    );
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "base\n");
    assert!(app.view_store.get(edit_view).is_none());
    assert_eq!(active_tab(&app, wid), caller_tab);

    let g = fixture("abort-after-save", b"base\n");
    let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    type_text(&mut app, wid, "kept ");
    app.save_document_checkpoint(document_of(&app, edit_view), edit_view)
        .unwrap();
    type_text(&mut app, wid, "dropped ");
    press(&mut app, wid, "C-c C-k");
    let aborted = outcome(&answer);
    assert_eq!(
        (aborted.result, aborted.saved),
        (EditResult::Aborted, false)
    );
    assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "kept base\n");
}

#[test]
fn finish_without_ticket_refuses_and_keeps_the_view() {
    let wid = WindowId(0);
    let mut app = app();
    let f = fixture("finish-none", b"mine\n");
    app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
    let (_, mine) = app.active_native_view(wid).unwrap();
    type_text(&mut app, wid, "typed ");
    press(&mut app, wid, "C-c C-c");
    assert_eq!(status(&app, mine).as_deref(), Some(NO_EDIT_WAITING));
    assert!(app.view_store.get(mine).is_some(), "the view stays");
    assert_eq!(buffer_text(&app, mine), "typed mine\n");
    assert_eq!(
        std::fs::read_to_string(&f.path).unwrap(),
        "mine\n",
        "nothing was saved"
    );

    let canonical =
        crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&f.path).unwrap())
            .unwrap();
    let (_, answer) = wait(&mut app, AwaitKey::Uri(canonical));
    assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
    press(&mut app, wid, "C-c C-c");
    let done = outcome(&answer);
    assert_eq!((done.result, done.saved), (EditResult::Finished, true));
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "typed mine\n");
    assert!(app.view_store.get(mine).is_none());
}

#[test]
fn abort_without_waiter_refuses_and_keeps_text() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);

    let f = fixture("abort-none", b"mine\n");
    app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
    let (_, mine) = app.active_native_view(wid).unwrap();
    type_text(&mut app, wid, "typed ");
    press(&mut app, wid, "C-c C-k");
    assert_eq!(status(&app, mine).as_deref(), Some(NO_EDIT_WAITING));
    assert_eq!(buffer_text(&app, mine), "typed mine\n", "nothing discarded");
    assert!(app.view_store.get(mine).is_some());

    select(&mut app, wid, caller_tab);
    let g = fixture("abort-orphan", b"theirs\n");
    let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
    let orphan = view(&ok(&line));
    let (waiter, _gone) = wait(&mut app, AwaitKey::Ticket(ticket));
    let left = Instant::now();
    app.withdraw_edit_waiter_at(waiter, left);
    app.sweep_orphaned_edits(left + ORPHAN_AFTER);
    assert!(orphaned(&app, ticket));
    type_text(&mut app, wid, "typed ");
    press(&mut app, wid, "C-c C-k");
    assert_eq!(status(&app, orphan).as_deref(), Some(ABORT_GONE));
    assert_eq!(buffer_text(&app, orphan), "typed theirs\n");
    assert!(app.view_store.get(orphan).is_some());
    press(&mut app, wid, "C-c C-c");
    assert!(app.view_store.get(orphan).is_none());
    assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "typed theirs\n");
    let (_, late) = wait(&mut app, AwaitKey::Ticket(ticket));
    let done = outcome(&late);
    assert_eq!((done.result, done.saved), (EditResult::Finished, true));

    select(&mut app, wid, caller_tab);
    let h = fixture("abort-unwaited", b"open\n");
    let mut unwaited = request(&h.uri, None);
    unwaited.wait = false;
    let (ticket, line) = edit(&mut app, sid, unwaited);
    let free = view(&ok(&line));
    app.sweep_orphaned_edits(Instant::now() + ORPHAN_AFTER * 4);
    assert!(
        !orphaned(&app, ticket),
        "an unwaited ticket is not orphaned"
    );
    type_text(&mut app, wid, "typed ");
    press(&mut app, wid, "C-c C-k");
    assert_eq!(status(&app, free).as_deref(), Some(ABORT_UNWAITED));
    assert_eq!(buffer_text(&app, free), "typed open\n");
    press(&mut app, wid, "C-c C-c");
    assert!(app.view_store.get(free).is_none());
    assert_eq!(std::fs::read_to_string(&h.path).unwrap(), "typed open\n");
    assert_eq!(
        app.native_documents
            .edits
            .outcome_of(ticket)
            .map(|o| o.result),
        Some(EditResult::Finished)
    );
}

#[test]
fn header_row_only_on_edit_views() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);
    let f = fixture("header", b"subject\n");
    app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
    let (_, mine) = app.active_native_view(wid).unwrap();
    assert_eq!(header_row(&app, wid), None, "the person's own view");

    select(&mut app, wid, caller_tab);
    let mut asked = request(&f.uri, None);
    asked.for_word = Some("git".to_string());
    let (ticket, line) = edit(&mut app, sid, asked);
    let edit_view = view(&ok(&line));
    let (waiter, _answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    assert_eq!(
        header_row(&app, wid),
        Some(("Editing for git".to_string(), true, true))
    );
    let (mine_tab, edit_tab) = (tab_of(&app, wid, mine), tab_of(&app, wid, edit_view));
    select(&mut app, wid, mine_tab);
    assert_eq!(header_row(&app, wid), None);
    select(&mut app, wid, edit_tab);

    let left = Instant::now();
    app.withdraw_edit_waiter_at(waiter, left);
    app.sweep_orphaned_edits(left + ORPHAN_AFTER);
    assert_eq!(
        header_row(&app, wid),
        Some((
            "The program waiting on this edit has gone".to_string(),
            true,
            false
        ))
    );
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    assert_eq!(
        header_row(&app, wid),
        Some(("Editing for git".to_string(), true, true))
    );

    type_text(&mut app, wid, "fix ");
    let (instance, _) = app.active_native_view(wid).unwrap();
    let document = document_of(&app, edit_view);
    let draft = app.document_store.snapshot(document).unwrap();
    let mut capability_failures = Vec::new();
    for prompt in [
        crate::native_editor::EditorCommand::GotoLine,
        crate::native_editor::EditorCommand::ExecuteCommand,
        crate::native_editor::EditorCommand::SwitchBuffer,
        crate::native_editor::EditorCommand::KillBuffer,
        crate::native_editor::EditorCommand::IncrementalSearch,
    ] {
        app.dispatch_native_event(wid, AppEvent::EditorCommand(prompt))
            .unwrap();
        type_text(&mut app, wid, "2");
        assert!(edit_buffer(&app, edit_view).minibuffer_active());
        let before = edit_buffer(&app, edit_view).clone();
        for action in ["editor/finish", "editor/abort"] {
            let gates = edit_action_gates(&app, wid, action);
            if gates != (false, false) {
                capability_failures.push(format!(
                    "{prompt:?}: {action} command/toolbar enabled: {gates:?}"
                ));
            }
            if app
                .act_app(crate::app_control::ActRequest {
                    view: edit_view,
                    ui_key: action.to_string(),
                    action: action.to_string(),
                    value: None,
                })
                .is_ok()
            {
                capability_failures.push(format!("{prompt:?}: {action} act did not refuse"));
            }
            assert_eq!(edit_buffer(&app, edit_view), &before);
            if app
                .activate_native_palette_target(&crate::palette::NativeCommandTarget {
                    window: wid,
                    instance,
                    view: edit_view,
                    generation: app.native_runtime.view_generation(edit_view).unwrap(),
                    action: crate::native_ui::ActionId::new(action),
                })
                .is_ok()
            {
                capability_failures.push(format!("{prompt:?}: {action} palette did not refuse"));
            }
            assert_eq!(edit_buffer(&app, edit_view), &before);
            assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
            assert_eq!(
                app.native_documents.edits.ticket_of_view(edit_view),
                Some(ticket)
            );
            assert!(!app.native_documents.edits.endings.contains_key(&edit_view));
            let after = app.document_store.snapshot(document).unwrap();
            assert_eq!((after.seq, after.text), (draft.seq, draft.text.clone()));
            assert_eq!(app.document_store.dirty(document), Some(true));
            assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "subject\n");
        }
        app.dispatch_native_event(
            wid,
            AppEvent::EditorCommand(crate::native_editor::EditorCommand::Abort),
        )
        .unwrap();
        assert!(!edit_buffer(&app, edit_view).minibuffer_active());
        for action in ["editor/finish", "editor/abort"] {
            assert_eq!(edit_action_gates(&app, wid, action), (true, true));
        }
    }
    app.act_app(crate::app_control::ActRequest {
        view: edit_view,
        ui_key: "editor/finish".to_string(),
        action: "editor/finish".to_string(),
        value: None,
    })
    .unwrap();
    let done = outcome(&answer);
    assert_eq!((done.result, done.saved), (EditResult::Finished, true));
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "fix subject\n");
    assert!(app.view_store.get(edit_view).is_none());
    assert!(
        app.view_store.get(mine).is_some(),
        "the person's view stays"
    );

    assert!(capability_failures.is_empty(), "{capability_failures:#?}");

    select(&mut app, wid, caller_tab);
    let g = fixture("header-click", b"body\n");
    let mut unwaited = request(&g.uri, None);
    unwaited.wait = false;
    let (ticket, line) = edit(&mut app, sid, unwaited);
    let clicked = view(&ok(&line));
    assert_eq!(
        header_row(&app, wid),
        Some(("Nothing waits on this edit".to_string(), true, false))
    );
    type_text(&mut app, wid, "clicked ");
    use crate::app_mouse::{NativePointerTarget, PointerAction};
    app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
        "editor/finish".to_string(),
    )))
    .expect("the pointer reaches done");
    app.pointer_cmd(PointerAction::Click).expect("the click");
    assert!(app.view_store.get(clicked).is_none());
    assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "clicked body\n");
    assert_eq!(
        app.native_documents
            .edits
            .outcome_of(ticket)
            .map(|o| o.result),
        Some(EditResult::Finished)
    );
}

#[test]
fn create_1_opens_a_missing_file_and_first_save_creates_it() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let f = fixture("create", b"");
    let path = f.path.with_file_name("new.txt");
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut asked = request(&uri, None);
    asked.create = true;
    let (ticket, line) = edit(&mut app, sid, asked);
    let edit_view = view(&ok(&line));
    assert!(!path.exists(), "opening creates nothing");
    assert_eq!(buffer_text(&app, edit_view), "");
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    type_text(&mut app, wid, "first line\n");
    press(&mut app, wid, "C-c C-c");
    let done = outcome(&answer);
    assert_eq!((done.result, done.saved), (EditResult::Finished, true));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "first line\n");

    let other = f.path.with_file_name("never.txt");
    let uri = crate::native_document_host::path_to_file_uri(&other).unwrap();
    let mut asked = request(&uri, None);
    asked.create = true;
    let (ticket, _) = edit(&mut app, sid, asked);
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    type_text(&mut app, wid, "unwanted");
    press(&mut app, wid, "C-c C-k");
    let aborted = outcome(&answer);
    assert_eq!(
        (aborted.result, aborted.saved),
        (EditResult::Aborted, false)
    );
    assert!(!other.exists(), "abort creates nothing");
}

#[test]
fn missing_without_create_is_refused() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let tabs = tab_ids(&app, wid);
    let f = fixture("missing", b"");
    let path = f.path.with_file_name("absent.txt");
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    assert_eq!(
        edit(&mut app, sid, request(&uri, None)).1,
        "ERR refused missing\n"
    );
    assert_eq!(tab_ids(&app, wid), tabs, "nothing opened");
    assert!(!path.exists());
    assert!(app.native_documents.edits.open.is_empty());
}

#[test]
fn edit_directory_opens_a_read_only_listing_and_closes_the_wait() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let f = fixture("directory", b"contents");
    let uri = crate::native_document_host::path_to_file_uri(f.path.parent().unwrap()).unwrap();
    let (ticket, line) = edit(&mut app, sid, request(&uri, None));
    assert!(line.starts_with("OK "), "{line}");
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    let document = document_of(&app, view(&ok(&line)));
    assert!(app.document_store.directory(document).is_some());
    type_text(&mut app, wid, "must not edit");
    assert_eq!(app.document_store.dirty(document), Some(false));
    let key = |app: &mut App, letter| {
        assert!(app.native_input_event(
            wid,
            &crate::input::InputEvent::Key {
                key: aterm_types::keyboard::Key::Character(letter),
                mods: aterm_types::keyboard::Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            }
        ));
    };
    std::fs::write(f.path.with_file_name("fresh.txt"), "new entry").unwrap();
    key(&mut app, 'g');
    assert!(
        app.document_store
            .snapshot(document)
            .unwrap()
            .text
            .contains("fresh.txt")
    );
    key(&mut app, 'n');
    key(&mut app, 'f');
    let (_, file_uri) = crate::native_document_host::open_path(&f.path).unwrap();
    assert!(app.document_store.id_for_uri(&file_uri).is_some());
    app.close_active_native_tab(wid).unwrap();
    key(&mut app, 'q');
    let done = outcome(&answer);
    assert_eq!((done.result, done.saved), (EditResult::Closed, false));
    assert_eq!(std::fs::read(&f.path).unwrap(), b"contents");
}

/// T10 (docs/DESIGN-amacs-grid-2026-10-28.md): an `edit` of a table opens as a grid, and
/// Done (C-c C-c) with a cell open commits the cell, saves, and answers finished, the file's
/// CRLF line ends held and only the cell's bytes changed.
#[test]
fn t10_done_from_a_grid_commits_the_open_cell_saves_and_answers_finished() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-edit-grid-")
        .tempdir()
        .unwrap();
    let path = dir.path().join("accounts.csv");
    std::fs::write(&path, b"name,fix\r\nm1,\r\nm2,\r\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let (ticket, line) = edit(&mut app, sid, request(&uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    let grid = |app: &App| match app.native_runtime.view_state(edit_view) {
        Some(AppViewState::Editor(state)) => state
            .buffer
            .as_ref()
            .and_then(|buffer| buffer.grid())
            .cloned(),
        _ => panic!("an Editor view"),
    };
    assert!(grid(&app).is_some(), "an edit of a .csv opens as a grid");
    for named in [NamedKey::ArrowDown, NamedKey::End] {
        let event = crate::input::InputEvent::Key {
            key: Key::Named(named),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        };
        assert!(app.native_input_event(wid, &event));
    }
    type_text(&mut app, wid, "x, \"y\"");
    assert!(grid(&app).is_some_and(|grid| grid.draft.is_some()));
    press(&mut app, wid, "C-c C-c");
    let done = outcome(&answer);
    assert_eq!((done.result, done.saved), (EditResult::Finished, true));
    assert_eq!(
        String::from_utf8(std::fs::read(&path).unwrap()).unwrap(),
        "name,fix\r\nm1,\"x, \"\"y\"\"\"\r\nm2,\r\n"
    );
    assert!(
        app.view_store.get(edit_view).is_none(),
        "done closes the view"
    );
}

#[test]
fn crlf_held_buffer_takes_typed_line_ends_as_its_own() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let f = fixture("crlf", b"one\r\ntwo\r\n");
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    assert_eq!(buffer_text(&app, edit_view), "one\ntwo\n");
    type_text(&mut app, wid, "zero\r\n");
    assert_eq!(buffer_text(&app, edit_view), "zero\none\ntwo\n");
    press(&mut app, wid, "C-c C-c");
    let done = outcome(&answer);
    assert_eq!((done.result, done.saved), (EditResult::Finished, true));
    assert_eq!(std::fs::read(&f.path).unwrap(), b"zero\r\none\r\ntwo\r\n");
}

#[test]
fn abort_keeps_text_the_edit_did_not_type() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);
    let f = fixture("abort-mine", b"base\n");
    app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
    let (_, mine) = app.active_native_view(wid).unwrap();
    type_text(&mut app, wid, "mine ");
    select(&mut app, wid, caller_tab);
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    type_text(&mut app, wid, "theirs ");
    press(&mut app, wid, "C-c C-k");
    let aborted = outcome(&answer);
    assert_eq!(
        (aborted.result, aborted.saved),
        (EditResult::Aborted, false)
    );
    assert!(app.view_store.get(edit_view).is_none());
    let kept = buffer_text(&app, mine);
    assert!(
        kept.contains("mine ") && kept.contains("theirs "),
        "{kept:?}"
    );
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "base\n");
    assert_eq!(status(&app, mine).as_deref(), Some(ABORT_KEPT));

    let g = fixture("abort-mine-alone", b"base\n");
    app.open_document_tab(AppKind::Editor, &g.uri).unwrap();
    let (_, mine) = app.active_native_view(wid).unwrap();
    type_text(&mut app, wid, "mine ");
    select(&mut app, wid, caller_tab);
    let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    let edit_tab = tab_of(&app, wid, edit_view);
    let mine_tab = tab_of(&app, wid, mine);
    select(&mut app, wid, mine_tab);
    app.close_active_native_tab(wid).unwrap();
    select(&mut app, wid, edit_tab);
    press(&mut app, wid, "C-c C-k");
    assert_eq!(status(&app, edit_view).as_deref(), Some(ABORT_FOREIGN));
    assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(buffer_text(&app, edit_view), "mine base\n");
    assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "base\n");
}

/// Keep Mine over another view's unsaved text leaves that text unsaved
/// (it rebinds the file the next Save writes over, and marks nothing
/// durable), so an abort of the edit view still keeps it, and the file
/// keeps the disk's text.
#[test]
fn abort_after_keep_mine_keeps_text_the_edit_did_not_type() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);
    let f = fixture("abort-after-keep", b"base\n");
    app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
    let (_, mine) = app.active_native_view(wid).unwrap();
    type_text(&mut app, wid, "mine ");
    select(&mut app, wid, caller_tab);
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    let document = document_of(&app, edit_view);
    std::fs::write(&f.path, "changed\n").unwrap();
    app.native_document_file_changed(document.get());
    // Keep Mine refuses ("Nothing to keep") unless the change was heard.
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("editor/keep-local"),
            value: None,
        }),
    )
    .unwrap();
    assert_eq!(app.document_store.dirty(document), Some(true));
    press(&mut app, wid, "C-c C-k");
    let aborted = outcome(&answer);
    assert_eq!(
        (aborted.result, aborted.saved),
        (EditResult::Aborted, false)
    );
    assert_eq!(
        buffer_text(&app, mine),
        "mine base\n",
        "the other view's text stays"
    );
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "changed\n");
    assert_eq!(status(&app, mine).as_deref(), Some(ABORT_KEPT));
}

#[test]
fn done_whose_save_does_not_land_says_why_and_stays() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let f = fixture("finish-halts", b"base\n");
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    type_text(&mut app, wid, "lost ");
    let document = document_of(&app, edit_view);
    let held = app.hold_document_save_for_test(document);
    press(&mut app, wid, "C-c C-c");
    std::fs::write(&f.path, "changed\n").unwrap();
    assert!(
        app.land_held_document_save_for_test(document, edit_view, held)
            .is_err()
    );
    assert_eq!(
        status(&app, edit_view),
        Some(format!(
            "Not done: {}",
            crate::native_app::DISK_CONFLICT_RECOVERY
        ))
    );
    assert!(app.view_store.get(edit_view).is_some(), "the view stays");
    assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
    assert!(!app.native_documents.edits.endings.contains_key(&edit_view));
    press(&mut app, wid, "C-c C-k");
    let aborted = outcome(&answer);
    assert_eq!(
        (aborted.result, aborted.saved),
        (EditResult::Aborted, false)
    );
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "changed\n");
}

#[test]
fn abort_refused_while_done_saves_keeps_the_done() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let f = fixture("abort-during-done", b"base\n");
    let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    type_text(&mut app, wid, "kept ");
    let document = document_of(&app, edit_view);
    let held = app.hold_document_save_for_test(document);
    press(&mut app, wid, "C-c C-c");
    press(&mut app, wid, "C-c C-k");
    assert_eq!(status(&app, edit_view).as_deref(), Some(ABORT_SAVING));
    assert_eq!(buffer_text(&app, edit_view), "kept base\n");
    let (instance, _) = app.active_native_view(wid).unwrap();
    assert!(app.document_save_owed(document));
    let Some(crate::native_app::NativeApp::Editor(editor)) = app.native_runtime.app(instance)
    else {
        panic!("an Editor instance")
    };
    assert!(editor.checkpoint_pending);
    assert!(!app.can_save_document(wid));
    assert_eq!(edit_action_gates(&app, wid, "editor/finish"), (true, true));
    let generation = app.native_runtime.view_generation(edit_view).unwrap();
    let target = |action: &str| crate::palette::NativeCommandTarget {
        window: wid,
        instance,
        view: edit_view,
        generation,
        action: crate::native_ui::ActionId::new(action),
    };
    app.activate_native_palette_target(&target("editor/finish"))
        .unwrap();
    let ending = app.native_documents.edits.endings.get(&edit_view).copied();
    assert!(matches!(ending, Some(Ending::Finish { .. })));
    let before = edit_buffer(&app, edit_view).clone();
    let mut capability_failures = Vec::new();
    let gates = edit_action_gates(&app, wid, "editor/abort");
    if gates != (false, false) {
        capability_failures.push(format!("saving: Abort command/toolbar enabled: {gates:?}"));
    }
    if app
        .act_app(crate::app_control::ActRequest {
            view: edit_view,
            ui_key: "editor/abort".to_string(),
            action: "editor/abort".to_string(),
            value: None,
        })
        .is_ok()
    {
        capability_failures.push("saving: Abort act did not refuse".to_string());
    }
    assert_eq!(edit_buffer(&app, edit_view), &before);
    if app
        .activate_native_palette_target(&target("editor/abort"))
        .is_ok()
    {
        capability_failures.push("saving: Abort palette did not refuse".to_string());
    }
    assert_eq!(edit_buffer(&app, edit_view), &before);
    assert_eq!(
        app.native_documents.edits.endings.get(&edit_view),
        ending.as_ref()
    );
    assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "base\n");
    app.land_held_document_save_for_test(document, edit_view, held)
        .unwrap();
    let done = outcome(&answer);
    assert_eq!((done.result, done.saved), (EditResult::Finished, true));
    assert!(app.view_store.get(edit_view).is_none());
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "kept base\n");
    assert!(capability_failures.is_empty(), "{capability_failures:#?}");
}

#[test]
fn done_behind_the_person_leaves_them_on_their_tab() {
    let wid = WindowId(0);
    let mut app = app();
    let sid = caller(&mut app, wid);
    let caller_tab = active_tab(&app, wid);
    let elsewhere = {
        caller(&mut app, wid);
        active_tab(&app, wid)
    };
    select(&mut app, wid, caller_tab);
    let g = fixture("finish-moved-on", b"base\n");
    let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    type_text(&mut app, wid, "pressed ");
    let document = document_of(&app, edit_view);
    let held = app.hold_document_save_for_test(document);
    press(&mut app, wid, "C-c C-c");
    select(&mut app, wid, elsewhere);
    app.land_held_document_save_for_test(document, edit_view, held)
        .unwrap();
    let done = outcome(&answer);
    assert_eq!((done.result, done.saved), (EditResult::Finished, true));
    assert!(app.view_store.get(edit_view).is_none());
    assert_eq!(active_tab(&app, wid), elsewhere, "the person stays put");

    select(&mut app, wid, caller_tab);
    let h = fixture("finish-beside", b"base\n");
    let (ticket, line) = edit(&mut app, sid, request(&h.uri, None));
    let edit_view = view(&ok(&line));
    let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
    let edit_tab = tab_of(&app, wid, edit_view);
    let (_, beside) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    focus_leaf(&mut app, wid, edit_view);
    type_text(&mut app, wid, "split ");
    let document = document_of(&app, edit_view);
    let held = app.hold_document_save_for_test(document);
    press(&mut app, wid, "C-c C-c");
    focus_leaf(&mut app, wid, beside);
    app.land_held_document_save_for_test(document, edit_view, held)
        .unwrap();
    let done = outcome(&answer);
    assert_eq!((done.result, done.saved), (EditResult::Finished, true));
    assert!(app.view_store.get(edit_view).is_none());
    assert_eq!(active_tab(&app, wid), edit_tab);
    assert_eq!(app.windows[&wid].tab_set.active().unwrap().focus, beside);
}

#[test]
fn an_edit_tab_closed_from_behind_leaves_the_person_on_their_tab() {
    let wid = WindowId(0);
    for closer in ["close_tab_at", "close_editor_view"] {
        let mut app = app();
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        let elsewhere = {
            caller(&mut app, wid);
            active_tab(&app, wid)
        };
        select(&mut app, wid, caller_tab);
        let g = fixture(&format!("closed-behind-{closer}"), b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        let edit_tab = tab_of(&app, wid, edit_view);
        select(&mut app, wid, elsewhere);
        if closer == "close_tab_at" {
            let index = app.windows[&wid]
                .tab_set
                .tabs()
                .iter()
                .position(|tab| tab.id == edit_tab)
                .unwrap();
            assert!(!app.close_tab_at(wid, index));
        } else {
            app.close_editor_view(edit_view).unwrap();
        }
        assert!(app.view_store.get(edit_view).is_none(), "{closer}");
        assert_eq!(outcome(&answer).result, EditResult::Closed, "{closer}");
        assert_eq!(
            active_tab(&app, wid),
            elsewhere,
            "{closer}: the person stays put; the caller's tab stays behind"
        );
        assert_eq!(app.structural_invariant_violation(), None, "{closer}");
    }
}

#[test]
fn an_edit_tab_moved_to_another_window_closes_without_turning_its_old_window() {
    let wid = WindowId(0);
    for closer in ["close_tab_at", "close_editor_view"] {
        let mut app = app();
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        let elsewhere = {
            caller(&mut app, wid);
            active_tab(&app, wid)
        };
        select(&mut app, wid, caller_tab);
        let g = fixture(&format!("moved-away-{closer}"), b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert_eq!(
            app.windows[&wid].tab_set.active().map(|tab| tab.focus),
            Some(edit_view),
            "PRECONDITION: the edit tab in front"
        );
        let moved = app
            .detach_active_tab_logical()
            .expect("the edit tab moves to a window of its own");
        select(&mut app, wid, elsewhere);

        if closer == "close_tab_at" {
            let _ = app.close_tab_at(moved, 0);
        } else {
            app.close_editor_view(edit_view).unwrap();
        }
        assert!(app.view_store.get(edit_view).is_none(), "{closer}");
        assert_eq!(outcome(&answer).result, EditResult::Closed, "{closer}");
        assert_eq!(
            active_tab(&app, wid),
            elsewhere,
            "{closer}: the old window stays on the tab the person left it on"
        );
        assert_eq!(app.structural_invariant_violation(), None, "{closer}");
    }
}

#[test]
fn undo_back_to_a_mapped_files_text_reads_clean() {
    use crate::native_editor::EditorCommand;
    let wid = WindowId(0);
    for (name, bytes) in [
        ("undo-crlf", &b"one\r\ntwo\r\n"[..]),
        ("undo-bom", &b"\xef\xbb\xbfone\ntwo\n"[..]),
    ] {
        let f = fixture(name, bytes);
        let mut app = app();
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let (_, mine) = app.active_native_view(wid).unwrap();
        let document = document_of(&app, mine);
        type_text(&mut app, wid, "x");
        assert_eq!(app.document_store.dirty(document), Some(true), "{name}");
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::Undo))
            .unwrap();
        assert_eq!(buffer_text(&app, mine), "one\ntwo\n", "{name}");
        assert_eq!(
            app.document_store.dirty(document),
            Some(false),
            "{name}: the buffer is the file again"
        );
        assert_eq!(std::fs::read(&f.path).unwrap(), bytes, "{name}");
    }
}

/// A dirty CRLF or BOM document's draft rides an update: the journal's
/// base is the text the file was admitted as, which is what the carry
/// compares, not the file's bytes.
#[cfg(unix)]
#[test]
fn a_mapped_documents_draft_is_carried() {
    let wid = WindowId(0);
    for (name, bytes) in [
        ("carry-crlf", &b"one\r\ntwo\r\n"[..]),
        ("carry-bom", &b"\xef\xbb\xbfone\ntwo\n"[..]),
    ] {
        let f = fixture(name, bytes);
        let mut app = App::headless_for_test();
        app.use_document_journal_root_for_test(f.path.with_file_name("drafts"));
        app.carry_drafts_as_windowed_for_test();
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let (_, mine) = app.active_native_view(wid).unwrap();
        let document = document_of(&app, mine);
        type_text(&mut app, wid, "x");
        assert_eq!(
            app.draft_carry(document),
            crate::app_documents::DraftCarry::Carried {
                bytes: "xone\ntwo\n".len()
            },
            "{name}"
        );
    }
}
