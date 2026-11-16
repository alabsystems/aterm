// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aterm_seal as seal;
use aterm_toml::Value;

use super::{
    CRATES, Copy, Drift, MANIFEST, Target, VENDOR, drift, manifest_text, pinned_source, protected,
    run, vendored, verdict, write,
};
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

/// Run git in `dir` with the caller's repository redirects removed; its stdout, trimmed.
fn git(dir: &Path, args: &[&str]) -> String {
    use aterm_uds::output::CommandOutput as _;
    let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-c",
            "user.name=aterm-verify test",
            "-c",
            "user.email=verify@invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output_owned()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A test-owned canonical copy and pin, staged as tools/pin's caller stages them.
fn canonical(root: &Path, tag: &str) {
    git(root, &["init", "-q"]);
    tree(
        root,
        &[
            ("vendor/alab-harness/Cargo.toml", "[workspace]\n"),
            (
                "vendor/alab-harness/crates/alab-policy/src/lib.rs",
                &format!("//! Canonical policy {tag}.\n"),
            ),
            ("vendor/alab-harness/.gitignore", "ignored\n"),
        ],
    );
    git(root, &["add", "-f", "vendor/alab-harness"]);
    let pinned = git(root, &["write-tree", "--prefix=vendor/alab-harness/"]);
    tree(
        root,
        &[(
            "vendor/alab-harness.pin",
            &format!("tag={tag}\ntree={pinned}\n"),
        )],
    );
    git(root, &["add", "-f", "vendor/alab-harness.pin"]);
    git(
        root,
        &["commit", "-q", "--no-verify", "-m", "canonical pin"],
    );
}

fn target(root: &Path) -> Target {
    canonical(root, "v0.9.0");
    Target::checked(root).unwrap_or_else(|(_, why)| panic!("{why}"))
}

fn projection_source(root: &Path) {
    canonical(root, "v8.0.0");
    tree(
        root,
        &[
            ("crates/aterm-forge/src/provenance.rs", "//! The roster.\n"),
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"vendor\"]\n\n\
                 [workspace.dependencies]\nalab-policy = { path = \
                 \"vendor/alab-harness/crates/alab-policy\" }\n",
            ),
            (
                "crates/aterm-phase/Cargo.toml",
                "[package]\nname = \"aterm-phase\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\n\
                 [dependencies]\nalab-policy = { workspace = true }\n\
                 aterm-agent-names = { path = \"../aterm-agent-names\" }\n",
            ),
            ("crates/aterm-phase/src/lib.rs", "//! Phase.\n"),
            (
                "crates/aterm-agent-names/Cargo.toml",
                "[package]\nname = \"aterm-agent-names\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
            ),
            ("crates/aterm-agent-names/src/lib.rs", "//! Names.\n"),
        ],
    );
}

fn sealed(files: &[(&str, &str)]) -> Copy {
    let mut copy: Copy = files
        .iter()
        .map(|(path, bytes)| (path.to_string(), bytes.as_bytes().to_vec()))
        .collect();
    copy.insert(MANIFEST.into(), manifest_text(&copy).unwrap().into_bytes());
    copy
}

fn put_copy(vendor: &Path, copy: &Copy) {
    for (path, bytes) in copy {
        let path = vendor.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

fn snapshot(vendor: &Path) -> BTreeMap<String, Vec<u8>> {
    files_under(vendor)
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(vendor.join(&path)).unwrap();
            (path, bytes)
        })
        .collect()
}

fn dependency(copy: &Copy, package: &str, name: &str) -> Value {
    let cargo = std::str::from_utf8(&copy[&format!("{package}/Cargo.toml")]).unwrap();
    let parsed: Value = aterm_toml::from_str(cargo).unwrap();
    parsed
        .get("dependencies")
        .unwrap()
        .get(name)
        .unwrap()
        .clone()
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
            .filter(|path| !protected(path) && !listed.contains(path)),
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
    assert!(!want.keys().any(|path| path.starts_with("alab-policy/")));
    assert_eq!(
        dependency(&want, "aterm-phase", "alab-policy")
            .get("path")
            .and_then(Value::as_str),
        Some("../alab-harness/crates/alab-policy")
    );
    assert!(vendored(&repo(), &["alab-policy"]).is_err());
    let tests: Vec<&String> = want
        .keys()
        .filter(|path| path.ends_with("_tests.rs"))
        .collect();
    assert!(tests.is_empty(), "a test file in the copy: {tests:?}");
    let into = aterm_tempfile::tempdir().unwrap();
    let target = target(into.path());
    let vendor = into.path().join(VENDOR);
    write(&target, &want).unwrap();
    assert_eq!(
        files_under(&vendor)
            .iter()
            .filter(|p| !protected(p))
            .count(),
        want.len()
    );

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

#[test]
fn independent_source_and_target_pins_leave_canonical_ownership_untouched() {
    let source = aterm_tempfile::tempdir().unwrap();
    projection_source(source.path());
    let into = aterm_tempfile::tempdir().unwrap();
    canonical(into.path(), "v9.0.0");
    let vendor = into.path().join(VENDOR);
    let original = snapshot(&vendor);
    let args = [
        "--into".to_string(),
        into.path().to_string_lossy().into_owned(),
    ];
    assert_eq!(run(&args, source.path()), exit::PASS);
    let mut check = args.to_vec();
    check.push("--check".into());
    assert_eq!(run(&check, source.path()), exit::PASS);
    for (path, bytes) in original {
        assert_eq!(std::fs::read(vendor.join(path)).unwrap(), bytes);
    }
    assert_eq!(unsealed(&vendor), Vec::<String>::new());
}

#[test]
fn an_invalid_target_pin_stops_both_modes_before_any_projection_changes() {
    let source = aterm_tempfile::tempdir().unwrap();
    projection_source(source.path());
    for defect in [
        "missing",
        "malformed",
        "indexed",
        "dirty",
        "ignored",
        "untracked",
    ] {
        let into = aterm_tempfile::tempdir().unwrap();
        canonical(into.path(), "v9.0.0");
        let vendor = into.path().join(VENDOR);
        put_copy(
            &vendor,
            &sealed(&[("alab-policy/src/lib.rs", "//! Legacy.\n")]),
        );
        match defect {
            "missing" => std::fs::remove_file(vendor.join("alab-harness.pin")).unwrap(),
            "malformed" => std::fs::write(vendor.join("alab-harness.pin"), "bad\n").unwrap(),
            "indexed" | "dirty" => {
                tree(
                    &vendor,
                    &[(
                        "alab-harness/crates/alab-policy/src/lib.rs",
                        "//! Changed.\n",
                    )],
                );
                if defect == "indexed" {
                    git(into.path(), &["add", "-f", "vendor/alab-harness"]);
                }
            }
            "ignored" | "untracked" => {
                tree(
                    &vendor,
                    &[(
                        if defect == "ignored" {
                            "alab-harness/ignored"
                        } else {
                            "alab-harness/extra"
                        },
                        "extra\n",
                    )],
                );
            }
            _ => unreachable!(),
        }
        let before = snapshot(&vendor);
        for check in [false, true] {
            let mut args = vec!["--into".into(), into.path().to_string_lossy().into_owned()];
            if check {
                args.push("--check".into());
            }
            assert_eq!(run(&args, source.path()), exit::FAILED, "{defect}");
            assert_eq!(snapshot(&vendor), before, "{defect}");
        }
    }
}

#[test]
fn legacy_policy_cleanup_requires_sealed_ownership_and_preserves_stray_refusal() {
    let into = aterm_tempfile::tempdir().unwrap();
    let target = target(into.path());
    let vendor = &target.vendor;
    let want = sealed(&[("aterm-phase/src/lib.rs", "//! New phase.\n")]);
    put_copy(
        vendor,
        &sealed(&[("alab-policy/src/lib.rs", "//! Legacy.\n")]),
    );
    for stray in [
        "alab-policy/src/unlisted.rs",
        "alab-harness-extra/src/lib.rs",
        "alab-harness.pin.backup",
    ] {
        tree(vendor, &[(stray, "//! Stray.\n")]);
        let before = snapshot(vendor);
        let (code, why) = write(&target, &want).unwrap_err();
        assert_eq!(code, exit::FAILED);
        assert!(why.contains(stray), "{why}");
        assert_eq!(snapshot(vendor), before);
        std::fs::remove_file(vendor.join(stray)).unwrap();
    }
    let canonical = snapshot(vendor)
        .into_iter()
        .filter(|(path, _)| protected(path))
        .collect::<BTreeMap<_, _>>();
    write(&target, &want).unwrap();
    assert!(!vendor.join("alab-policy").exists());
    assert_eq!(drift(&target, &want).unwrap(), []);
    for (path, bytes) in canonical {
        assert_eq!(std::fs::read(vendor.join(path)).unwrap(), bytes);
    }
}

#[test]
fn neither_old_seals_nor_new_output_can_claim_canonical_paths() {
    let into = aterm_tempfile::tempdir().unwrap();
    let target = target(into.path());
    let want = sealed(&[("aterm-phase/src/lib.rs", "//! Phase.\n")]);
    write(&target, &want).unwrap();
    for path in [
        "alab-harness/Cargo.toml",
        "alab-harness.pin",
        "./alab-harness/Cargo.toml",
    ] {
        let overlap = sealed(&[(path, "claimed\n")]);
        let before = snapshot(&target.vendor);
        let (code, why) = write(&target, &overlap).unwrap_err();
        assert_eq!(code, exit::FAILED);
        assert!(why.contains("overlaps canonical"), "{why}");
        assert_eq!(snapshot(&target.vendor), before);

        // A prior valid seal cannot authorize deletion of another owner's files either.
        std::fs::write(target.vendor.join(MANIFEST), &overlap[MANIFEST]).unwrap();
        let before = snapshot(&target.vendor);
        let (code, why) = write(&target, &want).unwrap_err();
        assert_eq!(code, exit::FAILED);
        assert!(why.contains("overlaps canonical"), "{why}");
        assert_eq!(snapshot(&target.vendor), before);
        std::fs::write(target.vendor.join(MANIFEST), &want[MANIFEST]).unwrap();
    }
}

#[test]
fn canonical_policy_dependency_mapping_preserves_package_and_feature_settings() {
    let root = aterm_tempfile::tempdir().unwrap();
    checkout(
        root.path(),
        "rules = { path = \"../policy\", package = \"alab-policy\", \
                           features = [\"example\"], default-features = false }\n",
    );
    let copy = vendored(root.path(), &["one", "two"]).unwrap();
    let rules = dependency(&copy, "two", "rules");
    assert_eq!(
        rules.get("path").and_then(Value::as_str),
        Some("../alab-harness/crates/alab-policy")
    );
    assert_eq!(
        rules.get("package").and_then(Value::as_str),
        Some("alab-policy")
    );
    assert_eq!(rules.get("default-features"), Some(&Value::Boolean(false)));
    assert_eq!(
        rules.get("features"),
        Some(&Value::Array(vec![Value::String("example".into())]))
    );
    assert!(!copy.keys().any(|path| path.starts_with("alab-policy/")));
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
    // Control: every dependency of the real set has a projection or canonical owner.
    vendored(&repo(), &CRATES).expect("the vendored set vendors");
    let root = aterm_tempfile::tempdir().unwrap();
    checkout(root.path(), "");
    vendored(root.path(), &["one", "two"]).expect("one and two depend on nothing else");

    // The real set without the name table: aterm-phase's dependency on it leaves the set.
    let why = vendored(&repo(), &["aterm-phase"]).unwrap_err();
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
fn a_crate_under_an_excluded_vendored_workspace_takes_that_workspaces_version_and_lints() {
    let root = aterm_tempfile::tempdir().unwrap();
    // aterm's root: version 0.202.0 and its own lints, `three` taken by path from a copy of
    // another repository that the root excludes, `two` from crates/ like any member.
    tree(
        root.path(),
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"vendor/up\"]\n\n\
                 [workspace.package]\nversion = \"0.202.0\"\nedition = \"2024\"\n\
                 license = \"Apache-2.0\"\n\n[workspace.dependencies]\n\
                 two = { path = \"crates/two\" }\n\
                 three = { path = \"vendor/up/crates/three\" }\n\n\
                 [workspace.lints.clippy]\nall = { level = \"warn\", priority = -1 }\n",
            ),
            (
                "crates/two/Cargo.toml",
                "[package]\nname = \"two\"\nversion.workspace = true\nedition.workspace = true\n\n\
                 [lints]\nworkspace = true\n\n[dependencies]\nthree = { workspace = true }\n",
            ),
            ("crates/two/src/lib.rs", "//! Two.\n"),
            // The vendored repository's own workspace root, with its own version and lints.
            (
                "vendor/up/Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\n\
                 version = \"0.8.0\"\nedition = \"2024\"\nlicense = \"Apache-2.0\"\n\n\
                 [workspace.lints.rust]\nunsafe_code = \"forbid\"\n",
            ),
            (
                "vendor/up/crates/three/Cargo.toml",
                "[package]\nname = \"three\"\nversion.workspace = true\n\
                 edition.workspace = true\nlicense.workspace = true\n\n\
                 [lints]\nworkspace = true\n",
            ),
            (
                "vendor/up/crates/three/src/lib.rs",
                "//! Three.\n\nmod inner;\n",
            ),
            ("vendor/up/crates/three/src/inner.rs", "pub fn inner() {}\n"),
        ],
    );
    let want = vendored(root.path(), &["three", "two"]).expect("three and two vendor");
    assert_eq!(
        want.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            MANIFEST,
            "three/Cargo.toml",
            "three/src/inner.rs",
            "three/src/lib.rs",
            "two/Cargo.toml",
            "two/src/lib.rs",
        ]
    );
    let three = String::from_utf8(want["three/Cargo.toml"].clone()).unwrap();
    for line in [
        "# Written by `targo --unverified verify vendor-out` from aterm's \
         vendor/up/crates/three/Cargo.toml: rerun it there, never",
        "version = \"0.8.0\"",
        "license = \"Apache-2.0\"",
        "[lints.rust]",
        "unsafe_code = \"forbid\"",
        "# vendors it from (the workspace of aterm's vendor/up/Cargo.toml), beside the files they \
         read.",
    ] {
        assert!(three.lines().any(|l| l == line), "{line} in:\n{three}");
    }
    for gone in ["0.202.0", "clippy", "priority"] {
        assert!(!three.contains(gone), "{gone} in:\n{three}");
    }
    // A member still inherits from aterm's root, and its dependency on the vendored crate is a
    // path to that crate's copy.
    let two = String::from_utf8(want["two/Cargo.toml"].clone()).unwrap();
    for line in [
        "# Written by `targo --unverified verify vendor-out` from aterm's crates/two/Cargo.toml: \
         rerun it there, never",
        "version = \"0.202.0\"",
        "all = { level = \"warn\", priority = -1 }",
        "three = { path = \"../three\" }",
        "# The copy's one target. Its tests and doctests run in aterm, beside the files",
    ] {
        assert!(two.lines().any(|l| l == line), "{line} in:\n{two}");
    }
    assert!(!two.contains("forbid"), "{two}");

    // Without its own workspace root the vendored crate has none: aterm's root excludes it, and
    // cargo passes over a root that excludes a crate, so it is refused rather than given aterm's
    // version and lints. A manifest there with no [workspace] table changes nothing.
    std::fs::remove_file(root.path().join("vendor/up/Cargo.toml")).unwrap();
    for manifest in [
        None,
        Some("[package]\nname = \"up\"\nversion = \"0.1.0\"\n"),
    ] {
        if let Some(text) = manifest {
            tree(root.path(), &[("vendor/up/Cargo.toml", text)]);
        }
        let why = vendored(root.path(), &["three"]).unwrap_err();
        assert!(
            why.starts_with("no workspace takes in vendor/up/crates/three (Cargo.toml excludes it")
                && why.contains("needs its own root manifest"),
            "{manifest:?}: {why}"
        );
    }
    // Unless the root also names it among its members, which cargo honours over the exclusion:
    // then the root's workspace is its own.
    let top = std::fs::read_to_string(root.path().join("Cargo.toml")).unwrap();
    tree(
        root.path(),
        &[(
            "Cargo.toml",
            &top.replace(
                "members = [\"crates/*\"]",
                "members = [\"crates/*\", \"vendor/up/crates/three\"]",
            ),
        )],
    );
    let member = vendored(root.path(), &["three"]).unwrap();
    let three = String::from_utf8(member["three/Cargo.toml"].clone()).unwrap();
    assert!(
        three.lines().any(|l| l == "version = \"0.202.0\""),
        "{three}"
    );

    // A root that takes the crate from a registry, or from a path outside the checkout, holds
    // no source of it to copy.
    for spec in ["\"1\"", "{ path = \"../elsewhere/three\" }"] {
        let other = aterm_tempfile::tempdir().unwrap();
        tree(
            other.path(),
            &[(
                "Cargo.toml",
                &format!("[workspace]\n\n[workspace.dependencies]\nthree = {spec}\n"),
            )],
        );
        let why = vendored(other.path(), &["three"]).unwrap_err();
        assert!(
            why.contains("the root's [workspace.dependencies]") && why.contains("three"),
            "{spec}: {why}"
        );
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
    let target = target(into.path());
    let vendor = into.path().join(VENDOR);
    // A hand-made file at a path vendor-out writes is replaced, with no list there naming it.
    tree(&vendor, &[("one/src/lib.rs", "hand-made\n")]);
    let first = drift(&target, &want).unwrap();
    assert!(
        first.contains(&Drift::Differs("one/src/lib.rs".into())),
        "{first:?}"
    );
    write(&target, &want).unwrap();
    // Fresh: nothing named.
    assert_eq!(drift(&target, &want).unwrap(), []);
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
    assert_eq!(drift(&target, &want).unwrap(), stale);
    // The report counts what it names, and says the stray goes first.
    let report = verdict(&vendor, &stale, want.len());
    assert!(
        report.contains(": 3 files named above; move each file named stray out"),
        "{report}"
    );

    // A write removes no file an earlier run did not write, and changes nothing then.
    let (_, why) = write(&target, &want).unwrap_err();
    assert!(why.contains("one/src/stray.rs"), "{why}");
    assert_eq!(drift(&target, &want).unwrap(), stale);
    std::fs::remove_file(vendor.join("one/src/stray.rs")).unwrap();
    let rewrite = drift(&target, &want).unwrap();
    let report = verdict(&vendor, &rewrite, want.len());
    assert!(
        report.ends_with(": 2 files named above; rerun without --check to rewrite it"),
        "{report}"
    );
    write(&target, &want).unwrap();
    assert_eq!(drift(&target, &want).unwrap(), []);

    // A crate dropped from the set leaves files an earlier run wrote: the write removes them.
    let fewer = vendored(root.path(), &["two"]).unwrap();
    assert!(
        drift(&target, &fewer)
            .unwrap()
            .contains(&Drift::Extra("one/src/lib.rs".into()))
    );
    write(&target, &fewer).unwrap();
    assert_eq!(drift(&target, &fewer).unwrap(), []);
    assert!(!vendor.join("one").exists());

    // The verb on the real set: --check fails on an empty folder, passes once written.
    let real = aterm_tempfile::tempdir().unwrap();
    canonical(real.path(), "v9.0.0");
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

/// vendor-out reads nothing from a checkout whose pinned copy does not hold its pin: a hand edit of
/// `vendor/alab-harness` is refused as forge's `[OB-1]` refuses it. A
/// copy that holds lets the crates be read (and here refused for want of a root manifest).
#[test]
fn vendor_out_reads_no_source_whose_pinned_copy_does_not_hold_its_pin() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    tree(
        root,
        &[
            // The file that makes forge's roster describe this checkout.
            ("crates/aterm-forge/src/provenance.rs", "//! The roster.\n"),
            (
                "vendor/alab-harness/crates/alab-policy/src/lib.rs",
                "//! Policy.\n",
            ),
        ],
    );
    git(root, &["add", "-f", "."]);
    let pinned = git(root, &["write-tree", "--prefix=vendor/alab-harness/"]);
    tree(
        root,
        &[(
            "vendor/alab-harness.pin",
            &format!("tag=v0.8.1\ntree={pinned}\n"),
        )],
    );
    git(root, &["add", "-f", "vendor/alab-harness.pin"]);
    git(root, &["commit", "-q", "--no-verify", "-m", "pin"]);

    let (code, line) = pinned_source(root, &CRATES).unwrap_err();
    assert!(
        code == exit::FAILED && line.starts_with("REFUSED — Cargo.toml"),
        "{line}"
    );

    tree(
        root,
        &[(
            "vendor/alab-harness/crates/alab-policy/src/lib.rs",
            "//! Edited by hand.\n",
        )],
    );
    let (code, line) = pinned_source(root, &CRATES).unwrap_err();
    assert!(
        code == exit::FAILED
            && line.starts_with("REFUSED — [OB-1] the working copy changes `vendor/alab-harness`")
            && line.contains("vendor/alab-harness/crates/alab-policy/src/lib.rs")
            && line.ends_with("; nothing was copied"),
        "{line}"
    );
}
