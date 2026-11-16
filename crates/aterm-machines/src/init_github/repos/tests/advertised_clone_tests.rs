// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The real git every step here runs, as `user` in `at`: no system configuration, `global` for
/// the person's own, and no hook, filesystem monitor or maintenance.
fn isolated(sys: &Live, user: &User, at: Option<&Path>, global: &str) -> Cmd {
    git(sys, user, at)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global)
        .args(crate::checkout::NO_SIDE_RUNS)
}

/// A repository the inventory lists as `fixture/code` on `main`.
fn listed() -> Repo {
    Repo {
        owner: "fixture".into(),
        name: "code".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: Vec::new(),
    }
}

/// A PUSH DURING A CLONE KEEPS THE ADVERTISED CLONE: GitHub is read, then a push lands before the
/// clone's transfer (`main` moves on, a branch and a tag arrive, an advertised branch is
/// deleted). The clone ([`clone_advertised`]) fetches exactly the advertised objects into exactly
/// the advertised names, so it holds what was advertised and nothing the push brought, sits on
/// `main` at the advertised commit tracking origin's, and [`verify_new`] passes it. NEGATIVE
/// CONTROL: the `git clone` a clone ran before takes the references current when its own transfer
/// starts, and [`verify_new`] discards it, as every clone a push overtook was discarded.
#[test]
fn a_push_during_a_clone_keeps_the_advertised_clone() {
    crate::items::github::need_native_git();
    let sys = Live::new();
    let home = Scratch::new(&sys, "github-advertised-clone").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = home.path().to_string_lossy().into_owned();
    let run = |at: Option<&Path>, args: &[&str]| {
        text(
            &sys,
            &isolated(&sys, &who, at, "/dev/null")
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
    let source = home.path().join("source");
    let source_text = source.to_str().unwrap();
    run(
        None,
        &[
            "init",
            "--quiet",
            "--template=",
            "--initial-branch=main",
            source_text,
        ],
    );
    sys.write_file(&source.join("code"), b"advertised\n", 0o600)
        .unwrap();
    run(Some(&source), &["add", "--", "code"]);
    run(Some(&source), &["commit", "--quiet", "-m", "advertised"]);
    run(Some(&source), &["branch", "topic"]);
    run(Some(&source), &["tag", "--annotate", "-m", "v1", "v1"]);
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
    let head = advertised.head.clone().unwrap();
    let tag = advertised.refs["refs/tags/v1"].clone();
    // The push, after GitHub was read and before the clone's transfer.
    sys.write_file(&source.join("code"), b"pushed\n", 0o600)
        .unwrap();
    run(
        Some(&source),
        &["commit", "--quiet", "--all", "-m", "pushed"],
    );
    let pushed = run(Some(&source), &["rev-parse", "HEAD"]).trim().to_owned();
    run(Some(&source), &["branch", "pushed"]);
    run(Some(&source), &["tag", "pushed-tag"]);
    run(Some(&source), &["branch", "--delete", "--force", "topic"]);
    let repo = listed();

    // NEGATIVE CONTROL: `git clone`, as the clone ran before, after the same push.
    let cloned = home.path().join("cloned");
    run(
        None,
        &[
            "clone",
            "--quiet",
            "--no-local",
            "--no-single-branch",
            "--tags",
            "--origin",
            "origin",
            "--template=",
            "--",
            source_text,
            cloned.to_str().unwrap(),
        ],
    );
    let discarded = verify_new(&sys, &who, &repo, &cloned, &advertised);
    assert!(
        discarded
            .as_ref()
            .is_err_and(|why| why.contains("does not match")),
        "a clone the push overtook is discarded: {discarded:?}"
    );

    let staging = Scratch::new_in(&sys, home.path(), "github-clone").unwrap();
    let mut progress = Progress::new("code", home.path().join("code"));
    clone_advertised(
        &sys,
        &isolated(&sys, &who, None, "/dev/null"),
        source_text,
        &advertised,
        clone_branch(&repo, &advertised),
        staging.path(),
        &mut progress,
    )
    .unwrap();
    let clone = staged(staging.path());
    verify_new(&sys, &who, &repo, &clone, &advertised).unwrap();
    let at = |args: &[&str]| run(Some(&clone), args).trim().to_owned();
    assert_eq!(at(&["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(at(&["rev-parse", "HEAD"]), head);
    assert_eq!(
        at(&["for-each-ref", "--format=%(refname) %(objectname)"]),
        format!(
            "refs/heads/main {head}\nrefs/remotes/origin/HEAD {head}\nrefs/remotes/origin/main \
             {head}\nrefs/remotes/origin/topic {head}\nrefs/tags/v1 {tag}"
        ),
        "exactly the advertised names, at the advertised objects"
    );
    let lacks = sys
        .run_complete_stdout(
            &isolated(&sys, &who, Some(&clone), "/dev/null").args(["cat-file", "-e", &pushed]),
        )
        .unwrap();
    assert!(!lacks.success(), "the pushed commit was fetched");
    assert_eq!(
        at(&["symbolic-ref", "refs/remotes/origin/HEAD"]),
        "refs/remotes/origin/main"
    );
    assert_eq!(at(&["config", "--get", "branch.main.remote"]), "origin");
    assert_eq!(
        at(&["config", "--get", "branch.main.merge"]),
        "refs/heads/main"
    );
    assert_eq!(at(&["config", "--get", "remote.origin.url"]), source_text);
    assert_eq!(
        at(&["config", "--get-all", "remote.origin.fetch"]),
        ORIGIN_FETCH
    );
    assert_eq!(sys.read_file(&clone.join("code")).unwrap(), b"advertised\n");
    assert!(
        sys.stat(&clone.join(".git/FETCH_HEAD")).is_err(),
        "the fetch wrote FETCH_HEAD"
    );
}

/// AN EMPTY REMOTE STILL CLONES TO ITS DEFAULT BRANCH: GitHub advertises no commit, so nothing is
/// fetched, and the new repository is born on the default branch ([`clone_branch`]: the one an
/// unborn HEAD advertises, else the inventory's), tracking origin's, as `git clone` leaves an
/// empty one, whatever the person's own `init.defaultBranch` names. NEGATIVE CONTROL: a
/// repository `git init` made under that setting is on its branch, and [`verify_new`] refuses
/// it.
#[test]
fn an_empty_remote_still_clones_to_its_default_branch() {
    crate::items::github::need_native_git();
    let sys = Live::new();
    let home = Scratch::new(&sys, "github-empty-clone").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = home.path().to_string_lossy().into_owned();
    let global = home.path().join("gitconfig");
    sys.write_file(&global, b"[init]\n\tdefaultBranch = other\n", 0o600)
        .unwrap();
    let global = global.to_str().unwrap();
    let run = |at: Option<&Path>, args: &[&str]| {
        text(
            &sys,
            &isolated(&sys, &who, at, global).args(args.iter().copied()),
            "isolated local Git fixture failed",
        )
        .unwrap()
    };
    let source = home.path().join("source.git");
    let source_text = source.to_str().unwrap();
    run(
        None,
        &[
            "init",
            "--quiet",
            "--bare",
            "--template=",
            "--initial-branch=trunk",
            source_text,
        ],
    );
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
    assert!(advertised.head.is_none() && advertised.refs.is_empty());
    let repo = listed();
    let branch = clone_branch(&repo, &advertised).unwrap().to_owned();
    assert_ne!(branch, "other");

    let staging = Scratch::new_in(&sys, home.path(), "github-clone").unwrap();
    let mut progress = Progress::new("code", home.path().join("code"));
    clone_advertised(
        &sys,
        &isolated(&sys, &who, None, global),
        source_text,
        &advertised,
        Some(branch.as_str()),
        staging.path(),
        &mut progress,
    )
    .unwrap();
    let clone = staged(staging.path());
    verify_new(&sys, &who, &repo, &clone, &advertised).unwrap();
    let at = |args: &[&str]| run(Some(&clone), args).trim().to_owned();
    assert_eq!(
        at(&["symbolic-ref", "HEAD"]),
        format!("refs/heads/{branch}")
    );
    let born = sys
        .run_complete_stdout(&isolated(&sys, &who, Some(&clone), global).args([
            "rev-parse",
            "--verify",
            "--quiet",
            "HEAD^{commit}",
        ]))
        .unwrap();
    assert!(!born.success(), "an empty remote's clone has a commit");
    assert_eq!(at(&["for-each-ref", "--format=%(refname)"]), "");
    assert_eq!(
        at(&["config", "--get", &format!("branch.{branch}.remote")]),
        "origin"
    );
    assert_eq!(
        at(&["config", "--get", &format!("branch.{branch}.merge")]),
        format!("refs/heads/{branch}")
    );
    assert_eq!(at(&["config", "--get", "remote.origin.url"]), source_text);
    assert!(
        sys.stat(&staging.path().join(ADVERTISED)).is_err()
            && sys.stat(&clone.join(".git/FETCH_HEAD")).is_err(),
        "an empty remote's clone fetched"
    );

    // NEGATIVE CONTROL: what `git init` makes under the person's setting.
    let plain = home.path().join("plain");
    run(
        None,
        &["init", "--quiet", "--template=", plain.to_str().unwrap()],
    );
    assert_eq!(
        run(Some(&plain), &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/other"
    );
    run(Some(&plain), &["config", "remote.origin.url", source_text]);
    let refused = verify_new(&sys, &who, &repo, &plain, &advertised);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("default branch")),
        "{refused:?}"
    );
}

/// AN ADVERTISED OBJECT GITHUB NO LONGER HAS: a push after GitHub was read can take an advertised
/// reference's object with it (a force-push or a deletion, and the object collected), and the
/// clone's fetch then fails: as the network's failure, naming each advertised reference whose
/// object Git said it could not have, and nothing is published. NEGATIVE CONTROL: a fetch that
/// failed naming no advertised object fails with Git's own reason and names none.
#[test]
fn an_advertised_object_github_no_longer_has_is_a_network_failure_naming_it() {
    for names_it in [true, false] {
        let (sys, who) = fixture();
        let (repo, _, _) = script_clone(&sys, &who, false);
        let said = if names_it {
            format!("fatal: remote error: upload-pack: not our ref {B}\n")
        } else {
            "fatal: the remote end hung up unexpectedly\n".to_owned()
        };
        sys.simulate(move |_, command| {
            fetches_advertised(&command.display()).then(|| Ran::exit(128, &said))
        });
        let cloned = clone_all(&sys, &who, &[repo], &[], &BTreeSet::new(), None, None);
        assert!(cloned.network_failed, "{names_it}: {:?}", cloned.failed);
        let [why] = cloned.failed.as_slice() else {
            panic!("{names_it}: {:?}", cloned.failed);
        };
        assert_eq!(
            why.contains("no longer has")
                && why.contains(&format!("refs/heads/topic ({B})"))
                && why.contains(&format!("refs/tags/v1 ({B})")),
            names_it,
            "{why}"
        );
        assert!(!why.contains(A), "{why}");
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.starts_with("rename-noreplace ")),
            "{names_it}"
        );
        assert!(sys.stat(&Path::new(&who.home).join("new")).is_err());
    }
}
