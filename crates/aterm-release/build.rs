// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stamp the cutter with the commit it was BUILT from.
//!
//! Every pre-claim gate proves something about the TREE — `clean_tree`,
//! `on_main`, `head_matches_origin` — and none of them proves anything about
//! the binary doing the proving. That gap is not hypothetical: v0.63.0 shipped
//! a 1.07 GB seeded image and an `-x86_64.dmg` from a tree whose source had
//! carried the one-lean-download lane since 52c1936f, because the `aterm-release`
//! binary that ran had been built from an older tree. It validated its own
//! output against its own older rules and passed every gate.
//!
//! So the binary carries its own commit, and `gates::cutter_identity_gate`
//! refuses a real cut whose stamp is not the tree's `HEAD`.
//!
//! FAIL-CLOSED BY CONSTRUCTION. A build whose source closure is dirty carries
//! Git's all-zero null object ID, which can never equal `HEAD`; when Git or the
//! commit cannot be read the stamp is `unknown`. Neither value can pass the
//! runtime identity gate after the checkout is made clean. This matters for a
//! binary built from an uncommitted release edit and then run after that edit
//! was reverted: matching `HEAD` alone would otherwise bless code that commit
//! never contained.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The value the gate reads when the build could not learn its own commit.
const UNKNOWN: &str = "unknown";

/// A dirty build must retain the 40-hex shape the release crate's wiring test
/// checks, without ever being able to equal a real Git commit. The all-zero
/// object ID is Git's reserved null value: updating a ref to it DELETES the ref,
/// so `HEAD` can never resolve to this value.
const DIRTY_BUILD_COMMIT: &str = "0000000000000000000000000000000000000000";

/// Repository-owned inputs that can change the `aterm-release` binary.
///
/// Keep this a conservative SUPERSET of the first-party dependency closure.
/// The same list drives both Cargo's rerun tracking and the build-time dirty
/// probe: adding a watched source without adding it to the fail-closed check (or
/// vice versa) would recreate the stale-clean-stamp hole. Registry sources are
/// content-addressed by Cargo.lock; all workspace/path/patch sources live under
/// `crates` or `vendor`.
const SOURCE_INPUTS: &[&str] = &[
    "crates",
    "vendor",
    ".cargo",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
];

#[derive(Debug, PartialEq, Eq)]
enum BuildStamp {
    Clean(String),
    Dirty,
    Unknown,
}

impl BuildStamp {
    fn value(&self) -> &str {
        match self {
            Self::Clean(commit) => commit,
            Self::Dirty => DIRTY_BUILD_COMMIT,
            Self::Unknown => UNKNOWN,
        }
    }
}

fn command_stdout(command: &mut Command) -> Option<String> {
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|stdout| stdout.trim().to_string())
}

fn git_stdout(repo: &Path, args: &[&str]) -> Option<String> {
    command_stdout(Command::new("git").arg("-C").arg(repo).args(args))
}

fn absolute_git_output(repo: &Path, args: &[&str]) -> Option<PathBuf> {
    let value = git_stdout(repo, args)?;
    if value.is_empty() {
        return None;
    }
    let path = PathBuf::from(value);
    Some(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    })
}

fn git_path(repo: &Path, logical: &str) -> Option<PathBuf> {
    absolute_git_output(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-path", logical],
    )
    // `--path-format` arrived after `--git-path`. The fallback retains correct
    // behaviour on older Git; `-C repo` makes a relative answer relative to the
    // repository root.
    .or_else(|| absolute_git_output(repo, &["rev-parse", "--git-path", logical]))
}

fn git_dir(repo: &Path) -> Option<PathBuf> {
    absolute_git_output(repo, &["rev-parse", "--absolute-git-dir"])
}

fn git_common_dir(repo: &Path) -> Option<PathBuf> {
    absolute_git_output(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .or_else(|| absolute_git_output(repo, &["rev-parse", "--git-common-dir"]))
}

fn canonical_sha1(commit: &str) -> bool {
    commit.len() == 40
        && commit != DIRTY_BUILD_COMMIT
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn source_status(repo: &Path) -> Option<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args([
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
            "--",
        ])
        .args(SOURCE_INPUTS);
    command_stdout(&mut command)
}

fn build_stamp(repo: &Path) -> BuildStamp {
    let Some(commit) = git_stdout(repo, &["rev-parse", "--verify", "HEAD^{commit}"])
        .filter(|commit| canonical_sha1(commit))
    else {
        return BuildStamp::Unknown;
    };
    match source_status(repo) {
        Some(status) if status.is_empty() => BuildStamp::Clean(commit),
        Some(_) => BuildStamp::Dirty,
        None => BuildStamp::Unknown,
    }
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

/// If a loose ref is currently packed, watch the closest existing directory
/// below `refs/` so creation of the loose ref is observed. `packed-refs` itself
/// is watched separately. This avoids both the old false path under a linked
/// worktree's private git-dir and a permanently-missing Cargo input.
fn loose_ref_watch(ref_path: &Path, refs_root: &Path) -> Option<PathBuf> {
    if ref_path.exists() {
        return Some(ref_path.to_path_buf());
    }
    let mut candidate = ref_path.parent()?;
    loop {
        if !candidate.starts_with(refs_root) {
            return None;
        }
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        if candidate == refs_root {
            return None;
        }
        candidate = candidate.parent()?;
    }
}

fn git_watch_paths(repo: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Some(git_dir) = git_dir(repo) else {
        return paths;
    };
    let common_dir = git_common_dir(repo).unwrap_or_else(|| git_dir.clone());

    // HEAD is worktree-private. The index is too, and makes staging/unstaging a
    // dirty source transition observable even when file bytes did not move.
    for path in [git_dir.join("HEAD"), git_dir.join("index")] {
        if path.exists() {
            push_unique(&mut paths, path);
        }
    }

    // Ref-backend conversion is recorded in the common config; per-worktree
    // config can change how the private git-dir is interpreted.
    for path in [common_dir.join("config"), git_dir.join("config.worktree")] {
        if path.exists() {
            push_unique(&mut paths, path);
        }
    }

    if let Some(reference) = git_stdout(repo, &["symbolic-ref", "--quiet", "HEAD"])
        .filter(|reference| !reference.is_empty())
        && let (Some(ref_path), Some(refs_root)) =
            (git_path(repo, &reference), git_path(repo, "refs"))
        && let Some(path) = loose_ref_watch(&ref_path, &refs_root)
    {
        push_unique(&mut paths, path);
    }

    // Files backend: a currently packed active ref changes here. Reftable
    // backend: ref updates and compactions replace files below `reftable/`.
    // `--git-path reftable` is worktree-private on some Git versions, while
    // branch refs live in the common store, so probe BOTH locations.
    let storage = [
        git_path(repo, "packed-refs"),
        Some(common_dir.join("packed-refs")),
        git_path(repo, "reftable"),
        Some(common_dir.join("reftable")),
        Some(git_dir.join("reftable")),
    ];
    for path in storage.into_iter().flatten().filter(|path| path.exists()) {
        push_unique(&mut paths, path);
    }

    paths.sort();
    paths
}

fn workspace_root() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

fn main() {
    let Some(repo) = workspace_root() else {
        println!("cargo:rustc-env=ATERM_RELEASE_BUILD_COMMIT={UNKNOWN}");
        return;
    };

    // Cargo's build-script directives narrow its default whole-package scan.
    // Watch the complete repository-owned source superset explicitly, using the
    // SAME list the dirtiness probe judges.
    for input in SOURCE_INPUTS {
        println!("cargo:rerun-if-changed={}", repo.join(input).display());
    }
    for path in git_watch_paths(&repo) {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let stamp = build_stamp(&repo);
    match stamp {
        BuildStamp::Dirty => println!(
            "cargo:warning=aterm-release cutter stamp is fail-closed: its build-time source closure is dirty"
        ),
        BuildStamp::Unknown => println!(
            "cargo:warning=aterm-release cutter stamp is fail-closed: Git identity could not be established"
        ),
        BuildStamp::Clean(_) => {}
    }
    println!(
        "cargo:rustc-env=ATERM_RELEASE_BUILD_COMMIT={}",
        stamp.value()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aterm-release-build-stamp-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create build-stamp test directory");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        git_stdout(repo, args)
            .unwrap_or_else(|| panic!("git {args:?} failed in {}", repo.display()))
    }

    fn seed_files(repo: &Path) {
        fs::create_dir_all(repo.join("crates/aterm-release/src")).unwrap();
        fs::create_dir_all(repo.join("vendor/example/src")).unwrap();
        fs::create_dir_all(repo.join(".cargo")).unwrap();
        fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        fs::write(repo.join("Cargo.lock"), "# lock\n").unwrap();
        fs::write(
            repo.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"stable\"\n",
        )
        .unwrap();
        fs::write(repo.join(".cargo/config.toml"), "[build]\n").unwrap();
        fs::write(
            repo.join("crates/aterm-release/src/main.rs"),
            "fn main() {}\n",
        )
        .unwrap();
        fs::write(
            repo.join("vendor/example/src/lib.rs"),
            "pub fn value() {}\n",
        )
        .unwrap();
        fs::write(repo.join("README.md"), "outside the source closure\n").unwrap();
    }

    fn init_repo(repo: &Path, ref_format: Option<&str>) -> bool {
        fs::create_dir_all(repo).unwrap();
        let mut command = Command::new("git");
        command.arg("init").arg("-q");
        if let Some(format) = ref_format {
            command.arg(format!("--ref-format={format}"));
        }
        command.arg(repo);
        if !command.status().expect("run git init").success() {
            return false;
        }
        git(repo, &["config", "user.name", "Andrew Yates"]);
        git(repo, &["config", "user.email", "test@example.invalid"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
        seed_files(repo);
        git(repo, &["add", "--all"]);
        git(repo, &["commit", "-q", "-m", "initial"]);
        true
    }

    #[test]
    fn dirty_source_closure_uses_the_null_oid_but_unrelated_docs_do_not() {
        let scratch = Scratch::new("dirty");
        let repo = scratch.0.join("repo");
        assert!(init_repo(&repo, None));
        let head = git(&repo, &["rev-parse", "HEAD"]);
        assert_eq!(build_stamp(&repo), BuildStamp::Clean(head.clone()));

        fs::write(
            repo.join("crates/aterm-release/src/main.rs"),
            "fn main() { println!(\"dirty\"); }\n",
        )
        .unwrap();
        assert_eq!(build_stamp(&repo), BuildStamp::Dirty);
        assert_eq!(BuildStamp::Dirty.value(), DIRTY_BUILD_COMMIT);
        assert!(!canonical_sha1(DIRTY_BUILD_COMMIT));

        git(&repo, &["restore", "crates/aterm-release/src/main.rs"]);
        fs::write(repo.join("crates/new-untracked.rs"), "pub fn dirty() {}\n").unwrap();
        assert_eq!(build_stamp(&repo), BuildStamp::Dirty);
        fs::remove_file(repo.join("crates/new-untracked.rs")).unwrap();

        // The watcher/dirtiness set is deliberately the compiler closure, not
        // every runbook or prose file in the repository.
        fs::write(repo.join("README.md"), "documentation-only edit\n").unwrap();
        assert_eq!(build_stamp(&repo), BuildStamp::Clean(head));
    }

    #[test]
    fn linked_worktree_watches_the_common_branch_ref_and_packed_store() {
        let scratch = Scratch::new("worktree");
        let repo = scratch.0.join("repo");
        let linked = scratch.0.join("linked");
        assert!(init_repo(&repo, None));
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "build-stamp-test",
                linked.to_str().unwrap(),
            ],
        );

        let reference = git(&linked, &["symbolic-ref", "HEAD"]);
        let expected_ref = git_path(&linked, &reference).expect("resolve common loose ref");
        let private_git_dir = git_dir(&linked).expect("linked worktree git dir");
        assert!(expected_ref.exists());
        assert_ne!(expected_ref, private_git_dir.join(&reference));
        let paths = git_watch_paths(&linked);
        assert!(
            paths.contains(&expected_ref),
            "linked-worktree watchers omit {}: {paths:?}",
            expected_ref.display()
        );

        git(&repo, &["pack-refs", "--all"]);
        let packed = git_common_dir(&linked).unwrap().join("packed-refs");
        assert!(packed.exists());
        assert!(git_watch_paths(&linked).contains(&packed));
    }

    #[test]
    fn reftable_store_is_watched_when_this_git_supports_it() {
        let scratch = Scratch::new("reftable");
        let repo = scratch.0.join("repo");
        if !init_repo(&repo, Some("reftable")) {
            // Compatibility lane for Git versions predating `--ref-format`.
            return;
        }
        let reftable = git_common_dir(&repo).unwrap().join("reftable");
        assert!(reftable.is_dir());
        assert!(
            git_watch_paths(&repo).contains(&reftable),
            "reftable directory is not watched"
        );
    }

    #[test]
    fn rerun_and_dirty_inputs_are_one_source_of_truth() {
        assert_eq!(
            SOURCE_INPUTS,
            &[
                "crates",
                "vendor",
                ".cargo",
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml",
            ]
        );
        assert!(SOURCE_INPUTS.contains(&"crates"));
        assert!(SOURCE_INPUTS.contains(&"vendor"));
        assert!(SOURCE_INPUTS.contains(&"Cargo.lock"));
    }
}
