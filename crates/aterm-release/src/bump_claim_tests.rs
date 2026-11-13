// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::GitCli;

struct Fixture {
    _root: aterm_tempfile::TempDir,
    repo: std::path::PathBuf,
    bare: std::path::PathBuf,
    peer: std::path::PathBuf,
    journal: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let repo = root.path().join("clone");
        let bare = root.path().join("origin.git");
        let peer = root.path().join("peer");
        std::fs::create_dir(&repo).unwrap();
        let git = GitCli::new(&repo);
        text(&git, &["init", "-q", "-b", "main"]).unwrap();
        text(&git, &["config", "user.email", "fixture@example.invalid"]).unwrap();
        text(&git, &["config", "user.name", "Andrew Yates"]).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "version = '1.0.0'\n").unwrap();
        std::fs::write(repo.join("Cargo.lock"), "version = '1.0.0'\n").unwrap();
        text(&git, &["add", "."]).unwrap();
        text(&git, &["commit", "-qm", "base"]).unwrap();
        text(
            &git,
            &[
                "clone",
                "-q",
                "--bare",
                repo.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        )
        .unwrap();
        text(
            &git,
            &[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                peer.to_str().unwrap(),
            ],
        )
        .unwrap();
        let peer_git = GitCli::new(&peer);
        text(
            &peer_git,
            &["config", "user.email", "fixture@example.invalid"],
        )
        .unwrap();
        text(&peer_git, &["config", "user.name", "Andrew Yates"]).unwrap();
        text(
            &git,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/fixture/app.git",
            ],
        )
        .unwrap();
        let journal = repo.join(".git/bump-claim.json");
        Self {
            _root: root,
            repo,
            bare,
            peer,
            journal,
        }
    }

    fn git(&self) -> GitCli {
        GitCli::new(&self.repo)
    }

    fn bump(&self) -> String {
        for file in ["Cargo.toml", "Cargo.lock"] {
            let before = std::fs::read_to_string(self.repo.join(file)).unwrap();
            std::fs::write(self.repo.join(file), before.replace("1.0.0", "2.0.0")).unwrap();
        }
        text(
            &self.git(),
            &[
                "commit",
                "-qam",
                "release: 2.0.0 — the version and the lockfile move together",
            ],
        )
        .unwrap();
        record(
            &self.git(),
            &self.journal,
            "release: 2.0.0 — the version and the lockfile move together",
            vec!["Cargo.toml".into(), "Cargo.lock".into()],
        )
        .unwrap();
        text(&self.git(), &["rev-parse", "HEAD"]).unwrap()
    }

    fn advance(&self, overlap: bool, n: usize) {
        let file = if overlap { "Cargo.lock" } else { "unrelated" };
        std::fs::write(
            self.peer.join(file),
            format!("version = '1.0.0'\n# peer {n}\n"),
        )
        .unwrap();
        let git = GitCli::new(&self.peer);
        text(&git, &["add", file]).unwrap();
        text(&git, &["commit", "-qm", "fleet landing"]).unwrap();
        text(&git, &["push", "-q", "origin", "main"]).unwrap();
    }
}

#[test]
fn fixture_origin_bump_races_compose_redo_and_exhaust_cleanly() {
    for (name, overlap, races) in [
        ("compose", false, 1),
        ("redo-lock", true, 1),
        ("exhaust", false, 3),
    ] {
        let f = Fixture::new();
        let remote = crate::github_remote::claim_fixture(&f.repo, &f.bare);
        let tip = remote.refresh_main().unwrap();
        let mut edits = 0;
        let mut writes = 0;
        let result = push(
            &f.git(),
            &f.journal,
            tip,
            |_| {
                edits += 1;
                Ok(f.bump())
            },
            |tip, bumped| {
                writes += 1;
                if writes <= races {
                    f.advance(overlap, writes);
                }
                remote.write_ref("refs/heads/main", Some(tip), bumped)
            },
            || remote.refresh_main(),
        );
        let qualified = result.as_ref().ok().cloned();
        let pushed = remote.refresh_main().unwrap();
        if races == 3 {
            assert!(qualified.is_none());
            let lost = result.unwrap_err();
            assert!(lost.why.contains("main moved under the bump three times"));
            assert!(
                lost.could_not_run && lost.failing.is_none(),
                "a lost race is a resumable stop, never a red or a refusal to wait out"
            );
            assert_eq!(text(&f.git(), &["rev-parse", "HEAD"]).unwrap(), pushed);
        } else {
            assert_eq!(
                qualified,
                Some(pushed.clone()),
                "qualify must receive the pushed bump"
            );
            assert_eq!(edits, if overlap { 2 } else { 1 });
            if overlap {
                assert!(
                    std::fs::read_to_string(f.repo.join("Cargo.lock"))
                        .unwrap()
                        .contains("# peer 1")
                );
            }
        }
        assert!(
            text(&f.git(), &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
        println!(
            "bump-timeline {name}: writes={writes} edits={edits} qualified={} clean=true",
            qualified.is_some()
        );
    }
}

#[test]
fn stranded_owned_bump_recovers_but_foreign_history_does_not() {
    let f = Fixture::new();
    let remote = crate::github_remote::claim_fixture(&f.repo, &f.bare);
    let old = remote.refresh_main().unwrap();
    let stranded = f.bump();
    f.advance(false, 1);
    assert_eq!(
        remote
            .write_ref("refs/heads/main", Some(&old), &stranded)
            .unwrap(),
        crate::github_remote::RefWrite::Conflict
    );
    let tip = remote.refresh_main().unwrap();
    assert!(git_ok(&f.git(), &["merge-base", "--is-ancestor", &stranded, &tip]).is_err());
    println!("bump-baseline after-rejected-write: owned-extra-commits=1 startup-divergence=true");
    recover(&f.git(), &f.journal, &tip, false).unwrap();
    assert_eq!(text(&f.git(), &["rev-parse", "HEAD"]).unwrap(), tip);
    let bumped = f.bump();
    assert_eq!(
        remote
            .write_ref("refs/heads/main", Some(&tip), &bumped)
            .unwrap(),
        crate::github_remote::RefWrite::Written
    );
    // A foreign child must remain for startup's divergence judge to refuse.
    std::fs::write(f.repo.join("Cargo.lock"), "foreign\n").unwrap();
    text(&f.git(), &["commit", "-qam", "foreign"]).unwrap();
    let foreign = text(&f.git(), &["rev-parse", "HEAD"]).unwrap();
    recover(&f.git(), &f.journal, &bumped, false).unwrap();
    assert_eq!(text(&f.git(), &["rev-parse", "HEAD"]).unwrap(), foreign);
    assert!(
        git_ok(
            &f.git(),
            &["merge-base", "--is-ancestor", &foreign, &bumped]
        )
        .is_err(),
        "startup's divergence predicate must still refuse"
    );
    println!(
        "bump-timeline rerun: dropped=1 pushed=true; foreign-negative-control retained=true divergence=true"
    );
}

#[test]
fn native_refusals_retain_the_owned_commit_without_retrying() {
    let f = Fixture::new();
    let tip = text(&f.git(), &["rev-parse", "HEAD"]).unwrap();
    let mut writes = 0;
    let refusal = push(
        &f.git(),
        &f.journal,
        tip.clone(),
        |_| Ok(f.bump()),
        |_, _| {
            writes += 1;
            Err("protected branch".into())
        },
        || panic!("a policy refusal must not refresh or retry"),
    )
    .unwrap_err();
    assert_eq!(writes, 1);
    assert!(refusal.detail.iter().any(|line| line == "protected branch"));
    assert_ne!(text(&f.git(), &["rev-parse", "HEAD"]).unwrap(), tip);
    recover(&f.git(), &f.journal, &tip, false).unwrap();
    assert_eq!(text(&f.git(), &["rev-parse", "HEAD"]).unwrap(), tip);
}

#[test]
fn octopus_bump_recovery_checks_parents_already_in_the_base() {
    for overlap in [false, true] {
        let f = Fixture::new();
        let git = f.git();
        let base = text(&git, &["rev-parse", "HEAD"]).unwrap();
        let tree = text(&git, &["rev-parse", "HEAD^{tree}"]).unwrap();
        if overlap {
            std::fs::write(f.repo.join("Cargo.lock"), "other parent's lock\n").unwrap();
        }
        text(&git, &["add", "."]).unwrap();
        let other = text(&git, &["write-tree"]).unwrap();
        let a = text(&git, &["commit-tree", &other, "-p", &base, "-m", "a"]).unwrap();
        let b = text(&git, &["commit-tree", &tree, "-p", &base, "-m", "b"]).unwrap();
        let released = text(
            &git,
            &["commit-tree", &tree, "-p", &a, "-p", &b, "-m", "released"],
        )
        .unwrap();
        text(&git, &["reset", "--hard", &released]).unwrap();
        f.bump();
        let archive = text(
            &git,
            &[
                "commit-tree",
                &tree,
                "-p",
                &released,
                "-p",
                &a,
                "-p",
                &b,
                "-m",
                "archive",
            ],
        )
        .unwrap();
        // NEGATIVE CONTROL: the first-parent and combined diff both miss a
        // lock chosen from a parent. The release floor already reaches a and b.
        assert!(paths(&git, &released, &archive).unwrap().is_empty());
        assert!(
            text(
                &git,
                &[
                    "show",
                    "--format=",
                    "--name-only",
                    "--diff-merges=combined",
                    &archive
                ]
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            touched(&git, &released, &archive)
                .unwrap()
                .contains(&"Cargo.lock".into()),
            overlap
        );
        assert_eq!(
            recover(&git, &f.journal, &archive, true).unwrap().is_some(),
            !overlap
        );
    }
}

#[test]
fn a_lockfile_touch_then_revert_still_requires_fresh_edits() {
    let f = Fixture::new();
    let remote = crate::github_remote::claim_fixture(&f.repo, &f.bare);
    let tip = remote.refresh_main().unwrap();
    f.bump();
    f.advance(true, 1);
    let peer = GitCli::new(&f.peer);
    text(&peer, &["revert", "--no-edit", "HEAD"]).unwrap();
    text(&peer, &["push", "-q", "origin", "main"]).unwrap();
    let moved = remote.refresh_main().unwrap();
    assert!(paths(&f.git(), &tip, &moved).unwrap().is_empty());
    assert!(
        recover(&f.git(), &f.journal, &moved, true)
            .unwrap()
            .is_none()
    );
    assert_eq!(text(&f.git(), &["rev-parse", "HEAD"]).unwrap(), moved);
}

#[test]
fn every_custody_field_must_match_before_history_can_be_dropped() {
    let f = Fixture::new();
    let tip = text(&f.git(), &["rev-parse", "HEAD"]).unwrap();
    let bumped = f.bump();
    let original = crate::durable::read_text(&f.journal).unwrap();
    for field in ["commit", "parent", "subject", "paths"] {
        let mut owned: Owned = aterm_json::from_str(&original).unwrap();
        match field {
            "commit" => owned.commit = tip.clone(),
            "parent" => owned.parent = bumped.clone(),
            "subject" => owned.subject = "foreign".into(),
            "paths" => owned.paths = vec!["Cargo.toml".into()],
            _ => unreachable!(),
        }
        crate::durable::write_atomic_durable(
            &f.journal,
            aterm_json::to_string(&owned).unwrap().as_bytes(),
            0o600,
        )
        .unwrap();
        assert!(
            recover(&f.git(), &f.journal, &tip, false)
                .unwrap()
                .is_none(),
            "{field}"
        );
        assert_eq!(
            text(&f.git(), &["rev-parse", "HEAD"]).unwrap(),
            bumped,
            "{field}"
        );
    }
    assert!(record(&f.git(), &f.journal, "foreign", vec!["Cargo.toml".into()]).is_err());
}

#[test]
fn a_declared_path_the_bump_left_as_it_was_is_no_mismatch_and_an_undeclared_change_is() {
    let subject = "release: 2.0.0 — the version and the lockfile move together";
    let f = Fixture::new();
    let bumped = f.bump();
    record(
        &f.git(),
        &f.journal,
        subject,
        vec![
            "Cargo.toml".into(),
            "Cargo.lock".into(),
            aterm_phase::CliHarness::ClaudeCode.version_file().into(),
        ],
    )
    .unwrap();
    let owned: Owned = aterm_json::from_str(&std::fs::read_to_string(&f.journal).unwrap()).unwrap();
    assert_eq!(owned.commit, bumped);
    assert_eq!(owned.paths, ["Cargo.lock", "Cargo.toml"]);
    assert!(
        record(&f.git(), &f.journal, subject, vec!["Cargo.toml".into()]).is_err(),
        "Cargo.lock changed undeclared"
    );
}
