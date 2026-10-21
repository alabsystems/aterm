// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Panic sweep: the reader never panics on any window of any screen.
//!
//! The window runs [`aterm_phase::read`] on its main thread every frame for
//! every agent session. A screen is whatever the pane shows — any contiguous
//! run of rows, each cut at any width — so this sweep reads the fixture files
//! under `aterm-phase/src/fixtures/` and (when in the workspace)
//! `aterm-agent/src/supervise/policy/fixtures/`, plus the in-crate builders
//! (`prompt::fixtures`), through:
//!
//! * every tail cut `rows[k..]` and every head cut `rows[..k]`;
//! * every middle window of heights 1..=12;
//! * each row truncated to [`WIDTHS`] (on char boundaries), under the same
//!   tail and head cuts and a deterministic sample of the middles;
//!
//! across every [`Program::ALL`] reader, [`aterm_phase::continuation_suggestion`]
//! at several cursor columns, the standalone phase/prompt/launch-card entry
//! points, and every [`PromptV2`](aterm_phase::PromptV2) accessor
//! ([`read_box`]).

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Mutex, PoisonError};

use aterm_phase::Program;
use aterm_phase::prompt::fixtures as fx;

#[path = "common/fixture_files.rs"]
mod fixture_files;

/// One screen to sweep: where it came from and its rows.
struct Screen {
    name: String,
    rows: Vec<String>,
}

fn screens() -> Vec<Screen> {
    let mut out: Vec<Screen> = fixture_files::fixture_screens("panic sweep")
        .into_iter()
        .map(|(name, rows)| Screen { name, rows })
        .collect();
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
    // Each `Program::ALL` reader reads the window once at cursor column 0:
    // the cursor column only affects `suggestion` (`continuation_suggestion`,
    // tested across columns below), and `read(None, …)` dispatches to one of
    // `Program::ALL` via `identify`.
    for col in [0, 1, 2, 200] {
        let _ = aterm_phase::continuation_suggestion(w, col);
    }
    for program in Program::ALL {
        if let Some(p) = program.reader().read(w, Some(0)).prompt {
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
    let _ = aterm_phase::phase::composer_draft(w);
    let _ = aterm_phase::phase::composer_rules(w);
    let _ = aterm_phase::phase::composer_index(w);
    let _ = aterm_phase::phase::composer_bottom(w);
    let _ = aterm_phase::phase::last_said_row(w);
    let _ = aterm_phase::limit_notice(w);
    let _ = aterm_phase::interrupted(w);
    // The window's harness card reads the card's place every live frame.
    let _ = aterm_phase::launch_card(w);
    if let Some(place) = aterm_phase::launch_card_place(w) {
        let _ = aterm_phase::launch_card_version(w, &place);
        // A place read off other rows: the rows above it, cut short.
        let _ = aterm_phase::launch_card_version(&w[..place.top], &place);
        // …and holds it on the next frame by its words: on these rows, on
        // the rows above it, and on these rows one row down the screen.
        if let Some(words) = aterm_phase::launch_card_words(w, &place) {
            for rows in [w, &w[..place.top], w.get(1..).unwrap_or_default()] {
                let scan = aterm_phase::launch_card_scan(rows);
                let _ = aterm_phase::launch_card_held(rows, scan, &place, words);
            }
        }
        let far = aterm_phase::CardPlace {
            top: usize::MAX,
            text_col: usize::MAX,
            ..place
        };
        let _ = aterm_phase::launch_card_words(w, &far);
    }
    // …and keeps its reading without a scan while the rows it reads hold:
    // every row changed, and indices past the rows.
    let (banner, _) = aterm_phase::launch_card_scan(w);
    let every: Vec<usize> = (0..=w.len()).chain([usize::MAX]).collect();
    let _ = aterm_phase::launch_card_unmoved(w, banner, &every);
    let _ = aterm_phase::launch_card_unmoved(w, banner.map(|b| b + 2), &every);
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

/// Check one window and record any panic. Each distinct window is read once
/// (sharded by hash across 64 locks to avoid contention across threads).
fn check(name: &str, what: &str, w: &[String], failures: &Mutex<Vec<String>>) {
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
    for h in 1..=12.min(n) {
        for s in (part..=n - h).step_by(parts) {
            if every_middle || (s + h) % 5 == 0 {
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

/// Number of parts to split an `n`-row screen's quadratic tail/head sweep into.
fn parts_for(n: usize) -> usize {
    n.div_ceil(32).max(1)
}

/// One job of the sweep: a screen as captured, or cut to a width.
#[derive(Clone, Copy)]
enum Cut {
    Whole,
    Width(usize),
}

/// The widths each row is also cut to.
const WIDTHS: &[usize] = &[1, 2, 3, 6, 12, 40];

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
    let all = screens();
    let failures = Mutex::new(Vec::new());
    // One job per (screen, cut, part), tallest screen first so quadratic
    // sweeps of tall captures do not straggle at the end of the queue.
    let mut jobs: Vec<(&Screen, Cut, (usize, usize))> = Vec::new();
    for s in &all {
        let parts = parts_for(s.rows.len());
        for cut in std::iter::once(Cut::Whole).chain(WIDTHS.iter().map(|&w| Cut::Width(w))) {
            jobs.extend((0..parts).map(|p| (s, cut, (p, parts))));
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
                    }
                }
            });
        }
    });
    panic::set_hook(prev);
    let failures = failures.into_inner().unwrap();
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
