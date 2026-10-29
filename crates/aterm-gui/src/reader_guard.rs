// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The screen reader's panic fence on the window's own threads.

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
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static CAUGHT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The panic hook's question.
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

/// Whether this thread runs inside [`guarded`] now: a test's view of a caller's fence.
#[cfg(all(test, unix))]
pub(crate) fn inside() -> bool {
    DEPTH.with(Cell::get) > 0
}

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

/// [`guarded`] for a caller with no answer of its own to degrade to: the value, or — on a panic,
/// warned once per `session` per location ([`warn_once`], `what` naming the caller) — `None`.
pub(crate) fn read_or_none<T>(
    session: impl std::fmt::Display,
    what: &str,
    rows: &[String],
    read: impl FnOnce() -> T,
) -> Option<T> {
    match guarded(read) {
        Ok(value) => Some(value),
        Err(panic) => {
            warn_once(&session.to_string(), what, &panic, rows);
            None
        }
    }
}

const WARN_CAP: usize = 256;

static WARNED: Mutex<Option<HashSet<(String, String)>>> = Mutex::new(None);

/// Log ONE warn for `p` per `session` per panic location.
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
    // The message names the panic's kind and length, never its text: std's slice panics quote
    // the row they cut, and a row is the session's screen (DESIGN §3.6).
    aterm_log::warn!(
        "screen reader: aterm_phase panicked reading session {session}'s screen for the {what} \
         at {} ({}); read as no verdict. Screen: {} rows, widest {} chars, shape {} \
         (one per row: _ blank, - rule, | bar, # numbered option, > caret, ? question, \
         x text) — a fixture of that shape reproduces it",
        p.location,
        aterm_log::words::Panic::of_text(&p.message),
        rows.len(),
        rows.iter().map(|r| r.chars().count()).max().unwrap_or(0),
        screen_shape(rows)
    );
    true
}

/// The screen's shape, one glyph class per row and no text.
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
        let outer = guarded(|| guarded(|| -> () { panic!("inner") }).unwrap_err());
        assert_eq!(outer.expect("the outer read returns").message, "inner");
        assert_eq!(DEPTH.with(Cell::get), 0);
    }

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
