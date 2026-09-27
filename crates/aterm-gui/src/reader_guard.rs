// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SCREEN READER'S PANIC FENCE on the window's own threads.
//!
//! The window reads every agent session's screen with `aterm_phase` (and the
//! `aterm_agent` harness readers built on it) on threads whose panic is the
//! whole terminal's: every session in every window goes with it. Every such
//! call site reads through this fence:
//!
//! * the published agent verdict ([`crate::presence::agent_verdict_guarded`],
//!   from `session_status`'s sweep — the main thread);
//! * the operator's approval check (`crate::operator_host::approval_gate`,
//!   behind `looks_like_approval` for the refusal sites in `control.rs`, and
//!   the operator Classifier / `manage` baseline that announce boxes);
//! * the Claude Code footer (`crate::claude_footer`: `footer::mode_row` +
//!   `footer::plan_row`, every frame on the main thread);
//! * the Claude Code lights (`crate::claude_lights`: `lights::read_screen` +
//!   `composer_draft` every frame, and a toggle's `composer_draft` +
//!   `upgrade::composer_is_empty` — the main thread).
//!
//! The
//! reader is pure functions over the screen's rows, and a screen is whatever
//! the pane shows; the live harness validation of 2026-09-24 found a window
//! of rows (a question box cut below its top by the 40-row tail) that
//! panicked it (`rows[1..0]`). `aterm-phase`'s panic sweep
//! (`tests/panic_sweep.rs`) now reads every fixture through every window
//! and width, but a sweep is evidence about the screens it holds, not a
//! proof about the next one — so the callers read through [`guarded`], and
//! a panic becomes the caller's own no-verdict answer:
//!
//! * the agent verdict degrades to `unknown` (never `prompt`: nothing is
//!   pressed or announced off a screen the reader could not read);
//! * the approval check reads `Unreadable`: the refusal sites FAIL CLOSED
//!   (their contract: a miss there is a keystroke into a box — the operator
//!   is refused, as over a box), and the announcing sites announce NOTHING
//!   (no "waiting for human approval" off a screen nobody could read);
//! * the footer paints no row (the vendor's own row stays), and the lights
//!   keep what they last showed, take no toggle step, and refuse a toggle
//!   (`NotShown`) — no keystroke leaves off an unread screen.
//!
//! The release profile unwinds (`Cargo.toml` `[profile.release]`), so the
//! catch holds in the shipped binary. The panic hook (`logging.rs`, and the
//! wrapper [`guarded`] installs once) hands a panic raised INSIDE the fence
//! to [`absorb`] instead of filing `crash-<pid>.log` and writing stderr —
//! a screen that panics the reader is redrawn every frame, and a crash
//! report per frame would flood the disk and banner a crash that never
//! happened. [`warn_once`] logs ONE warn per session per distinct panic
//! location, with the screen's SHAPE (one glyph class per row, no text) —
//! enough to build the fixture that reproduces it.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, PanicHookInfo};
use std::sync::{Mutex, Once, PoisonError};

/// A reader panic the fence caught.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReaderPanic {
    /// `file:line:column` of the panic, as the hook saw it.
    pub(crate) location: String,
    /// The panic's message.
    pub(crate) message: String,
}

std::thread_local! {
    /// How deep this thread is inside [`guarded`] (the hook asks).
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// The location of the panic [`absorb`] took on this thread.
    static CAUGHT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The panic hook's question: a panic on a thread inside [`guarded`] is the
/// fence's — its location is recorded for [`guarded`] and `true` tells the
/// hook to file nothing (no crash report, no stderr). Anything else is
/// `false` and the hook proceeds as ever.
pub(crate) fn absorb(info: &PanicHookInfo<'_>) -> bool {
    if DEPTH.with(Cell::get) == 0 {
        return false;
    }
    let at = info.location().map_or_else(
        || "an unknown location".to_string(),
        |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
    );
    CAUGHT.with(|c| *c.borrow_mut() = Some(at));
    true
}

/// Wrap whatever hook is installed with [`absorb`], once per process. The
/// window's own hook (`logging.rs`) asks [`absorb`] too, so either order of
/// installation files nothing for a fenced panic.
fn ensure_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !absorb(info) {
                prev(info);
            }
        }));
    });
}

/// Run `read` inside the fence: its value, or the panic it raised.
pub(crate) fn guarded<T>(read: impl FnOnce() -> T) -> Result<T, ReaderPanic> {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }
    ensure_hook();
    DEPTH.with(|d| d.set(d.get() + 1));
    let leave = Leave;
    CAUGHT.with(|c| c.borrow_mut().take());
    let run = std::panic::catch_unwind(AssertUnwindSafe(read));
    drop(leave);
    run.map_err(|payload| ReaderPanic {
        location: CAUGHT
            .with(|c| c.borrow_mut().take())
            .unwrap_or_else(|| "an unknown location".to_string()),
        message: payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a non-text panic".to_string()),
    })
}

/// [`guarded`] for a caller with no answer of its own to degrade to: the
/// value, or — on a panic, warned once per `session` per location
/// ([`warn_once`], `what` naming the caller) — `None`.
pub(crate) fn read_or_none<T>(
    session: &str,
    what: &str,
    rows: &[String],
    read: impl FnOnce() -> T,
) -> Option<T> {
    match guarded(read) {
        Ok(value) => Some(value),
        Err(panic) => {
            warn_once(session, what, &panic, rows);
            None
        }
    }
}

/// How many (session, location) pairs are remembered; past it, one last
/// warn says so and nothing more is logged (a bounded log, whatever the
/// screens do).
const WARN_CAP: usize = 256;

/// The (session, location) pairs already warned about.
static WARNED: Mutex<Option<HashSet<(String, String)>>> = Mutex::new(None);

/// Log ONE warn for `p` per `session` per panic location — `what` names the
/// caller (`agent verdict`, `operator approval gate`), `rows` the screen
/// (only its [`screen_shape`] is logged, never its text). Whether this call
/// logged.
pub(crate) fn warn_once(session: &str, what: &str, p: &ReaderPanic, rows: &[String]) -> bool {
    let mut warned = WARNED.lock().unwrap_or_else(PoisonError::into_inner);
    let warned = warned.get_or_insert_with(HashSet::new);
    let key = (session.to_string(), p.location.clone());
    if warned.contains(&key) {
        return false;
    }
    if warned.len() >= WARN_CAP {
        if warned.len() == WARN_CAP {
            warned.insert((String::new(), String::new()));
            aterm_log::warn!(
                "screen reader: {WARN_CAP} distinct reader panics logged; further ones are \
                 caught and not logged"
            );
        }
        return false;
    }
    warned.insert(key);
    aterm_log::warn!(
        "screen reader: aterm_phase panicked reading session {session}'s screen for the {what} \
         at {} ({}); read as no verdict. Screen: {} rows, widest {} chars, shape {} \
         (one per row: _ blank, - rule, | bar, # numbered option, > caret, ? question, \
         x text) — a fixture of that shape reproduces it",
        p.location,
        p.message,
        rows.len(),
        rows.iter().map(|r| r.chars().count()).max().unwrap_or(0),
        screen_shape(rows)
    );
    true
}

/// The screen's shape, one glyph class per row and no text: `_` blank, `-`
/// a rule (`─`/`╌`), `|` a bar-led row, `#` a numbered option (`❯ 1. …`,
/// `  2. …`), `>` a caret-led row, `?` a row ending in `?`, `x` any other
/// text. At most 80 rows (the last 80), so the warn stays one line.
pub(crate) fn screen_shape(rows: &[String]) -> String {
    let from = rows.len().saturating_sub(80);
    rows[from..]
        .iter()
        .map(|row| {
            let t = row.trim();
            let after_caret = t.trim_start_matches('❯').trim_start();
            let numbered = after_caret
                .split_once(". ")
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
            if t.is_empty() {
                '_'
            } else if t.starts_with('─') || t.starts_with('╌') {
                '-'
            } else if t.starts_with('│') {
                '|'
            } else if numbered {
                '#'
            } else if t.starts_with('❯') {
                '>'
            } else if t.ends_with('?') {
                '?'
            } else {
                'x'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    /// A panic inside the fence is caught with its location and message; the
    /// value passes through untouched otherwise; the depth is restored after
    /// either, so a later panic outside the fence is not absorbed.
    #[test]
    fn a_fenced_panic_is_caught_with_its_location_and_the_depth_unwinds() {
        assert_eq!(guarded(|| 7), Ok(7));
        let p = guarded(|| -> u8 {
            let v: Vec<u8> = std::hint::black_box(Vec::new());
            v[3]
        })
        .expect_err("the stand-in panics");
        assert!(p.location.contains("reader_guard.rs"), "{p:?}");
        assert!(p.message.contains("index out of bounds"), "{p:?}");
        let s = guarded(|| -> () { panic!("{}", String::from("formatted")) }).unwrap_err();
        assert_eq!(s.message, "formatted");
        assert_eq!(DEPTH.with(Cell::get), 0, "the fence is left after a panic");
        // Nested: an inner fence's panic leaves the outer one standing.
        let outer = guarded(|| guarded(|| -> () { panic!("inner") }).unwrap_err());
        assert_eq!(outer.expect("the outer read returns").message, "inner");
        assert_eq!(DEPTH.with(Cell::get), 0);
    }

    /// `read_or_none` passes a value through and turns a panic into `None`.
    #[test]
    fn read_or_none_degrades_a_panic_to_none() {
        let screen = rows(&["x"]);
        assert_eq!(read_or_none("test-ron", "test", &screen, || 3), Some(3));
        assert_eq!(
            read_or_none("test-ron", "test", &screen, || -> u8 { panic!("boom") }),
            None
        );
        assert_eq!(DEPTH.with(Cell::get), 0);
    }

    /// One warn per session per location: the same panic again on the same
    /// session is not logged, the same panic on another session is, and a
    /// different location on the first session is.
    #[test]
    fn one_warn_per_session_per_location() {
        let screen = rows(&["", "─────", " Bash command", " ❯ 1. Yes", "   2. No"]);
        let at = |loc: &str| ReaderPanic {
            location: format!("crates/aterm-phase/src/prompt.rs:{loc}"),
            message: "slice index starts at 1 but ends at 0".to_string(),
        };
        assert!(warn_once(
            "test-warn-a",
            "agent verdict",
            &at("966:21"),
            &screen
        ));
        for _ in 0..1000 {
            assert!(!warn_once(
                "test-warn-a",
                "agent verdict",
                &at("966:21"),
                &screen
            ));
        }
        assert!(warn_once(
            "test-warn-b",
            "agent verdict",
            &at("966:21"),
            &screen
        ));
        assert!(warn_once(
            "test-warn-a",
            "agent verdict",
            &at("12:1"),
            &screen
        ));
    }

    /// The logged shape carries each row's class and none of its text.
    #[test]
    fn the_shape_names_classes_not_text() {
        let screen = rows(&[
            "",
            &"─".repeat(40),
            " Bash command",
            "   │ rm -rf secret-dir",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. No",
            "❯ ",
            "╌╌╌",
        ]);
        assert_eq!(screen_shape(&screen), "_-x|?##>-");
        let tall: Vec<String> = (0..200).map(|i| format!("row {i}")).collect();
        assert_eq!(screen_shape(&tall).len(), 80);
    }
}
