// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn a_real_git_dependency_is_measured_from_its_resolved_checkout() {
    use aterm_uds::output::CommandOutput as _;
    use aterm_uds::spawnfd::command;

    let scratch = aterm_tempfile::tempdir().unwrap();
    let repo = scratch.path().join("source");
    let project = scratch.path().join("consumer");
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(&project).unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"leaf\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"lib.rs\"\n").unwrap();
    std::fs::write(repo.join("lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
    let git = |args: &[&str]| {
        let out = command("git")
            .current_dir(&repo)
            .args(args)
            .output_owned()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=fixture",
        "-c",
        "user.email=fixture@invalid",
        "commit",
        "-qm",
        "fixture",
    ]);
    let revision = git(&["rev-parse", "HEAD"]);
    std::fs::write(project.join("Cargo.toml"), format!(
        "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n[lib]\npath = \"lib.rs\"\n[dependencies]\nleaf = {{ git = \"file://{}\", rev = \"{}\" }}\n",
        repo.display(), revision.trim(),
    )).unwrap();
    std::fs::write(
        project.join("lib.rs"),
        "pub fn answer() -> u32 { leaf::answer() }\n",
    )
    .unwrap();
    let cargo = resolve::Cargo::from_env();
    let fetched = cargo.command("fetch", &project).output_owned().unwrap();
    assert!(
        fetched.status.success(),
        "{}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    let cell = Cell {
        name: "fixture".to_owned(),
        triple: "x86_64-unknown-linux-gnu".to_owned(),
        package: "consumer".to_owned(),
    };
    let survey = survey_cell(&cargo, &project, &cell).unwrap();
    let facts = &survey.facts[&PkgId::new("leaf", "0.1.0")];
    assert_eq!(facts.loc, 1);
    assert!(
        facts.is_third_party,
        "a local file URL is not same-owner GitHub provenance"
    );
    let resolved = facts.root_dir.as_ref().unwrap();
    assert_ne!(resolved, &repo, "measure the resolver's immutable checkout");
    assert_eq!(
        std::fs::read_to_string(resolved.join("lib.rs")).unwrap(),
        "pub fn answer() -> u32 { 42 }\n"
    );
}

#[test]
fn manifest_facts_read_licence_and_the_proc_macro_flag() {
    let (lic, pm) = manifest_facts("[package]\nlicense = \"MIT OR Apache-2.0\"\n");
    assert_eq!(lic, "MIT OR Apache-2.0");
    assert!(!pm);
    let (_, pm) = manifest_facts("[package]\nname = \"x\"\n[lib]\nproc-macro = true\n");
    assert!(pm);
    let (lic, _) = manifest_facts("[package]\nlicense.workspace = true\n");
    assert_eq!(lic, "");
    assert_eq!(manifest_facts("this is not toml ][").0, "");
}

/// Where a package's source is measured, and whether it is billed as
/// third-party, over a fixture workspace: a member answers by its MANIFEST
/// name (`crates/aterm-tracing` is package `tracing`) and only at its own
/// version; a patch target answers from the path that compiles, only at
/// the fork's version; the printed directory is the last resort. Being
/// first-party buys a crate its own lines, never what it drags in.
#[test]
fn package_dir_prefers_the_member_then_the_patch_and_bills_by_directory() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let w = |rel: &str, body: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, body).expect("write");
    };
    w(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\n[workspace.package]\nversion = \"0.9.0\"\n\
         [patch.crates-io]\ntracing = { path = \"crates/aterm-tracing\" }\n\
         indexmap = { path = \"vendor/indexmap\" }\n",
    );
    w(
        "crates/aterm-tracing/Cargo.toml",
        "[package]\nname = \"tracing\"\nversion = \"0.1.44\"\n",
    );
    w(
        "crates/aterm-core/Cargo.toml",
        "[package]\nname = \"aterm-core\"\nversion.workspace = true\n",
    );
    w(
        "vendor/indexmap/Cargo.toml",
        "[package]\nname = \"indexmap\"\nversion_note = \"9.9.9\"\nversion = \"2.14.0\"\n",
    );
    w("vendor/astream/crates/astream-aead/src/lib.rs", "");
    w(
        "registry/chacha20poly1305-0.10.1/src/lib.rs",
        "pub fn seal() {}\n",
    );
    let dir = |name: &str, version: &str, printed: Option<&Path>| {
        package_dir(root, &PkgId::new(name, version), printed)
    };

    assert_eq!(
        dir("tracing", "0.1.44", None),
        Some(root.join("crates/aterm-tracing"))
    );
    assert_ne!(
        dir("tracing", "0.1.41", None),
        Some(root.join("crates/aterm-tracing")),
        "another version of the member's name is somebody else's source"
    );
    assert_eq!(
        dir("aterm-core", "0.9.0", None),
        Some(root.join("crates/aterm-core")),
        "an inherited version is the workspace's"
    );
    assert_eq!(
        dir("aterm-core", "9.9.9", None),
        None,
        "a member whose folder matches its package name still answers only at its own version"
    );
    assert_eq!(
        dir("indexmap", "2.14.0", None),
        Some(root.join("vendor/indexmap"))
    );
    assert_ne!(
        dir("indexmap", "0.0.0-not-published", None),
        Some(root.join("vendor/indexmap"))
    );
    let hint = root.join("registry/chacha20poly1305-0.10.1");
    assert_eq!(dir("nope-not-real", "0.0.1", Some(&hint)), Some(hint));

    let third =
        |id: PkgId, printed: &str| measure(root, &id, Some(&root.join(printed))).is_third_party;
    assert!(!third(
        PkgId::new("tracing", "0.1.44"),
        "crates/aterm-tracing"
    ));
    assert!(third(PkgId::new("indexmap", "2.14.0"), "vendor/indexmap"));
    assert!(!third(
        PkgId::new("astream-aead", "0.2.0"),
        "vendor/astream/crates/astream-aead"
    ));
    assert!(third(
        PkgId::new("chacha20poly1305", "0.10.1"),
        "registry/chacha20poly1305-0.10.1"
    ));
}

#[cfg(unix)]
#[test]
fn collect_rs_ignores_symlinked_rs_files() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::write(root.join("real.rs"), "pub fn ok() {}\n").expect("real.rs");
    std::os::unix::fs::symlink(root.join("missing.rs"), root.join("dangling.rs")).expect("symlink");
    let mut out = Vec::new();
    collect_rs(root, &mut out);
    assert_eq!(out, vec![root.join("real.rs")]);
}
