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

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use aterm_phase::prompt::fixtures as fx;
use aterm_phase::{Program, Reading};

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
    // `read` is a function of its arguments alone, so each reading is taken
    // once and its box handed to `read_box` from that one reading. The
    // cursor's column enters a reading only as its `suggestion` (the one
    // `ScreenReader::read`, which no reader overrides), so a reading that
    // differs from one already taken only by that column is not taken again
    // (the reader was 2/3 of the sweep's CPU, Claude Code's 9/10 of that):
    // * Claude Code's with the cursor at column 2 is the reading without it
    //   plus `continuation_suggestion(w, 2)`, called with the other columns;
    // * the screen-identified reading (`read(None, …)`) is `identify`'s
    //   reader's `read(w, None)`: for Claude Code that is `claude`, for
    //   Codex `codex` (whose suggestion at column 0 `read` computes too), so
    //   only the generic reader's is read here.
    // `the_sweep_reads_each_reading_it_skips_already` holds both, on every
    // screen at every width.
    let claude = aterm_phase::read(Some("claude"), w, None);
    let codex = aterm_phase::read(Some("codex"), w, Some(0));
    let by_screen = aterm_phase::identify(None, w);
    let generic = (by_screen.program() == Program::Generic).then(|| by_screen.read(w, None));
    for col in [0, 1, 2, 200] {
        let _ = aterm_phase::continuation_suggestion(w, col);
    }
    for reading in [Some(claude), generic, Some(codex)].into_iter().flatten() {
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
    let _ = w
        .get(aterm_phase::busy_zone_start(w)..)
        .expect("the live zone starts on the screen");
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
    /// This thread is one of the sweep's: its panics are the hook's to
    /// record. Another test's, while the hook is installed, are printed.
    static SWEEPING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// One window that panicked: the fixture, the window, the location and the
/// message. Each distinct window is read once: the reader is a function of
/// the rows alone, and half the sweep's windows repeat one already read — a
/// shared footer, a blank run, the narrow cuts (2026-09-30, the fixture
/// files: 162,884 of 321,033 windows distinct).
fn check(name: &str, what: &str, w: &[String], failures: &Mutex<Vec<String>>) {
    // Sharded by hash: one set under one lock had the sweep's threads
    // queueing on it.
    static READ: [Mutex<BTreeSet<u64>>; 64] = [const { Mutex::new(BTreeSet::new()) }; 64];
    let mut h = std::hash::DefaultHasher::new();
    w.hash(&mut h);
    let h = h.finish();
    if !READ[(h % 64) as usize]
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(h)
    {
        return;
    }
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

/// Part `part` of `parts` of the windows of one screen: every tail and head
/// cut, and the middles of heights 1..=12 — all of them when `every_middle`,
/// else one in five (by a fixed stride offset per height, so a rerun sweeps
/// the same windows). The parts are disjoint and together are the whole: part
/// `p` takes the cuts at `k ≡ p (mod parts)` — a tail `rows[k..]` and a head
/// `rows[..k]` together are `n` rows whatever `k`, so the parts weigh the same
/// — and the middles starting at `s ≡ p`, and part 0 the whole screen.
fn sweep(
    name: &str,
    rows: &[String],
    every_middle: bool,
    (part, parts): (usize, usize),
    failures: &Mutex<Vec<String>>,
) {
    let n = rows.len();
    if part == 0 {
        check(name, "whole", rows, failures);
    }
    for k in (part..=n).step_by(parts) {
        check(name, &format!("tail rows[{k}..]"), &rows[k..], failures);
        check(name, &format!("head rows[..{k}]"), &rows[..k], failures);
    }
    let tallest = if full() { n } else { 12.min(n) };
    for h in 1..=tallest {
        for s in (part..=n - h).step_by(parts) {
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

/// How many parts one cut of an `n`-row screen is swept in ([`sweep`]): its
/// tail and head cuts are quadratic in its rows, and as one job per cut the
/// 528-row capture's seven were the run's wall (2026-10-05: its whole cut 50 s
/// of an 86 s run on 6 threads, every other job under 6 s).
fn parts_for(n: usize) -> usize {
    n.div_ceil(32).max(1)
}

/// One job of the sweep: a screen as captured, cut to a width, or (the full
/// sweep) with one row dropped and doubled.
#[derive(Clone, Copy)]
enum Cut {
    Whole,
    Width(usize),
    Edit(usize),
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
        if !SWEEPING.get() {
            eprintln!("{info}");
            return;
        }
        let at = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        LAST_PANIC.with(|l| *l.borrow_mut() = Some(at));
    }));
    let started = Instant::now();
    let all = screens();
    let failures = Mutex::new(Vec::new());
    // One job per screen, cut and part ([`parts_for`]), tallest screen first:
    // the sweep of one tall capture is quadratic in its rows (every tail and
    // head), and as one job per screen the 528-row capture was the run's wall
    // on a single thread (2026-09-30: 62 s of wall for 152 s of CPU on 14
    // threads), as one job per cut still (2026-10-05).
    let widths: Vec<usize> = if full() {
        (1..=80).collect()
    } else {
        WIDTHS.to_vec()
    };
    let mut jobs: Vec<(&Screen, Cut, (usize, usize))> = Vec::new();
    for s in &all {
        let parts = parts_for(s.rows.len());
        for cut in std::iter::once(Cut::Whole).chain(widths.iter().map(|&w| Cut::Width(w))) {
            jobs.extend((0..parts).map(|p| (s, cut, (p, parts))));
        }
        if full() {
            jobs.extend((0..s.rows.len()).map(|k| (s, Cut::Edit(k), (0, 1))));
        }
    }
    jobs.sort_by_key(|(s, _, _)| std::cmp::Reverse(s.rows.len()));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                SWEEPING.set(true);
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&(s, cut, part)) = jobs.get(i) else {
                        break;
                    };
                    match cut {
                        Cut::Whole => sweep(&s.name, &s.rows, true, part, &failures),
                        Cut::Width(w) => sweep(
                            &format!("{} @width {w}", s.name),
                            &truncated(&s.rows, w),
                            false,
                            part,
                            &failures,
                        ),
                        Cut::Edit(k) => {
                            let mut dropped = s.rows.clone();
                            dropped.remove(k);
                            sweep(
                                &format!("{} row {k} dropped", s.name),
                                &dropped,
                                false,
                                part,
                                &failures,
                            );
                            let mut doubled = s.rows.clone();
                            doubled.insert(k, s.rows[k].clone());
                            sweep(
                                &format!("{} row {k} doubled", s.name),
                                &doubled,
                                false,
                                part,
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

/// What [`read_all`] leaves out is a reading it already took: on every
/// screen, whole and at every width, Claude Code's reading with the cursor at
/// column 2 is its reading without one plus `continuation_suggestion(rows, 2)`,
/// and the screen-identified reading of a screen identified as Claude Code's
/// or Codex's is that program's reading (Codex's without its suggestion). If
/// a reader ever reads the cursor anywhere else, this fails, and the sweep
/// must take those readings again.
#[test]
fn the_sweep_reads_each_reading_it_skips_already() {
    for s in screens() {
        for width in std::iter::once(None).chain(WIDTHS.iter().copied().map(Some)) {
            let rows = width.map_or_else(|| s.rows.clone(), |w| truncated(&s.rows, w));
            let at = || format!("{} @width {width:?}", s.name);
            let claude = aterm_phase::read(Some("claude"), &rows, None);
            let codex = aterm_phase::read(Some("codex"), &rows, Some(0));
            assert_eq!(
                aterm_phase::read(Some("claude"), &rows, Some(2)),
                Reading {
                    suggestion: aterm_phase::continuation_suggestion(&rows, 2),
                    ..claude.clone()
                },
                "{}",
                at()
            );
            let by_screen = aterm_phase::read(None, &rows, None);
            match by_screen.program {
                Program::Claude => assert_eq!(by_screen, claude, "{}", at()),
                Program::Codex => assert_eq!(
                    by_screen,
                    Reading {
                        suggestion: None,
                        ..codex
                    },
                    "{}",
                    at()
                ),
                Program::Generic => {}
            }
        }
    }
}
