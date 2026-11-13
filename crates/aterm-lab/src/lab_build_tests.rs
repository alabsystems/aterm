// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
