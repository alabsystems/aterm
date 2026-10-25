// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The census of the Aos `compile_fail` locks (design §13.7): deleting one fails here, where otherwise it would pass silently.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A lock: its source file under `crates/`, its error code, and its first body line that is neither hidden (`# `) nor a `use`.
struct Lock {
    file: &'static str,
    code: &'static str,
    line: &'static str,
}

/// The Aos sources, as a directory under `crates/` and the prefix a file's path below it must have.
const ROOTS: &[(&str, &str)] = &[
    ("aterm-aos/src", ""),
    ("aterm-models-aos/src", ""),
    ("aterm-effects/src", "aos_"),
    ("aterm-gpu/src/compute", ""),
    ("aterm-lab/src", "aos_"),
];

const M4: &str = "aterm-effects/src/aos_momentum/m4.rs";
const PUBLISH: &str = "aterm-effects/src/aos_momentum/publish.rs";
const DIST: &str = "aterm-effects/src/aos_momentum/dist.rs";
const SEARCH: &str = "aterm-effects/src/aos_momentum/search.rs";
const M6: &str = "aterm-effects/src/aos_momentum/m6.rs";
const JOB: &str = "aterm-gpu/src/compute/job.rs";

const LOCKS: &[Lock] = &[
    Lock {
        file: M4,
        code: "E0451",
        line: "let _ = Admitted {",
    },
    Lock {
        file: M4,
        code: "E0277",
        line: "fn copy(a: &Admitted) -> Admitted { Clone::clone(a) }",
    },
    Lock {
        file: M4,
        code: "E0451",
        line: "let _ = AcceptancePolicy { id: todo!(), terms: todo!() };",
    },
    Lock {
        file: M4,
        code: "E0451",
        line: "let _ = TrainFeedback { plan: todo!(), admitted: true, law: None, ticks: Some(1) };",
    },
    Lock {
        file: M4,
        code: "E0624",
        line: "let _ = TrainFeedback::admitted(Digest([0; 32]), 1);",
    },
    Lock {
        file: M4,
        code: "E0624",
        line: "let _ = PrebuiltMomentum::planned(SHIPPED_PLAN, \"build\");",
    },
    Lock {
        file: M4,
        code: "E0451",
        line: "let _ = Selection {",
    },
    Lock {
        file: M4,
        code: "E0423",
        line: "let _ = CandidateBundle(todo!());",
    },
    Lock {
        file: M4,
        code: "E0382",
        line: "fn twice(ev: &Evaluator) {",
    },
    Lock {
        file: M4,
        code: "E0451",
        line: "let _ = TrainLabel { id: todo!(), plan: todo!(), outcome: todo!() };",
    },
    Lock {
        file: M4,
        code: "E0624",
        line: "let _ = TrainLabel::of(todo!(), todo!());",
    },
    Lock {
        file: M4,
        code: "E0599",
        line: "fn peek(l: &TrainLabel) -> Option<u64> { l.held_out_ticks() }",
    },
    Lock {
        file: M4,
        code: "E0616",
        line: "fn peek(l: &TrainLabel) { let _ = &l.outcome; }",
    },
    Lock {
        file: M4,
        code: "E0451",
        line: "let _ = LookToken { bound: todo!() };",
    },
    Lock {
        file: M4,
        code: "E0451",
        line: "let _ = ReadToken { bound: todo!() };",
    },
    Lock {
        file: M4,
        code: "E0451",
        line: "let _ = ReplayToken { bound: todo!() };",
    },
    Lock {
        file: M4,
        code: "E0277",
        line: "fn copy(t: &LookToken) -> LookToken { Clone::clone(t) }",
    },
    Lock {
        file: M4,
        code: "E0277",
        line: "fn copy(t: &ReadToken) -> ReadToken { Clone::clone(t) }",
    },
    Lock {
        file: M4,
        code: "E0277",
        line: "fn copy(t: &ReplayToken) -> ReplayToken { Clone::clone(t) }",
    },
    Lock {
        file: M4,
        code: "E0382",
        line: "fn look_twice(ev: &Evaluator, t: LookToken, plans: &[[u8; 12]]) {",
    },
    Lock {
        file: M4,
        code: "E0382",
        line: "fn read_twice(ev: &Evaluator, t: ReadToken, plans: &[[u8; 12]]) {",
    },
    Lock {
        file: M4,
        code: "E0382",
        line: "fn replay_twice(ev: &Evaluator, t: ReplayToken, plans: &[[u8; 12]]) {",
    },
    Lock {
        file: M4,
        code: "E0308",
        line: "fn gate(ev: &Evaluator, o: PoolOutcome) { let _ = ev.held_out(o); }",
    },
    Lock {
        file: PUBLISH,
        code: "E0382",
        line: "fn twice(host: &mut MomentumHost, admitted: Admitted) {",
    },
    Lock {
        file: DIST,
        code: "E0616",
        line: "fn rows(c: &Claim) -> usize { c.rows.len() }",
    },
    Lock {
        file: DIST,
        code: "E0308",
        line: "fn gate(ev: &Evaluator, c: Claim) { let _ = ev.held_out(c); }",
    },
    Lock {
        file: SEARCH,
        code: "E0599",
        line: "fn peek(f: &TrainFeedback) -> Option<u64> {",
    },
    Lock {
        file: SEARCH,
        code: "E0616",
        line: "fn peek(f: &TrainFeedback) -> Option<u64> {",
    },
    Lock {
        file: JOB,
        code: "E0599",
        line: "// `out` on a prepared job.",
    },
    Lock {
        file: JOB,
        code: "E0599",
        line: "// `out` on a job in flight.",
    },
    Lock {
        file: JOB,
        code: "E0599",
        line: "// `load` on a job in flight.",
    },
    Lock {
        file: JOB,
        code: "E0382",
        line: "// The job after `submit` moved it.",
    },
    Lock {
        file: JOB,
        code: "E0599",
        line: "// A write to sealed params.",
    },
    Lock {
        file: JOB,
        code: "E0594",
        line: "// A write through the bytes sealed params read back.",
    },
    Lock {
        file: JOB,
        code: "E0616",
        line: "// A write to a field of sealed params held uniquely.",
    },
    Lock {
        file: JOB,
        code: "E0599",
        line: "// `load` on a completed job that was not recycled.",
    },
    Lock {
        file: JOB,
        code: "E0277",
        line: "// A job sent to another thread.",
    },
    Lock {
        file: M6,
        code: "E0451",
        line: "let _ = ModelAdmitted {",
    },
    Lock {
        file: M6,
        code: "E0277",
        line: "fn copy(a: &ModelAdmitted) -> ModelAdmitted { Clone::clone(a) }",
    },
    Lock {
        file: M6,
        code: "E0382",
        line: "fn twice<S: DurableStore>(store: &mut Store<S>, a: ModelAdmitted, ledger: Digest) {",
    },
];

/// Each `compile_fail` block of `src`'s doc comments (`///` and `//!`), as its error code (empty when it names none) and its first line that is neither hidden nor a `use`.
fn blocks(src: &str) -> Vec<(String, String)> {
    enum State {
        Outside,
        Other,
        Lock(String, Vec<String>),
    }
    let mut out = Vec::new();
    let mut state = State::Outside;
    for raw in src.lines() {
        let t = raw.trim_start();
        let Some(doc) = t
            .strip_prefix("//!")
            .or_else(|| t.strip_prefix("///").filter(|d| !d.starts_with('/')))
        else {
            continue;
        };
        let doc = doc.trim();
        let fence = doc.strip_prefix("```");
        state = match (state, fence) {
            (State::Outside, Some(attrs)) => {
                let attrs: Vec<&str> = attrs.split(',').map(str::trim).collect();
                if attrs.contains(&"compile_fail") {
                    let code = attrs
                        .iter()
                        .find(|a| {
                            a.strip_prefix('E').is_some_and(|n| {
                                !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
                            })
                        })
                        .map_or_else(String::new, |a| (*a).to_owned());
                    State::Lock(code, Vec::new())
                } else {
                    State::Other
                }
            }
            (State::Lock(code, body), Some(_)) => {
                let first = body
                    .into_iter()
                    .find(|l| !(l == "#" || l.starts_with("# ") || l.starts_with("use ")))
                    .unwrap_or_default();
                out.push((code, first));
                State::Outside
            }
            (State::Other, Some(_)) => State::Outside,
            (State::Lock(code, mut body), None) => {
                body.push(doc.to_owned());
                State::Lock(code, body)
            }
            (state, None) => state,
        };
    }
    assert!(
        matches!(state, State::Outside),
        "an unclosed doc code block"
    );
    out
}

/// Every `.rs` file under `dir`, recursively.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
}

/// Every lock the census names is a `compile_fail` block of its file under its error code, and no Aos source holds a lock the census does not name.
#[test]
fn aos_locks_are_present() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits under crates/");
    let mut found = BTreeSet::new();
    for &(root, prefix) in ROOTS {
        let dir = crates.join(root);
        let mut files = Vec::new();
        sources(&dir, &mut files);
        for path in files {
            let below = path.strip_prefix(&dir).expect("under its root");
            if !below.to_string_lossy().starts_with(prefix) {
                continue;
            }
            let file = path
                .strip_prefix(crates)
                .expect("under crates/")
                .to_string_lossy()
                .into_owned();
            let src = std::fs::read_to_string(&path).expect("a readable source");
            for (code, line) in blocks(&src) {
                assert!(
                    !code.is_empty(),
                    "{file}: the compile_fail lock `{line}` names no error code; every Aos lock pins its code"
                );
                assert!(
                    found.insert((file.clone(), code.clone(), line.clone())),
                    "{file}: two {code} locks begin `{line}`; give one a distinct first line"
                );
            }
        }
    }
    let named: BTreeSet<(String, String, String)> = LOCKS
        .iter()
        .map(|l| (l.file.to_owned(), l.code.to_owned(), l.line.to_owned()))
        .collect();
    assert_eq!(named.len(), LOCKS.len(), "the census names a lock twice");
    let gone: Vec<String> = named
        .difference(&found)
        .map(|(file, code, line)| format!("the {code} lock `{line}` of {file}"))
        .collect();
    assert!(
        gone.is_empty(),
        "locks gone: restore them (a pruning wave deleted one before): {gone:#?}"
    );
    let unnamed: Vec<String> = found
        .difference(&named)
        .map(|(file, code, line)| format!("the {code} lock `{line}` of {file}"))
        .collect();
    assert!(
        unnamed.is_empty(),
        "locks the census does not name: name them here: {unnamed:#?}"
    );
}

/// The parser reads `///` and `//!` blocks fenced with `compile_fail` among other attributes, skips other blocks and hidden and `use` lines, and reports a lock with no code.
#[test]
fn the_census_reads_every_lock_form() {
    let fence = "```";
    let src = [
        format!("//! {fence}compile_fail,E0001"),
        "//! # use a::b;".to_owned(),
        "//! use a::c;".to_owned(),
        "//! let one = 1;".to_owned(),
        format!("//! {fence}"),
        format!("/// {fence}rust,compile_fail,E0002"),
        "/// fn two() {}".to_owned(),
        format!("/// {fence}"),
        format!("/// {fence}"),
        "/// let not_a_lock = 0;".to_owned(),
        format!("/// {fence}"),
        format!("/// {fence}compile_fail"),
        "/// let three = 3;".to_owned(),
        format!("/// {fence}"),
        format!("//// {fence}compile_fail,E0004"),
    ]
    .join("\n");
    assert_eq!(
        blocks(&src),
        [
            ("E0001".to_owned(), "let one = 1;".to_owned()),
            ("E0002".to_owned(), "fn two() {}".to_owned()),
            (String::new(), "let three = 3;".to_owned()),
        ]
    );
}
