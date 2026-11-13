// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Every release writer, including materialization, vendoring and retained bundles,
/// sits in the quiet cache. Previously only compiler intermediates were quiet.
#[test]
fn a_release_builds_in_the_caches_quiet_folder() {
    let cache = CacheRoot {
        root: PathBuf::from("/c/.ubd/a-release.noindex"),
    };
    for built in [
        cache.target(),
        cache.tmp(),
        cache.build_cargo_home(),
        cache.stage(),
    ] {
        assert_eq!(built.parent(), Some(cache.quiet().as_path()), "{built:?}");
        assert!(aterm_tempfile::is_quiet(&built), "{built:?}");
    }
    assert_eq!(cache.quiet(), cache.path());
    for kept in [
        cache.snapshot(),
        cache.vendor_cargo_home(),
        cache.artifacts(),
    ] {
        assert!(aterm_tempfile::is_quiet(&kept), "{kept:?}");
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = crate::ledger::testing::git()
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn the_object_reader_holds_every_answer_to_its_id_and_kind() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-objects-")
        .tempdir()
        .unwrap();
    let repo = scratch.path();
    git(repo, &["init", "-q"]);
    fs::write(repo.join("a.txt"), "alpha\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "one"]);
    let head = git(repo, &["rev-parse", "HEAD"]);
    let tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
    let other = git(repo, &["commit-tree", "-p", &head, "-m", "planted", &tree]);
    let reader = || ObjectReader::spawn(repo).unwrap();

    let mut good = reader();
    let bytes = good.read(&head, "commit").unwrap();
    assert_eq!(
        commit_tree(&bytes, head.len()).as_deref(),
        Some(tree.as_str())
    );
    let bytes = good.read(&tree, "tree").unwrap();
    let entries = tree_entries(&bytes, head.len() / 2).unwrap();
    let blob = git(repo, &["rev-parse", "HEAD:a.txt"]);
    assert!(
        matches!(&entries[..], [RawEntry { mode: b"100644", name: b"a.txt", id }] if *id == blob)
    );
    good.finish().unwrap();

    let mut kind = reader();
    kind.read(&tree, "commit").unwrap_err();
    let err = kind.read(&head, "commit").unwrap_err();
    assert!(err.contains("closed"), "{err}");
    let err = reader().read(&"0".repeat(head.len()), "tree").unwrap_err();
    assert!(err.contains("missing"), "{err}");

    let loose = |oid: &str| repo.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
    fs::remove_file(loose(&head)).unwrap();
    fs::copy(loose(&other), loose(&head)).unwrap();
    assert!(
        git(repo, &["cat-file", "commit", &head]).contains("planted"),
        "the negative control: git serves the planted bytes under the commit's id"
    );
    reader().read(&head, "commit").unwrap_err();
}

#[test]
fn a_path_the_commit_dropped_is_gone_from_src_before_vendoring_reads_it() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-take-")
        .tempdir()
        .unwrap();
    let workspace = "[workspace]\nmembers = [\"crates/*\"]\n";
    let kept = "[package]\nname = \"kept\"\n";
    let repo = scratch.path().join("repo");
    fs::create_dir_all(repo.join("crates/kept")).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("Cargo.toml"), workspace).unwrap();
    fs::write(repo.join("crates/kept/Cargo.toml"), kept).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "the release"]);
    let commit = git(&repo, &["rev-parse", "HEAD"]);

    let root = scratch.path().join("snapshot");
    for (rel, data) in [
        ("src/Cargo.toml", workspace),
        ("src/crates/kept/Cargo.toml", kept),
        ("src/crates/gone/Cargo.toml", "[package]\nname = \"gone\"\n"),
        ("vendor/old-1.0.0/src/lib.rs", "pub fn old() {}\n"),
    ] {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
    }
    let mtime = |rel: &str| fs::metadata(root.join(rel)).unwrap().modified().unwrap();
    let before = mtime("src/crates/kept/Cargo.toml");
    let listed = |rel: &str| {
        let mut names: Vec<String> = fs::read_dir(root.join(rel))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };

    let mut sync = Syncer::new(&root);
    take_tree(&repo, &commit, &mut sync).unwrap();
    assert_eq!(
        listed("src/crates"),
        ["kept"],
        "the tree vendoring resolves is the commit's alone"
    );
    assert_eq!(mtime("src/crates/kept/Cargo.toml"), before);
    assert_eq!(listed("vendor"), ["old-1.0.0"]);

    sync.put(
        Path::new("vendor/new-1.0.0/src/lib.rs"),
        b"pub fn new() {}\n",
        false,
    )
    .unwrap();
    sync.put(
        Path::new(".cargo/config.toml"),
        SEALED_CONFIG.as_bytes(),
        false,
    )
    .unwrap();
    let report = sync.finish().unwrap();
    assert_eq!(listed("vendor"), ["new-1.0.0"]);
    assert_eq!(listed(".cargo"), ["config.toml"]);
    assert_eq!(
        report,
        SyncReport {
            written: 2,
            unchanged: 2,
            removed: 2,
        }
    );
}
