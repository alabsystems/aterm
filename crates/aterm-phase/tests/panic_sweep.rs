// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PANIC SWEEP: the reader never panics, on any window of any screen.
//!
//! The window runs [`aterm_phase::read`] on its MAIN THREAD, every frame, for
//! every agent session, and a panic there takes down the terminal and every
//! session in it. The live harness validation of 2026-09-24 found one (a
//! question box cut below its top by a 40-row `tail=`: `rows[1..0]`). A
//! screen is whatever the pane shows — any contiguous run of any screen,
//! each row cut at any width — so this sweep reads the fixture FILES under
//! `aterm-phase/src/fixtures/` and (when this crate sits in the aterm
//! workspace) `aterm-agent/src/supervise/policy/fixtures/`, read at test
//! time, and the in-crate builders (`prompt::fixtures`). It does NOT read the
//! box screens written inline in other crates' unit tests. Each is read
//! through:
//!
//! * every tail cut `rows[k..]` and every head cut `rows[..k]`;
//! * every middle window of heights 1..=12;
//! * each row truncated to a few widths (on char boundaries), under the same
//!   tail and head cuts and a deterministic sample of the middles;
//!
//! with the reader for Claude Code, the screen-identified reader (`None`)
//! and Codex's, with no cursor and a couple of cursor columns, plus the
//! other pure functions a caller runs on the same rows (the approval loop's
//! `parse_prompt_v2`, the lights' `composer_draft`, …) and every accessor a
//! caller runs on a box the reader returns ([`read_box`]). It asserts ONLY that
//! nothing panics, and names the fixture, the window and the panic's
//! location when something does.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use aterm_phase::prompt::fixtures as fx;

/// One screen to sweep: where it came from and its rows.
struct Screen {
    name: String,
    rows: Vec<String>,
}

/// A fixture file's rows: the provenance line (`# …`) dropped, and a saved
/// waiter capture's `== …` head and `exit=` tail dropped, the way the
/// crate's own loaders read them.
fn file_rows(text: &str) -> Vec<String> {
    let mut r = fx::screen(text);
    if r.first().is_some_and(|l| l.starts_with("== ")) {
        r.remove(0);
    }
    if r.last().is_some_and(|l| l.starts_with("exit=")) {
        r.pop();
    }
    r
}

fn dir_screens(dir: &Path, into: &mut Vec<Screen>) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("a fixture entry").path())
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "{}: no fixtures", dir.display());
    for p in paths {
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        into.push(Screen {
            name: p
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(&p)
                .display()
                .to_string(),
            rows: file_rows(&text),
        });
    }
}

fn screens() -> Vec<Screen> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    dir_screens(&crate_dir.join("src/fixtures"), &mut out);
    // The supervisor's saved captures live in a sibling crate: read them when
    // this crate sits in the aterm workspace, and say so when it does not (a
    // packaged `aterm-phase` has no sibling, and its own sweep still runs).
    let agent = crate_dir.join("../aterm-agent/src/supervise/policy/fixtures");
    if agent.is_dir() {
        dir_screens(&agent, &mut out);
    } else {
        eprintln!(
            "panic sweep: {} absent (not in the aterm workspace): its captures are not swept",
            agent.display()
        );
    }
    let mut builder = |name: &str, rows: Vec<String>| {
        out.push(Screen {
            name: format!("builder {name}"),
            rows,
        });
    };
    builder("bash_one_row", fx::bash_one_row());
    builder("bash_multi_row", fx::bash_multi_row());
    builder("bash_multi_row_with_note", fx::bash_multi_row_with_note());
    builder("workflow_box", fx::workflow_box());
    builder("edit_box", fx::edit_box());
    builder("read_box", fx::read_box());
    builder("tall_bash_box(70)", fx::tall_bash_box(70));
    builder("tall_edit_box(50)", fx::tall_edit_box(50));
    builder(
        "tall_footerless(NETWORK, 45)",
        fx::tall_footerless(fx::BOX_NETWORK, 45),
    );
    builder(
        "tall_footerless(HELD_MESSAGE, 45)",
        fx::tall_footerless(fx::HELD_MESSAGE, 45),
    );
    builder(
        "goal_with_forged_box",
        fx::goal_with_forged_box(&[" Ship the release", ""], 3),
    );
    builder(
        "launch_under_a_table",
        fx::launch_under_a_table("❯ Try \"fix lint errors\"", "  ? for shortcuts"),
    );
    builder(
        "first_turn_under_a_table",
        fx::first_turn_under_a_table(&["❯ and then the tests", "  too"]),
    );
    builder("trust_box", fx::trust_box());
    // The degenerate rows a pane can hold: the reader's anchors alone, with
    // nothing around them.
    builder(
        "anchors alone",
        fx::rows(&[
            "─",
            "❯",
            "│",
            "❯ 1.",
            " ❯ 1. Yes",
            "1.",
            " Esc to cancel · Tab to amend",
            "Do you want to proceed?",
            "╌",
            "⎿",
            "⏺",
            "",
            "  ",
            "✻ Thinking… (",
            "% until auto-compact",
            "─",
        ]),
    );
    out
}

/// `rows` with every row cut to at most `width` chars.
fn truncated(rows: &[String], width: usize) -> Vec<String> {
    rows.iter()
        .map(|r| r.chars().take(width).collect())
        .collect()
}

/// Every call the sweep makes on one window.
fn read_all(w: &[String]) {
    let _ = aterm_phase::read(Some("claude"), w, None);
    let _ = aterm_phase::read(Some("claude"), w, Some(2));
    let _ = aterm_phase::read(None, w, None);
    let _ = aterm_phase::read(Some("codex"), w, Some(0));
    for col in [0, 1, 200] {
        let _ = aterm_phase::continuation_suggestion(w, col);
    }
    for reading in [
        aterm_phase::read(Some("claude"), w, None),
        aterm_phase::read(None, w, None),
        aterm_phase::read(Some("codex"), w, Some(0)),
    ] {
        if let Some(p) = reading.prompt {
            read_box(&p);
        }
    }
    let _ = aterm_phase::parse_prompt(w);
    if let Some(p) = aterm_phase::parse_prompt_v2(w) {
        read_box(&p);
    }
    let _ = aterm_phase::prompt_box_span(w);
    let _ = aterm_phase::prompt_box_first_row(w);
    let _ = aterm_phase::transcript_end(w);
    let _ = aterm_phase::status_row_stall(w, w);
    if let Some(rest) = w.get(1..) {
        let _ = aterm_phase::status_row_stall(rest, w);
        let _ = aterm_phase::status_row_stall(w, rest);
    }
    let _ = aterm_phase::phase::composer_draft(w);
    let _ = aterm_phase::phase::composer_rules(w);
    let _ = aterm_phase::phase::composer_index(w);
    let _ = aterm_phase::phase::composer_bottom(w);
    let _ = aterm_phase::phase::last_said_row(w);
    let _ = aterm_phase::limit_notice(w);
    let _ = aterm_phase::interrupted(w);
    let _ = aterm_phase::status_row_progress(w);
}

/// Every accessor a caller runs on a parsed box (the presence detail's
/// `readings`, the approval policy's `base_title` / `rm_breaker` /
/// `runs_on`, the question dialog's `question` / `guard_row` / `focus` /
/// `same_question`, …).
fn read_box(p: &aterm_phase::PromptV2) {
    use aterm_phase::Role;
    let _ = p.readings();
    let _ = p.focused();
    for role in [
        Role::Once,
        Role::Session,
        Role::Persist,
        Role::ModeSwitch,
        Role::Deny,
        Role::Exit,
        Role::Trust,
        Role::Other,
    ] {
        let _ = p.with_role(role);
    }
    let _ = p.header();
    let _ = p.base_title();
    let _ = p.origin();
    let _ = p.runs_on();
    let _ = p.unsandboxed();
    let _ = p.tool_card();
    let _ = p.rm_breaker();
    if let Some(d) = &p.question_dialog {
        let _ = d.question();
        let _ = d.guard_row();
        let _ = d.focus();
        let _ = d.same_question(d);
    }
}

std::thread_local! {
    /// Where the last panic on this thread happened (the hook's record).
    static LAST_PANIC: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// One window that panicked: the fixture, the window, the location and the
/// message.
fn check(name: &str, what: &str, w: &[String], failures: &Mutex<Vec<String>>) {
    if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| read_all(w))) {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let at = LAST_PANIC
            .with(|l| l.borrow_mut().take())
            .unwrap_or_default();
        failures
            .lock()
            .unwrap()
            .push(format!("{name} {what} ({} rows): {at}: {msg}", w.len()));
    }
}

/// The windows of one screen: every tail and head cut, and the middles of
/// heights 1..=12 — all of them when `every_middle`, else one in five (by a
/// fixed stride offset per height, so a rerun sweeps the same windows).
fn sweep(name: &str, rows: &[String], every_middle: bool, failures: &Mutex<Vec<String>>) {
    let n = rows.len();
    check(name, "whole", rows, failures);
    for k in 0..=n {
        check(name, &format!("tail rows[{k}..]"), &rows[k..], failures);
        check(name, &format!("head rows[..{k}]"), &rows[..k], failures);
    }
    let tallest = if full() { n } else { 12.min(n) };
    for h in 1..=tallest {
        for s in 0..=n - h {
            if every_middle || full() || (s + h) % 5 == 0 {
                check(
                    name,
                    &format!("middle rows[{s}..{}]", s + h),
                    &rows[s..s + h],
                    failures,
                );
            }
        }
    }
}

/// The widths each row is also cut to.
const WIDTHS: &[usize] = &[1, 2, 3, 6, 12, 40];

/// `ATERM_PANIC_SWEEP_FULL=1`: the exhaustive sweep for hunting — every
/// middle window of every height, every width 1..=80, and each screen with
/// one row dropped or doubled (minutes, not seconds; not the suite's).
fn full() -> bool {
    std::env::var_os("ATERM_PANIC_SWEEP_FULL").is_some_and(|v| v == "1")
}

#[test]
fn the_reader_never_panics_on_any_window_of_any_fixture() {
    let prev = panic::take_hook();
    panic::set_hook(Box::new(|info| {
        let at = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        LAST_PANIC.with(|l| *l.borrow_mut() = Some(at));
    }));
    let started = Instant::now();
    let all = screens();
    let failures = Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(s) = all.get(i) else { break };
                    sweep(&s.name, &s.rows, true, &failures);
                    let widths: Vec<usize> = if full() {
                        (1..=80).collect()
                    } else {
                        WIDTHS.to_vec()
                    };
                    for w in widths {
                        let cut = truncated(&s.rows, w);
                        sweep(&format!("{} @width {w}", s.name), &cut, false, &failures);
                    }
                    if full() {
                        for k in 0..s.rows.len() {
                            let mut dropped = s.rows.clone();
                            dropped.remove(k);
                            sweep(
                                &format!("{} row {k} dropped", s.name),
                                &dropped,
                                false,
                                &failures,
                            );
                            let mut doubled = s.rows.clone();
                            doubled.insert(k, s.rows[k].clone());
                            sweep(
                                &format!("{} row {k} doubled", s.name),
                                &doubled,
                                false,
                                &failures,
                            );
                        }
                    }
                }
            });
        }
    });
    panic::set_hook(prev);
    let failures = failures.into_inner().unwrap();
    eprintln!(
        "panic sweep: {} screens, {} threads, {:?}",
        all.len(),
        threads,
        started.elapsed()
    );
    if !failures.is_empty() {
        let mut distinct: Vec<&String> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for f in &failures {
            // One line per distinct panic location, its first window.
            let at = f.split(": ").nth(1).unwrap_or("").to_string();
            if seen.insert(at) {
                distinct.push(f);
            }
        }
        panic!(
            "{} windows panicked the reader, {} distinct locations:\n{}",
            failures.len(),
            distinct.len(),
            distinct
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
