// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn e(path: &str, byte: char) -> Entry {
    Entry {
        mode: "100644".into(),
        object: byte.to_string().repeat(40),
        path: path.into(),
    }
}

fn plan(entries: &[Entry], context: &str) -> Plan {
    from_entries(entries, context, None)
}

fn appkit_plan(entries: &[Entry], context: &str) -> Plan {
    from_entries(entries, "tree-context", Some(AppkitContext { context }))
}

/// A cfg(test)-only edit changes the bounded readers. Readers whose complete closure is not
/// declared have no key at either tree and therefore must run. Negative control: salting the
/// context differently moves every bounded key, as a verifier, harness, environment or
/// toolchain change must.
#[test]
fn a_test_only_edit_moves_only_the_bounded_check_inputs() {
    let base = vec![
        e("Cargo.toml", '1'),
        e("Cargo.lock", '2'),
        e("crates/aterm-gui/Cargo.toml", '3'),
        e("crates/aterm-gui/src/app.rs", '4'),
        e("crates/aterm-db/Cargo.toml", '5'),
        e("crates/aterm-db/src/lib.rs", '6'),
        e("crates/aterm-forge/src/lib.rs", '7'),
        e("crates/aterm-libc/src/lib.rs", '8'),
        e("crates/aterm-models-gui/src/lib.rs", '9'),
    ];
    let mut hop = base.clone();
    hop[3].object = "a".repeat(40);
    let before = plan(&base, "context-a");
    let after = plan(&hop, "context-a");
    assert_ne!(before.input("test").key, after.input("test").key);
    for undeclared in ["fmt", "lint", "doc", "census", "forge", "libc", "spec"] {
        assert!(!before.input(undeclared).bounded(), "{undeclared}");
        assert!(!after.input(undeclared).bounded(), "{undeclared}");
    }

    let other_context = plan(&base, "context-b");
    for check in aterm_verify::checks::RELEASE_TREE {
        let a = before.input(check.name());
        let b = other_context.input(check.name());
        if a.bounded() {
            assert_ne!(a.key, b.key, "{} binds the execution context", check.name());
        } else {
            assert_eq!(a.key, None, "{} stays fail-closed", check.name());
            assert_eq!(b.key, None, "{} stays fail-closed", check.name());
        }
    }
}

/// A fixture is part of the aggregate test input even when no executable byte names it. It is
/// outside fmt's declared reader; every check with an undeclared closure still runs. This is
/// the check-level half of the binary fixture negative control: the binary key further
/// narrows which reader runs.
#[test]
fn a_fixture_change_moves_the_test_input_without_pretending_it_is_source() {
    let base = vec![
        e("Cargo.toml", '1'),
        e("Cargo.lock", '2'),
        e("crates/aterm-db/Cargo.toml", '3'),
        e("crates/aterm-db/src/lib.rs", '4'),
        e("crates/aterm-db/tests/fixtures/rows.bin", '5'),
    ];
    let mut hop = base.clone();
    hop[4].object = "6".repeat(40);
    let before = plan(&base, "context");
    let after = plan(&hop, "context");
    assert_ne!(before.input("test").key, after.input("test").key);
    for undeclared in ["fmt", "lint", "doc", "census", "forge", "libc", "spec"] {
        assert!(!before.input(undeclared).bounded(), "{undeclared}");
        assert!(!after.input(undeclared).bounded(), "{undeclared}");
    }
}

/// AppKit is never carried as a whole-check source approximation. Its recorded context is still
/// canonical and lets the verifier accept only immediately-prior group proofs made by the same
/// frozen verifier and admission contract. The exact current executables are rebuilt and hashed
/// before either group can avoid opening windows.
#[test]
fn appkit_whole_check_stays_unbounded_and_records_its_own_context() {
    let base = vec![
        e("Cargo.toml", '1'),
        e("Cargo.lock", '2'),
        e("crates/aterm-gui/Cargo.toml", '3'),
        e("crates/aterm-gui/examples/objc_live_class_audit.rs", '4'),
        e("crates/aterm-window/Cargo.toml", '5'),
        e(
            "crates/aterm-window/src/platform_impl/macos/window_delegate.rs",
            '6',
        ),
        e("crates/aterm-window/build.rs", '7'),
        e("crates/unrelated/Cargo.toml", '8'),
        e("crates/unrelated/src/lib.rs", '9'),
        e("unknown/input.txt", 'a'),
    ];
    let before = appkit_plan(&base, "bundle-context").input("appkit");
    assert!(!before.bounded());
    assert_eq!(before.context, "bundle-context");
    let mut other_tree = base.clone();
    other_tree[3].object = "c".repeat(40);
    let changed = appkit_plan(&other_tree, "bundle-context").input("appkit");
    assert!(!changed.bounded());
    assert_eq!(changed.context, before.context);
    let other_context = appkit_plan(&base, "other-bundle-context").input("appkit");
    assert_ne!(before.context, other_context.context);
    assert!(
        appkit_plan(&other_tree, "bundle-context")
            .input("test")
            .bounded(),
        "AppKit's finer proof does not weaken another check's declaration"
    );
}

/// `fmt` is deliberately unbounded: ignored/untracked source or config files and repository
/// or global Git exclude state can change its reader set without changing the commit tree.
#[test]
fn fmt_has_no_cross_hop_key_for_a_commit_tree_alone() {
    let base = vec![
        e("Cargo.toml", '1'),
        e("crates/a/Cargo.toml", '2'),
        e("crates/a/src/lib.rs", '3'),
        e("crates/a/.gitignore", '4'),
    ];
    let mut hop = base.clone();
    hop[3].object = "5".repeat(40);
    assert!(!plan(&base, "context").input("fmt").bounded());
    assert!(!plan(&hop, "context").input("fmt").bounded());
}

/// Facts are compared before the qualify pool exists, while the actual verifier runs inside
/// it. No currently bounded whole-check verdict can cross a changed tree, but the context is
/// still recorded with the aggregate test receipt and must stay identical across this
/// production boundary. NEGATIVE CONTROL: an ordinary effective-environment change moves it.
#[test]
fn release_pool_coordination_does_not_move_whole_check_context() {
    let command = |pool: Option<&str>, ordinary: &str| {
        let mut command = Command::new("verify-fixture");
        command
            .current_dir("fixture-repository")
            .env("ATERM_CONTENT_CONTROL", ordinary);
        match pool {
            Some(value) => {
                command.env(aterm_capacity::POOL_ENV, value);
            }
            None => {
                command.env_remove(aterm_capacity::POOL_ENV);
            }
        }
        command
    };
    let outside = context(
        "tools",
        "seal",
        &command(None, "same"),
        ReleaseHalf::Tree,
        Admission::WaitDefault,
    );
    let inside = context(
        "tools",
        "seal",
        &command(Some("12:/tmp/qualify.jobs"), "same"),
        ReleaseHalf::Tree,
        Admission::WaitDefault,
    );
    assert_eq!(outside, inside);
    assert_ne!(
        outside,
        context(
            "tools",
            "seal",
            &command(None, "changed"),
            ReleaseHalf::Tree,
            Admission::WaitDefault,
        )
    );
    assert_ne!(
        inside,
        context(
            "tools",
            "seal",
            &command(Some("12:/tmp/qualify.jobs"), "same"),
            ReleaseHalf::Bundle,
            Admission::WaitDefault,
        ),
        "a behavior-affecting verifier mode is part of the context"
    );
    assert_ne!(
        inside,
        context(
            "tools",
            "seal",
            &command(Some("12:/tmp/qualify.jobs"), "same"),
            ReleaseHalf::Tree,
            Admission::Now,
        ),
        "the early gate's non-waiting admission is part of its proof context"
    );
}

/// The Unix tree wrapper is part of the child that a whole-check context describes, not a
/// scheduling decoration. NEGATIVE CONTROLS: changing only its program, or each one of its
/// arguments in turn, must move the context independently.
#[test]
fn whole_check_context_binds_the_verifier_program_and_each_argument() {
    let command = |program: &str, arguments: &[&str]| {
        let mut command = Command::new(program);
        command
            .args(arguments)
            .current_dir("fixture-repository")
            .env("ATERM_CONTENT_CONTROL", "same");
        command
    };
    let arguments = ["-n", "10", "/frozen/verifier"];
    let base = context(
        "tools",
        "seal",
        &command("/usr/bin/nice", &arguments),
        ReleaseHalf::Tree,
        Admission::WaitDefault,
    );
    assert_ne!(
        base,
        context(
            "tools",
            "seal",
            &command("/fixture/not-nice", &arguments),
            ReleaseHalf::Tree,
            Admission::WaitDefault,
        ),
        "the wrapper program is an input even when every argument is equal"
    );
    for (index, replacement) in ["--adjustment", "11", "/other/verifier"]
        .into_iter()
        .enumerate()
    {
        let mut changed = arguments;
        changed[index] = replacement;
        assert_ne!(
            base,
            context(
                "tools",
                "seal",
                &command("/usr/bin/nice", &changed),
                ReleaseHalf::Tree,
                Admission::WaitDefault,
            ),
            "wrapper argument {index} is an independent context input"
        );
    }
}

/// Fine-grained test and lint receipts share one hop-stable salt. A changed frozen tool set
/// or build-input seal invalidates every item even when its own bytes are equal.
#[test]
fn item_context_binds_the_frozen_tools_and_build_inputs() {
    let base = item_context("tools-a", "seal-a");
    assert_eq!(base, item_context("tools-a", "seal-a"));
    assert_ne!(base, item_context("tools-b", "seal-a"));
    assert_ne!(base, item_context("tools-a", "seal-b"));
}

#[test]
fn ls_tree_records_are_bounded_before_they_are_keys() {
    let text = format!(
        "100644 blob {}\tcrates/a/Cargo.toml\0100644 blob {}\tcrates/a/src/lib.rs\0",
        "a".repeat(40),
        "b".repeat(40)
    );
    let entries = parse_entries(text.as_bytes()).unwrap();
    assert_eq!(entries.len(), 2);
    for bad in [
        b"100644 blob abc\tp\0".as_slice(),
        b"100644 tree aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tp\0".as_slice(),
        b"100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa p\0".as_slice(),
    ] {
        assert!(parse_entries(bad).is_err(), "{bad:?}");
    }
}
