// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const AT: &str = "crates/aterm-gui/src/app_input.rs";

fn found(src: &str) -> Vec<String> {
    spots(&run(&TERM_WRITE, &[(AT, src)]))
}

#[test]
fn every_shape_of_a_write_through_a_raw_guard_is_found() {
    let cases: &[(&str, &str, usize)] = &[
        (
            "let Ok(mut …) = try_lock",
            "fn f(s: &S) {\n    let Ok(mut t) = s.term.try_lock() else {\n        \
             return;\n    };\n    t.text_selection_mut().clear();\n}\n",
            2,
        ),
        (
            "let mut … = lock().unwrap()",
            "fn f(term: &Mutex<Terminal>) {\n    let mut t = term.lock().unwrap();\n    \
             t.resize(2, 3);\n}\n",
            2,
        ),
        (
            "if let Some(mut …) after ok()",
            "fn f(s: &S) {\n    if let Some(mut t) = s.term.try_lock().ok() {\n        \
             t.scroll_display(1);\n    }\n}\n",
            2,
        ),
        (
            "a match arm binding mut",
            "fn f(h: &H) {\n    match h.term.try_lock() {\n        Ok(mut t) => t.reset(),\n        \
             Err(_) => {}\n    }\n}\n",
            2,
        ),
        (
            "a later match arm, after a block arm",
            "fn f(h: &H) {\n    match self.pool.get(id)?.term.try_lock() {\n        \
             Err(TryLockError::WouldBlock) => {\n            return;\n        }\n        \
             Err(TryLockError::Poisoned(mut p)) => p.get_mut().reset(),\n        \
             Ok(t) => t.rows(),\n    };\n}\n",
            2,
        ),
        (
            "a mut binding of the match's value",
            "fn f(h: &H) {\n    let mut t = match h.term.try_lock() {\n        \
             Ok(t) => t,\n        Err(_) => return,\n    };\n}\n",
            2,
        ),
        (
            "a closure taking it mut",
            "fn f(s: &S) {\n    s.term.try_lock().map(|mut t| t.reset());\n}\n",
            2,
        ),
        (
            "&mut of the guard",
            "fn f(term: &Mutex<Terminal>) {\n    apply(&mut term.lock().unwrap(), 3);\n}\n",
            2,
        ),
        (
            "&mut * of the guard",
            "fn f(term: &Mutex<Terminal>) {\n    apply(&mut *term.lock_or_recover());\n}\n",
            2,
        ),
        (
            "a method on the guard itself, wrapped by rustfmt",
            "fn f(s: &S) {\n    s.term\n        .lock()\n        .unwrap_or_else(|p| \
             p.into_inner())\n        .process(b\"x\");\n}\n",
            5,
        ),
        (
            "a method on the guard after ok()?",
            "fn f(s: &S) -> Option<()> {\n    s.term.try_lock().ok()?.resize(1, 2);\n    \
             Some(())\n}\n",
            2,
        ),
        (
            "an assignment through it",
            "fn f(term: &Mutex<Terminal>, other: Terminal) {\n    *term.lock().unwrap() = \
             other;\n}\n",
            2,
        ),
        (
            "a raw guard type handed out",
            "fn engine(&self) -> MutexGuard<'_, Terminal> {\n    todo!()\n}\n",
            1,
        ),
    ];
    for (why, src, line) in cases {
        assert_eq!(found(src), [format!("{AT}:{line}")], "MISS — {why}:\n{src}");
    }
}

#[test]
fn reads_other_locks_test_code_and_prose_are_silent() {
    let reads = "fn f(s: &S, term: &Mutex<Terminal>) -> Option<u16> {\n\
                 let Ok(t) = s.term.try_lock() else { return None };\n\
                 let fresh = match s.term.try_lock() {\n    Ok(t) => Some(label(&t)),\n    \
                 Err(TryLockError::Poisoned(p)) => Some(label(&p.into_inner())),\n    \
                 Err(TryLockError::WouldBlock) => None,\n};\n\
                 let vi = s.term.try_lock().is_ok_and(|t| t.vi_is_active());\n\
                 let e = s.term.try_lock().ok().map(|t| t.title_epoch());\n\
                 let t = h.term.try_lock().ok()?;\n\
                 let k = Fingerprint::of(&term.lock().unwrap());\n\
                 let r = term.try_lock().map_or(fallback, |t| (t.rows(), t.cols()));\n\
                 if let Some(t) = patiently(p, || s.term.try_lock()) {}\n\
                 let mut n = term_lock(term);\n\
                 let mut m = self.meta.lock().unwrap();\n\
                 let mut c = s.cast.try_lock().unwrap();\n\
                 let e = term.lock().unwrap() == other;\n\
                 // let mut t = term.lock().unwrap();\n\
                 let s = \"let Ok(mut t) = s.term.try_lock()\";\n\
                 None\n}\n\
                 #[cfg(test)]\nmod tests {\n    fn t(term: &Mutex<Terminal>) {\n        \
                 term.lock().unwrap().process(b\"x\");\n    }\n}\n";
    let lib = "pub(crate) fn term_lock(term: &Mutex<Terminal>) -> TermGuard<'_> {\n    \
               let guard = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());\n    \
               TermGuard { guard }\n}\n\
               pub(crate) struct TermGuard<'a> {\n    guard: MutexGuard<'a, Terminal>,\n}\n\
               #[cfg(all(test, unix))]\n#[path = \"rig_conformance.rs\"]\nmod rig;\n";
    let write = "fn f(term: &Mutex<Terminal>) {\n    term.lock().unwrap().process(b\"x\");\n}\n";
    let found = run(
        &TERM_WRITE,
        &[
            (AT, reads),
            (GUARD_HOME, lib),
            ("crates/aterm-gui/src/rig_conformance.rs", write),
            ("crates/aterm-gui/src/lib_tests.rs", write),
            ("crates/aterm-control/src/conformance.rs", write),
        ],
    );
    assert!(found.is_empty(), "{found:?}");
    let found = run(
        &TERM_WRITE,
        &[
            (GUARD_HOME, "#[path = \"rig_conformance.rs\"]\nmod rig;\n"),
            ("crates/aterm-gui/src/rig_conformance.rs", write),
        ],
    );
    assert_eq!(spots(&found), ["crates/aterm-gui/src/rig_conformance.rs:2"]);
}
