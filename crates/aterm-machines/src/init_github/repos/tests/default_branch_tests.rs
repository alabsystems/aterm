// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Saved inventory describes a repository's identity, not an immutable default branch.
/// The fresh remote advertisement still binds the checkout's actual branch and commit.
#[test]
fn a_real_new_checkout_uses_the_advertised_default_and_refuses_a_later_remote_change() {
    crate::items::github::need_clt_git();
    let sys = Live::new();
    let home = Scratch::new(&sys, "github-default-branch").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = home.path().to_string_lossy().into_owned();
    let run = |at: Option<&Path>, args: &[&str]| {
        text(
            &sys,
            &git(&sys, &who, at)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(crate::checkout::NO_SIDE_RUNS)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                ])
                .args(args.iter().copied()),
            "isolated local Git fixture failed",
        )
        .unwrap()
    };
    for (index, cached) in [Some("main"), None].into_iter().enumerate() {
        for change in ["none", "commit", "branch"] {
            let source = home.path().join(format!("source-{index}-{change}"));
            let source_text = source.to_str().unwrap();
            run(
                None,
                &["init", "--template=", "--initial-branch=trunk", source_text],
            );
            sys.write_file(&source.join("code"), b"first commit\n", 0o600)
                .unwrap();
            run(Some(&source), &["add", "--", "code"]);
            run(Some(&source), &["commit", "-m", "first commit"]);
            let advertised = remote(&run(
                None,
                &[
                    "ls-remote",
                    "--symref",
                    "--",
                    source_text,
                    "HEAD",
                    "refs/heads/*",
                    "refs/tags/*",
                ],
            ))
            .unwrap();
            match change {
                "commit" => run(
                    Some(&source),
                    &["commit", "--allow-empty", "-m", "after advertisement"],
                ),
                "branch" => run(Some(&source), &["branch", "--move", "after-advertisement"]),
                _ => String::new(),
            };
            let clone = home.path().join(format!("clone-{index}-{change}"));
            run(
                None,
                &[
                    "clone",
                    "--no-local",
                    "--no-single-branch",
                    "--tags",
                    "--template=",
                    "--",
                    source_text,
                    clone.to_str().unwrap(),
                ],
            );
            let repo = Repo {
                owner: "fixture".into(),
                name: "code".into(),
                default_branch: cached.map(str::to_owned),
                size_kib: 0,
                fork: false,
                archived: false,
                required_submodules: Vec::new(),
            };
            let result = verify_new(&sys, &who, &repo, &clone, &advertised);
            assert_eq!(
                result.is_ok(),
                change == "none",
                "cached={cached:?}, change={change}: {result:?}"
            );
            assert_eq!(
                run(Some(&clone), &["symbolic-ref", "--quiet", "HEAD"]).trim(),
                if change == "branch" {
                    "refs/heads/after-advertisement"
                } else {
                    "refs/heads/trunk"
                }
            );
            assert_eq!(
                run(Some(&clone), &["rev-parse", "HEAD"]).trim()
                    == advertised.head.as_ref().unwrap(),
                change != "commit"
            );
            assert_eq!(
                sys.read_file(&clone.join("code")).unwrap(),
                b"first commit\n"
            );
        }
    }
}
