// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_main_checkout_is_the_first_worktree_listed() {
    let listing = "worktree C:/Users//a b/aterm\nHEAD 1a37de31a340\nbranch refs/heads/main\n\n\
                   worktree C:/Users//a b/aterm-w11\nHEAD 18a56f966eb2\ndetached\n";
    assert_eq!(first_worktree(listing), Some("C:/Users//a b/aterm"));
    // Negative controls: a listing that names none.
    assert_eq!(first_worktree(""), None);
    assert_eq!(first_worktree("HEAD 1a37de31a340\nworktree \n"), None);
}

#[test]
fn the_record_is_read_back_as_it_was_written() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let layout = Layout {
        local: scratch.path().to_path_buf(),
    };
    assert_eq!(recorded(&layout), None, "no record yet");
    let repo = scratch.path().join("a b").join("aterm");
    record(&layout, &repo).expect("record");
    assert_eq!(recorded(&layout), Some(repo));
    assert!(layout.repo_record().starts_with(scratch.path()));
    // An empty record names nothing.
    std::fs::write(layout.repo_record(), "\n").expect("write");
    assert_eq!(recorded(&layout), None);
}

/// A repository with one commit on `main` that carries the Windows build
/// script, a clone of it (whose `origin/main` is that commit), and the layout
/// of a scratch `%LOCALAPPDATA%`.
#[cfg(windows)]
struct Fixture {
    scratch: aterm_tempfile::TempDir,
}

#[cfg(windows)]
impl Fixture {
    fn run(dir: &Path, args: &[&str]) {
        let out = aterm_uds::deadline::output_within(
            aterm_uds::fixture_git::git().arg("-C").arg(dir).args(args),
            Duration::from_secs(60),
        )
        .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn new() -> Self {
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        let root = scratch.path();
        let origin = root.join("origin");
        let script = origin.join("apps").join("aterm-win");
        std::fs::create_dir_all(&script).expect("dirs");
        std::fs::write(script.join("build.ps1"), "exit 0\n").expect("script");
        Self::run(
            root,
            &["init", "--quiet", "--initial-branch=main", "origin"],
        );
        Self::run(&origin, &["add", "."]);
        Self::run(&origin, &["commit", "--quiet", "-m", "one"]);
        Self::run(root, &["clone", "--quiet", "origin", "repo"]);
        Self::run(root, &["clone", "--quiet", "origin", "fork"]);
        Self { scratch }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.scratch.path().join(name)
    }

    fn layout(&self) -> Layout {
        Layout {
            local: self.path("local"),
        }
    }
}

#[cfg(windows)]
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[cfg(windows)]
#[test]
fn the_repository_is_the_flag_then_the_record_then_the_tree_then_here() {
    let fx = Fixture::new();
    let layout = fx.layout();
    let (repo, fork) = (fx.path("repo"), fx.path("fork"));
    let outside = fx.path("local");
    std::fs::create_dir_all(&outside).expect("dir");

    // Nothing names one: refused, and the refusal says how to name it.
    let none = resolve(None, &layout, &outside).expect_err("nothing names a repository");
    assert!(none.contains("--repo <dir>"), "{none}");

    // Started inside an aterm checkout, that checkout is it.
    let inside = repo.join("apps").join("aterm-win");
    let here = resolve(None, &layout, &inside).expect("found from the working directory");
    assert_eq!(
        (canonical(&here.dir), here.from),
        (canonical(&repo), Source::Here)
    );

    // The build tree outranks the working directory.
    let src = layout.src();
    std::fs::create_dir_all(src.parent().expect("parent")).expect("dirs");
    Fixture::run(
        &fork,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            &src.to_string_lossy(),
            "origin/main",
        ],
    );
    let tree = resolve(None, &layout, &inside).expect("found from the build tree");
    assert_eq!(
        (canonical(&tree.dir), tree.from),
        (canonical(&fork), Source::Tree)
    );

    // The record outranks the tree, and --repo outranks the record.
    record(&layout, &repo).expect("record");
    let rec = resolve(None, &layout, &outside).expect("found from the record");
    assert_eq!(
        (canonical(&rec.dir), rec.from),
        (canonical(&repo), Source::Recorded)
    );
    let flag = resolve(Some(&fork), &layout, &outside).expect("found from the flag");
    assert_eq!(
        (canonical(&flag.dir), flag.from),
        (canonical(&fork), Source::Flag)
    );

    // A linked worktree names the repository it hangs off: the build tree is
    // one of `fork`, so naming it names `fork`'s main checkout.
    let linked = resolve(Some(&src), &layout, &outside).expect("a linked worktree");
    assert_eq!(canonical(&linked.dir), canonical(&fork));
    // A relative --repo is read from the working directory, and comes back absolute.
    let relative = resolve(Some(Path::new("..\\repo")), &layout, &fork).expect("relative");
    assert!(relative.dir.is_absolute(), "{}", relative.dir.display());
    assert_eq!(canonical(&relative.dir), canonical(&repo));

    // Negative controls: a flag or a record that names no repository is refused, not skipped.
    let bad = resolve(Some(&outside), &layout, &inside).expect_err("not a repository");
    assert!(bad.contains("is not a git repository"), "{bad}");
    record(&layout, &outside).expect("record");
    let stale = resolve(None, &layout, &inside).expect_err("a stale record");
    assert!(stale.contains("repo.txt"), "{stale}");

    assert!(same(&src, &fork).expect("comparable"));
    assert!(!same(&src, &repo).expect("comparable"));
}

#[cfg(windows)]
fn named(dir: &Path, from: Source) -> Repo {
    Repo {
        dir: dir.to_path_buf(),
        from,
    }
}

/// The build tree is made as a worktree of the repository, the repository is
/// recorded, and a tree deleted by hand is taken back without pruning: another
/// worktree whose folder is missing keeps its registration.
#[cfg(windows)]
#[test]
fn the_build_tree_is_made_and_remade_without_pruning_the_repository() {
    use crate::update::{Out, ensure_tree};

    let fx = Fixture::new();
    let layout = fx.layout();
    let repo = fx.path("repo");
    let src = layout.src();
    let mut out = Out::terminal();

    ensure_tree(&named(&repo, Source::Flag), &layout, &mut out).expect("the tree is made");
    assert!(
        src.join("apps")
            .join("aterm-win")
            .join("build.ps1")
            .is_file()
    );
    assert!(same(&src, &repo).expect("comparable"));
    assert_eq!(
        recorded(&layout),
        Some(repo.clone()),
        "the repository is recorded"
    );

    // Another worktree of the repository whose folder is missing (moved by
    // hand, or on a drive that is not there): what a prune would destroy.
    let other = fx.path("other");
    Fixture::run(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            &other.to_string_lossy(),
            "origin/main",
        ],
    );
    let registration = repo.join(".git").join("worktrees").join("other");
    assert!(registration.join("HEAD").is_file());
    std::fs::rename(&other, fx.path("moved")).expect("the folder is moved away");

    // The build tree deleted by hand leaves its registration behind too.
    std::fs::remove_dir_all(&src).expect("the tree is deleted by hand");
    // Control: git refuses to add over that registration unless forced.
    let refused = git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            &src.to_string_lossy(),
            "origin/main",
        ],
        GIT_LOCAL,
    )
    .expect_err("a registered path is refused without --force");
    assert!(refused.contains("already registered"), "{refused}");

    ensure_tree(&named(&repo, Source::Recorded), &layout, &mut out).expect("the tree is remade");
    assert!(
        src.join("apps")
            .join("aterm-win")
            .join("build.ps1")
            .is_file()
    );
    assert!(
        registration.join("HEAD").is_file(),
        "the missing worktree's registration was pruned"
    );
}

/// A build tree of one repository is never built in another's name.
#[cfg(windows)]
#[test]
fn a_build_tree_of_another_repository_is_refused() {
    use crate::update::{Out, ensure_tree};

    let fx = Fixture::new();
    let layout = fx.layout();
    let (repo, fork) = (fx.path("repo"), fx.path("fork"));
    let mut out = Out::terminal();
    ensure_tree(&named(&repo, Source::Flag), &layout, &mut out).expect("the tree is made");

    let refused = ensure_tree(&named(&fork, Source::Flag), &layout, &mut out)
        .expect_err("the tree belongs to another repository");
    assert!(refused.contains("is a worktree of"), "{refused}");
    assert!(refused.contains("worktree remove --force"), "{refused}");
    assert!(
        refused.contains(&fork.display().to_string()),
        "names the repository asked for: {refused}"
    );
    // Controls: its own repository is taken, by either of its worktrees' names.
    ensure_tree(&named(&repo, Source::Flag), &layout, &mut out).expect("its own repository");
    ensure_tree(&named(&layout.src(), Source::Recorded), &layout, &mut out)
        .expect("the tree itself names the same repository");
}
