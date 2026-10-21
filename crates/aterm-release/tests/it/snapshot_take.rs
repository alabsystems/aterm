// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release snapshot (`crate::snapshot`), against real git: a fixture
//! repository with a submodule and an executable bit, materialized from its objects.
//! Vendoring runs a stand-in `targo` that lays the directories and the archive the
//! real one would (`vendor --versioned-dirs`), so no network is touched.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::snapshot::{
    CacheHold, CacheRoot, LockedCrate, check_vendored, locked_crates, materialize,
    verify_crate_cache,
};

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-snapshot-")
        .tempdir()
        .unwrap()
}

/// git with nothing of this machine's own configuration in it, run as the developer
/// directory's git, not macOS's `/usr/bin/git` trampoline
/// ([`crate::ledger::testing::git_path`]). No template: a repository it makes carries no
/// sample hooks, description or info/exclude, files nothing here reads. No `git
/// maintenance run --auto` after a commit or clone: it does nothing in a fixture this small.
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
            "protocol.file.allow=always",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.templateDir=",
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// The bytes of the one `.crate` the fixture's lockfile names.
const ARCHIVE: &[u8] = b"the itoa 1.0.0 archive\n";

/// Its checksum, as `Cargo.lock` records it: the archive's SHA-256.
fn checksum() -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(ARCHIVE))
}

/// A checksum nothing in a fixture hashes to.
const OTHER: &str = "4e07408562bedb8b60ce05c1decfe3ad16b72230967de01f640b7e4729b49fce";

/// The cache at `root`, held.
fn held(root: PathBuf) -> CacheHold {
    CacheRoot::at(root).unwrap().hold().unwrap()
}

/// A workspace whose lockfile names one crates.io package, a submodule and an
/// executable — its HEAD commit, and the repository.
///
/// Everything before the commit is the same in every test bar where it lies, so it is
/// made once per process ([`template`]) and laid down here, the submodule's URL (the
/// absolute path of `sub-origin`, as `git submodule add` records it in `.gitmodules` and
/// both configs) moved to this `root`; the commit is made here, so `.gitmodules` as
/// committed names this test's own `sub-origin`, which a `--recurse-submodules` clone
/// fetches from.
fn fixture(root: &Path) -> (PathBuf, String) {
    let template = template();
    let here = root.to_str().unwrap().as_bytes();
    for (rel, entry) in &template.entries {
        let path = root.join(rel);
        match entry {
            Entry::Dir => std::fs::create_dir_all(&path).unwrap(),
            Entry::File(bytes, mode) => {
                std::fs::write(&path, moved(bytes, template.at.as_bytes(), here)).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(*mode)).unwrap();
            }
        }
    }
    let repo = root.join("repo");
    let modules = std::fs::read_to_string(repo.join(".gitmodules")).unwrap();
    assert!(
        modules.contains(&format!("url = {}\n", root.join("sub-origin").display())),
        "{modules}"
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "fixture"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    (repo, head)
}

/// [`fixture`] up to its commit, at `root`: `sub-origin` committed, `repo` with its files
/// written and `sub-origin` added as the submodule `vendor/sub`.
fn uncommitted_fixture(root: &Path) {
    let sub = root.join("sub-origin");
    std::fs::create_dir_all(&sub).unwrap();
    git(&sub, &["init", "-q"]);
    std::fs::write(sub.join("lib.rs"), "pub fn sub() {}\n").unwrap();
    git(&sub, &["add", "-A"]);
    git(&sub, &["commit", "-qm", "sub"]);

    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join(".cargo")).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::write(
        repo.join("Cargo.lock"),
        format!(
            "version = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n\
             [[package]]\nname = \"itoa\"\nversion = \"1.0.0\"\nsource = \
             \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{}\"\n",
            checksum()
        ),
    )
    .unwrap();
    std::fs::write(repo.join(".cargo/config.toml"), "[build]\n").unwrap();
    std::fs::write(repo.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(repo.join("gone.txt"), "removed by the second commit\n").unwrap();
    std::fs::write(repo.join("run.sh"), "#!/bin/sh\necho run\n").unwrap();
    std::fs::set_permissions(repo.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &repo,
        &[
            "submodule",
            "add",
            "-q",
            sub.to_str().unwrap(),
            "vendor/sub",
        ],
    );
}

/// One path of the [`Template`]: a directory, or a file's bytes and permission bits.
enum Entry {
    Dir,
    File(Vec<u8>, u32),
}

/// [`uncommitted_fixture`] as read back from where it was made: that root, and every
/// path under it, each directory before what it holds.
struct Template {
    at: String,
    entries: Vec<(PathBuf, Entry)>,
}

/// The [`Template`], made once per process in a scratch directory removed once it is read.
fn template() -> &'static Template {
    static TEMPLATE: OnceLock<Template> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let dir = scratch();
        uncommitted_fixture(dir.path());
        let mut entries = Vec::new();
        read_tree(dir.path(), Path::new(""), &mut entries);
        Template {
            at: dir.path().to_str().unwrap().to_owned(),
            entries,
        }
    })
}

/// Every path under `root.join(rel)`, depth first.
fn read_tree(root: &Path, rel: &Path, into: &mut Vec<(PathBuf, Entry)>) {
    for entry in std::fs::read_dir(root.join(rel)).unwrap() {
        let entry = entry.unwrap();
        let rel = rel.join(entry.file_name());
        let meta = std::fs::symlink_metadata(entry.path()).unwrap();
        if meta.is_dir() {
            into.push((rel.clone(), Entry::Dir));
            read_tree(root, &rel, into);
        } else {
            assert!(
                meta.is_file(),
                "the fixture makes no link: {}",
                rel.display()
            );
            let mode = meta.permissions().mode() & 0o7777;
            into.push((rel, Entry::File(std::fs::read(entry.path()).unwrap(), mode)));
        }
    }
}

/// `bytes` with every `from` replaced by `to`.
fn moved(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(at) = rest.windows(from.len()).position(|w| w == from) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(to);
        rest = &rest[at + from.len()..];
    }
    out.extend_from_slice(rest);
    out
}

/// A stand-in `targo`: `vendor … <out>` lays one versioned directory (with its
/// `.cargo-checksum.json`) per `name version checksum` line of `crates`, and leaves the
/// `.crate` (the file `archive-<name>-<version>` beside it) in `$CARGO_HOME/registry/cache`.
/// A marker `link-<name>-<version>` adds a symbolic link to that crate. It refuses to run
/// while `$CARGO_HOME` holds a Cargo config.
fn stub_targo(root: &Path, crates: &str) -> PathBuf {
    let bin = root.join("toolchain/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("crates"), crates).unwrap();
    std::fs::write(bin.join("archive-itoa-1.0.0"), ARCHIVE).unwrap();
    let targo = bin.join("targo");
    std::fs::write(
        &targo,
        "#!/bin/sh\n\
         [ \"$1\" = vendor ] || exit 64\n\
         for f in config.toml config; do\n\
         \t[ ! -e \"$CARGO_HOME/$f\" ] || { echo \"CARGO_HOME holds $f\" >&2; exit 65; }\n\
         done\n\
         for out; do :; done\n\
         here=$(dirname \"$0\")\n\
         mkdir -p \"$out\" || exit 1\n\
         while read -r name version sum; do\n\
         \t[ -n \"$name\" ] || continue\n\
         \tmkdir -p \"$out/$name-$version/src\"\n\
         \tprintf 'pub fn f() {}\\n' >\"$out/$name-$version/src/lib.rs\"\n\
         \tprintf '{\"files\":{},\"package\":\"%s\"}' \"$sum\" >\"$out/$name-$version/.cargo-checksum.json\"\n\
         \tif [ -f \"$here/archive-$name-$version\" ]; then\n\
         \t\tmkdir -p \"$CARGO_HOME/registry/cache/index.crates.io-test\"\n\
         \t\tcp \"$here/archive-$name-$version\" \"$CARGO_HOME/registry/cache/index.crates.io-test/$name-$version.crate\"\n\
         \tfi\n\
         \t[ ! -f \"$here/link-$name-$version\" ] || ln -s ../../../../escape \"$out/$name-$version/src/up\"\n\
         done <\"$here/crates\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&targo, std::fs::Permissions::from_mode(0o755)).unwrap();
    targo
}

/// The names `dir` lists, exactly as they are on disk (what `exists()` cannot tell on
/// a volume that ignores case).
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::symlink_metadata(path).unwrap().modified().unwrap()
}

/// The take is the commit's objects, vendored and sealed, the submodule at its gitlink; the
/// digest is the same from two clones at different paths and across runs.
#[test]
fn a_snapshot_is_the_commits_objects_and_its_digest_is_path_independent() {
    let dir = scratch();
    let (repo, head) = fixture(dir.path());
    let targo = stub_targo(dir.path(), &format!("itoa 1.0.0 {}\n", checksum()));

    let cache = held(dir.path().join("a-release.noindex"));
    // An untracked file and an uncommitted edit are not git objects: neither reaches
    // the take.
    std::fs::write(repo.join("untracked.txt"), "not in the commit").unwrap();
    std::fs::write(repo.join("a.txt"), "an edit nobody committed\n").unwrap();
    let snap = materialize(&repo, "HEAD", &cache, &targo).unwrap();
    assert_eq!(snap.commit, head);
    assert_eq!(
        std::fs::read_to_string(snap.src.join("a.txt")).unwrap(),
        "alpha\n"
    );
    assert!(!snap.src.join("untracked.txt").exists());
    assert!(!snap.src.join(".git").exists(), "the take has no git dir");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&snap.src.join("run.sh")), 0o755);
    assert_eq!(mode(&snap.src.join("a.txt")), 0o644);
    assert_eq!(
        std::fs::read_to_string(snap.src.join("vendor/sub/lib.rs")).unwrap(),
        "pub fn sub() {}\n",
        "the submodule's files come from its own objects"
    );
    assert!(
        snap.root
            .join("vendor")
            .join("itoa-1.0.0/.cargo-checksum.json")
            .is_file()
    );
    let config = std::fs::read_to_string(&snap.sealed_config).unwrap();
    assert!(
        config.contains("replace-with = \"aterm-vendored\"")
            && config.contains("directory = \"vendor\"")
            && config.contains("offline = true"),
        "{config}"
    );
    assert!(aterm_codec::hex::is_lower(&snap.digest, 64));
    snap.verify().unwrap();

    // Again: nothing to write, the same digest.
    let again = materialize(&repo, &head, &cache, &targo).unwrap();
    assert_eq!(again.digest, snap.digest);

    // Another clone, at another path, into another cache: the same digest.
    let other = dir.path().join("elsewhere/clone");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            "--recurse-submodules",
            repo.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    let other_cache = held(dir.path().join("elsewhere/clone-release.noindex"));
    let theirs = materialize(&other, &head, &other_cache, &targo).unwrap();
    assert_eq!(theirs.digest, snap.digest);
}

/// The sync writes only what differs: an unchanged file keeps its mtime (cargo's path
/// fingerprints stay warm), a changed one is rewritten, a removed one goes.
#[test]
fn the_sync_rewrites_only_changed_files_and_removes_what_the_commit_dropped() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    let targo = stub_targo(dir.path(), &format!("itoa 1.0.0 {}\n", checksum()));
    let cache = held(dir.path().join("c-release.noindex"));
    let first = materialize(&repo, "HEAD", &cache, &targo).unwrap();
    let kept = mtime(&first.src.join("a.txt"));
    let vendored = mtime(&first.root.join("vendor").join("itoa-1.0.0/src/lib.rs"));
    // Residue a crashed run could leave in the take: removed by the next sync.
    std::fs::write(first.root.join("stray"), "residue").unwrap();

    std::fs::write(repo.join("run.sh"), "#!/bin/sh\necho changed\n").unwrap();
    std::fs::remove_file(repo.join("gone.txt")).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "second"]);
    let second = materialize(&repo, "HEAD", &cache, &targo).unwrap();

    assert_eq!(
        mtime(&second.src.join("a.txt")),
        kept,
        "an unchanged file is not written"
    );
    assert_eq!(
        mtime(&second.root.join("vendor").join("itoa-1.0.0/src/lib.rs")),
        vendored,
        "an unchanged vendored file is not written"
    );
    assert_eq!(
        std::fs::read_to_string(second.src.join("run.sh")).unwrap(),
        "#!/bin/sh\necho changed\n"
    );
    assert!(!second.src.join("gone.txt").exists());
    assert!(!second.root.join("stray").exists());
    assert_ne!(second.digest, first.digest);
}

/// `verify` refuses a byte changed or a file added after the take was digested, naming the
/// path.
#[test]
fn verify_refuses_a_take_changed_after_it_was_digested() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    let targo = stub_targo(dir.path(), &format!("itoa 1.0.0 {}\n", checksum()));
    let cache = held(dir.path().join("v-release.noindex"));
    let snap = materialize(&repo, "HEAD", &cache, &targo).unwrap();
    snap.verify().unwrap();

    std::fs::write(snap.src.join("a.txt"), "alphA\n").unwrap();
    let err = snap.verify().unwrap_err();
    assert!(err.contains("src/a.txt (changed)"), "{err}");

    std::fs::write(snap.src.join("a.txt"), "alpha\n").unwrap();
    snap.verify().unwrap();
    std::fs::write(
        snap.root.join("vendor").join("itoa-1.0.0/build-output"),
        "x",
    )
    .unwrap();
    let err = snap.verify().unwrap_err();
    assert!(
        err.contains("vendor/itoa-1.0.0/build-output (added)"),
        "{err}"
    );
}

/// A gitlink whose repository is not checked out is refused, naming it — never a take
/// with the submodule missing.
#[test]
fn an_unfetched_gitlink_refuses() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    let targo = stub_targo(dir.path(), "");
    let bare = dir.path().join("plain-clone");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            repo.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let cache = held(dir.path().join("u-release.noindex"));
    let err = materialize(&bare, "HEAD", &cache, &targo).unwrap_err();
    assert!(err.contains("vendor/sub"), "{err}");
}

/// Vendoring is held to the lockfile: a differing vendored checksum, a missing or differing
/// archive, an unnamed crate and a named one left out each refuse.
#[test]
fn vendored_crates_are_held_to_the_lockfile_checksums() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    let cache = held(dir.path().join("k-release.noindex"));
    let wrong = "0".repeat(64);
    let err = materialize(
        &repo,
        "HEAD",
        &cache,
        &stub_targo(&dir.path().join("wrong"), &format!("itoa 1.0.0 {wrong}\n")),
    )
    .unwrap_err();
    assert!(err.contains("itoa-1.0.0"), "{err}");

    // A matching json is not enough: the archive vendoring extracted must exist and hash to
    // the lock's checksum.
    let missing = stub_targo(
        &dir.path().join("missing"),
        &format!("itoa 1.0.0 {}\n", checksum()),
    );
    std::fs::remove_file(missing.with_file_name("archive-itoa-1.0.0")).unwrap();
    let fresh = held(dir.path().join("m-release.noindex"));
    let err = materialize(&repo, "HEAD", &fresh, &missing).unwrap_err();
    assert!(err.contains("itoa-1.0.0"), "{err}");
    let forged = stub_targo(
        &dir.path().join("forged"),
        &format!("itoa 1.0.0 {}\n", checksum()),
    );
    std::fs::write(forged.with_file_name("archive-itoa-1.0.0"), b"other bytes").unwrap();
    let err = materialize(&repo, "HEAD", &fresh, &forged).unwrap_err();
    assert!(err.contains("itoa-1.0.0.crate"), "{err}");

    let locked = vec![LockedCrate {
        name: "itoa".into(),
        version: "1.0.0".into(),
        checksum: checksum(),
    }];
    let vendor = dir.path().join("vendor-out");
    std::fs::create_dir_all(vendor.join("serde-1.0.0")).unwrap();
    let err = check_vendored(&locked, &vendor).unwrap_err();
    assert!(err.contains("serde-1.0.0"), "{err}");
    std::fs::remove_dir_all(vendor.join("serde-1.0.0")).unwrap();
    let err = check_vendored(&locked, &vendor).unwrap_err();
    assert!(err.contains("itoa-1.0.0"), "{err}");
}

/// A cached `.crate` whose SHA-256 differs from its lockfile checksum is deleted
/// before vendoring (cargo trusts its cache), and a matching one is kept.
#[test]
fn a_cached_crate_that_differs_from_the_lock_is_deleted() {
    let dir = scratch();
    let index = dir
        .path()
        .join("cargo-home/registry/cache/index.crates.io-0000");
    std::fs::create_dir_all(&index).unwrap();
    let good = b"the archive the lock names";
    let good_sum = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(good));
    std::fs::write(index.join("good-1.0.0.crate"), good).unwrap();
    std::fs::write(index.join("bad-2.0.0.crate"), b"tampered").unwrap();
    let locked = vec![
        LockedCrate {
            name: "good".into(),
            version: "1.0.0".into(),
            checksum: good_sum,
        },
        LockedCrate {
            name: "bad".into(),
            version: "2.0.0".into(),
            checksum: OTHER.into(),
        },
        LockedCrate {
            name: "absent".into(),
            version: "3.0.0".into(),
            checksum: OTHER.into(),
        },
    ];
    assert_eq!(
        verify_crate_cache(&locked, &dir.path().join("cargo-home")).unwrap(),
        1
    );
    assert!(index.join("good-1.0.0.crate").is_file());
    assert!(!index.join("bad-2.0.0.crate").exists());
    assert_eq!(
        verify_crate_cache(&locked, &dir.path().join("no-such-home")).unwrap(),
        0
    );
}

/// The lockfile's sources: crates.io packages with a checksum are vendored, path
/// packages are the workspace's own, and anything else is refused by name.
#[test]
fn the_lockfile_admits_crates_io_and_path_packages_only() {
    let crates = locked_crates(&format!(
        "version = 4\n[[package]]\nname = \"ws\"\nversion = \"0.1.0\"\n\
         [[package]]\nname = \"itoa\"\nversion = \"1.0.0\"\nsource = \
         \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{OTHER}\"\n"
    ))
    .unwrap();
    assert_eq!(
        crates,
        vec![LockedCrate {
            name: "itoa".into(),
            version: "1.0.0".into(),
            checksum: OTHER.into(),
        }]
    );
    let err = locked_crates(
        "[[package]]\nname = \"g\"\nversion = \"0.1.0\"\nsource = \
         \"git+https://example.invalid/g#abc\"\n",
    )
    .unwrap_err();
    assert!(err.contains("g 0.1.0"), "{err}");
    locked_crates(
        "[[package]]\nname = \"n\"\nversion = \"0.1.0\"\nsource = \
         \"registry+https://github.com/rust-lang/crates.io-index\"\n",
    )
    .unwrap_err();
}

/// One `.noindex` cache beside each clone, shared by its linked worktrees, held by one take at
/// a time; a symlink in its place is refused.
#[test]
fn the_cache_is_one_noindex_directory_beside_the_clone() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    let cache = CacheRoot::for_repo(&repo).unwrap();
    assert_eq!(cache.path(), canonical.join("repo-release.noindex"));
    assert_eq!(
        std::fs::metadata(cache.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let tree = dir.path().join("repo-cut.noindex");
    git(
        &repo,
        &["worktree", "add", "-q", "--detach", tree.to_str().unwrap()],
    );
    assert_eq!(CacheRoot::for_repo(&tree).unwrap().path(), cache.path());

    // A second hold, from the clone or its worktree, is told the holder's pid; a drop frees it.
    let hold = cache.hold().unwrap();
    CacheRoot::for_repo(&tree)
        .unwrap()
        .try_hold()
        .unwrap()
        .err()
        .expect("a second take is not let in while the first holds the cache");
    drop(hold);
    assert!(
        cache.try_hold().unwrap().is_ok(),
        "a dropped hold frees the cache"
    );

    let link = dir.path().join("linked-release.noindex");
    std::os::unix::fs::symlink(dir.path(), &link).unwrap();
    CacheRoot::at(link).unwrap_err();
}

/// A case-only rename lands under the new spelling on a case-insensitive volume, and the take
/// digests as a fresh take of the same commit does.
#[test]
fn a_case_only_rename_lands_under_the_new_spelling() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    std::fs::create_dir_all(repo.join("src/dir")).unwrap();
    std::fs::write(repo.join("src/dir/x.rs"), "pub fn x() {}\n").unwrap();
    std::fs::write(repo.join("same.txt"), "same bytes\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "lower"]);
    let targo = stub_targo(dir.path(), &format!("itoa 1.0.0 {}\n", checksum()));
    let cache = held(dir.path().join("case-release.noindex"));
    let first = materialize(&repo, "HEAD", &cache, &targo).unwrap();
    assert!(names(&first.src.join("src")).contains(&"dir".to_owned()));

    // The renames are made in the index alone, so the fixture's own volume plays no part.
    let x = git(&repo, &["rev-parse", "HEAD:src/dir/x.rs"]);
    let same = git(&repo, &["rev-parse", "HEAD:same.txt"]);
    git(
        &repo,
        &["rm", "-q", "-r", "--cached", "src/dir", "same.txt"],
    );
    for (oid, path) in [(&x, "src/Dir/x.rs"), (&same, "Same.txt")] {
        git(
            &repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{oid},{path}"),
            ],
        );
    }
    git(&repo, &["commit", "-qm", "upper"]);
    let second = materialize(&repo, "HEAD", &cache, &targo).unwrap();

    let inner = names(&second.src.join("src"));
    assert!(
        inner.contains(&"Dir".to_owned()) && !inner.contains(&"dir".to_owned()),
        "{inner:?}"
    );
    assert_eq!(names(&second.src.join("src/Dir")), ["x.rs"]);
    assert_eq!(
        std::fs::read_to_string(second.src.join("src/Dir/x.rs")).unwrap(),
        "pub fn x() {}\n"
    );
    let top = names(&second.src);
    assert!(
        top.contains(&"Same.txt".to_owned()) && !top.contains(&"same.txt".to_owned()),
        "{top:?}"
    );
    second.verify().unwrap();
    let fresh = held(dir.path().join("fresh-release.noindex"));
    assert_eq!(
        materialize(&repo, "HEAD", &fresh, &targo).unwrap().digest,
        second.digest
    );
}

/// A replace ref (`git replace`) does not reach the take, and bytes that are not the object
/// their id names (a corrupted or planted loose object) are refused, naming the path.
#[test]
fn replaced_or_substituted_objects_never_reach_the_take() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    let targo = stub_targo(dir.path(), &format!("itoa 1.0.0 {}\n", checksum()));
    let cache = held(dir.path().join("r-release.noindex"));
    let good = git(&repo, &["rev-parse", "HEAD:a.txt"]);
    let planted = dir.path().join("evil.txt");
    std::fs::write(&planted, "evil\n").unwrap();
    let evil = git(&repo, &["hash-object", "-w", planted.to_str().unwrap()]);

    git(&repo, &["replace", &good, &evil]);
    assert_eq!(
        git(&repo, &["cat-file", "blob", &good]),
        "evil",
        "the negative control: a plain git honours the replace ref"
    );
    let snap = materialize(&repo, "HEAD", &cache, &targo).unwrap();
    assert_eq!(
        std::fs::read_to_string(snap.src.join("a.txt")).unwrap(),
        "alpha\n"
    );
    git(&repo, &["replace", "-d", &good]);

    // The planted object's bytes under the committed id: git serves them unchecked.
    let loose = |oid: &str| repo.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
    std::fs::remove_file(loose(&good)).unwrap();
    std::fs::copy(loose(&evil), loose(&good)).unwrap();
    assert_eq!(git(&repo, &["cat-file", "blob", &good]), "evil");
    let err = materialize(&repo, "HEAD", &cache, &targo).unwrap_err();
    assert!(err.contains("src/a.txt"), "{err}");
}

/// A planted tree is refused as a planted blob is. git reads trees below the root unchecked,
/// so a `.cargo` planted under the committed id (its config naming a `rustc-wrapper`) is
/// listed by a plain `git ls-tree -r` (the negative control), and must never reach the take.
#[test]
fn a_planted_subtree_never_reaches_the_take() {
    let dir = scratch();
    let (repo, head) = fixture(dir.path());
    let targo = stub_targo(dir.path(), &format!("itoa 1.0.0 {}\n", checksum()));
    let cache = held(dir.path().join("t-release.noindex"));
    let good = git(&repo, &["rev-parse", "HEAD:.cargo"]);
    // Every object in the planted tree hashes correctly; only the tree's own bytes betray it.
    std::fs::write(
        repo.join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = \"/tmp/evil\"\n",
    )
    .unwrap();
    git(&repo, &["add", ".cargo/config.toml"]);
    let planted_root = git(&repo, &["write-tree"]);
    let evil = git(&repo, &["rev-parse", &format!("{planted_root}:.cargo")]);
    let wrapper = git(
        &repo,
        &["rev-parse", &format!("{planted_root}:.cargo/config.toml")],
    );
    assert_ne!(evil, good);

    let loose = |oid: &str| repo.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
    std::fs::remove_file(loose(&good)).unwrap();
    std::fs::copy(loose(&evil), loose(&good)).unwrap();
    assert_eq!(git(&repo, &["rev-parse", "HEAD^{commit}"]), head);
    assert!(
        git(&repo, &["ls-tree", "-r", "HEAD"]).contains(&format!("{wrapper}\t.cargo/config.toml")),
        "the negative control: a plain git lists the planted tree's entry as C's"
    );
    let err = materialize(&repo, "HEAD", &cache, &targo).unwrap_err();
    assert!(err.contains(&good) && err.contains("src/.cargo"), "{err}");
    let taken = std::fs::read_to_string(cache.cache().snapshot().join("src/.cargo/config.toml"))
        .unwrap_or_default();
    assert!(!taken.contains("rustc-wrapper"), "{taken}");
}

/// The root trees (the commit's and each gitlink's) are held to their ids too: another root
/// tree planted under the recorded id is refused, naming where.
#[test]
fn a_planted_root_tree_never_reaches_the_take() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    let targo = stub_targo(dir.path(), &format!("itoa 1.0.0 {}\n", checksum()));
    let loose = |git_dir: &Path, oid: &str| git_dir.join("objects").join(&oid[..2]).join(&oid[2..]);
    let plant = |at: &Path, file: &str, of: &str| {
        let good = git(at, &["rev-parse", &format!("{of}^{{tree}}")]);
        std::fs::write(at.join(file), "planted\n").unwrap();
        git(at, &["add", file]);
        let evil = git(at, &["write-tree"]);
        assert_ne!(evil, good);
        let objects = PathBuf::from(git(
            at,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
        ));
        std::fs::remove_file(loose(&objects, &good)).unwrap();
        std::fs::copy(loose(&objects, &evil), loose(&objects, &good)).unwrap();
        good
    };

    let good = plant(&repo, "a.txt", "HEAD");
    let cache = held(dir.path().join("r-release.noindex"));
    let err = materialize(&repo, "HEAD", &cache, &targo).unwrap_err();
    assert!(err.contains(&good), "{err}");
}

/// As above, for a submodule's root tree.
#[test]
fn a_planted_submodule_root_tree_never_reaches_the_take() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    let targo = stub_targo(dir.path(), &format!("itoa 1.0.0 {}\n", checksum()));
    let sub = repo.join("vendor/sub");
    let gitlink = git(&repo, &["rev-parse", "HEAD:vendor/sub"]);
    let good = git(&sub, &["rev-parse", &format!("{gitlink}^{{tree}}")]);
    std::fs::write(sub.join("lib.rs"), "planted\n").unwrap();
    git(&sub, &["add", "lib.rs"]);
    let evil = git(&sub, &["write-tree"]);
    assert_ne!(evil, good);
    let objects = PathBuf::from(git(
        &sub,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ],
    ));
    let loose = |oid: &str| objects.join(&oid[..2]).join(&oid[2..]);
    std::fs::remove_file(loose(&good)).unwrap();
    std::fs::copy(loose(&evil), loose(&good)).unwrap();
    let cache = held(dir.path().join("s-release.noindex"));
    let err = materialize(&repo, "HEAD", &cache, &targo).unwrap_err();
    assert!(err.contains(&good) && err.contains("vendor/sub"), "{err}");
}

/// A symbolic link is refused, from the tree and from vendoring alike: what it reaches is no
/// byte the digest covers.
#[test]
fn a_symbolic_link_is_refused_in_the_tree_and_in_the_vendored_output() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    let targo = stub_targo(dir.path(), &format!("itoa 1.0.0 {}\n", checksum()));
    std::fs::write(targo.with_file_name("link-itoa-1.0.0"), "").unwrap();
    let cache = held(dir.path().join("l-release.noindex"));
    let err = materialize(&repo, "HEAD", &cache, &targo).unwrap_err();
    assert!(err.contains("vendor/itoa-1.0.0/src/up"), "{err}");

    std::os::unix::fs::symlink("../../../repo/a.txt", repo.join("escape")).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "link"]);
    let err = materialize(&repo, "HEAD", &cache, &targo).unwrap_err();
    assert!(err.contains("escape"), "{err}");
}

/// A Cargo config in the vendoring `CARGO_HOME` (one could repoint crates.io) is removed
/// before `targo vendor` runs; the stand-in refuses to vendor while one is there.
#[test]
fn a_cargo_config_in_the_vendoring_home_is_removed_before_vendoring() {
    let dir = scratch();
    let (repo, _) = fixture(dir.path());
    let targo = stub_targo(dir.path(), &format!("itoa 1.0.0 {}\n", checksum()));
    let cache = held(dir.path().join("h-release.noindex"));
    let home = cache.cache().vendor_cargo_home();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "[source.crates-io]\nreplace-with = \"elsewhere\"\n",
    )
    .unwrap();
    std::fs::write(home.join("config"), "").unwrap();
    materialize(&repo, "HEAD", &cache, &targo).unwrap();
    assert!(!home.join("config.toml").exists() && !home.join("config").exists());
}
