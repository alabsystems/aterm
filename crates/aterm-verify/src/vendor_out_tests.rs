// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use aterm_seal as seal;

use super::{CRATES, Drift, MANIFEST, VENDOR, drift, run, vendored, verdict, write};
use crate::exit;

/// The aterm checkout this test binary was built from.
fn repo() -> PathBuf {
    aterm_tempfile::manifest_dir().join("../..")
}

/// Write each `(path, text)` under `root`.
fn tree(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let at = root.join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, text).unwrap();
    }
}

/// Every file under `dir`, by path from it.
fn files_under(dir: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut folders = vec![(dir.to_path_buf(), String::new())];
    while let Some((folder, prefix)) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let path = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.file_type().unwrap().is_dir() {
                folders.push((entry.path(), format!("{path}/")));
            } else {
                found.insert(path);
            }
        }
    }
    found
}

/// What the receiving repository's own check finds wrong with the copy in `vendor`, reading
/// nothing but the copy, as one with no aterm does: [`MANIFEST`] where its seal does not hold,
/// each listed file whose bytes no longer digest to its line, and each file the list does not
/// name.
fn unsealed(vendor: &Path) -> Vec<String> {
    let list = std::fs::read_to_string(vendor.join(MANIFEST)).unwrap();
    let mut wrong = Vec::new();
    if !matches!(seal::check(&list), seal::Seal::Holds { .. }) {
        wrong.push(MANIFEST.to_string());
    }
    let mut listed = BTreeSet::from([MANIFEST.to_string()]);
    for line in list.lines().filter(|line| !line.starts_with('#')) {
        let (digest, path) = line.split_once(' ').unwrap();
        listed.insert(path.to_string());
        let bytes = std::fs::read(vendor.join(path)).unwrap_or_default();
        if format!("{:016x}", aterm_hash::fnv1a_64(&bytes)) != digest {
            wrong.push(path.to_string());
        }
    }
    wrong.extend(
        files_under(vendor)
            .into_iter()
            .filter(|path| !listed.contains(path)),
    );
    wrong
}

#[test]
fn a_vendored_copy_matches_its_manifest_and_a_changed_byte_is_named() {
    let want = vendored(&repo(), &CRATES).expect("the vendored set vendors");
    for name in CRATES {
        for file in ["Cargo.toml", "src/lib.rs"] {
            assert!(
                want.contains_key(&format!("{name}/{file}")),
                "{name}/{file}"
            );
        }
    }
    let tests: Vec<&String> = want
        .keys()
        .filter(|path| path.ends_with("_tests.rs"))
        .collect();
    assert!(tests.is_empty(), "a test file in the copy: {tests:?}");
    let into = aterm_tempfile::tempdir().unwrap();
    let vendor = into.path().join(VENDOR);
    write(&vendor, &want).unwrap();
    assert_eq!(files_under(&vendor).len(), want.len());

    // Control: the copy as written matches its own list, and the list's seal holds.
    assert_eq!(unsealed(&vendor), Vec::<String>::new());

    // One changed byte in one copy is named, and nothing else.
    let edited = "aterm-phase/src/lib.rs";
    let at = vendor.join(edited);
    let mut bytes = std::fs::read(&at).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x20;
    std::fs::write(&at, &bytes).unwrap();
    assert_eq!(unsealed(&vendor), vec![edited.to_string()]);

    // So is a byte changed in the list itself: its seal breaks.
    std::fs::write(&at, &want[edited]).unwrap();
    let mut list = want[MANIFEST].clone();
    // The first digit of the last line's digest.
    let digit = list[..list.len() - 1]
        .iter()
        .rposition(|&byte| byte == b'\n')
        .unwrap()
        + 1;
    list[digit] = if list[digit] == b'0' { b'1' } else { b'0' };
    std::fs::write(vendor.join(MANIFEST), &list).unwrap();
    assert!(unsealed(&vendor).contains(&MANIFEST.to_string()));
}

/// A checkout holding crates `one` and `two` (at `crates/<name>`), `one` depending on `two`
/// through the workspace, and `two` on `dep` as `spec` says.
fn checkout(root: &Path, spec: &str) {
    tree(
        root,
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nedition = \"2024\"\n\
                 license = \"Apache-2.0\"\nrepository = \"https://example.invalid/private\"\n\n\
                 [workspace.dependencies]\ntwo = { path = \"crates/two\" }\n\
                 helper = { path = \"crates/helper\" }\n\n\
                 [workspace.lints.rust]\nunexpected_cfgs = { level = \"warn\", check-cfg = \
                 ['cfg(trust_verify)'] }\n\n[workspace.lints.clippy]\nall = { level = \"warn\", \
                 priority = -1 }\n",
            ),
            (
                "crates/one/Cargo.toml",
                "[package]\nname = \"one\"\nversion = \"0.0.0\"\nedition.workspace = true\n\
                 license.workspace = true\nrepository.workspace = true\n\n\
                 [package.metadata.aterm.test-inputs]\npaths = [\"fixtures\"]\n\n\
                 [lints]\nworkspace = true\n\n[dependencies]\ntwo = { workspace = true }\n\n\
                 [dev-dependencies]\nhelper = { workspace = true }\n",
            ),
            (
                "crates/one/src/lib.rs",
                "//! One.\n\nmod shipped;\n\n#[cfg(any(test, debug_assertions))]\n\
                 pub const TABLE: &str = include_str!(\"../fixtures/table.txt\");\n\n\
                 #[cfg(test)]\nconst ONLY_A_TEST: &str = include_str!(\"../fixtures/test-only.txt\");\n\n\
                 #[cfg(test)]\n#[path = \"lib_tests.rs\"]\nmod tests;\n",
            ),
            ("crates/one/src/shipped.rs", "pub fn shipped() {}\n"),
            ("crates/one/src/lib_tests.rs", "#[test]\nfn t() {}\n"),
            ("crates/one/fixtures/table.txt", "a table\n"),
            (
                "crates/one/fixtures/test-only.txt",
                "only a test reads this\n",
            ),
            (
                "crates/two/Cargo.toml",
                &format!(
                    "[package]\nname = \"two\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
                     [dependencies]\n{spec}"
                ),
            ),
            ("crates/two/src/lib.rs", "//! Two.\n"),
            (
                "crates/helper/Cargo.toml",
                "[package]\nname = \"helper\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
            ),
            ("crates/helper/src/lib.rs", "//! Helper.\n"),
        ],
    );
}

#[test]
fn vendor_out_refuses_a_crate_with_a_dependency_outside_the_set() {
    // Control: the real set, every dependency of which is a crate of the set, is accepted.
    vendored(&repo(), &CRATES).expect("the vendored set vendors");
    let root = aterm_tempfile::tempdir().unwrap();
    checkout(root.path(), "");
    vendored(root.path(), &["one", "two"]).expect("one and two depend on nothing else");

    // The real set without the name table: aterm-phase's dependency on it leaves the set.
    let why = vendored(&repo(), &["alab-policy", "aterm-phase"]).unwrap_err();
    assert!(
        why.contains("aterm-phase depends on aterm-agent-names"),
        "{why}"
    );

    // A workspace path dependency outside the set.
    let why = vendored(root.path(), &["one"]).unwrap_err();
    assert!(why.contains("one depends on two"), "{why}");

    // A dependency that is no path at all, from a registry or git.
    for spec in [
        "anyhow = \"1\"\n",
        "remote = { git = \"https://example.invalid/r\" }\n",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        checkout(root.path(), spec);
        let why = vendored(root.path(), &["one", "two"]).unwrap_err();
        assert!(why.contains("two depends on "), "{spec}: {why}");
    }
}

#[test]
fn check_names_a_stale_copy_and_nothing_for_a_fresh_one() {
    let root = aterm_tempfile::tempdir().unwrap();
    checkout(root.path(), "");
    let want = vendored(root.path(), &["one", "two"]).unwrap();
    // What one's library builds with debug assertions on, and nothing only a test reads.
    assert_eq!(
        want.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            MANIFEST,
            "one/Cargo.toml",
            "one/fixtures/table.txt",
            "one/src/lib.rs",
            "one/src/shipped.rs",
            "two/Cargo.toml",
            "two/src/lib.rs",
        ]
    );
    let cargo = String::from_utf8(want["one/Cargo.toml"].clone()).unwrap();
    for line in [
        "edition = \"2024\"",
        "two = { path = \"../two\" }",
        "unexpected_cfgs = { check-cfg = [\"cfg(trust_verify)\"], level = \"warn\" }",
        "all = { level = \"warn\", priority = -1 }",
        "test = false",
    ] {
        assert!(cargo.lines().any(|l| l == line), "{line} in:\n{cargo}");
    }
    for gone in ["workspace", "repository", "helper", "metadata"] {
        assert!(!cargo.contains(gone), "{gone} in:\n{cargo}");
    }

    let into = aterm_tempfile::tempdir().unwrap();
    let vendor = into.path().join(VENDOR);
    // A hand-made file at a path vendor-out writes is replaced, with no list there naming it.
    tree(&vendor, &[("one/src/lib.rs", "hand-made\n")]);
    let first = drift(&vendor, &want).unwrap();
    assert!(
        first.contains(&Drift::Differs("one/src/lib.rs".into())),
        "{first:?}"
    );
    write(&vendor, &want).unwrap();
    // Fresh: nothing named.
    assert_eq!(drift(&vendor, &want).unwrap(), []);
    assert!(verdict(&vendor, &[], want.len()).ends_with(" matches (7 files)"));

    // Stale: a changed byte, a file gone and a file no run wrote are each named.
    std::fs::write(vendor.join("one/src/shipped.rs"), "pub fn shipped() { }\n").unwrap();
    std::fs::remove_file(vendor.join("two/src/lib.rs")).unwrap();
    std::fs::write(vendor.join("one/src/stray.rs"), "\n").unwrap();
    let stale = [
        Drift::Differs("one/src/shipped.rs".into()),
        Drift::Stray("one/src/stray.rs".into()),
        Drift::Missing("two/src/lib.rs".into()),
    ];
    assert_eq!(drift(&vendor, &want).unwrap(), stale);
    // The report counts what it names, and says the stray goes first.
    let report = verdict(&vendor, &stale, want.len());
    assert!(
        report.contains(": 3 files named above; move each file named stray out"),
        "{report}"
    );

    // A write removes no file an earlier run did not write, and changes nothing then.
    let why = write(&vendor, &want).unwrap_err();
    assert!(why.contains("one/src/stray.rs"), "{why}");
    assert_eq!(drift(&vendor, &want).unwrap(), stale);
    std::fs::remove_file(vendor.join("one/src/stray.rs")).unwrap();
    let rewrite = drift(&vendor, &want).unwrap();
    let report = verdict(&vendor, &rewrite, want.len());
    assert!(
        report.ends_with(": 2 files named above; rerun without --check to rewrite it"),
        "{report}"
    );
    write(&vendor, &want).unwrap();
    assert_eq!(drift(&vendor, &want).unwrap(), []);

    // A crate dropped from the set leaves files an earlier run wrote: the write removes them.
    let fewer = vendored(root.path(), &["two"]).unwrap();
    assert!(
        drift(&vendor, &fewer)
            .unwrap()
            .contains(&Drift::Extra("one/src/lib.rs".into()))
    );
    write(&vendor, &fewer).unwrap();
    assert_eq!(drift(&vendor, &fewer).unwrap(), []);
    assert!(!vendor.join("one").exists());

    // The verb on the real set: --check fails on an empty folder, passes once written.
    let real = aterm_tempfile::tempdir().unwrap();
    let dir = real.path().to_string_lossy().into_owned();
    let args = |check: bool| {
        let mut args = vec!["--into".to_string(), dir.clone()];
        if check {
            args.push("--check".to_string());
        }
        args
    };
    assert_eq!(run(&args(true), &repo()), exit::FAILED);
    assert_eq!(run(&args(false), &repo()), exit::PASS);
    assert_eq!(run(&args(true), &repo()), exit::PASS);
}
