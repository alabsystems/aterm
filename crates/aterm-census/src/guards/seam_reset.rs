// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `seam-reset`: a test seam's guard never puts a process-global value back in its Drop.
//!
//! libtest runs the tests of one binary at once, each on its own thread of one process, so a
//! seam a guard holds (`aterm_pty::hold_reap_kills_for_test`'s `ReapKillHold`) is held by every
//! test that takes it at the same time. A seam stored as one value that its guard's Drop resets
//! (a flag cleared, a mode or an override put back, a slot emptied) is released by the first of
//! those tests to end while the others still hold it: 0.161.0's release refused when aterm-pty's
//! reap hold, one `AtomicBool`, let a reap kill another test's shell mid-handler (0f45bdf954 made
//! it a count).
//!
//! The rule fails, inside a Drop that only a test or debug build compiles (a test file, or code
//! under a `cfg` that entails `test` or `debug_assertions`), a `.store(` or `.swap(` on a static
//! (a SCREAMING_CASE receiver; a field such as `self.flag` is the guard's own), and a reset of a
//! static behind its lock: a `.take()` or `.clear()` of what `.lock(` or `.write(` reaches, or
//! an assignment whose left side reaches it. A reset no other holder can be inside of passes: the
//! guard holds an exclusive lock (a `MutexGuard` or `RwLockWriteGuard` field), so its holders run
//! one at a time; the file takes the seam with `assert!(!SEAM.swap(true, …))`, so a second holder
//! fails at once; or the reset runs only once the holders are gone (inside an `if` whose
//! condition reads `is_empty()`, `== 0` or `fetch_sub(`). The fix is the seam's: a count each
//! guard adds one to and its Drop takes one from (`fetch_sub`), a scope per test (a
//! `thread_local!`, which `.with` reaches, or a key the test owns that its Drop removes from a
//! shared map), or the lock that serialises the seam's tests held by the guard itself.

use super::rules::{CENSUS_CRATE, PRODUCT};
use super::{Finding, Rule, SourceFile, Tree, brace_close, is_word};

pub(super) const SEAM_RESET: Rule = Rule {
    id: "seam-reset",
    why: "tests of one binary run at once: a test seam stored as one value its guard's Drop \
          resets is released by the first holder to end while another test still holds it \
          (0.161.0's reap hold killed a held shell mid-handler); count the holders \
          (fetch_add/fetch_sub) or scope the seam per test",
    scope: &[],
    check: seam_reset,
};

fn seam_reset(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let test_code = tree.corpus.test_code();
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(CENSUS_CRATE)
            || !file.mentions(&["Drop for"])
            || !file.mentions(&[
                ".store(", ".swap(", ".take()", ".clear()", ".lock(", ".write(",
            ])
        {
            continue;
        }
        check_file(file, test_code.holds(&file.rel), out);
    }
}

fn check_file(file: &SourceFile, whole_file_is_test: bool, out: &mut Vec<Finding>) {
    let code = &file.lexed().code;
    let test_only = if whole_file_is_test {
        None
    } else {
        Some(crate::lock_order::unmasked_lines(
            code,
            &super::debug_seam::release_never_compiles,
        ))
    };
    let starts: Vec<usize> = std::iter::once(0)
        .chain(code.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let line_of = |at: usize| starts.partition_point(|&start| start <= at) - 1;
    let gated = |at: usize| {
        test_only
            .as_ref()
            .is_none_or(|shipped| !shipped.get(line_of(at)).copied().unwrap_or(true))
    };
    for (guard, open, close) in drop_bodies(code) {
        if holds_exclusive_lock(code, guard) {
            continue;
        }
        for (at, seam, how) in resets(code, open, close) {
            if gated(at) && !taken_exclusively(code, seam) && !after_the_last_holder(code, open, at)
            {
                out.push(file.finding(
                    SEAM_RESET.id,
                    line_of(at) + 1,
                    format!(
                        "`{guard}`'s Drop {how} the static `{seam}`: a test still holding it \
                         loses it; count the holders or scope the seam per test"
                    ),
                ));
            }
        }
    }
}

/// Every `impl … Drop for T` in `code`: `T`, and its body's `{` and `}`.
fn drop_bodies(code: &str) -> Vec<(&str, usize, usize)> {
    let mut out = Vec::new();
    for (at, _) in code.match_indices("Drop for ") {
        if at > 0 && is_word(code.as_bytes()[at - 1]) {
            continue;
        }
        let line_start = code[..at].rfind('\n').map_or(0, |n| n + 1);
        let head = code[line_start..at].trim_start();
        let head = head.strip_prefix("unsafe ").unwrap_or(head);
        if !(head.starts_with("impl ") || head.starts_with("impl<")) {
            continue;
        }
        let rest = &code[at + "Drop for ".len()..];
        let name_end = rest
            .bytes()
            .position(|b| !(is_word(b) || b == b':'))
            .unwrap_or(rest.len());
        let name = rest[..name_end].rsplit("::").next().unwrap_or("");
        let Some(open) = code[at..].find('{').map(|o| at + o) else {
            continue;
        };
        if let Some(close) = brace_close(code, open) {
            out.push((name, open, close));
        }
    }
    out
}

/// Every reset of a static in `code[open..close]`: where, the static, and how it is reset.
fn resets(code: &str, open: usize, close: usize) -> Vec<(usize, &str, &'static str)> {
    let body = &code[open..close];
    let mut out = Vec::new();
    for (method, how) in [(".store(", "stores into"), (".swap(", "swaps")] {
        for (at, _) in body.match_indices(method) {
            if let Some(seam) = receiver(body, at).filter(|r| is_static_name(r)) {
                out.push((open + at, seam, how));
            }
        }
    }
    for method in [".lock(", ".write("] {
        for (at, _) in body.match_indices(method) {
            let Some(seam) = receiver(body, at).filter(|r| is_static_name(r)) else {
                continue;
            };
            let stmt_start = body[..at].rfind([';', '{', '}']).map_or(0, |s| s + 1);
            let stmt_end = body[at..].find(';').map_or(body.len(), |e| at + e);
            let reached = &body[at..stmt_end];
            let how = if reached.contains(".take()") {
                Some("takes")
            } else if reached.contains(".clear()") {
                Some("clears")
            } else if assigns_through(&body[stmt_start..at], reached) {
                Some("assigns")
            } else {
                None
            };
            if let Some(how) = how {
                out.push((open + at, seam, how));
            }
        }
    }
    out.sort_unstable();
    out
}

/// The identifier `.method(` at `dot` is called on, past any whitespace a wrap put before the
/// dot.
fn receiver(body: &str, dot: usize) -> Option<&str> {
    let b = body.as_bytes();
    let mut end = dot;
    while end > 0 && b[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_word(b[start - 1]) {
        start -= 1;
    }
    (start < end).then(|| &body[start..end])
}

/// A static's spelling: SCREAMING_CASE, a letter first (a field or a local is lowercase).
fn is_static_name(name: &str) -> bool {
    name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && name.bytes().any(|b| b.is_ascii_uppercase())
        && name.len() > 1
}

/// Whether the statement split at a lock call into `before` and `reached` assigns to what the
/// lock reaches: a plain `=` after the call (not `==`, `!=`, `<=`, `>=`, `=>`, nor a compound
/// `-=` or `+=`, which is a count's), in a statement that is no binding (`let`, `if let`,
/// `while let`, `match`).
fn assigns_through(before: &str, reached: &str) -> bool {
    let lead = before.trim_start();
    if ["let ", "if ", "while ", "match ", "else ", "return "]
        .iter()
        .any(|kw| lead.starts_with(kw))
        || before.contains('=')
    {
        return false;
    }
    let b = reached.as_bytes();
    (0..b.len()).any(|i| {
        b[i] == b'='
            && !matches!(b.get(i + 1), Some(b'=' | b'>'))
            && !(i > 0 && b"=!<>+-*/%&|^".contains(&b[i - 1]))
    })
}

/// Whether the struct `guard` holds an exclusive lock's guard as a field, so its holders run one
/// at a time.
fn holds_exclusive_lock(code: &str, guard: &str) -> bool {
    let mut from = 0;
    while let Some(found) = code[from..].find("struct ") {
        let at = from + found;
        from = at + "struct ".len();
        if at > 0 && is_word(code.as_bytes()[at - 1]) {
            continue;
        }
        let rest = &code[from..];
        let len = rest.bytes().take_while(|&b| is_word(b)).count();
        if &rest[..len] != guard {
            continue;
        }
        let after = &rest[len..];
        let end =
            after
                .find(['{', '(', ';'])
                .map_or(after.len(), |open| match after.as_bytes()[open] {
                    b'{' => brace_close(after, open).unwrap_or(after.len()),
                    b'(' => after[open..].find(';').map_or(after.len(), |e| open + e),
                    _ => open,
                });
        let fields = &after[..end];
        if fields.contains("MutexGuard<") || fields.contains("RwLockWriteGuard<") {
            return true;
        }
    }
    false
}

/// Whether `code` takes `seam` with `assert!(!SEAM.swap(true`: a second holder fails at once.
fn taken_exclusively(code: &str, seam: &str) -> bool {
    let squeezed: String = code.split_whitespace().collect();
    squeezed.contains(&format!("assert!(!{seam}.swap(true"))
}

/// Whether the reset at `at` sits in an `if` block of the Drop body opened at `open` whose
/// condition reads that no holder is left (`is_empty()`, `== 0`, `fetch_sub(`).
fn after_the_last_holder(code: &str, open: usize, at: usize) -> bool {
    let b = code.as_bytes();
    let mut depth = 0usize;
    let mut i = at;
    while i > open {
        i -= 1;
        match b[i] {
            b'}' => depth += 1,
            b'{' if depth > 0 => depth -= 1,
            b'{' => {
                let head_start = code[open + 1..i]
                    .rfind([';', '{', '}'])
                    .map_or(open + 1, |s| open + 1 + s + 1);
                let head = code[head_start..i].trim();
                let squeezed: String = head.split_whitespace().collect();
                return head.starts_with("if ")
                    && (squeezed.contains("is_empty()")
                        || squeezed.contains("==0")
                        || squeezed.contains("fetch_sub("));
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    /// 0.161.0's reap hold: a flag the guard's Drop clears.
    const FLAG: &str = "#[cfg(any(test, debug_assertions))]\n\
         static HOLD: AtomicBool = AtomicBool::new(false);\n\
         #[cfg(any(test, debug_assertions))]\n\
         pub fn hold() -> Hold {\n    \
             HOLD.store(true, Ordering::Release);\n    \
             Hold(())\n\
         }\n\
         #[cfg(any(test, debug_assertions))]\n\
         pub struct Hold(());\n\
         #[cfg(any(test, debug_assertions))]\n\
         impl Drop for Hold {\n    \
             fn drop(&mut self) {\n        \
                 HOLD.store(false, Ordering::Release);\n    \
             }\n\
         }\n";

    #[test]
    fn a_flag_a_test_guard_resets_is_found_and_its_count_is_not() {
        let count = FLAG
            .replace("AtomicBool::new(false)", "AtomicUsize::new(0)")
            .replace("AtomicBool", "AtomicUsize")
            .replace("HOLD.store(true, ", "HOLD.fetch_add(1, ")
            .replace("HOLD.store(false, ", "HOLD.fetch_sub(1, ");
        let shipped = FLAG.replace("#[cfg(any(test, debug_assertions))]\n", "");
        let found = run(
            &SEAM_RESET,
            &[
                ("crates/x/src/flag.rs", FLAG),
                ("crates/x/src/count.rs", &count),
                ("crates/x/src/shipped.rs", &shipped),
            ],
        );
        assert_eq!(spots(&found), ["crates/x/src/flag.rs:13"]);
    }

    #[test]
    fn a_restore_in_a_test_file_and_a_wrapped_swap_are_found() {
        let restore = "struct Restore(u8);\n\
             impl Drop for Restore {\n    \
                 fn drop(&mut self) {\n        \
                     crate::mode::MODE\n            \
                         .store(self.0, Ordering::SeqCst);\n        \
                     LAST.swap(0, Ordering::SeqCst);\n    \
                 }\n\
             }\n";
        let found = run(&SEAM_RESET, &[("crates/x/tests/restore.rs", restore)]);
        assert_eq!(
            spots(&found),
            ["crates/x/tests/restore.rs:5", "crates/x/tests/restore.rs:6"]
        );
    }

    #[test]
    fn a_slot_emptied_behind_its_lock_is_found_and_a_keyed_removal_is_not() {
        let slot = "#[cfg(test)]\n\
             mod tests {\n    \
                 struct Armed;\n    \
                 impl Drop for Armed {\n        \
                     fn drop(&mut self) {\n            \
                         *HOOK.lock().unwrap() = None;\n            \
                         SEEN.lock().unwrap().clear();\n            \
                         let _ = NEXT.lock().unwrap().take();\n            \
                         ARMED.lock().unwrap().remove(&self.0);\n            \
                         let held = HELD.lock().unwrap().len() == 0;\n            \
                         self.flag.store(false, Ordering::SeqCst);\n            \
                         OWN.with(|own| own.set(None));\n        \
                     }\n    \
                 }\n\
             }\n";
        let found = run(&SEAM_RESET, &[("crates/x/src/slot.rs", slot)]);
        assert_eq!(
            spots(&found),
            [
                "crates/x/src/slot.rs:6",
                "crates/x/src/slot.rs:7",
                "crates/x/src/slot.rs:8",
            ]
        );
    }

    #[test]
    fn a_reset_no_other_holder_can_be_inside_of_passes() {
        let serialised = "#[cfg(test)]\n\
             struct Override {\n    \
                 _serial: std::sync::MutexGuard<'static, ()>,\n\
             }\n\
             #[cfg(test)]\n\
             impl Drop for Override {\n    \
                 fn drop(&mut self) {\n        \
                     *DIR.write().unwrap() = None;\n    \
                 }\n\
             }\n";
        let shared = serialised.replace("MutexGuard<", "RwLockReadGuard<");
        let exclusive = "static ACTIVE: AtomicBool = AtomicBool::new(false);\n\
             struct Measuring;\n\
             impl Drop for Measuring {\n    \
                 fn drop(&mut self) {\n        \
                     ACTIVE.store(false, Ordering::SeqCst);\n    \
                 }\n\
             }\n\
             fn measure() {\n    \
                 assert!(!ACTIVE\n        .swap(true, Ordering::SeqCst));\n    \
                 let _m = Measuring;\n\
             }\n";
        let unasserted =
            exclusive.replace("assert!(!ACTIVE\n        .swap(true", "ACTIVE.store(true");
        let last = "#[cfg(test)]\n\
             impl Drop for OwnLane {\n    \
                 fn drop(&mut self) {\n        \
                     if let Ok(mut q) = INBOX.lock() {\n            \
                         q.retain(|(at, _)| *at != self.lane);\n            \
                         if q.is_empty() {\n                \
                             PENDING.store(false, Ordering::Relaxed);\n            \
                         }\n            \
                         if self.lane > 0 {\n                \
                             LAST.store(0, Ordering::Relaxed);\n            \
                         }\n        \
                     }\n        \
                     if HOLDS.fetch_sub(1, Ordering::AcqRel) == 1 {\n            \
                         HELD.store(false, Ordering::Release);\n        \
                     }\n    \
                 }\n\
             }\n";
        let found = run(
            &SEAM_RESET,
            &[
                ("crates/x/src/serialised.rs", serialised),
                ("crates/x/src/shared.rs", &shared),
                ("crates/x/tests/exclusive.rs", exclusive),
                ("crates/x/tests/unasserted.rs", &unasserted),
                ("crates/x/src/last.rs", last),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/x/src/last.rs:10",
                "crates/x/src/shared.rs:8",
                "crates/x/tests/unasserted.rs:5",
            ]
        );
    }

    #[test]
    fn a_binding_a_comparison_or_a_read_through_a_lock_is_no_reset() {
        let reads = "#[cfg(test)]\n\
             impl Drop for Rig {\n    \
                 fn drop(&mut self) {\n        \
                     if let Some(m) = WATCHES.lock().unwrap().as_mut() {\n            \
                         m.remove(&self.pid);\n        \
                     }\n        \
                     *self.seen = Some((ACTIVITY.lock().unwrap().subjects.is_empty(), 1));\n        \
                     let idle = QUEUE.lock().unwrap().next.is_none() == true;\n        \
                     assert!(LINK.try_lock().is_err());\n    \
                 }\n\
             }\n";
        assert!(run(&SEAM_RESET, &[("crates/x/src/reads.rs", reads)]).is_empty());
        assert!(assigns_through("\n    *", "HOOK.lock().unwrap() = None"));
        assert!(assigns_through("", "SLOT.lock().unwrap().field = 1"));
        assert!(!assigns_through("", "HELD.lock().unwrap().len() == 0"));
        assert!(!assigns_through("let g = ", "HELD.lock()"));
        assert!(!assigns_through("if let Ok(q) = ", "HELD.lock() {"));
        assert!(!assigns_through("match ", "HELD.lock() { Ok(g) => g }"));
        assert!(!assigns_through("", "HELD.lock().map(|g| g.len() >= 1)"));
        assert!(!assigns_through(
            "*self.0 = Some((",
            "A.lock().unwrap().x, 1))"
        ));
        assert!(!assigns_through("\n    *", "HELD.lock().unwrap() -= 1"));
    }

    #[test]
    fn statics_are_screaming() {
        assert!(is_static_name("HOLD_REAP_KILLS"));
        assert!(is_static_name("A1"));
        assert!(!is_static_name("hold"));
        assert!(!is_static_name("Hold"));
        assert!(!is_static_name("_"));
    }
}
