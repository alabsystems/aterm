// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::collections::BTreeSet;

use super::{Plan, Refusal, holds, plan, sibling};
use crate::items::Source;

/// A derived file's head: its licence and what aterm changed.
const LICENCE: &str = "// Copyright 2026 Andrew Yates\n// Derived from upstream 1.0; Copyright \
                       Upstream.\n// Modifications: its tests moved.\n\
                       // SPDX-License-Identifier: Apache-2.0\n";

const HEAD: &str = concat!(
    "// Copyright 2026 Andrew Yates\n// Derived from upstream 1.0; Copyright Upstream.\n",
    "// Modifications: its tests moved.\n// SPDX-License-Identifier: Apache-2.0\n//! Ops.\n\n"
);

fn planned(rel: &str, owns: bool, text: &str, existing: &[&str]) -> Plan {
    plan(rel, "crates/x", &BTreeSet::from([owns]), text, &|path| {
        existing.contains(&path)
    })
}

fn refused(plan: &Plan) -> Vec<(&str, Refusal)> {
    plan.refused
        .iter()
        .map(|(name, why)| (name.as_str(), why.clone()))
        .collect()
}

fn taken(rel: &str) -> Refusal {
    Refusal::Taken(rel.to_string())
}

/// A top-level test module moves to `<stem>_tests.rs` beside its file, under the parent's whole
/// head (its licence and what aterm changed in what it derives from); its declaration keeps its
/// docs and attributes. Each body line loses one level of indent, but a line inside a string
/// keeps its text, unless an escaped line break before it makes its leading whitespace no part
/// of the string.
///
/// NEGATIVE CONTROLS: dedenting a line inside a raw or cooked string changes the string's value,
/// so the check refuses the move; dedenting none of a `\`-continued string's lines leaves
/// `extra` at its old indent; copying the parent's `//!` docs, which follow its licence with no
/// blank line between, puts `Ops.` in the new file, and copying only its licence lines drops
/// `Modifications`.
#[test]
fn a_top_level_test_module_moves_beside_its_file() {
    let text = format!(
        "{HEAD}pub fn f() -> u8 {{\n    1\n}}\n\n/// Its tests.\n#[cfg(test)]\nmod tests {{\n    \
         use super::f;\n\n    #[test]\n    fn t() {{\n        let s = \"a\n  b\";\n        \
         let r = r\"c\n    d\";\n        assert_eq!(f(), 1, \"one \\\n                    \
         extra\");\n    }}\n}}\n"
    );
    let got = planned("crates/x/src/ops.rs", false, &text, &[]);
    assert!(got.refused.is_empty(), "{:?}", got.refused);
    assert_eq!(
        got.parent.as_deref(),
        Some(
            format!(
                "{HEAD}pub fn f() -> u8 {{\n    1\n}}\n\n/// Its tests.\n#[cfg(test)]\n\
                 #[path = \"ops_tests.rs\"]\nmod tests;\n"
            )
            .as_str()
        )
    );
    assert_eq!(
        got.files,
        [(
            "crates/x/src/ops_tests.rs".to_string(),
            format!(
                "{LICENCE}\nuse super::f;\n\n#[test]\nfn t() {{\n    let s = \"a\n  b\";\n    \
                 let r = r\"c\n    d\";\n    assert_eq!(f(), 1, \"one \\\n                \
                 extra\");\n}}\n"
            )
        )]
    );
}

/// The new file's name: `<stem>_tests.rs` for `tests`, `<stem>_<name>.rs` for a name ending
/// `_tests`, else `<stem>_<name>_tests.rs`, so every one ends `_tests.rs`, which the census reads
/// as test code ([`crate::is_test_file`]).
///
/// NEGATIVE CONTROL: naming every file `<stem>_<name>_tests.rs` gives `lib_tests_tests.rs`.
#[test]
fn a_moved_file_is_named_for_its_parent_and_module() {
    assert_eq!(sibling("lib", "tests"), "lib_tests.rs");
    assert_eq!(sibling("ops", "proof_tests"), "ops_proof_tests.rs");
    assert_eq!(sibling("mod", "sim"), "mod_sim_tests.rs");
}

/// A test module inside a shipped inline module takes the file Rust looks for there: in the
/// folder its file's stem (unless the file owns its folder) and the inline modules name, under
/// its own name where that ends `_tests`, else under `#[path = "<name>_tests.rs"]`.
///
/// NEGATIVE CONTROLS: leaving out the stem for a plain module file puts `bar.rs`'s module in
/// `src/a/tests.rs`; adding it for a crate root puts `lib.rs`'s in `src/lib/a/tests.rs`.
#[test]
fn a_test_module_in_a_shipped_inline_module_takes_the_file_rust_looks_for() {
    let text = format!(
        "{HEAD}pub mod a {{\n    #[cfg(test)]\n    mod tests {{\n        #[test]\n        fn t() \
         {{}}\n    }}\n    #[cfg(test)]\n    mod sim {{}}\n}}\n"
    );
    let plain = planned("crates/x/src/bar.rs", false, &text, &[]);
    let files: Vec<&str> = plain.files.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(
        files,
        [
            "crates/x/src/bar/a/tests.rs",
            "crates/x/src/bar/a/sim_tests.rs"
        ]
    );
    assert_eq!(
        plain.parent.as_deref(),
        Some(
            format!(
                "{HEAD}pub mod a {{\n    #[cfg(test)]\n    mod tests;\n    #[cfg(test)]\n    \
                 #[path = \"sim_tests.rs\"]\n    mod sim;\n}}\n"
            )
            .as_str()
        )
    );
    assert!(
        plain.files[0].1.ends_with("\n#[test]\nfn t() {}\n"),
        "{:?}",
        plain.files[0]
    );
    let root = planned("crates/x/src/lib.rs", true, &text, &[]);
    assert_eq!(root.files[0].0, "crates/x/src/a/tests.rs");
}

/// A body that mounts files of its own moves to the file Rust's lookup finds, whose children
/// resolve in the folder they did inline: `<module>.rs` where every child's file is found by its
/// name, `<module>/mod.rs` where one carries `#[path]`, whose path only a `mod.rs` reads from that
/// folder.
///
/// NEGATIVE CONTROLS: moving `tests` beside its file as `ops_tests.rs`, a `#[path]`-mounted file
/// that owns `src/`, looks for `deep` at `src/deep.rs`; a flat `src/ops/pathed.rs` reads
/// `p.rs` from `src/ops/`.
#[test]
fn a_body_that_mounts_files_keeps_its_children_where_they_are() {
    let text = format!(
        "{HEAD}#[cfg(test)]\nmod tests {{\n    mod deep;\n}}\n#[cfg(test)]\nmod pathed {{\n    \
         #[path = \"p.rs\"]\n    mod q;\n}}\n"
    );
    let plain = planned("crates/x/src/ops.rs", false, &text, &[]);
    assert!(plain.refused.is_empty(), "{:?}", plain.refused);
    let files: Vec<&str> = plain.files.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(
        files,
        [
            "crates/x/src/ops/tests.rs",
            "crates/x/src/ops/pathed/mod.rs"
        ]
    );
    assert_eq!(
        plain.parent.as_deref(),
        Some(format!("{HEAD}#[cfg(test)]\nmod tests;\n#[cfg(test)]\nmod pathed;\n").as_str())
    );
    assert_eq!(
        plain.files[1].1,
        format!("{LICENCE}\n#[path = \"p.rs\"]\nmod q;\n")
    );
    let root = planned("crates/x/src/lib.rs", true, &text, &[]);
    assert_eq!(root.files[0].0, "crates/x/src/tests.rs");
    let rival = planned(
        "crates/x/src/ops.rs",
        false,
        &text,
        &["crates/x/src/ops/tests/mod.rs"],
    );
    assert_eq!(
        refused(&rival),
        [("tests", taken("crates/x/src/ops/tests/mod.rs"))]
    );
}

/// A body whose inline module carries `#[path]` moves to `<module>/mod.rs`, which resolves that
/// path in the folder the inline module did; `<module>.rs` would resolve it beside itself.
///
/// NEGATIVE CONTROL: taking a mount inside an inline module as found by name moves `tests` to
/// `src/ops/tests.rs`, where `one` is `src/ops/fixtures/one.rs` instead of
/// `src/ops/tests/fixtures/one.rs`.
#[test]
fn a_body_whose_inline_module_carries_a_path_takes_a_mod_rs() {
    let text = format!(
        "{HEAD}#[cfg(test)]\nmod tests {{\n    #[path = \"fixtures\"]\n    mod fx {{\n        mod \
         one;\n    }}\n}}\n"
    );
    let got = planned("crates/x/src/ops.rs", false, &text, &[]);
    assert!(got.refused.is_empty(), "{:?}", got.refused);
    let files: Vec<&str> = got.files.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(files, ["crates/x/src/ops/tests/mod.rs"]);
}

/// What would read differently stays inline, with why: a module that carries `#[path]` (an inner
/// `#![path]` in its body included), one whose every candidate file exists, a nested or mounting
/// module whose body includes by a relative path, whose container carries `#[path]`, or whose
/// file is reached both owning its folder and not, one with an attribute a macro may read, one
/// gated by an inner `cfg`, a generated file, and a file all of which is test-only. A taken name,
/// or one beside a `<name>/mod.rs` a lookup of that name would also find, falls back to
/// `_own_tests.rs`, so two modules never share one.
///
/// NEGATIVE CONTROLS: each refusal removed moves its module (`inc` would read `src/ops/n/data.txt`;
/// `inner` would look for `one` beside the moved file); a generated file's modules move; without
/// the fallback `taken` stays and the second `twice` is refused; without the rival check `tests`
/// takes `ops_tests.rs`.
#[test]
fn what_would_read_differently_stays_inline() {
    let text = format!(
        "{HEAD}#[cfg(test)]\nmod mounts {{\n    mod helper;\n    const D: &[u8] = \
         include_bytes!(\"d.bin\");\n}}\n#[cfg(test)]\n#[path = \
         \"p.rs\"]\nmod pathed {{}}\n#[cfg(test)]\nmod taken {{}}\n#[cfg(test)]\nmod full {{}}\n\
         #[cfg(all(test, unix))]\nmod \
         twice {{}}\n#[cfg(all(test, windows))]\nmod twice {{}}\nmod n {{\n    #[cfg(test)]\n    \
         mod inc {{\n        const D: &str = include_str!(\"data.txt\");\n    }}\n}}\n#[path = \
         \"q.rs\"]\nmod q {{\n    #[cfg(test)]\n    mod tests {{}}\n}}\n"
    );
    let got = planned(
        "crates/x/src/ops.rs",
        false,
        &text,
        &[
            "crates/x/src/ops_taken_tests.rs",
            "crates/x/src/ops_full_tests.rs",
            "crates/x/src/ops_full_own_tests.rs",
            "crates/x/src/ops/ops_full_tests.rs",
        ],
    );
    assert_eq!(
        refused(&got),
        [
            ("mounts", Refusal::RelativeInclude),
            ("pathed", Refusal::Pathed),
            ("full", taken("crates/x/src/ops_full_tests.rs")),
            ("inc", Refusal::RelativeInclude),
            ("tests", Refusal::ContainerPathed),
        ]
    );
    let files: Vec<&str> = got.files.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(
        files,
        [
            "crates/x/src/ops_taken_own_tests.rs",
            "crates/x/src/ops_twice_tests.rs",
            "crates/x/src/ops_twice_own_tests.rs",
        ]
    );

    let nested = format!("{HEAD}mod n {{\n    #[cfg(test)]\n    mod tests {{}}\n}}\n");
    let both = plan(
        "crates/x/src/n.rs",
        "crates/x",
        &BTreeSet::from([false, true]),
        &nested,
        &|_| false,
    );
    assert_eq!(refused(&both), [("tests", Refusal::Ownership)]);

    let sealed = format!(
        "{}{HEAD}#[cfg(test)]\nmod tests {{}}\n",
        crate::seal::line("//", "gen")
    );
    let generated = planned("crates/x/src/g.rs", false, &sealed, &[]);
    assert_eq!(refused(&generated), [("tests", Refusal::Generated)]);
    assert!(generated.parent.is_none());

    let whole = planned(
        "crates/x/src/w.rs",
        false,
        "#![cfg(test)]\nfn f() {}\n",
        &[],
    );
    assert_eq!(refused(&whole), [("", Refusal::Whole)]);

    let read = format!(
        "{HEAD}#[cfg(test)]\n#[some_macro]\nmod rewritten {{}}\nmod gated {{\n    #![cfg(test)]\n}}\n\
         #[cfg(test)]\nmod inner {{\n    #![path = \"x\"]\n    mod one;\n}}\n#[cfg(test)]\nmod deep {{\n    \
         mod n {{\n        #![path = \"y\"]\n    }}\n}}\n"
    );
    let got = planned("crates/x/src/ops.rs", false, &read, &[]);
    assert_eq!(
        refused(&got),
        [
            ("rewritten", Refusal::Rewritten),
            ("gated", Refusal::InnerGate),
            ("inner", Refusal::Pathed),
            ("deep", Refusal::Pathed),
        ]
    );
    let beside = planned(
        "crates/x/src/ops.rs",
        false,
        &format!("{HEAD}#[cfg(test)]\nmod tests {{}}\n"),
        &["crates/x/src/ops_tests/mod.rs"],
    );
    let files: Vec<&str> = beside.files.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(files, ["crates/x/src/ops_own_tests.rs"]);
}

/// The check before anything is written refuses a plan whose new file holds other code, whose
/// parent's shipped code changed (a token, a comment's text, or where a comment sits among the
/// code), or that leaves a moved module inline.
///
/// NEGATIVE CONTROLS: a check that compares nothing passes them all; one that compares only the
/// code passes the doc edit, and one that compares comments' text alone passes the moved comment.
#[test]
fn a_plan_that_does_not_read_as_the_code_it_came_from_is_refused() {
    let text = format!("{HEAD}pub fn f() {{}}\n#[cfg(test)]\nmod tests {{\n    fn t() {{}}\n}}\n");
    let good = planned("crates/x/src/ops.rs", false, &text, &[]);
    let parent = good.parent.clone().unwrap();
    let before = Source::read(text.clone()).unwrap();
    let modules = before.test_modules();
    let moved = vec![(
        &modules[0],
        Some("ops_tests.rs".to_string()),
        good.files[0].0.clone(),
    )];
    assert_eq!(holds(&before, &parent, &moved, &good.files), Ok(()));

    let mut other = good.files.clone();
    other[0].1 = other[0].1.replace("fn t()", "fn u()");
    assert_eq!(
        holds(&before, &parent, &moved, &other),
        Err("`crates/x/src/ops_tests.rs` holds different code".to_string())
    );
    let shipped = parent.replace("pub fn f()", "pub fn g()");
    assert_eq!(
        holds(&before, &shipped, &moved, &good.files),
        Err("its shipped code changes".to_string())
    );
    let doc = parent.replace("//! Ops.", "//! Other ops.");
    assert_eq!(
        holds(&before, &doc, &moved, &good.files),
        Err("its shipped code changes".to_string())
    );
    // The same comments in the same order, one of them on the other side of `f`.
    let after_f = text.replace("pub fn f() {}\n", "pub fn f() {}\n// End.\n");
    let good_after_f = planned("crates/x/src/ops.rs", false, &after_f, &[]);
    let before_after_f = Source::read(after_f.clone()).unwrap();
    let moved_after_f = vec![(
        &before_after_f.test_modules()[0],
        Some("ops_tests.rs".to_string()),
        good_after_f.files[0].0.clone(),
    )];
    let kept = good_after_f.parent.clone().unwrap();
    assert_eq!(
        holds(&before_after_f, &kept, &moved_after_f, &good_after_f.files),
        Ok(())
    );
    let placed = kept.replace("pub fn f() {}\n// End.\n", "// End.\npub fn f() {}\n");
    assert_eq!(
        holds(
            &before_after_f,
            &placed,
            &moved_after_f,
            &good_after_f.files
        ),
        Err("its shipped code changes".to_string())
    );
    assert_eq!(
        holds(&before, &text, &moved, &good.files),
        Err("a moved module is still inline".to_string())
    );
}

/// A file Cargo would build as a target of its own is never taken: a binary root's module moves
/// into a folder named for it, and a build script's mounting module that only `tests/mod.rs`
/// would hold stays.
///
/// NEGATIVE CONTROL: without the check `tool.rs`'s module takes `src/bin/tool_tests.rs`, a
/// binary of its own, and `build.rs`'s takes `tests/mod.rs`, an integration test.
#[test]
fn a_file_cargo_builds_on_its_own_is_never_taken() {
    let text = format!("{HEAD}fn main() {{}}\n#[cfg(test)]\nmod tests {{}}\n");
    let bin = planned("crates/x/src/bin/tool.rs", true, &text, &[]);
    let files: Vec<&str> = bin.files.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(files, ["crates/x/src/bin/tool/tool_tests.rs"]);
    assert!(
        bin.parent
            .as_deref()
            .is_some_and(|p| p.contains("#[path = \"tool/tool_tests.rs\"]")),
        "{bin:?}"
    );
    let script = format!(
        "{HEAD}fn main() {{}}\n#[cfg(test)]\nmod tests {{\n    #[path = \"p.rs\"]\n    mod p;\n}}\n"
    );
    let build = planned("crates/x/build.rs", true, &script, &[]);
    assert_eq!(
        refused(&build),
        [(
            "tests",
            Refusal::Target("crates/x/tests/mod.rs".to_string())
        )]
    );
}

/// A `path` anywhere in a moved module's inner attributes is refused, and a moved body that reads
/// its own place is listed.
///
/// NEGATIVE CONTROLS: reading only `#![path` moves `conditional`; not scanning for `file!`
/// leaves `located` empty.
#[test]
fn an_inner_path_stays_and_a_body_that_reads_its_place_is_listed() {
    let text = format!(
        "{HEAD}#[cfg(test)]\nmod conditional {{\n    #![cfg_attr(unix, path = \"p\")]\n    mod deep;\n}}\n\
         #[cfg(test)]\nmod tests {{\n    fn here() -> &'static str {{\n        file!()\n    }}\n}}\n"
    );
    let got = planned("crates/x/src/ops.rs", false, &text, &[]);
    assert_eq!(refused(&got), [("conditional", Refusal::Pathed)]);
    assert_eq!(got.located, ["tests"]);
}
