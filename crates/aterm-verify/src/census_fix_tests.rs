// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::collections::BTreeSet;

use super::{apply, run, run_until};
use crate::ladder::{Entry, Outcome, Report, Severity};

const PARENT: &str = "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: Apache-2.0\n\n\
                      pub fn f() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n";

fn unread(rel: &str) -> aterm_census::Unread {
    aterm_census::Unread {
        rel: rel.to_string(),
        package: "crates/x".to_string(),
        owns_dir: BTreeSet::from([false]),
        whole: false,
        modules: Vec::new(),
    }
}

/// A plan is written to disk, the new file beside its parent and the parent rewritten to mount
/// it; written again, the moved file plans nothing.
///
/// NEGATIVE CONTROL: writing only the parent leaves `ops_tests.rs` absent; writing nothing leaves
/// the module inline, so the second plan moves it again.
#[test]
fn a_plan_is_written_and_then_has_nothing_left_to_move() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("crates/x/src")).unwrap();
    std::fs::write(root.join("crates/x/src/ops.rs"), PARENT).unwrap();
    let (plan, files) = apply(root, &unread("crates/x/src/ops.rs")).unwrap();
    assert!(plan.refused.is_empty(), "{:?}", plan.refused);
    assert_eq!(files, ["crates/x/src/ops_tests.rs"]);
    let parent = std::fs::read_to_string(root.join("crates/x/src/ops.rs")).unwrap();
    assert!(
        parent.ends_with("#[cfg(test)]\n#[path = \"ops_tests.rs\"]\nmod tests;\n"),
        "{parent}"
    );
    let moved = std::fs::read_to_string(root.join("crates/x/src/ops_tests.rs")).unwrap();
    assert!(moved.ends_with("\n#[test]\nfn t() {}\n"), "{moved}");
    let (again, files) = apply(root, &unread("crates/x/src/ops.rs")).unwrap();
    assert_eq!((again.parent, files), (None, Vec::new()));
}

/// Files already where the module would go are never written over: with every name it may take
/// taken the module stays inline and the parent is untouched.
///
/// NEGATIVE CONTROL: opening the new file without `create_new` (or a planner that ignores what
/// exists) replaces `keep`.
#[test]
fn a_file_in_the_way_is_never_written_over() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("crates/x/src")).unwrap();
    std::fs::write(root.join("crates/x/src/ops.rs"), PARENT).unwrap();
    std::fs::write(root.join("crates/x/src/ops_tests.rs"), "keep").unwrap();
    std::fs::write(root.join("crates/x/src/ops_own_tests.rs"), "keep").unwrap();
    std::fs::create_dir_all(root.join("crates/x/src/ops")).unwrap();
    std::fs::write(root.join("crates/x/src/ops/ops_tests.rs"), "keep").unwrap();
    let (plan, files) = apply(root, &unread("crates/x/src/ops.rs")).unwrap();
    assert!(files.is_empty());
    assert_eq!(plan.refused.len(), 1, "{:?}", plan.refused);
    assert_eq!(
        std::fs::read_to_string(root.join("crates/x/src/ops_tests.rs")).unwrap(),
        "keep"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("crates/x/src/ops.rs")).unwrap(),
        PARENT
    );
    assert!(super::write_new(&root.join("crates/x/src/ops_tests.rs"), "x").is_err());
}

/// A file whose plan cannot be written whole is left as it was: the new file written before the
/// one that failed is removed, and the file that would mount them is never rewritten.
///
/// NEGATIVE CONTROLS: writing the parent first rewrites `ops.rs` to mount files that are not
/// there; not undoing leaves `ops_tests.rs` behind.
#[test]
fn a_plan_that_cannot_be_written_whole_leaves_its_file_as_it_was() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("crates/x/src")).unwrap();
    let text = "#[cfg(test)]\nmod tests {}\npub mod a {\n    #[cfg(test)]\n    mod tests {}\n}\n";
    std::fs::write(root.join("crates/x/src/ops.rs"), text).unwrap();
    // The folder `a`'s test module's file goes in is a file.
    std::fs::write(root.join("crates/x/src/ops"), "").unwrap();
    assert!(apply(root, &unread("crates/x/src/ops.rs")).is_err());
    assert_eq!(
        std::fs::read_to_string(root.join("crates/x/src/ops.rs")).unwrap(),
        text
    );
    assert!(!root.join("crates/x/src/ops_tests.rs").exists());
}

/// A run that meets a file it cannot write still formats what it wrote before it, then cannot
/// finish: nothing it wrote goes unformatted.
///
/// NEGATIVE CONTROL: returning at the failure books no formatting for `lib.rs`'s new file (no
/// skip row, with no formatter); passing despite it books no COULD NOT RUN.
#[test]
fn a_run_that_cannot_write_a_file_still_formats_what_it_wrote() {
    use crate::{Ctx, EnvSnapshot, Scope};
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir_all(root.join("crates/x/src")).unwrap();
    std::fs::write(
        root.join("crates/x/Cargo.toml"),
        "[package]\nname = \"x\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("crates/x/src/lib.rs"),
        "pub mod ops;\n#[cfg(test)]\nmod tests {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("crates/x/src/ops.rs"),
        "pub mod a {\n    #[cfg(test)]\n    mod tests {}\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("crates/x/src/ops"), "").unwrap();
    let mut ctx = Ctx::new(
        root.clone(),
        Scope::crate_only("x"),
        EnvSnapshot::default(),
        dir.path().join("scratch"),
    );
    ctx.tools.trustfmt = None;
    let mut r = Report::new("census");
    run(&ctx, &mut r);
    assert!(root.join("crates/x/src/lib_tests.rs").exists());
    let outcomes: Vec<&Outcome> = r
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Ladder { outcome, .. } => Some(outcome),
            Entry::Raw(_) => None,
        })
        .collect();
    assert_eq!(
        outcomes,
        [&Outcome::Skip, &Outcome::Fail(Severity::CouldNotRun)],
        "{r:?}"
    );
    let lib = std::fs::read_to_string(root.join("crates/x/src/lib.rs")).unwrap();
    assert!(lib.contains("#[path = \"lib_tests.rs\"]"), "{lib}");
}

/// A run told to stop finishes the file it is on and starts no other, then cannot finish; a
/// stale file of its own beside a parent, which a killed run left before its rename, never
/// blocks the next.
///
/// NEGATIVE CONTROLS: not asking moves `ops.rs`'s module too; writing the parent's next text
/// with `create_new` alone fails on the stale file.
#[test]
fn a_run_stops_between_files_and_a_stale_file_of_its_own_never_blocks_it() {
    use crate::{Ctx, EnvSnapshot, Scope};
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir_all(root.join("crates/x/src")).unwrap();
    std::fs::write(
        root.join("crates/x/Cargo.toml"),
        "[package]\nname = \"x\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("crates/x/src/lib.rs"),
        "pub mod ops;\n#[cfg(test)]\nmod tests {}\n",
    )
    .unwrap();
    let ops = "pub fn f() {}\n#[cfg(test)]\nmod tests {}\n";
    std::fs::write(root.join("crates/x/src/ops.rs"), ops).unwrap();
    std::fs::write(root.join("crates/x/src/lib.rs.census-fix.noindex"), "stale").unwrap();
    let mut ctx = Ctx::new(
        root.clone(),
        Scope::crate_only("x"),
        EnvSnapshot::default(),
        dir.path().join("scratch"),
    );
    ctx.tools.trustfmt = None;
    let asked = std::cell::Cell::new(0);
    let stopping = || {
        asked.set(asked.get() + 1);
        asked.get() > 1
    };
    let mut r = Report::new("census");
    run_until(&ctx, &mut r, &stopping);
    assert!(root.join("crates/x/src/lib_tests.rs").exists(), "{r:?}");
    assert!(!root.join("crates/x/src/lib.rs.census-fix.noindex").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("crates/x/src/ops.rs")).unwrap(),
        ops
    );
    assert!(
        r.entries.iter().any(|entry| matches!(
            entry,
            Entry::Ladder {
                outcome: Outcome::Fail(Severity::CouldNotRun),
                ..
            }
        )),
        "{r:?}"
    );
}
