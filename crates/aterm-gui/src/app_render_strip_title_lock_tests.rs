// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, term_lock};
use std::sync::mpsc;
use std::time::Duration;

/// `refill_strip_titles` try-locks each tab's terminal: on contention a slot
/// keeps its previous title (`"aterm"` for a never-filled slot) and converges
/// once the lock is free.
#[test]
fn strip_titles_keep_stale_on_contention() {
    let app = App::headless_for_test();
    let wid = WindowId(0);
    // Set the lone session's title through the real OSC 2 path.
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\x1b]2;hello\x07");

    // Uncontended: the slot reads the live title.
    let mut titles = Vec::new();
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["hello".to_string()]);

    // Contended (`try_lock` from the holding thread reports `WouldBlock`,
    // like a mid-parse PTY reader): a warm slot keeps its stale title, a
    // fresh slot falls back to "aterm", and neither read blocks.
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

    // Lock released: the next refill converges to the live title.
    term_lock(&term).process(b"\x1b]2;world\x07");
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["world".to_string()]);
}

/// A busy session-metadata lock does not delay the strip refill or partially
/// recompose the previous label.
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
        // A second thread holds the metadata lock, so a blocking `.lock()`
        // regression costs a bounded hold rather than hanging the suite. Both
        // waits are one-minute hang detectors.
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

/// The per-frame label is served from the per-session presented-label cache:
/// a clean title presents nothing at all, an unchanged title that needs the
/// sanitizer is a cache hit, and a title change presents exactly once.
#[test]
fn strip_refill_serves_unchanged_labels_from_the_label_cache() {
    let app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\x1b]2;build shell\x07");

    // A presentation-clean title: the fast path keeps the raw slot verbatim
    // and presents nothing at all.
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

    // A title the sanitizer must see (a program's spinner glyph and a doubled
    // space): presented once, then every unchanged frame is a cache hit.
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

    // A title change presents exactly once.
    term_lock(&term).process("\x1b]2;✳ linker\x07".as_bytes());
    app.refill_strip_titles(wid, &mut titles);
    assert_eq!(titles, vec!["✳ linker".to_string()]);
    assert_eq!(app.chrome_labels.runs(), warm + 1);
}

/// The native toolbar strip path ([`crate::App::tab_titles`], run by
/// `refresh_window_tabs` on every tab mutation including a switch) uses the
/// same try-lock keep-stale rule and never blocks.
#[test]
fn tab_titles_keep_stale_on_contention_and_never_block() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"\x1b]2;hello\x07");

    // Uncontended: the live title, and the keep-stale cache warms.
    assert_eq!(app.tab_titles(wid), vec!["hello".to_string()]);

    {
        let _busy = term.lock().unwrap();
        // Contended with a warm cache: the last-read title, no block.
        assert_eq!(
            app.tab_titles(wid),
            vec!["hello".to_string()],
            "warm cache keeps the last-read title under contention"
        );
        // Contended with a cold cache: the presentation fallback, no block.
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.tab_title_cache.clear();
        }
        assert_eq!(
            app.tab_titles(wid),
            vec!["aterm".to_string()],
            "cold cache shows the fallback under contention"
        );
    }

    // Lock released: converges to the live title again.
    term_lock(&term).process(b"\x1b]2;world\x07");
    assert_eq!(app.tab_titles(wid), vec!["world".to_string()]);
}
