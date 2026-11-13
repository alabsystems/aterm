// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// Shapes the detector must flag, each with at least one finding.
const MUST_FIRE: &[(&str, &str)] = &[
    (
        "the original un-fixed cross_resize: the whole reflow on the held guard",
        r##"
fn cross_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    term_lock(term).resize(rows, cols);
}
"##,
    ),
    (
        "one zero-arg accessor between the guard and the work",
        r##"
fn cross_resize_hop(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock(term);
    t.grid_mut().resize(rows, cols);
}
"##,
    ),
    (
        "alias and work on ONE line: the binding registers before the hit test",
        r##"
fn hydrate_same_line(term: &Mutex<Terminal>, rows: usize) {
    let mut t = term_lock(term);
    let u = &mut *t; u.hydrate(rows);
}
"##,
    ),
    (
        "a `{` inside a test-module string must not hold the mask open past it",
        r##"
#[cfg(test)]
mod tests {
    #[test]
    fn reports_progress() {
        emit("rewrap {of the scrollback");
    }
}

fn cross_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    term_lock(term).resize(rows, cols);
}
"##,
    ),
    (
        "an unbalanced paren inside a char literal must not close the region",
        r##"
fn verb_hydrate(host: &mut dyn SessionHost, sid: SessionId, rows: usize) {
    host.with_terminal_mut(sid, |t| {
        if t.last_byte() == b')' {
            t.hydrate(rows);
        }
    });
}
"##,
    ),
    (
        "aterm-control borrows through the host closure: no `term_lock` to anchor on",
        r##"
fn verb_resize(host: &mut dyn SessionHost, sid: SessionId, rows: usize, cols: usize) {
    host.with_terminal_mut(sid, |t| {
        t.resize(rows, cols);
    });
}
"##,
    ),
    (
        "an unbalanced paren in a trailing comment must not close the region",
        r##"
fn verb_decompress(host: &mut dyn SessionHost, sid: SessionId, rows: usize) {
    host.with_terminal_mut(sid, |t| {
        let n = t.rows(); // clamp) to the visible window
        t.decompress(n + rows);
    });
}
"##,
    ),
    (
        "DerefMut is a spelling of the reborrow",
        r##"
fn take_via_deref(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    let inner = t.deref_mut();
    inner.take_scrollback();
}
"##,
    ),
    (
        "a pattern binds the guard as well as a bare identifier",
        r##"
fn cross_resize_destructured(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let (mut t, limit) = (term_lock(term), cols);
    t.resize(rows, limit);
}
"##,
    ),
    (
        "the guard and the work in an inner block",
        r##"
fn resize_in_branch(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    if rows > 0 {
        let mut t = term_lock(term);
        t.take_scrollback();
    }
}
"##,
    ),
    (
        "an explicit deref through parens",
        r##"
fn cross_resize_deref(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock(term);
    (*t).resize(rows, cols);
}
"##,
    ),
    (
        "`crate::term_lock(..)` is the same acquire",
        r##"
fn rebuild_path_qualified(term: &Mutex<Terminal>) {
    let mut t = crate::term_lock(term);
    t.rebuild_index();
}
"##,
    ),
    (
        "a reborrow keeps the lock held; the alias inherits the guard's scope",
        r##"
fn cross_resize_reborrow(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock(term);
    let u = &mut *t;
    u.resize(rows, cols);
}
"##,
    ),
    (
        "`&mut` in front of the acquire still binds the guard",
        r##"
fn cross_resize_refmut(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let t = &mut term_lock(term);
    t.resize(rows, cols);
}
"##,
    ),
    (
        "rustfmt breaks a long chain: receiver and work on different lines",
        r##"
fn cross_resize_wrapped(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock(term);
    t.grid_mut()
        .resize(rows, cols);
}
"##,
    ),
    (
        "the UI thread's registering acquire hands back the same guard",
        r##"
fn redraw_reflow(term: &Mutex<Terminal>, waiting: &AtomicU32, rows: usize, cols: usize) {
    let site = TermWaitSite::Redraw;
    let mut t = crate::term_lock_ui(term, waiting, site);
    t.resize(rows, cols);
}
"##,
    ),
    (
        "a type annotation used to hide the binding",
        r##"
fn cross_resize_typed(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t: TermGuard<'_> = term_lock(term);
    t.resize(rows, cols);
}
"##,
    ),
    (
        "touching the compressed cold tier while the guard lives",
        r##"
fn decode_cold(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    let raw = aterm_lz4::decompress_size_prepended(t.cold_bytes()).unwrap();
    consume(raw);
}
"##,
    ),
    (
        "a `}` inside a test module's raw string must not end the module and strand \
         the string's closer",
        r##"
fn production(term: &Mutex<Terminal>) {
    record(term_lock(term).cols());
}

#[cfg(test)]
mod tests {
    const PLANT: &str = r#"
fn f() {
}
"#;
}

fn cross_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    term_lock(term).resize(rows, cols);
}
"##,
    ),
    (
        "a shipped raw string quoting `#[cfg(test)]` gates nothing after it",
        r##"
const TEMPLATE: &str = r#"
#[cfg(test)]
fn helper() {
"#;

fn cross_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    term_lock(term).resize(rows, cols);
}
"##,
    ),
    (
        "a control read's whole-frame hold is the same guard, chained",
        r##"
fn read_then_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    crate::term_lock_whole(term).resize(rows, cols);
}
"##,
    ),
    (
        "a control read's whole-frame hold is the same guard, bound",
        r##"
fn read_then_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock_whole(term);
    t.grid_mut().resize(rows, cols);
}
"##,
    ),
    (
        "a read's whole-frame hold that decides when to wait is the same guard, chained",
        r##"
fn read_then_resize(term: &Mutex<Terminal>, rows: usize, cols: usize, on: ReadOn) {
    crate::term_lock_whole_when(term, |_| on == ReadOn::Control).resize(rows, cols);
}
"##,
    ),
    (
        "a read's whole-frame hold that decides when to wait is the same guard, bound",
        r##"
fn read_then_resize(term: &Mutex<Terminal>, rows: usize, cols: usize, on: ReadOn) {
    let mut t = term_lock_whole_when(term, |t| on == ReadOn::Control && t.rows() > 0);
    t.grid_mut().resize(rows, cols);
}
"##,
    ),
    (
        "the host's hold of the terminal as it stands is the same guard",
        r##"
fn verb_rewrap(host: &impl SessionHost, sid: SessionId) {
    host.with_terminal_as_it_stands(sid, |t: &Terminal| {
        t.rewrap(0);
    });
}
"##,
    ),
];

/// Documented-safe shapes that must draw no finding.
const MUST_SILENT: &[(&str, &str)] = &[
    (
        "balanced single-line reads open no region",
        r##"
fn dims(term: &Mutex<Terminal>) -> (usize, usize) {
    (term_lock(term).rows(), term_lock(term).cols())
}
"##,
    ),
    (
        "a test resizes a terminal under a lock it owns",
        r##"
fn production(term: &Mutex<Terminal>) {
    record(term_lock(term).cols());
}

#[cfg(test)]
mod tests {
    #[test]
    fn resize_reflows() {
        let term = Mutex::new(Terminal::new(24, 80));
        let mut t = term_lock(&term);
        t.resize(48, 200);
    }
}
"##,
    ),
    (
        "a multi-line raw string in a test module ends no block early",
        r##"
#[cfg(test)]
mod tests {
    #[test]
    fn replays_a_recorded_index() {
        write(
            path,
            br#"{"frames":[
                {"n":1,"file":"frame_0001.png"}
            ]}"#,
        );
    }

    #[test]
    fn reflows_after_replay() {
        let term = Mutex::new(Terminal::new(24, 80));
        let mut t = term_lock(&term);
        t.reflow(200);
    }
}
"##,
    ),
    (
        "a format string's braces are not a test-only statement's braces",
        r##"
fn truncate_front_lines(term: &Mutex<Terminal>, n: usize) {
    #[cfg(test)]
    debug_assert!(
        n <= line_count(),
        "truncate_front_lines({n}) exceeds line_count({})",
        term_lock(term).hydrate(n)
    );
    record(n);
}
"##,
    ),
    (
        "prose naming the hazard, on its own line and trailing",
        r##"
fn documented(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    // t.resize(rows, cols) here would freeze the whole machine.
    record(t.rows()); // ...and so would t.reflow(), which is why it is offloaded
}
"##,
    ),
    (
        "`drop` releases the lock before the expensive part",
        r##"
fn cross_resize_dropped(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let mut t = term_lock(term);
    let pending = t.resize_offloading_scrollback(rows, cols);
    drop(t);
    let reflowed = pending.reflow();
    term_lock(term).finish_resize_offload(reflowed);
}
"##,
    ),
    (
        "the guard's block closed before the work",
        r##"
fn resize_after_scope(term: &Mutex<Terminal>, pending: Pending) {
    {
        let t = term_lock(term);
        record(t.cols());
    }
    pending.reflow();
}
"##,
    ),
    (
        "unbounded work with no terminal lock anywhere",
        r##"
fn reflow_detached(job: &mut Pending, grid: &mut Grid, rows: usize, cols: usize) {
    grid.resize(rows, cols);
    job.reflow();
    job.rebuild_index();
}
"##,
    ),
    (
        "THE FIX ITSELF: the rewrap off the lock, between two brief holds",
        r##"
fn cross_resize(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let pending = term_lock(term).resize_offloading_scrollback(rows, cols);
    let reflowed = pending.reflow();
    term_lock(term).finish_resize_offload(reflowed);
}
"##,
    ),
    (
        "the same offload through a bound guard whose scope closes first",
        r##"
fn cross_resize_bound(term: &Mutex<Terminal>, rows: usize, cols: usize) {
    let pending = {
        let mut t = term_lock(term);
        t.resize_offloading_scrollback(rows, cols)
    };
    let reflowed = pending.reflow();
    term_lock(term).finish_resize_offload(reflowed);
}
"##,
    ),
    (
        "the name rebound to a detached job",
        r##"
fn reflow_shadowed(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    record(t.cols());
    let mut t = detached_job();
    t.reflow();
}
"##,
    ),
    (
        "a TYPED re-let shadows the guard name too",
        r##"
fn reflow_shadowed_typed(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    record(t.cols());
    let mut t: Pending = detached_job();
    t.reflow();
}
"##,
    ),
    (
        "`.resize(` inside a string is prose",
        r##"
fn log_resize(term: &Mutex<Terminal>) {
    let mut t = term_lock(term);
    log(t.rows(), "resize( is named here but never called");
}
"##,
    ),
    (
        "the registering acquire's chain reads a VALUE out",
        r##"
fn resize_from_ui_read(term: &Mutex<Terminal>, waiting: &AtomicU32, grid: &mut Grid, rows: usize) {
    let cols = crate::term_lock_ui(term, waiting, TermWaitSite::Press).cols();
    grid.resize(rows, cols);
}
"##,
    ),
    (
        "a VALUE read out of the guard is not the guard",
        r##"
fn rebuild_detached(term: &Mutex<Terminal>) {
    let t = term_lock(term);
    let snapshot = t.cold_snapshot();
    drop(t);
    snapshot.rebuild_index();
}
"##,
    ),
    (
        "a chain reads a VALUE out; the guard is gone by the semicolon",
        r##"
fn resize_from_read(term: &Mutex<Terminal>, grid: &mut Grid, rows: usize) {
    let cols = term_lock(&term).cols();
    grid.resize(rows, cols);
}
"##,
    ),
    (
        "a wrapped chain ending in the SAFE offload, and a bare tail expression",
        r##"
fn cross_resize_wrapped_safe(term: &Mutex<Terminal>, rows: usize, cols: usize) -> Pending {
    let mut t = term_lock(term);
    let cols_now = t
        .cols();
    t.grid_mut()
        .resize_offloading_scrollback(rows, cols_now.min(cols))
}
"##,
    ),
    (
        "a test-module raw string holding a column-0 `}`, then more test code \
         (app_native.rs's unix-only-scan PLANT)",
        r##"
fn production(term: &Mutex<Terminal>) {
    record(term_lock(term).cols());
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_scan_goes_red_on_a_plant() {
        const PLANT: &str = r#"
#[cfg(unix)]
enum Overflow {
    Fail,
}
"#;
        assert_eq!(scan(PLANT), 1);
    }

    #[test]
    fn names_the_hazard() {
        let mut t = term_lock(&term);
        t.resize(48, 200);
        log("never call term_lock(t).resize(1, 2) on the main thread");
    }
}
"##,
    ),
    (
        "a block comment and a doc comment naming the work",
        r##"
fn cold_len(term: &Mutex<Terminal>) -> usize {
    let mut t = term_lock(term);
    /* t.resize(rows, cols) would
       t.reflow() here */
    /// t.hydrate(rows)
    t.cold_len()
}
"##,
    ),
];

#[test]
fn every_must_fire_shape_draws_a_finding() {
    for (why, src) in MUST_FIRE {
        assert!(
            !scan_source(src).0.is_empty(),
            "MISS — {why}: expected at least one finding in\n{src}"
        );
    }
}

#[test]
fn every_must_silent_shape_draws_none() {
    for (why, src) in MUST_SILENT {
        let hits = scan_source(src).0;
        assert!(
            hits.is_empty(),
            "CRIES WOLF — {why}: expected no finding, got lines {hits:?} in\n{src}"
        );
    }
}

/// The finding names the line the work is on, so the diagnostic points at
/// the site to repair, and the acquire count sees every acquiring line.
#[test]
fn a_finding_is_the_line_of_the_work() {
    let (_, wrapped) = MUST_FIRE
        .iter()
        .find(|(why, _)| {
            *why == "rustfmt breaks a long chain: receiver and work on different lines"
        })
        .expect("named wrapped-chain fixture");
    let (hits, acquires) = scan_source(wrapped);
    assert_eq!(hits, vec![5], "the wrapped chain's `.resize(` line");
    assert_eq!(acquires, 1);

    for (why, src) in MUST_FIRE {
        let lines: Vec<&str> = src.lines().collect();
        let (hits, acquires) = scan_source(src);
        assert!(acquires >= 1, "{why}: expected at least one acquire");
        for hit in hits {
            let line = lines
                .get(hit.saturating_sub(1))
                .copied()
                .expect("1-based line within source");
            assert!(
                has_blocked_work(line) || touches_cold_codec(line),
                "{why}: line {hit} ({line:?}) does not hold the work"
            );
        }
    }
}

/// The walk fails closed: a part of the scan set it cannot read refuses
/// the scan, never a clean scan of what it could read, in a scope
/// directory (an entry its listing cannot read) and in a split-out
/// library (a source directory it may not list). Negative control: the
/// same tree read whole is scanned.
#[test]
fn a_scan_set_it_cannot_read_whole_refuses_the_scan() {
    let files = [
        "crates/aterm-gui/src/main.rs",
        "crates/aterm-gui/src/pane/mod.rs",
        "crates/aterm-control/src/lib.rs",
        "crates/aterm-gui-term/src/lib.rs",
    ]
    .map(|rel| (rel.to_string(), String::new()));
    let scan = |fault: fn(&crate::tree::memory::Tree)| {
        let root =
            crate::scan_set::test_fixtures::synth_tree("aterm-census-held", "unread", &files);
        fault(&root);
        scan_tree(&root).map(|scan| scan.files)
    };
    assert_eq!(scan(|_| {}), Ok(4));
    assert!(scan(|root| root.unreadable("crates/aterm-gui/src/pane/mod.rs")).is_err());
    assert!(scan(|root| root.unlistable("crates/aterm-gui-term/src")).is_err());
}
