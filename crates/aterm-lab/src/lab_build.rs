// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The build the lab's evaluator judges in, which aos-search's bundles and the M6 learner's
//! stores name. A neutral module (design §13.3) so the learner reaches it without naming
//! `aos_search`.
//!
//! The commit is read when the lab runs, never compiled in: a compiled-in stamp made the lab's
//! build script watch every tracked file, so any edit anywhere rebuilt the lab. `targo lab` builds
//! the checkout before it runs it, so there the checkout's commit names the source the program was
//! built from, and an edit made since marks it `-dirty`; a dirty build names no clean commit, so
//! the gates that need one (`aos-learn-bench`'s G0, a bundle verified against an instance) refuse
//! it. A binary run some other way names the checkout as it is when it runs.

use std::path::Path;
use std::sync::OnceLock;

use crate::Fail;
use crate::paths::{CRATE_DIR, paths};

/// The paths a commit's `-dirty` is of, one rule with every other stamp producer's.
#[path = "../../aterm/stamp_paths.rs"]
pub(crate) mod stamp_paths;

/// The variable that names the commit a run reports instead of the checkout's: a run with no
/// work tree pins it, and `aos-learn-bench` passes its own to the copy of itself it runs.
pub const PIN: &str = aterm_stamp::input::COMMIT;

/// The commit this run names: [`PIN`] when it is set and not blank, else the checkout's
/// ([`commit_of`] of [`paths`]' `repo`). Read once.
pub fn lab_commit() -> &'static str {
    static COMMIT: OnceLock<String> = OnceLock::new();
    COMMIT.get_or_init(|| {
        let pin = std::env::var(PIN).ok();
        commit_from(pin.as_deref(), &paths().repo)
    })
}

/// The build the lab's evaluator judges in and its bundles name: `aterm-lab+g` and
/// [`lab_commit`]. A dev runner carries no product version (no library compiles one in,
/// `aterm_stamp`), and builds pair by their commit.
pub fn lab_build() -> &'static str {
    static BUILD: OnceLock<String> = OnceLock::new();
    BUILD.get_or_init(|| format!("aterm-lab+g{}", lab_commit()))
}

/// [`lab_commit`]'s rule: the pin when it is set and not blank, else [`commit_of`] `repo`.
fn commit_from(pin: Option<&str>, repo: &Path) -> String {
    match pin.map(str::trim).filter(|p| !p.is_empty()) {
        Some(pin) => pin.to_owned(),
        None => commit_of(repo),
    }
}

/// The checkout `repo`'s commit: HEAD's first 12 hex digits, `-dirty` when `git status
/// --porcelain` over [`stamp_paths::PATHSPEC`] prints anything; `unknown` when `repo` holds no
/// lab crate or git cannot read it.
fn commit_of(repo: &Path) -> String {
    let unknown = || "unknown".to_owned();
    if !repo.join(CRATE_DIR).join("Cargo.toml").is_file() {
        return unknown();
    }
    let git = |args: &[&str]| crate::measure::git(repo, args).ok();
    let Some(head) = git(&["rev-parse", "--short=12", "HEAD"]).filter(|h| !h.is_empty()) else {
        return unknown();
    };
    match git(&[&["status", "--porcelain"][..], &stamp_paths::PATHSPEC].concat()) {
        Some(changes) if changes.is_empty() => head,
        Some(_) => format!("{head}-dirty"),
        None => unknown(),
    }
}

/// Refuses a build that names no commit.
/// # Errors
/// Exit 1, naming the build.
pub fn check_build(build: &str) -> Result<(), Fail> {
    let commit = build.rsplit_once("+g").map_or("", |(_, commit)| commit);
    let hash = commit.strip_suffix("-dirty").unwrap_or(commit);
    if hash.len() >= 12 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(Fail::die(format!(
            "the build `{build}` names no commit (run outside a git checkout; pin one with \
             {PIN}), so what it writes could not name the interpreter that judged it"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `git -C dir args…`, which must succeed, as a test's own commits need no identity.
    fn git(dir: &Path, args: &[&str]) {
        let ok = aterm_uds::spawnfd::command("/usr/bin/git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=aterm-lab",
                "-c",
                "user.email=lab@aterm.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .expect("git runs");
        assert!(
            ok.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&ok.stderr)
        );
    }

    #[test]
    fn the_commit_is_the_checkouts_read_at_run_time() {
        let dir = aterm_tempfile::tempdir().expect("a scratch dir");
        let repo = dir.path().join("checkout");
        std::fs::create_dir_all(repo.join(CRATE_DIR)).expect("crate dir");
        git(&repo, &["init", "-q"]);
        let manifest = repo.join(CRATE_DIR).join("Cargo.toml");
        std::fs::write(&manifest, "[package]\n").expect("manifest");
        assert_eq!(commit_of(&repo), "unknown", "no commit yet");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        // A work tree with a commit but no lab crate names none.
        std::fs::remove_file(&manifest).expect("no manifest");
        assert_eq!(commit_of(&repo), "unknown", "no lab crate");
        std::fs::write(&manifest, "[package]\n").expect("manifest back");
        let head = commit_of(&repo);
        assert_eq!(head.len(), 12, "{head}");
        assert!(head.bytes().all(|b| b.is_ascii_hexdigit()), "{head}");
        assert!(check_build(&format!("aterm-lab+g{head}")).is_ok());

        // The measured ledger is no part of the stamp; any other change is.
        std::fs::create_dir_all(repo.join("docs/measured")).expect("ledger");
        std::fs::write(repo.join("docs/measured/run.md"), "x").expect("ledger entry");
        assert_eq!(commit_of(&repo), head, "a ledger entry leaves it clean");
        std::fs::write(repo.join("new.rs"), "x").expect("a new file");
        assert_eq!(
            commit_of(&repo),
            format!("{head}-dirty"),
            "an untracked file"
        );
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "two"]);
        let next = commit_of(&repo);
        assert_ne!(next, head, "a commit moves it");
        std::fs::write(&manifest, "[lib]\n").expect("an edit");
        assert_eq!(commit_of(&repo), format!("{next}-dirty"), "an edit");

        // The pin wins over the checkout; a blank one does not.
        assert_eq!(commit_from(Some(" 0123456789ab "), &repo), "0123456789ab");
        assert_eq!(commit_from(Some(" "), &repo), format!("{next}-dirty"));
        assert_eq!(commit_from(None, &dir.path().join("none")), "unknown");
    }

    #[test]
    fn a_build_that_names_no_commit_is_refused() {
        let v = "0.157.0";
        for commit in [
            "0123456789ab",
            "0123456789ab-dirty",
            "0123456789abcdef0123456789abcdef01234567",
        ] {
            let build = format!("aterm-lab {v}+g{commit}");
            assert!(check_build(&build).is_ok(), "{build}");
        }
        for build in [
            format!("aterm-lab {v}+gunknown"),
            format!("aterm-lab {v}"),
            format!("aterm-lab {v}+g"),
            format!("aterm-lab {v}+g0123456789a"),
            format!("aterm-lab {v}+g0123456789AB"),
            format!("aterm-lab {v}+g0123456789ab-dirty-dirty"),
        ] {
            let f = check_build(&build).expect_err(&build);
            assert_eq!(f.code, 1, "{build}");
            let msg = f.msg.unwrap_or_default();
            assert!(
                msg.contains(&format!("the build `{build}` names no commit")),
                "{msg}"
            );
        }
    }
}
