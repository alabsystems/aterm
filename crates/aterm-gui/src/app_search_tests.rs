// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    FindAction, SearchDirection, SearchEdit, SearchMatch, SearchState, map_matches,
    next_word_boundary, prev_word_boundary,
};
use crate::{App, WindowId, term_lock};

/// An App whose session writes to an observer pipe, so a test can assert that a
/// control verb put nothing on the PTY.
#[cfg(unix)]
fn app_observing_pty() -> (App, [i32; 2]) {
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    (
        App::headless_for_test_with_sink(Arc::new(SinkWriter::new(pipe[1]))),
        pipe,
    )
}

#[cfg(unix)]
fn drained(pipe: [i32; 2]) -> Vec<u8> {
    let mut bytes = [0u8; 256];
    let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
    if read <= 0 {
        return Vec::new();
    }
    bytes[..read as usize].to_vec()
}

#[cfg(unix)]
#[test]
fn command_scope_find_control_does_not_open_native_search() {
    use crate::native_app::{AppEvent, AppKind, AppViewState, TextInputEvent};
    use crate::native_editor::EditorCommand;

    let scratch = aterm_tempfile::TempDir::new().unwrap();
    let path = scratch.path().join("find-control.md");
    let source = "# needle\n\nA second needle.\n";
    std::fs::write(&path, source).unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    let mut changed = Vec::new();
    for (kind, prompt) in [
        (AppKind::Editor, "idle"),
        (AppKind::Editor, "search"),
        (AppKind::Editor, "goto"),
        (AppKind::Settings, "idle"),
        (AppKind::Settings, "search"),
        (AppKind::Markdown, "idle"),
    ] {
        let (mut app, pipe) = app_observing_pty();
        let wid = WindowId(0);
        let hidden = app.front_terminal(wid).unwrap().term.clone();
        term_lock(&hidden).process(b"hidden needle\r\nsecond needle");
        let hidden_before = {
            let terminal = term_lock(&hidden);
            (
                terminal.content_seq(),
                terminal.cursor().row,
                terminal.cursor().col,
            )
        };
        if kind == AppKind::Settings {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        } else {
            app.open_document_tab(kind, &uri).unwrap();
        }
        if prompt == "search" {
            app.find_requested_in(wid);
            app.dispatch_native_event(
                wid,
                AppEvent::TextInput(TextInputEvent::Commit("needle".into())),
            )
            .unwrap();
        } else if prompt == "goto" {
            app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::GotoLine))
                .unwrap();
            app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::Commit("2".into())))
                .unwrap();
        }
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance);
        let before_document = document.map(|id| app.document_store.snapshot(id).unwrap());
        let state = app.native_runtime.view_state(view).unwrap();
        let before_common = state.common().clone();
        let before_editor = match state {
            AppViewState::Editor(state) => Some(state.clone()),
            _ => None,
        };
        let before_settings = match state {
            AppViewState::Settings(state) => Some((
                state.route,
                state.search.clone(),
                state.search_input.clone(),
                state.editing_field.clone(),
            )),
            _ => None,
        };
        for action in [
            FindAction::Open,
            FindAction::Type("control query".into()),
            FindAction::Edit(SearchEdit::MoveStart),
            FindAction::Step { forward: true },
            FindAction::Step { forward: false },
            FindAction::ToggleCase,
            FindAction::ToggleRegex,
            FindAction::Accept,
            FindAction::Cancel,
            FindAction::Status,
        ] {
            let action_name = format!("{action:?}");
            assert_eq!(app.find_cmd(action), Some(super::FindStatus::CLOSED));
            let state = app.native_runtime.view_state(view).unwrap();
            let unchanged =
                state.common() == &before_common
                    && match state {
                        AppViewState::Editor(state) => Some(state) == before_editor.as_ref(),
                        AppViewState::Settings(state) => before_settings.as_ref().is_some_and(
                            |(route, search, input, editing)| {
                                state.route == *route
                                    && state.search == *search
                                    && state.search_input == *input
                                    && state.editing_field == *editing
                            },
                        ),
                        _ => unreachable!(),
                    };
            if !unchanged {
                changed.push(format!("{kind:?}/{prompt}: {action_name}"));
            }
        }
        assert!(app.windows[&wid].search.is_none());
        if let Some((id, before)) = document.zip(before_document) {
            let after = app.document_store.snapshot(id).unwrap();
            assert_eq!((after.seq, after.text), (before.seq, before.text));
            assert_eq!(app.document_store.dirty(id), Some(false));
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        let terminal = term_lock(&hidden);
        assert_eq!(
            (
                terminal.content_seq(),
                terminal.cursor().row,
                terminal.cursor().col
            ),
            hidden_before
        );
        drop(terminal);
        assert!(drained(pipe).is_empty(), "{kind:?}/{prompt}: no PTY input");
        drop(app);
        // SAFETY: app_observing_pty created both descriptors for this test.
        // The app's sink borrowed the write end and has been dropped; neither
        // descriptor has been closed, and no reader or writer still uses it.
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }

    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);
    term_lock(&app.front_terminal(wid).unwrap().term).process(b"needle one needle two");
    assert!(app.find_cmd(FindAction::Open).unwrap().open);
    let found = app.find_cmd(FindAction::Type("needle".into())).unwrap();
    assert_eq!(found.query, "needle");
    assert_eq!(found.matches, 2);
    let next = app.find_cmd(FindAction::Step { forward: true }).unwrap();
    assert_ne!(next.current, found.current);
    assert!(!app.find_cmd(FindAction::Cancel).unwrap().open);
    assert!(
        drained(pipe).is_empty(),
        "terminal control stays out of the PTY"
    );
    drop(app);
    // SAFETY: these are this test's still-open pipe descriptors. The app's
    // borrowing sink has been dropped, and no other reader or writer uses them.
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
    assert!(changed.is_empty(), "native state changed: {changed:?}");
}

/// The `find` verb never puts a byte on the PTY: with the bar closed every editing
/// arm is a no-op, so `find type` is no back door to the shell for a read-only
/// edge; with it open the text lands in the query.
#[cfg(unix)]
#[test]
fn typing_into_the_find_bar_never_reaches_the_pty() {
    let (mut app, pipe) = app_observing_pty();
    let wid = WindowId(0);

    let status = app
        .find_cmd(FindAction::Type("rm -rf /".to_string()))
        .expect("a front window");
    assert!(!status.open, "no find bar was opened by typing at one");
    assert!(app.windows[&wid].search.is_none());
    assert_eq!(
        drained(pipe),
        Vec::<u8>::new(),
        "a find type against a CLOSED bar put bytes on the PTY"
    );

    assert!(app.find_cmd(FindAction::Open).expect("front").open);
    let status = app
        .find_cmd(FindAction::Type("needle".to_string()))
        .expect("front");
    assert!(status.open);
    assert_eq!(status.query, "needle");
    assert_eq!(
        app.windows[&wid]
            .search
            .as_ref()
            .expect("find active")
            .query,
        "needle"
    );
    assert_eq!(
        drained(pipe),
        Vec::<u8>::new(),
        "a find type against an OPEN bar put bytes on the PTY"
    );
}

#[cfg(unix)]
#[test]
fn delayed_find_paste_keeps_its_field_across_focus_but_not_close_and_reopen() {
    let (mut app, pipe) = app_observing_pty();
    let a = WindowId(0);
    app.find_requested_in(a);
    app.search_edit_in(a, SearchEdit::Insert("same".to_string()));
    let owner = app.windows[&a]
        .search
        .as_ref()
        .unwrap()
        .paste_lifetime
        .clone();
    assert!(app.complete_search_paste_in(a, owner.clone(), " query".to_string()));
    assert_eq!(app.windows[&a].search.as_ref().unwrap().query, "same query");

    app.search_close_in(a, false);
    assert!(!app.complete_search_paste_in(a, owner.clone(), " closed".to_string()));
    assert!(app.windows[&a].search.is_none());
    app.find_requested_in(a);
    app.search_edit_in(a, SearchEdit::Insert("same query".to_string()));
    assert!(!app.complete_search_paste_in(a, owner, " reopened".to_string()));
    assert_eq!(app.windows[&a].search.as_ref().unwrap().query, "same query");

    let owner = app.windows[&a]
        .search
        .as_ref()
        .unwrap()
        .paste_lifetime
        .clone();
    let session = app.front_terminal(a).unwrap().session;
    let sink = app.pool.get(session).unwrap().ctx.sink.clone();
    let sid = app.next_session_id;
    let b = app.insert_logical_window(crate::stub_session_with_sink(sid, sink), 24, 80);
    app.find_requested_in(b);
    app.search_edit_in(b, SearchEdit::Insert("other".to_string()));
    assert!(app.complete_search_paste_in(a, owner.clone(), " kept".to_string()));
    assert_eq!(app.frontmost_window, Some(b));
    assert_eq!(
        app.windows[&a].search.as_ref().unwrap().query,
        "same query kept"
    );
    assert_eq!(app.windows[&b].search.as_ref().unwrap().query, "other");
    assert!(!app.complete_search_paste_in(b, owner, " wrong window".to_string()));
    assert_eq!(app.windows[&b].search.as_ref().unwrap().query, "other");
    assert!(
        drained(pipe).is_empty(),
        "find paste never writes to either PTY"
    );
    drop(app);
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

#[cfg(unix)]
#[test]
fn delayed_find_paste_does_not_follow_a_replaced_tab_pane_or_window() {
    for replacement in ["tab", "pane", "window"] {
        let (mut app, pipe) = app_observing_pty();
        let a = WindowId(0);
        app.find_requested_in(a);
        app.search_edit_in(a, SearchEdit::Insert("same query".to_string()));
        let owner = app.windows[&a]
            .search
            .as_ref()
            .unwrap()
            .paste_lifetime
            .clone();
        let session = app.front_terminal(a).unwrap().session;
        let sink = app.pool.get(session).unwrap().ctx.sink.clone();
        let sid = app.next_session_id;
        let target = match replacement {
            "tab" => {
                app.next_session_id += 1;
                app.install_tab_session(a, crate::stub_session_with_sink(sid, sink), None);
                a
            }
            "pane" => {
                app.install_split_pane(
                    a,
                    crate::pane::SplitDir::Vertical,
                    crate::stub_session_with_sink(sid, sink),
                )
                .unwrap();
                a
            }
            _ => {
                app.close_window_logical(a);
                app.insert_logical_window(crate::stub_session_with_sink(sid, sink), 24, 80)
            }
        };
        assert!(app.windows.get(&a).is_none_or(|ws| ws.search.is_none()));
        app.find_requested_in(target);
        app.search_edit_in(target, SearchEdit::Insert("same query".to_string()));
        assert!(app.front_terminal(target).unwrap().session != session);
        assert!(!app.complete_search_paste_in(a, owner, " late".to_string()));
        assert_eq!(
            app.windows[&target].search.as_ref().unwrap().query,
            "same query",
            "{replacement}"
        );
        assert!(drained(pipe).is_empty(), "{replacement}: no PTY input");
        drop(app);
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }
}

/// The verb runs the field editor the keystrokes run, and reports the query the
/// bar holds.
#[test]
fn find_key_edits_run_the_field_editor_the_keystrokes_run() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.find_cmd(FindAction::Open).expect("front").open);
    app.find_cmd(FindAction::Type("hello world".to_string()));
    let status = app
        .find_cmd(FindAction::Edit(SearchEdit::DeleteWordBack))
        .expect("front");
    assert_eq!(status.query, "hello ");
    app.find_cmd(FindAction::Edit(SearchEdit::MoveStart));
    let status = app
        .find_cmd(FindAction::Edit(SearchEdit::KillToEnd))
        .expect("front");
    assert_eq!(status.query, "");
    assert_eq!(
        app.windows[&wid]
            .search
            .as_ref()
            .expect("find active")
            .query,
        "",
        "the wire and the bar report one query, not two"
    );
}

/// `case`/`regex` toggle, as their keystrokes do, and the reply says which way the
/// flag went; `accept`/`cancel` close the bar, so the reply is `open=0`.
#[test]
fn the_find_toggles_report_the_flag_they_left_behind() {
    let mut app = App::headless_for_test();
    assert!(app.find_cmd(FindAction::Open).expect("front").open);
    let before = app.find_cmd(FindAction::Status).expect("front");
    let after = app.find_cmd(FindAction::ToggleCase).expect("front");
    assert_eq!(after.case_sensitive, !before.case_sensitive);
    let after_regex = app.find_cmd(FindAction::ToggleRegex).expect("front");
    assert_eq!(after_regex.is_regex, !before.is_regex);

    assert!(!app.find_cmd(FindAction::Cancel).expect("front").open);
    assert!(
        !app.find_cmd(FindAction::Step { forward: true })
            .expect("front")
            .open
    );
}

/// A `SearchState` seeded with three matches (scrollback row -1, live rows 0/1),
/// for driving `step`/`current_match` headlessly.
fn seeded() -> SearchState {
    SearchState {
        query: "foo".to_string(),
        matches: vec![(-1, 0, 2), (0, 2, 4), (0, 12, 14)],
        ..Default::default()
    }
}

#[test]
fn native_front_never_arms_or_traps_host_terminal_search() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));

    app.search_enter();
    assert!(
        app.windows[&wid].search.is_none(),
        "direct terminal-search entry fails closed on native content"
    );

    app.windows.get_mut(&wid).unwrap().search = Some(SearchState::default());
    app.search_exit();
    assert!(
        app.windows[&wid].search.is_none(),
        "close always clears stale host search even without a front terminal"
    );
}

/// Native-title ownership: find outranks the continuously refreshed canonical
/// title.
#[test]
fn native_title_override_transitions_preserve_latest_canonical_title() {
    use crate::app_window::{WindowTitleAuthority, window_title_authority};

    assert_eq!(
        window_title_authority(false),
        WindowTitleAuthority::Canonical
    );
    assert_eq!(window_title_authority(true), WindowTitleAuthority::Search);

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().current_title = "build — compiling workspace".to_string();
    app.search_enter();
    let search_status = app.windows[&wid]
        .search
        .as_ref()
        .expect("find active")
        .window_title();

    app.windows.get_mut(&wid).unwrap().current_title = "build — tests are passing".to_string();
    assert_eq!(
        window_title_authority(app.windows[&wid].search.is_some()),
        WindowTitleAuthority::Search
    );
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().window_title(),
        search_status,
        "a canonical title refresh cannot mutate the find status"
    );

    app.search_close(true);
    assert!(app.windows[&wid].search.is_none());
    assert_eq!(
        app.windows[&wid].current_title, "build — tests are passing",
        "close restores the latest canonical cache, not literal aterm or stale pre-find text"
    );
    assert_eq!(
        window_title_authority(app.windows[&wid].search.is_some()),
        WindowTitleAuthority::Canonical
    );
}

/// An invalid pattern is not a miss: while the history is away for a rewrap,
/// the window title and the spoken status still give the bad-pattern verdict,
/// the same one they give with the history home, never the history-away one.
#[test]
fn an_invalid_pattern_outranks_the_history_being_away() {
    let pattern = |regex_error, history_away| SearchState {
        query: "(".to_string(),
        regex_error,
        history_away,
        ..Default::default()
    };
    let (home, away, miss) = (
        pattern(true, false),
        pattern(true, true),
        pattern(false, true),
    );
    assert_eq!(away.window_title(), home.window_title());
    assert_eq!(away.spoken_status(), home.spoken_status());
    assert!(!away.window_title().contains(super::HISTORY_AWAY_NONE));
    assert_ne!(
        away.window_title(),
        miss.window_title(),
        "non-vacuous: a valid pattern's miss reads otherwise"
    );
}

fn set_query(app: &mut App, wid: WindowId, query: &str) {
    app.windows
        .get_mut(&wid)
        .and_then(|window| window.search.as_mut())
        .expect("active search")
        .set_query(query.into());
}

/// Occurrences of `needle` in the visible rows, read off the grid: the ground
/// truth, independent of every index and cache the search path builds.
#[cfg(test)]
fn visible_occurrences(
    term: &std::sync::Arc<std::sync::Mutex<super::Terminal>>,
    needle: &str,
) -> usize {
    let t = term_lock(term);
    (0..t.rows())
        .map(|row| {
            t.get_line_text(i32::from(row), None)
                .unwrap_or_default()
                .matches(needle)
                .count()
        })
        .sum()
}

fn hits(app: &App, wid: WindowId) -> usize {
    app.windows[&wid]
        .search
        .as_ref()
        .expect("active search")
        .matches
        .len()
}

/// A count the terminal has moved past is never stated as a census: after output
/// the bar and the wire mark it stale, and the next recompute drops the mark.
#[test]
fn output_since_the_count_is_marked_on_the_glass_and_on_the_wire() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"needle one\r\nneedle two\r\nneedle three\r\n");

    app.search_enter();
    set_query(&mut app, wid, "needle");
    app.search_recompute();
    let counted = hits(&app, wid);
    assert_eq!(counted, 3);
    let fresh = app.find_status_in(wid);
    assert!(!fresh.stale, "a just-run count is the present census");
    assert_eq!(
        app.windows[&wid]
            .search
            .as_ref()
            .expect("find")
            .count_qualifier(),
        "",
        "and is painted bare"
    );

    term_lock(&term).process(b"needle four\r\nneedle five\r\n");
    app.search_refresh_for_output(0);
    assert_eq!(
        visible_occurrences(&term, "needle"),
        5,
        "five hits are on the glass"
    );

    let stale = app.find_status_in(wid);
    assert_eq!(
        stale.matches, counted,
        "the batch is deliberately not re-run"
    );
    assert!(
        stale.stale,
        "so the reply says the count is older than the screen"
    );
    assert_eq!(
        app.windows[&wid]
            .search
            .as_ref()
            .expect("find")
            .count_qualifier(),
        "…",
        "and the bar marks it rather than asserting 3/3"
    );

    app.search_recompute();
    assert_eq!(hits(&app, wid), 5);
    let refreshed = app.find_status_in(wid);
    assert!(!refreshed.stale);
    assert_eq!(
        app.windows[&wid]
            .search
            .as_ref()
            .expect("find")
            .count_qualifier(),
        "",
        "an exact count is stated plainly — the marker is not a permanent scar"
    );
}

/// After a `clear` (`ED 2` + `ED 3`: no history left) a rows-grow appends blank rows
/// at the bottom, and find still sees every hit on the glass: the end-to-end check
/// that index reuse keys the retained rows correctly across such a grow.
#[test]
fn growing_the_window_after_a_clear_keeps_the_visible_screen_searchable() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    {
        let mut t = term_lock(&term);
        for line in 0..40 {
            t.process(format!("old {line:02}\r\n").as_bytes());
        }
        t.process(b"\x1b[H\x1b[2J\x1b[3J");
        t.process(b"needle alpha\r\nneedle beta\r\nneedle gamma\r\n");
    }
    let before = visible_occurrences(&term, "needle");
    assert_eq!(before, 3, "fixture: three hits on the glass");

    app.search_enter();
    set_query(&mut app, wid, "needle");
    app.search_recompute();
    assert_eq!(hits(&app, wid), before, "the pre-resize count is exact");

    let (rows, cols) = {
        let t = term_lock(&term);
        (t.rows(), t.cols())
    };
    term_lock(&term).resize(rows.saturating_add(8), cols);
    let after = visible_occurrences(&term, "needle");
    assert_eq!(after, before, "the grow keeps every hit on the glass");

    app.search_recompute();
    assert_eq!(
        hits(&app, wid),
        after,
        "find must see every hit the reader can see after the window grew"
    );
}

/// A shrink that trims a screen-clear's trailing blanks moves the counter with the
/// rows it drops (`Grid::keep_keys_across_bottom_rows`), so no key slides: the deep
/// needle keeps its absolute row and that row holds it.
#[test]
fn shrinking_the_window_after_a_screen_clear_keeps_history_correctly_keyed() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    {
        let mut t = term_lock(&term);
        for line in 0..40 {
            t.process(format!("needle {line:02}\r\n").as_bytes());
        }
        t.process(b"\x1b[H\x1b[2J");
    }
    let deep = "needle 03";
    let absolute_of = |app: &mut App, query: &str| -> Option<i64> {
        set_query(app, wid, query);
        app.search_recompute();
        let search = app.windows[&wid].search.as_ref().expect("find");
        search
            .current_match()
            .map(|(row, _, _)| search.match_base_y + i64::from(row))
    };

    app.search_enter();
    let before = absolute_of(&mut app, deep).expect("the deep needle is in history");

    let (rows, cols) = {
        let t = term_lock(&term);
        (t.rows(), t.cols())
    };
    let oldest_before = term_lock(&term).grid().oldest_absolute_row();
    let history_before = term_lock(&term).grid().scrollback_lines();
    term_lock(&term).resize(rows.saturating_sub(6).max(6), cols);
    assert_eq!(
        term_lock(&term).grid().scrollback_lines(),
        history_before,
        "fixture: the shrink only trimmed blanks — no row was demoted into history"
    );
    assert_eq!(
        term_lock(&term).grid().oldest_absolute_row(),
        oldest_before,
        "the trailing-blank trim slides no key"
    );

    let after = absolute_of(&mut app, deep).expect("the deep needle is still in history");
    assert_eq!(after, before, "the needle keeps its absolute row");
    let text = {
        let t = term_lock(&term);
        let oldest = i64::try_from(t.grid().oldest_absolute_row()).unwrap_or(0);
        let index = usize::try_from(after - oldest).expect("a retained history row");
        t.grid()
            .get_history_line(index)
            .map(|line| line.to_string())
            .unwrap_or_default()
    };
    assert!(
        text.contains(deep),
        "the reported absolute row holds the match: {text:?}"
    );
}

/// A find state holding `query` with the caret at its end — the state typing
/// leaves behind, and the starting point for the editing tests below.
fn field(query: &str) -> SearchState {
    let mut state = SearchState::default();
    state.set_query(query.to_string());
    assert_eq!(
        state.cursor,
        query.len(),
        "typing parks the caret at the end"
    );
    state
}

/// Insertion happens at the caret, and the caret follows the inserted text.
#[test]
fn insert_lands_at_the_caret() {
    let mut s = field("bc");
    assert!(!s.edit(SearchEdit::MoveStart));
    assert_eq!((s.query.as_str(), s.cursor), ("bc", 0));

    assert!(s.edit(SearchEdit::Insert("a".into())));
    assert_eq!((s.query.as_str(), s.cursor), ("abc", 1));
    assert!(!s.edit(SearchEdit::MoveEnd));
    assert!(s.edit(SearchEdit::Insert("d".into())));
    assert_eq!((s.query.as_str(), s.cursor), ("abcd", 4));
}

/// ⎋/⏎/⇥ are commands in find mode: their text (`\u{1b}`, `\r`, `\t`, as winit
/// reports it) never reaches the query, and a multi-line paste is flattened.
#[test]
fn control_characters_never_enter_the_query() {
    let mut s = field("");
    for control in ["\u{1b}", "\r", "\n", "\t", "\u{8}"] {
        assert!(
            !s.edit(SearchEdit::Insert(control.into())),
            "{control:?} is not query text"
        );
        assert_eq!(s.query, "", "{control:?} left the query untouched");
    }
    assert!(s.edit(SearchEdit::Insert("two\nlines\there".into())));
    assert_eq!(s.query, "twolineshere", "a pasted query stays one line");
}

/// Deletions respect the caret and the word boundaries readline uses: ⌫ / ⌦ by
/// character, ^W / ⌥⌫ by word, ^U / ^K to the line edges.
#[test]
fn deletions_respect_caret_and_word_boundaries() {
    let mut s = field("alpha beta");
    assert!(s.edit(SearchEdit::DeleteBack));
    assert_eq!((s.query.as_str(), s.cursor), ("alpha bet", 9));

    assert!(s.edit(SearchEdit::DeleteWordBack));
    assert_eq!((s.query.as_str(), s.cursor), ("alpha ", 6));

    let mut s = field("src/main.rs");
    s.edit(SearchEdit::MoveStart);
    assert!(s.edit(SearchEdit::DeleteForward));
    assert_eq!((s.query.as_str(), s.cursor), ("rc/main.rs", 0));
    assert!(s.edit(SearchEdit::DeleteWordForward));
    assert_eq!(s.query, "/main.rs");

    let mut s = field("keep this");
    s.edit(SearchEdit::MoveWordLeft);
    assert_eq!(s.cursor, 5, "⌥← lands before the last word");
    assert!(s.edit(SearchEdit::KillToEnd));
    assert_eq!((s.query.as_str(), s.cursor), ("keep ", 5));
    assert!(s.edit(SearchEdit::KillToStart));
    assert_eq!((s.query.as_str(), s.cursor), ("", 0));
    assert!(!s.edit(SearchEdit::KillToEnd));
    assert!(!s.edit(SearchEdit::DeleteBack));
    assert!(!s.edit(SearchEdit::DeleteForward));
}

/// Every motion and edit is character-wise on a multi-byte query, and a caret
/// mid-codepoint or past the end is floored/clamped, never a panic or a split.
#[test]
fn editing_is_utf8_safe() {
    let mut s = field("héllo");
    assert!(s.edit(SearchEdit::DeleteBack));
    assert_eq!(s.query, "héll");
    s.edit(SearchEdit::MoveStart);
    s.edit(SearchEdit::MoveCharRight);
    assert_eq!(s.cursor, 1);
    s.edit(SearchEdit::MoveCharRight);
    assert_eq!(s.cursor, 3, "the caret steps OVER the two-byte é");
    assert!(s.edit(SearchEdit::DeleteBack));
    assert_eq!((s.query.as_str(), s.cursor), ("hll", 1));

    let mut s = field("éß");
    s.cursor = 1;
    assert!(s.edit(SearchEdit::Insert("x".into())));
    assert_eq!(
        s.query, "xéß",
        "a mid-codepoint caret floors to the boundary"
    );
    let mut s = field("é");
    s.cursor = 99;
    assert!(s.edit(SearchEdit::DeleteBack));
    assert_eq!(s.query, "");
    let mut s = field("abc");
    assert!(!s.edit(SearchEdit::MoveTo(99)));
    assert_eq!(s.cursor, 3);
}

/// The words ⌥→ then ⌥← find in `text`, as byte ranges.
fn field_words(text: &str) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut at = 0;
    loop {
        let end = next_word_boundary(text, at);
        let start = prev_word_boundary(text, end);
        if end == at || start < at {
            return words;
        }
        words.push((start, end));
        at = end;
    }
}

/// The words a double-click selects in `text`, as byte ranges.
fn clicked_words(text: &str) -> Vec<(usize, usize)> {
    let smart = aterm_core::selection::SmartSelection::new();
    let mut words: Vec<(usize, usize)> = text
        .char_indices()
        .filter_map(|(at, _)| smart.word_boundaries_at(text, at))
        .collect();
    words.dedup();
    words
}

/// ⌥←/⌥→, ^W and ⌥⌫ step over a word whole in every script and stop where a
/// double-click ends it: a mark, a virama or a format control that spells the word
/// (ZWNJ, ZWJ, the Mongolian vowel separator) belongs to it, while an invisible
/// character between two words (a joiner between Latin letters, a bidi override,
/// the word joiner) still separates them.
#[test]
fn word_motions_step_over_whole_words_like_a_double_click() {
    for word in [
        "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}",
        "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
        "\u{0DC1}\u{0DCA}\u{200D}\u{0DBB}\u{0DD3}",
        "\u{0926}\u{0930}\u{094D}\u{200D}\u{092F}\u{093E}",
        "\u{09B0}\u{200D}\u{09CD}\u{09AF}\u{09BE}\u{09AC}",
        "\u{182C}\u{1820}\u{1837}\u{180E}\u{1820}",
        "cafe\u{301}",
    ] {
        let text = format!("x {word} y");
        let end = 2 + word.len();
        assert_eq!(
            field_words(&text),
            [(0, 1), (2, end), (end + 1, end + 2)],
            "{word:?}"
        );
        assert_eq!(field_words(&text), clicked_words(&text), "{word:?}");
        let mut s = field(&format!("x {word}"));
        assert!(s.edit(SearchEdit::DeleteWordBack));
        assert_eq!(s.query, "x ", "⌥⌫ deletes {word:?} whole");
    }
    for text in [
        "foo\u{200D}bar",
        "foo\u{200C}bar",
        "abc\u{202E}def",
        "a\u{2060}b",
        "x\u{200E}y",
        "ab\u{180E}cd",
    ] {
        let split = text.find(|c: char| !c.is_ascii()).unwrap();
        let after = split + text[split..].chars().next().unwrap().len_utf8();
        assert_eq!(
            field_words(text),
            [(0, split), (after, text.len())],
            "{text:?}"
        );
        assert_eq!(field_words(text), clicked_words(text), "{text:?}");
    }
}

/// The caret is painted state, so it is part of the repaint fingerprint.
#[test]
fn fingerprint_tracks_the_caret() {
    let mut s = field("abc");
    let at_end = s.fingerprint();
    s.edit(SearchEdit::MoveStart);
    assert_ne!(s.fingerprint(), at_end, "a caret move must re-present");
    s.edit(SearchEdit::MoveEnd);
    assert_eq!(s.fingerprint(), at_end, "and settle back when it returns");
}

/// `map_matches` turns absolute rows into selection rows (`abs − base_y`, negative
/// = scrollback), exclusive ends inclusive, and sorts into visual order whatever
/// the engine's order.
#[test]
fn map_matches_abs_to_selection_inclusive_sorted() {
    let base_y = 100;
    let ms = vec![
        SearchMatch::new(102, 2, 4),
        SearchMatch::new(99, 0, 3),
        SearchMatch::new(100, 5, 6),
        SearchMatch::new(100, 1, 3),
    ];
    assert_eq!(
        map_matches(&ms, base_y),
        vec![(-1, 0, 2), (0, 1, 2), (0, 5, 5), (2, 2, 3)]
    );
}

/// Out-of-`i32`/`u16`-range rows or columns are dropped, never wrapped, so a
/// pathological absolute row cannot corrupt the selection coordinates.
#[test]
fn map_matches_drops_out_of_range() {
    let ms = vec![SearchMatch::new(0, 0, 1)];
    assert!(map_matches(&ms, i64::from(i32::MAX) + 10).is_empty());
    let ms = vec![
        SearchMatch::new(5, 0, 1),
        SearchMatch::new(usize::MAX, 0, 1),
    ];
    assert_eq!(map_matches(&ms, 5), vec![(0, 0, 0)]);
}

/// Next and previous walk the matches with wraparound at both ends.
#[test]
fn find_state_next_prev_wraparound() {
    let mut s = seeded();
    let n = s.matches.len();
    assert_eq!(n, 3);

    assert_eq!(s.current_match(), Some((-1, 0, 2)));
    s.step(true);
    assert_eq!(s.current_match(), Some((0, 2, 4)));
    s.step(true);
    assert_eq!(s.current_match(), Some((0, 12, 14)));
    s.step(true);
    assert_eq!(s.current, 0);
    assert_eq!(s.current_match(), Some((-1, 0, 2)));
    s.step(false);
    assert_eq!(s.current, n - 1);
    assert_eq!(s.current_match(), Some((0, 12, 14)));
    s.step(false);
    assert_eq!(s.current_match(), Some((0, 2, 4)));
    s.step(false);
    assert_eq!(s.current_match(), Some((-1, 0, 2)));
}

/// With no matches a step is a no-op with no current match, but it still sets the
/// direction.
#[test]
fn find_state_step_no_matches_is_noop() {
    let mut s = SearchState {
        query: "zzz".to_string(),
        ..Default::default()
    };
    assert!(s.matches.is_empty());
    assert_eq!(s.current_match(), None);
    s.step(true);
    assert_eq!(s.current, 0);
    s.step(false);
    assert_eq!(s.current, 0);
    assert_eq!(s.current_match(), None);
    assert_eq!(
        s.direction,
        SearchDirection::Backward,
        "a zero-hit reversal still governs the next incremental query"
    );
}

/// Truncated point navigation: an Emacs point outside the capped batch is
/// selected on its own, never inserted into (or shifting) the batch; a point
/// inside the batch is selected in place.
#[test]
fn truncated_point_install_selects_an_out_of_batch_point_without_touching_the_batch() {
    let mut state = SearchState {
        matches: (0..8).map(|row| (row, 0, 0)).collect(),
        truncated: true,
        ..Default::default()
    };
    let batch = state.matches.clone();
    assert!(!state.install_point_match((100, 0, 0)));
    assert_eq!(state.current_match(), Some((100, 0, 0)));
    assert_eq!(state.matches, batch, "the capped batch is untouched");
    assert!(state.install_point_match((3, 0, 0)));
    assert_eq!(state.current_match(), Some((3, 0, 0)));
    assert_eq!(state.matches, batch);
}

/// End-to-end state semantics over a real terminal: backward initial typing lands
/// on the last hit, reversals and repeats wrap, empty-query recall honors direction,
/// and legacy Cmd-F resets an open search to forward/first-match behavior.
#[test]
fn emacs_direction_reversal_repeat_wrap_empty_recall_and_cmd_f_legacy() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"hit one\r\nhit two\r\nhit three");

    app.search_enter_direction(false);
    set_query(&mut app, wid, "hit");
    app.search_recompute();
    let search = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(search.matches.len(), 3);
    assert_eq!(search.current, 2, "backward typing installs the last hit");
    assert_eq!(search.direction, SearchDirection::Backward);

    app.search_repeat(true);
    let search = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(search.current, 0, "reverse to forward wraps last → first");
    assert_eq!(search.direction, SearchDirection::Forward);
    app.search_repeat(false);
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().current,
        2,
        "reverse to backward wraps first → last"
    );

    app.search_accept();
    app.search_enter_direction(false);
    app.search_repeat(false);
    let recalled = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(recalled.query, "hit");
    assert_eq!(
        recalled.current, 2,
        "backward empty recall lands last directly"
    );

    app.search_enter();
    let legacy = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(legacy.direction, SearchDirection::Forward);
    assert_eq!(legacy.current, 0, "Cmd-F remains forward/first-match");
}

/// Incremental search starts at the content the user is reading, matching
/// Emacs point semantics instead of jumping to a global buffer extreme.
#[test]
fn emacs_search_is_anchored_to_scrolled_viewport_in_both_directions() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    let mut content = String::new();
    for line in 0..100 {
        let marker = if line % 10 == 0 { " NEEDLE" } else { "" };
        content.push_str(&format!("line {line:03}{marker}\r\n"));
    }
    {
        let mut terminal = term_lock(&term);
        terminal.process(content.as_bytes());
        terminal.scroll_display(30);
    }
    let (top, bottom) = {
        let terminal = term_lock(&term);
        let base = i64::try_from(terminal.grid().base_y()).unwrap();
        let top = base - i64::try_from(terminal.grid().display_offset()).unwrap();
        (top, top + i64::from(terminal.rows().saturating_sub(1)))
    };

    app.search_enter_direction(true);
    set_query(&mut app, wid, "NEEDLE");
    app.search_recompute();
    let forward = app.windows[&wid].search.as_ref().unwrap();
    let forward_abs =
        forward.match_base_y + i64::from(forward.current_match().expect("forward hit").0);
    assert!(forward_abs >= top);
    assert!(
        forward.matches[..forward.current]
            .iter()
            .all(|&(row, _, _)| forward.match_base_y + i64::from(row) < top)
    );
    assert!(
        forward.current > 0,
        "fixture must not choose the oldest hit"
    );
    app.search_cancel();

    app.search_enter_direction(false);
    set_query(&mut app, wid, "NEEDLE");
    app.search_recompute();
    let backward = app.windows[&wid].search.as_ref().unwrap();
    let backward_abs =
        backward.match_base_y + i64::from(backward.current_match().expect("backward hit").0);
    assert!(backward_abs <= bottom);
    assert!(
        backward.matches[backward.current + 1..]
            .iter()
            .all(|&(row, _, _)| backward.match_base_y + i64::from(row) > bottom)
    );
    assert!(
        backward.current + 1 < backward.matches.len(),
        "fixture must not choose the newest global hit"
    );
}

/// Streaming output invalidates a batch without a rescan per chunk; the next repeat
/// refreshes once and navigates from the old point, reaching the appended hit.
#[test]
fn streaming_output_marks_search_dirty_then_repeat_refreshes_latest_content() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"hit one");

    app.search_enter_direction(true);
    set_query(&mut app, wid, "hit");
    app.search_recompute();
    let before = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(before.matches.len(), 1);
    let before_seq = before.match_content_seq;
    let before_abs = before.match_base_y + i64::from(before.current_match().unwrap().0);
    assert!(term_lock(&term).text_selection().has_selection());

    term_lock(&term).process(b"\r\nhit two");
    assert_ne!(term_lock(&term).content_seq(), before_seq);
    app.search_refresh_for_output(0);
    assert!(app.windows[&wid].search.as_ref().unwrap().results_dirty);
    assert!(
        before_abs >= i64::try_from(term_lock(&term).grid().base_y()).unwrap(),
        "fixture hit must be on the live screen"
    );
    assert!(!term_lock(&term).text_selection().has_selection());

    app.search_repeat(true);
    let after = app.windows[&wid].search.as_ref().unwrap();
    assert!(!after.results_dirty);
    assert_eq!(after.matches.len(), 2);
    assert_eq!(after.match_content_seq, term_lock(&term).content_seq());
    let after_abs = after.match_base_y + i64::from(after.current_match().unwrap().0);
    assert!(after_abs > before_abs, "repeat must reach the streamed hit");
    assert!(term_lock(&term).text_selection().has_selection());
}

/// Scrollback twin of the test above, pinning the narrowed clear: output that
/// invalidates the matches keeps a highlight on archived rows (`post_process` has
/// already accounted for eviction and renumbering).
#[test]
fn streaming_output_keeps_a_scrollback_highlight_alive() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    {
        let mut terminal = term_lock(&term);
        terminal.process(b"NEEDLE here\r\n");
        for line in 0..200 {
            terminal.process(format!("filler {line:03}\r\n").as_bytes());
        }
    }

    app.search_enter_direction(true);
    set_query(&mut app, wid, "NEEDLE");
    app.search_recompute();
    let hit_abs = {
        let search = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(search.matches.len(), 1);
        search.match_base_y + i64::from(search.current_match().unwrap().0)
    };
    assert!(
        hit_abs < i64::try_from(term_lock(&term).grid().base_y()).unwrap(),
        "fixture hit must be in scrollback"
    );
    let highlighted = term_lock(&term)
        .selection_to_string()
        .expect("the match is highlighted");

    term_lock(&term).process(b"streamed non-match\r\n");
    app.search_refresh_for_output(0);
    assert!(
        app.windows[&wid].search.as_ref().unwrap().results_dirty,
        "the match VECTOR is still refused — only the highlight survives"
    );
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some(highlighted.as_str()),
        "an archived highlight still covers exactly the text it did before"
    );
}

/// ⌘F on a scrolled-back viewport with nothing to go to (empty query, no match)
/// leaves the view alone: positioning by absolute row makes no row mean no move.
#[test]
fn opening_find_with_nothing_to_show_does_not_move_the_viewport() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    {
        let mut terminal = term_lock(&term);
        for line in 0..120 {
            terminal.process(format!("line {line:03}\r\n").as_bytes());
        }
        terminal.scroll_display(17);
    }
    let reading = term_lock(&term).row_text(0).expect("a top visible row");

    app.search_enter();
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        17,
        "a bare ⌘F on an empty query must not move the view"
    );
    set_query(&mut app, wid, "no such text anywhere");
    app.search_recompute();
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().matches.len(),
        0,
        "fixture query must miss"
    );
    assert_eq!(
        term_lock(&term).row_text(0).as_deref(),
        Some(reading.as_str()),
        "a zero-match query leaves the reader where they were"
    );
}

/// ⎋ restores a scrolled-back origin across a protected-footer splice (the
/// codex/Claude Code shape), asserted on row text since the splice moves rows
/// relative to the tail.
#[test]
fn cancel_restores_a_scrolled_back_origin_across_a_footer_splice() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows[&wid].rows;
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    {
        let mut terminal = term_lock(&term);
        for line in 0..120 {
            terminal.process(format!("hit {line:03}\r\n").as_bytes());
        }
        terminal.scroll_display(23);
    }
    let reading = term_lock(&term).row_text(0).expect("a top visible row");

    app.search_enter();
    set_query(&mut app, wid, "hit 004");
    app.search_recompute();
    assert_ne!(
        term_lock(&term).grid().display_offset(),
        23,
        "find navigation must have moved the viewport away from the origin"
    );

    let region_bottom = rows - 2;
    term_lock(&term).process(
        format!("\x1b[1;1HA\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes(),
    );
    assert_eq!(term_lock(&term).absolute_row_revision(), 1);

    app.search_cancel();
    assert_eq!(
        term_lock(&term).row_text(0).as_deref(),
        Some(reading.as_str()),
        "⎋ puts the reader back on the LINE they were reading, splice or no splice"
    );
}

/// A tab/pane switch is not a find exit: the neutral close hands the title back and
/// parks the origin, so a reopen and ⎋ land on what the user was reading before
/// the interrupted find.
#[test]
fn a_neutral_close_parks_the_find_origin_for_the_next_open() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    {
        let mut terminal = term_lock(&term);
        for line in 0..120 {
            terminal.process(format!("hit {line:03}\r\n").as_bytes());
        }
        terminal.scroll_display(31);
    }
    let reading = term_lock(&term).row_text(0).expect("a top visible row");

    app.search_enter();
    set_query(&mut app, wid, "hit 007");
    app.search_recompute();
    assert_ne!(term_lock(&term).grid().display_offset(), 31);

    app.search_close_in(wid, false);
    assert!(app.windows[&wid].search.is_none(), "the bar is gone");
    assert!(
        app.find_origins.contains_key(&(0, wid)),
        "but the origin is parked, not discarded"
    );

    app.search_enter();
    app.search_cancel();
    assert_eq!(
        term_lock(&term).row_text(0).as_deref(),
        Some(reading.as_str()),
        "the reopened find cancels home, not to where the interrupted one stopped"
    );
    assert!(
        !app.find_origins.contains_key(&(0, wid)),
        "⎋ is a user exit and takes the parked origin with it"
    );
}

#[test]
fn query_edit_after_repeat_stays_at_emacs_point() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"target one\r\ntarget two\r\ntarget three");

    app.search_enter_direction(false);
    set_query(&mut app, wid, "tar");
    app.search_recompute();
    app.search_repeat(false);
    let anchored = {
        let search = app.windows[&wid].search.as_ref().unwrap();
        assert_eq!(search.current, 1);
        search.match_base_y + i64::from(search.current_match().unwrap().0)
    };

    set_query(&mut app, wid, "target");
    app.search_recompute();
    let edited = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(edited.current, 1, "editing must not snap back to newest");
    assert_eq!(
        edited.match_base_y + i64::from(edited.current_match().unwrap().0),
        anchored
    );
}

#[test]
fn resize_without_output_wake_cannot_step_or_accept_stale_results() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"hit one\r\nhit two\r\nhit three");
    app.search_enter_direction(true);
    set_query(&mut app, wid, "hit");
    app.search_recompute();

    let (rows, cols, old_seq) = {
        let terminal = term_lock(&term);
        (terminal.rows(), terminal.cols(), terminal.content_seq())
    };
    term_lock(&term).resize(rows, cols.saturating_sub(1).max(1));
    assert_ne!(term_lock(&term).content_seq(), old_seq);
    app.search_step(true);
    let refreshed = app.windows[&wid].search.as_ref().unwrap();
    assert!(!refreshed.results_dirty);
    assert_eq!(refreshed.match_content_seq, term_lock(&term).content_seq());

    let current_cols = term_lock(&term).cols();
    term_lock(&term).resize(rows, current_cols.saturating_sub(1).max(1));
    app.search_accept();
    assert!(app.windows[&wid].search.is_none());
    assert!(term_lock(&term).text_selection().has_selection());
}

/// A fault injected into the real app right after a real step, named by
/// the model action that step ends on.
#[cfg(unix)]
type NavPatch = fn(&mut App, WindowId, &str);

/// The shipped host, untouched.
#[cfg(unix)]
fn no_nav_patch(_: &mut App, _: WindowId, _: &str) {}

/// `EmacsSearchNavigation`'s `Buggy = 1`, one member per law, each as the
/// net effect it would have on the real app at its own action.
#[cfg(unix)]
fn nav_member(member: &str, app: &mut App, wid: WindowId, action: &str) {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let term = app.front_terminal(wid).expect("terminal").term.clone();
    match (member, action) {
        ("leak", "OpenBackward") => {
            let sink = app.windows[&wid]
                .active_terminal
                .as_ref()
                .expect("session")
                .sink
                .clone();
            sink.write_frame(b"\x12")
                .expect("the observer pipe accepts a byte");
        }
        ("past the end", "RepeatForward") => {
            let search = app.windows.get_mut(&wid).unwrap().search.as_mut().unwrap();
            if search.current == 0 {
                search.current = search.matches.len();
            }
        }
        ("stale selection", "Output") => {
            let mut terminal = term_lock(&term);
            let sel = terminal.text_selection_mut();
            sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 3, SelectionSide::Right);
            sel.complete_selection();
        }
        ("viewport kept", "Cancel") => term_lock(&term).scroll_to_bottom(),
        ("hit dropped", "Accept") => term_lock(&term).text_selection_mut().clear(),
        _ => {}
    }
}

/// One run of the navigation trace: the real app, the model state it must
/// match, and every PTY byte the observer pipe has seen so far.
#[cfg(unix)]
struct NavRun<'m> {
    app: App,
    wid: WindowId,
    pipe: [i32; 2],
    pty_bytes: usize,
    model: &'m aterm_spec::derive::Model,
    state: aterm_spec::interp::State,
    patch: NavPatch,
}

#[cfg(unix)]
impl NavRun<'_> {
    /// Drive one real step, inject the patch, advance the model through
    /// `actions`, and require every projected variable to be the model's.
    fn step(
        &mut self,
        actions: &[&'static str],
        drive: impl FnOnce(&mut App, WindowId),
    ) -> Result<(), String> {
        let action = *actions.last().expect("a step is at least one action");
        drive(&mut self.app, self.wid);
        (self.patch)(&mut self.app, self.wid, action);
        for &each in actions {
            let next = self.model.successors(each, &self.state);
            let [next] = next.as_slice() else {
                return Err(format!(
                    "the model's {each} is not one successor from {:?}: {next:?}",
                    self.state
                ));
            };
            self.state = next.clone();
        }
        self.pty_bytes += drained(self.pipe).len();
        let term = self
            .app
            .front_terminal(self.wid)
            .expect("terminal")
            .term
            .clone();
        let mut real = vec![
            (
                "active",
                i64::from(self.app.windows[&self.wid].search.is_some()),
            ),
            (
                "selection",
                i64::from(term_lock(&term).text_selection().has_selection()),
            ),
            ("pty_writes", i64::from(self.pty_bytes > 0)),
        ];
        if let Some(search) = self.app.windows[&self.wid].search.as_ref() {
            real.extend([
                ("hits", i64::try_from(search.matches.len()).unwrap()),
                ("current", i64::try_from(search.current).unwrap()),
                (
                    "forward",
                    i64::from(search.direction == SearchDirection::Forward),
                ),
                ("dirty", i64::from(search.results_dirty)),
            ]);
        }
        for (var, value) in real {
            if self.state[var] != value {
                return Err(format!(
                    "real {action}: {var} = {value}, the model's {action} says {} ({:?})",
                    self.state[var], self.state
                ));
            }
        }
        Ok(())
    }
}

/// Drive the navigation trace on a real headless App whose PTY is an observer
/// pipe, with `patch` injected after every real step.
#[cfg(unix)]
fn emacs_navigation_trace(
    model: &aterm_spec::derive::Model,
    patch: NavPatch,
) -> Result<(), String> {
    use aterm_window::keyboard::{KeyCode, PhysicalKey};

    let (app, pipe) = app_observing_pty();
    let mut run = NavRun {
        app,
        wid: WindowId(0),
        pipe,
        pty_bytes: 0,
        model,
        state: model.init_state(),
        patch,
    };
    let term = run
        .app
        .front_terminal(run.wid)
        .expect("terminal")
        .term
        .clone();
    let mut content = String::from("\x1b[>2u");
    for line in 0..80 {
        let marker = if matches!(line, 7 | 39 | 71) {
            " NEEDLE"
        } else {
            ""
        };
        content.push_str(&format!("line {line:02}{marker}\r\n"));
    }
    {
        let mut terminal = term_lock(&term);
        terminal.process(content.as_bytes());
        terminal.scroll_display(9);
    }
    assert!(
        term_lock(&term).grid().display_offset() > 0,
        "the fixture must start away from the live bottom"
    );
    let chord = |forward: bool| {
        let key = PhysicalKey::Code(if forward {
            KeyCode::KeyS
        } else {
            KeyCode::KeyR
        });
        move |app: &mut App, wid: WindowId| {
            app.terminal_emacs_search_pressed(wid, key, forward);
            app.release_physical_press(wid, key);
        }
    };
    let publish = |app: &mut App, wid: WindowId| {
        set_query(app, wid, "NEEDLE");
        app.search_recompute();
    };
    const THREE_HITS: [&str; 3] = ["PublishHit"; 3];

    run.step(&["OpenBackward"], chord(false))?;
    run.step(&THREE_HITS, publish)?;
    run.step(&["RepeatForward"], chord(true))?;
    run.step(&["RepeatBackward"], chord(false))?;
    run.step(&["Output"], |app, _| {
        term_lock(&term).process(b"\r\nstreamed non-match");
        app.search_refresh_for_output(0);
    })?;
    run.step(&["RefreshRepeatForward"], chord(true))?;

    let origin_offset = {
        let origin = run
            .app
            .find_origins
            .get(&(0, run.wid))
            .expect("parked find origin");
        let base_now = i64::try_from(term_lock(&term).grid().base_y()).unwrap();
        usize::try_from(base_now - i64::try_from(origin.top_visible_absolute_row).unwrap()).unwrap()
    };
    run.step(&["Cancel"], |app, _| app.search_cancel())?;
    let restored = term_lock(&term).grid().display_offset() == origin_offset;
    if restored != (run.state["viewport"] == run.state["origin"]) {
        return Err(format!(
            "real Cancel: viewport restored = {restored}, the model's Cancel says {} ({:?})",
            run.state["viewport"] == run.state["origin"],
            run.state
        ));
    }

    run.step(&["OpenBackward"], chord(false))?;
    run.step(&THREE_HITS, publish)?;
    run.step(&["Accept"], |app, _| app.search_accept())?;

    unsafe {
        libc::close(run.pipe[0]);
        libc::close(run.pipe[1]);
    }
    Ok(())
}

/// Tier-1 conformance for the derived `EmacsSearchNavigation` model: the trace
/// drives the real chord seam, search, output refresh and ⎋/⏎ exits on a headless
/// App, and after every step requires the real find state, selection and PTY
/// observer (and the viewport restore at ⎋) to be the model's.
#[cfg(unix)]
#[test]
fn emacs_navigation_conforms_to_derived_transition_model() {
    let model = aterm_models_render::emacs_search_navigation_model();
    emacs_navigation_trace(&model, no_nav_patch).unwrap_or_else(|why| panic!("{why}"));

    let members: [(&str, NavPatch, &str); 5] = [
        (
            "leak",
            |a, w, x| nav_member("leak", a, w, x),
            "OpenBackward",
        ),
        (
            "past the end",
            |a, w, x| nav_member("past the end", a, w, x),
            "RepeatForward",
        ),
        (
            "stale selection",
            |a, w, x| nav_member("stale selection", a, w, x),
            "Output",
        ),
        (
            "viewport kept",
            |a, w, x| nav_member("viewport kept", a, w, x),
            "Cancel",
        ),
        (
            "hit dropped",
            |a, w, x| nav_member("hit dropped", a, w, x),
            "Accept",
        ),
    ];
    for (member, patch, at) in members {
        let Err(refused) = emacs_navigation_trace(&model, patch) else {
            panic!("the healthy model must refuse the {member} member");
        };
        assert!(
            refused.contains(&format!("the model's {at} ")),
            "the {member} member must be refused at {at}: {refused}"
        );
    }
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let shipped = emacs_navigation_trace(&buggy, no_nav_patch)
        .expect_err("the shipped host must not conform to the Buggy = 1 model");
    assert!(shipped.contains("the model's"), "{shipped}");
}

/// Codex-style protected footer refreshes rebuild absolute coordinates without
/// losing reverse-search direction: after the piecewise insertion the last footer
/// hit is still current, never a stale or transient first hit.
#[test]
fn codex_protected_footer_refresh_preserves_backward_last_match() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows[&wid].rows;
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term)
        .process(format!("\x1b[{};1HFOOTERNEEDLE\x1b[{rows};1HFOOTERNEEDLE", rows - 1).as_bytes());

    app.search_enter_direction(false);
    set_query(&mut app, wid, "FOOTERNEEDLE");
    app.search_recompute();
    let before = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(before.matches.len(), 2);
    assert_eq!(before.current, 1);
    assert_eq!(before.match_absolute_row_revision, 0);

    let region_bottom = rows - 2;
    term_lock(&term).process(
        format!("\x1b[1;1HA\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes(),
    );
    assert_eq!(term_lock(&term).absolute_row_revision(), 1);
    app.search_refresh_for_output(0);

    let invalidated = app.windows[&wid].search.as_ref().unwrap();
    assert!(invalidated.results_dirty);
    assert_eq!(invalidated.match_absolute_row_revision, 0);
    assert!(!term_lock(&term).text_selection().has_selection());
    app.search_repeat(false);

    let after = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(after.direction, SearchDirection::Backward);
    assert_eq!(after.current, after.matches.len() - 1);
    assert_eq!(after.match_absolute_row_revision, 1);
    let (row, _, _) = after.current_match().expect("last footer hit");
    assert_eq!(after.match_base_y + i64::from(row), i64::from(rows));
}

/// After a protected-footer revision a forward repeat restarts inclusively at the
/// true beginning: the before-first sentinel must not clamp to `(0, 0)` under a
/// strict comparison and skip the first retained hit.
#[test]
fn codex_protected_footer_forward_refresh_keeps_row_zero_match() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows[&wid].rows;
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"EDGE\r\nEDGE");

    app.search_enter_direction(false);
    set_query(&mut app, wid, "EDGE");
    app.search_recompute();
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().matches.len(), 2);

    let region_bottom = rows - 2;
    term_lock(&term)
        .process(format!("\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes());
    assert_eq!(term_lock(&term).absolute_row_revision(), 1);
    app.search_refresh_for_output(0);
    app.search_repeat(true);

    let after = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(after.direction, SearchDirection::Forward);
    assert!(!after.results_dirty);
    assert_eq!(after.match_absolute_row_revision, 1);
    assert!(after.matches.len() >= 2);
    let (row, col, _) = after.current_match().expect("first retained hit");
    assert_eq!(after.match_base_y + i64::from(row), 0);
    assert_eq!(col, 0);
}

/// Claude's classic in-place UI and alternate-screen UI both search the currently
/// active grid.
#[test]
fn claude_classic_and_alternate_active_grid_search_routing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"CLASSIC_NEEDLE\r\nCLASSIC_NEEDLE");

    app.search_enter_direction(false);
    set_query(&mut app, wid, "CLASSIC_NEEDLE");
    app.search_recompute();
    let classic = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(classic.matches.len(), 2);
    assert_eq!(classic.current, 1);
    app.search_cancel();

    term_lock(&term).process(b"\x1b[?1049h\x1b[HALT_NEEDLE\r\nALT_NEEDLE");
    app.search_enter_direction(false);
    set_query(&mut app, wid, "ALT_NEEDLE");
    app.search_recompute();
    let alternate = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(alternate.matches.len(), 2);
    assert_eq!(alternate.current, 1);

    set_query(&mut app, wid, "CLASSIC_NEEDLE");
    app.search_recompute();
    assert!(
        app.windows[&wid]
            .search
            .as_ref()
            .unwrap()
            .matches
            .is_empty(),
        "parked classic grid cannot leak into active alternate-grid search"
    );
    app.search_cancel();

    term_lock(&term).process(b"\x1b[?1049l");
    app.search_enter();
    set_query(&mut app, wid, "CLASSIC_NEEDLE");
    app.search_recompute();
    let restored = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(restored.matches.len(), 2);
    assert_eq!(restored.current, 0);
}

/// Press custody: a search jump selects with no gesture around it, so it records
/// the `UserSelect` itself.
#[test]
fn a_search_jump_records_who_took_the_selection() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let Some(terminal) = app.front_terminal(wid).map(|t| t.term.clone()) else {
        panic!("headless app must front a terminal");
    };
    {
        let mut t = crate::term_lock(&terminal);
        for i in 0..6 {
            t.process(format!("needle{i} tail\r\n").as_bytes());
        }
    }

    let (base_y, revision, seq) = {
        let t = crate::term_lock(&terminal);
        (
            t.grid().base_y() as i64,
            t.absolute_row_revision(),
            t.content_seq(),
        )
    };
    let st = SearchState {
        query: "needle".to_string(),
        matches: vec![(1, 0, 5)],
        current: 0,
        match_base_y: base_y,
        match_absolute_row_revision: revision,
        match_content_seq: seq,
        ..SearchState::default()
    };
    app.windows.get_mut(&wid).unwrap().search = Some(st);

    app.search_apply_current_in(wid);

    let t = crate::term_lock(&terminal);
    assert!(
        t.text_selection().has_selection(),
        "precondition: the jump really did select the hit"
    );
    assert_eq!(
        t.last_custody_transition(),
        Some(aterm_core::terminal::CustodyTransition::UserSelect),
        "the search jump is a UserSelect producer and must say so — without this \
         the `custody` verb answers a stale transition beside a changed selection"
    );
}

#[test]
fn search_refresh_for_output_invalidates_open_find_in_non_frontmost_window() {
    let mut app = App::headless_for_test();
    let w0 = WindowId(0);
    let w1 = WindowId(1);
    app.install_window_state(w1, crate::stub_session(99), 24, 80);
    let s0 = app.front_terminal(w0).expect("w0 terminal").session;
    let term0 = app.front_terminal(w0).expect("w0 terminal").term.clone();
    crate::term_lock(&term0).process(b"needle line\r\n");

    app.find_requested_in(w0);
    set_query(&mut app, w0, "needle");
    app.search_recompute_in(w0);
    assert_eq!(
        app.windows[&w0].search.as_ref().map(|s| s.matches.len()),
        Some(1)
    );
    assert!(
        !app.windows[&w0]
            .search
            .as_ref()
            .expect("search")
            .results_dirty
    );
    assert!(crate::term_lock(&term0).text_selection().has_selection());

    app.frontmost_window = Some(w1);
    crate::term_lock(&term0).process(b"more output\r\n");
    app.search_refresh_for_output(s0);

    let search = app.windows[&w0].search.as_ref().expect("w0 search open");
    assert!(
        search.results_dirty,
        "output on w0's session must mark w0's open find dirty even when w1 is frontmost"
    );
    assert!(
        !crate::term_lock(&term0).text_selection().has_selection(),
        "stale match selection in w0 must be cleared when output advances row revision"
    );
}
