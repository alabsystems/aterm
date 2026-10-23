// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, term_lock};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn strip_titles_keep_stale_on_contention() {
    let app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\x1b]2;hello\x07");

    let mut titles = Vec::new();
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["hello".to_string()]);

    {
        let _busy = term.lock().unwrap();
        app.refill_strip_titles(wid, &mut titles);
        assert_eq!(
            titles,
            vec!["hello".to_string()],
            "warm slot keeps the last-read title under contention"
        );
        let mut fresh = Vec::new();
        app.refill_strip_titles(wid, &mut fresh);
        assert_eq!(
            fresh,
            vec!["aterm".to_string()],
            "fresh slot shows the default under contention"
        );
    }

    term_lock(&term).process(b"\x1b]2;world\x07");
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["world".to_string()]);
}

#[test]
fn strip_titles_keep_stale_when_session_metadata_is_busy() {
    let app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.pool.get(0).expect("session 0");
    let mut titles = Vec::new();

    assert_eq!(
        session
            .ctx
            .meta
            .lock()
            .unwrap()
            .set("title", Some("first title".to_string())),
        Some(true)
    );
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["first title".to_string()]);

    std::thread::scope(|scope| {
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let ctx = session.ctx.clone();
        let holder = scope.spawn(move || {
            let mut busy = ctx.meta.lock().unwrap();
            assert_eq!(
                busy.set("title", Some("next title".to_string())),
                Some(true)
            );
            held_tx.send(()).expect("announce held metadata lock");
            release_rx.recv_timeout(Duration::from_secs(60)).is_ok()
        });
        held_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("metadata holder started");

        app.refill_strip_titles(wid, &mut titles);
        assert_eq!(titles, vec!["first title".to_string()]);
        let mut fresh = Vec::new();
        app.refill_strip_titles(wid, &mut fresh);
        assert_eq!(fresh, vec!["aterm".to_string()]);
        release_tx.send(()).expect("release metadata holder");
        assert!(
            holder.join().expect("metadata holder"),
            "refill must not wait for the metadata lock timeout"
        );
    });

    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["next title".to_string()]);
}

#[test]
fn strip_refill_serves_unchanged_labels_from_the_label_cache() {
    let app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\x1b]2;build shell\x07");

    let mut titles = Vec::new();
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["build shell".to_string()]);
    let fast = app.chrome_labels.runs();
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(
        app.chrome_labels.runs(),
        fast,
        "a clean title must not be presented"
    );

    term_lock(&term).process("\x1b]2;✳ build  shell\x07".as_bytes());
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["✳ build shell".to_string()]);
    let warm = app.chrome_labels.runs();
    assert_eq!(warm, fast + 1);
    app.refill_strip_titles(wid, &mut titles);
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["✳ build shell".to_string()]);
    assert_eq!(
        app.chrome_labels.runs(),
        warm,
        "unchanged title frames must be cache hits"
    );

    term_lock(&term).process("\x1b]2;✳ linker\x07".as_bytes());
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["✳ linker".to_string()]);
    assert_eq!(app.chrome_labels.runs(), warm + 1);
}

#[test]
fn tab_titles_keep_stale_on_contention_and_never_block() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\x1b]2;hello\x07");

    assert_eq!(app.tab_titles(wid), vec!["hello".to_string()]);

    {
        let _busy = term.lock().unwrap();
        assert_eq!(
            app.tab_titles(wid),
            vec!["hello".to_string()],
            "warm cache keeps the last-read title under contention"
        );
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.tab_title_cache.clear();
        }
        assert_eq!(
            app.tab_titles(wid),
            vec!["aterm".to_string()],
            "cold cache shows the fallback under contention"
        );
    }

    term_lock(&term).process(b"\x1b]2;world\x07");
    assert_eq!(app.tab_titles(wid), vec!["world".to_string()]);
}

#[test]
fn a_contended_timeline_serves_the_cached_clause_and_retries() {
    use crate::session_log::clause_tests::{clause_app, open_step, open_turn};
    let (mut app, timeline) = clause_app(true);
    let wid = WindowId(0);
    let turn = open_turn(&timeline);
    let step = open_step(&timeline, turn, "shell");
    app.refresh_window_tabs(wid);
    let cached = &app.session_chrome[&0];
    assert!(
        cached
            .ext
            .tooltip
            .as_deref()
            .is_some_and(|t| t.contains("now: running shell")),
        "{:?}",
        cached.ext
    );
    let cached = cached.clause.clone().zip(cached.clause_line.clone());
    assert!(cached.is_some(), "the clause is cached");
    {
        let mut g = timeline.lock().unwrap();
        let now = g.slog().now();
        let log = g.slog_mut().log_mut_for_journal();
        log.close(
            step,
            aterm_slog::Close::new(
                aterm_slog::Out::Step(aterm_slog::StepOut::Ok),
                aterm_slog::Src::Transcript,
                aterm_slog::Conf::Exact,
            ),
            now,
        )
        .unwrap();
    }
    open_step(&timeline, turn, "edit");
    {
        let _held = timeline.lock().unwrap();
        let served = app.chrome_clause_read(wid, 0, &timeline, crate::turn_ledger::now_ms());
        assert_eq!(
            served, cached,
            "contention serves the clause the cache holds"
        );
        assert!(app.session_chrome_retry.contains(&0));
        assert!(app.title_drift.pending.contains(&0));
        assert!(!app.windows[&wid].tab_title_epochs.contains_key(&0));
    }
    assert!(app.flush_title_drift(0, std::time::Instant::now()));
    assert!(!app.session_chrome_retry.contains(&0));
    assert!(
        app.session_chrome[&0]
            .ext
            .tooltip
            .as_deref()
            .is_some_and(|t| t.contains("now: editing")),
        "the retry recomposes the moved clause: {:?}",
        app.session_chrome[&0].ext
    );
}
