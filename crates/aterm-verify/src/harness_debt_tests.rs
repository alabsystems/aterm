// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::fs;

fn run(root: &Path, args: &[&str]) {
    let mut git = crate::git_provider::command();
    git.current_dir(root).args(args);
    let out = aterm_uds::deadline::output_within(&mut git, READ_WAIT).unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn commit(root: &Path, message: &str) {
    run(root, &["add", "."]);
    run(root, &["commit", "-qm", message]);
}

fn tag(root: &Path, version: &str) {
    run(
        root,
        &["tag", "-am", "public release", &format!("v{version}")],
    );
}

fn init(root: &Path) {
    run(root, &["init", "-q"]);
    run(root, &["config", "user.name", "A"]);
    run(root, &["config", "user.email", "a@example.invalid"]);
}

fn manifest(version: &str) -> String {
    format!("[workspace]\n[workspace.package]\nversion = \"{version}\"\n")
}

#[test]
fn a_late_harness_change_is_red_until_the_window_has_one_bump() {
    let temp = aterm_tempfile::TempDir::new().unwrap();
    let root = temp.path();
    init(root);
    write(root, MANIFEST, &manifest("0.173.0"));
    write(
        root,
        CliHarness::ClaudeCode.version_file(),
        &format!("{}\n", source_line((0, 1))),
    );
    write(
        root,
        "crates/aterm-agent/src/harness/upgrade_drive.rs",
        "fn drive() {}\n",
    );
    commit(root, "public release");
    tag(root, "0.173.0");

    write(
        root,
        "crates/aterm-agent/src/harness/upgrade_drive.rs",
        "fn drive() { changed(); }\n",
    );
    commit(root, "harness source before the workspace bump");
    write(root, MANIFEST, &manifest("0.174.0"));
    commit(
        root,
        "workspace version bump does not reset the public window",
    );
    let path = std::env::var_os("PATH").unwrap_or_default();
    let Judgement::Owed { debts, .. } = judge(root, "HEAD", &path).unwrap() else {
        panic!("unbumped source change passed");
    };
    assert_eq!(debts.len(), 1);
    assert_eq!(debts[0].harness, CliHarness::ClaudeCode);

    // NEGATIVE CONTROL: reverting the landing check makes the preceding assertion pass the
    // bad tree; moving the compiled version once clears this release window's debt.
    write(
        root,
        CliHarness::ClaudeCode.version_file(),
        &format!("{}\n", source_line((0, 2))),
    );
    commit(root, "bump harness");
    assert!(matches!(
        judge(root, "HEAD", &path).unwrap(),
        Judgement::Clear { .. }
    ));

    write(
        root,
        "crates/aterm-agent/src/harness/upgrade_drive.rs",
        "fn drive() { changed_again(); }\n",
    );
    commit(root, "a second source change in the same public window");
    assert!(matches!(
        judge(root, "HEAD", &path).unwrap(),
        Judgement::Clear { .. }
    ));

    tag(root, "0.174.0");
    write(root, MANIFEST, &manifest("0.175.0"));
    commit(root, "next workspace version");
    write(
        root,
        "crates/aterm-agent/src/harness/upgrade_drive.rs",
        "fn drive() { next_public_window(); }\n",
    );
    commit(root, "change after the next public release");
    assert!(matches!(
        judge(root, "HEAD", &path).unwrap(),
        Judgement::Owed { .. }
    ));
}

#[test]
fn an_unbumped_source_cannot_hide_behind_a_later_version_or_ledger_commit() {
    let temp = aterm_tempfile::TempDir::new().unwrap();
    let root = temp.path();
    init(root);
    write(root, MANIFEST, &manifest("0.8.0"));
    write(
        root,
        CliHarness::ClaudeCode.version_file(),
        &format!("{}\n", source_line((0, 3))),
    );
    write(
        root,
        "crates/aterm-agent/src/harness/upgrade_drive.rs",
        "fn drive() {}\n",
    );
    commit(root, "public release");
    tag(root, "0.8.0");
    write(
        root,
        "crates/aterm-agent/src/harness/upgrade_drive.rs",
        "fn drive() { hidden(); }\n",
    );
    commit(root, "unbumped source");
    write(root, MANIFEST, &manifest("0.9.0"));
    commit(root, "later workspace bump");
    write(root, "RELEASES.ledger", "100 0.9.0\n");
    commit(root, "later abandoned claim");
    let path = std::env::var_os("PATH").unwrap_or_default();
    assert!(matches!(
        judge(root, "HEAD", &path).unwrap(),
        Judgement::Owed { .. }
    ));
}

/// Host git under no system or global config and no inherited repository redirect.
fn fixture_git(root: &Path, args: &[&str]) -> String {
    let mut git = crate::git_provider::command();
    git.current_dir(root).args(args);
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_") {
            git.env_remove(key);
        }
    }
    git.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.invalid")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.invalid");
    let out = aterm_uds::deadline::output_within(&mut git, READ_WAIT).unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Whether `root` holds `oid` itself, asked with no lazy fetch.
fn holds(root: &Path, oid: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    call(root, &path, &["cat-file", "-e", oid], None, None)
        .unwrap()
        .status
        .success()
}

/// A public release whose version file the candidate edits without bumping, served by a
/// local bare repository that answers filtered and exact-object requests, and two blobless
/// partial clones of it: the shape of a checkout whose newest public baseline's files
/// nobody here has read.
fn partial_clones(root: &Path) -> (std::path::PathBuf, [std::path::PathBuf; 2], String) {
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    fixture_git(&work, &["init", "-q", "-b", "main"]);
    let version_file = CliHarness::ClaudeCode.version_file();
    write(&work, MANIFEST, &manifest("0.8.0"));
    write(&work, version_file, &format!("{}\n", source_line((0, 3))));
    write(
        &work,
        "crates/aterm-agent/src/harness/upgrade_drive.rs",
        "fn drive() {}\n",
    );
    fixture_git(&work, &["add", "."]);
    fixture_git(&work, &["commit", "-qm", "public release"]);
    fixture_git(&work, &["tag", "-am", "public release", "v0.8.0"]);
    let baseline_blob = fixture_git(&work, &["rev-parse", &format!("v0.8.0:{version_file}")]);
    write(
        &work,
        "crates/aterm-agent/src/harness/upgrade_drive.rs",
        "fn drive() { changed(); }\n",
    );
    write(
        &work,
        version_file,
        &format!("{}\n// edited, not bumped\n", source_line((0, 3))),
    );
    write(&work, MANIFEST, &manifest("0.9.0"));
    fixture_git(&work, &["add", "."]);
    fixture_git(&work, &["commit", "-qm", "unbumped source"]);
    let server = root.join("server.git");
    let server_text = server.to_str().unwrap();
    fixture_git(root, &["clone", "-q", "--bare", "work", server_text]);
    fixture_git(&server, &["config", "uploadpack.allowFilter", "true"]);
    fixture_git(
        &server,
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    );
    let clones = ["partial", "offline"].map(|name| {
        let clone = root.join(name);
        fixture_git(
            root,
            &[
                "clone",
                "-q",
                "--filter=blob:none",
                &format!("file://{server_text}"),
                clone.to_str().unwrap(),
            ],
        );
        // The verifier's remote boundary admits a local repository by its path.
        fixture_git(&clone, &["remote", "set-url", "origin", server_text]);
        clone
    });
    (server, clones, baseline_blob)
}

#[test]
fn a_partial_clone_imports_exactly_the_baseline_blobs_it_lacks_or_names_them() {
    let temp = aterm_tempfile::TempDir::new().unwrap();
    let (server, [partial, offline], baseline_blob) = partial_clones(temp.path());
    let path = std::env::var_os("PATH").unwrap_or_default();

    // NEGATIVE CONTROL: the clone lacks the baseline's version file, so a read with lazy
    // fetch off (the judgement before the import, which reported COULD NOT RUN) fails.
    assert!(!holds(&partial, &baseline_blob));
    let refs = fixture_git(&partial, &["for-each-ref"]);
    let Judgement::Owed { debts, .. } = judge(&partial, "HEAD", &path).unwrap() else {
        panic!("an edited, unbumped version file passed");
    };
    assert_eq!(debts.len(), 1);
    assert_eq!(debts[0].harness, CliHarness::ClaudeCode);
    assert!(holds(&partial, &baseline_blob), "the import supplied it");
    assert_eq!(
        fixture_git(&partial, &["for-each-ref"]),
        refs,
        "an import publishes no ref"
    );

    // With the promisor remote gone the blob stays absent, and the verdict says which file
    // and how to get it rather than judging without it.
    fs::rename(&server, temp.path().join("gone.git")).unwrap();
    let why = judge(&offline, "HEAD", &path).unwrap_err();
    assert!(!holds(&offline, &baseline_blob));
    assert!(
        why.contains("1 blob this partial clone lacks")
            && why.contains(&format!("{} at ", CliHarness::ClaudeCode.version_file()))
            && why.contains("importing them from `origin` failed")
            && why.contains("fix: re-run where this clone's promisor remote is reachable")
            && why.contains(&format!("<promisor remote> {baseline_blob}")),
        "{why}"
    );
}

/// The object view reads a store in the store's own hash: a SHA-256 repository's baseline reads
/// its manifest blob through a view of the same format.
#[test]
fn a_sha256_repository_reads_its_baseline_through_a_view_of_its_own_format() {
    let temp = aterm_tempfile::TempDir::new().unwrap();
    let root = temp.path();
    run(root, &["init", "-q", "--object-format=sha256"]);
    run(root, &["config", "user.name", "A"]);
    run(root, &["config", "user.email", "a@example.invalid"]);
    write(root, MANIFEST, &manifest("0.8.0"));
    commit(root, "release");
    tag(root, "0.8.0");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let head = fixture_git(root, &["rev-parse", "HEAD"]);
    assert_eq!(head.len(), 64, "a SHA-256 commit: {head}");
    let reads = Reads {
        root,
        path_env: &path,
    };
    let (_, version, commit) = candidate_baseline(&reads, "HEAD").unwrap();
    assert_eq!((version, commit), ("0.8.0".to_string(), head));
}

#[test]
fn a_directory_source_matches_only_inside_its_slash_boundary() {
    assert!(matches_source(
        "crates/harness/",
        "crates/harness/reader.rs"
    ));
    assert!(!matches_source(
        "crates/harness/",
        "crates/harness-extra.rs"
    ));
}
