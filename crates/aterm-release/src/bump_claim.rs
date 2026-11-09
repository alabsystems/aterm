// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Custody of the cutter's one unpublished bump. No other local history is disposable.

use std::path::Path;

use crate::ledger::{GitRunner, git_ok};
use crate::release::{RELEASE_COMMAND, Refusal};

pub(crate) const MAX_ATTEMPTS: usize = 3;

/// The exact retry path used by the cutter and the local-bare fixture. Only a
/// definite lost compare-and-swap retries; uncertain/native refusals retain custody.
pub(crate) fn push(
    git: &dyn GitRunner,
    journal: &Path,
    mut tip: String,
    mut edit: impl FnMut(&str) -> Result<String, Refusal>,
    mut write: impl FnMut(&str, &str) -> Result<crate::ledger::RefWrite, String>,
    mut refresh: impl FnMut() -> Result<String, String>,
) -> Result<String, Refusal> {
    let mut composed = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let bumped = match composed.take() {
            Some(bumped) => bumped,
            None => edit(&tip)?,
        };
        match write(&tip, &bumped) {
            Ok(crate::ledger::RefWrite::Written) => return Ok(bumped),
            Ok(crate::ledger::RefWrite::Conflict) => {}
            Err(why) | Ok(crate::ledger::RefWrite::Transient(why)) => return Err(Refusal::new(
                "the native bump write did not establish a successful claim",
                "inspect the native refusal before rerunning; the journaled local bump is retained",
            )
            .detail([why])),
        }
        tip = refresh().map_err(|why| {
            Refusal::new(
                "cannot refresh main after the bump lost its compare-and-swap",
                "retry once the native origin answers; the journaled local bump is retained",
            )
            .detail([why])
        })?;
        composed = recover(git, journal, &tip, attempt < MAX_ATTEMPTS).map_err(|why| {
            Refusal::new(
                "cannot recover the owned bump after a race",
                "inspect the custody journal and clone",
            )
            .detail([why])
        })?;
        if composed.is_none() && text(git, &["rev-parse", "HEAD"]).as_deref() != Ok(tip.as_str()) {
            return Err(Refusal::new(
                "the local bump no longer matches its custody journal",
                "preserve the local history and inspect the divergence",
            ));
        }
        if attempt == MAX_ATTEMPTS {
            return Err(Refusal::new(
                "main moved under the bump three times (each push lost the race); the owned bump is undone",
                format!("{RELEASE_COMMAND} --bump again"),
            ));
        }
    }
    unreachable!("the nonzero attempt bound always returns a verdict")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Owned {
    commit: String,
    parent: String,
    subject: String,
    paths: Vec<String>,
}

fn text(git: &dyn GitRunner, args: &[&str]) -> Result<String, String> {
    git_ok(git, args)
        .map(|out| out.stdout_utf8().trim().to_owned())
        .map_err(|e| e.to_string())
}

fn paths(git: &dyn GitRunner, before: &str, after: &str) -> Result<Vec<String>, String> {
    let out =
        git_ok(git, &["diff", "--name-only", "-z", before, after]).map_err(|e| e.to_string())?;
    Ok(out
        .stdout_utf8()
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect())
}

fn touched(git: &dyn GitRunner, before: &str, after: &str) -> Result<Vec<String>, String> {
    let commits = text(git, &["rev-list", &format!("{before}..{after}")])?;
    let mut touched = Vec::new();
    for commit in commits.lines() {
        let out = git_ok(
            git,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-m",
                "-z",
                commit,
            ],
        )
        .map_err(|e| e.to_string())?;
        touched.extend(
            out.stdout_utf8()
                .split('\0')
                .filter(|p| !p.is_empty())
                .map(str::to_owned),
        );
    }
    Ok(touched)
}

pub(crate) fn record(
    git: &dyn GitRunner,
    journal: &Path,
    subject: &str,
    mut declared: Vec<String>,
) -> Result<(), String> {
    let commit = text(git, &["rev-parse", "HEAD"])?;
    let parent = text(git, &["rev-parse", "HEAD^"])?;
    let actual = paths(git, &parent, &commit)?;
    declared.sort();
    declared.dedup();
    if actual != declared || text(git, &["show", "-s", "--format=%s", "HEAD"])? != subject {
        return Err("the bump does not match its declared subject and changed set".into());
    }
    let record = Owned {
        commit,
        parent,
        subject: subject.into(),
        paths: actual,
    };
    let bytes = aterm_json::to_string(&record).map_err(|e| e.to_string())?;
    crate::durable::write_atomic_durable(journal, bytes.as_bytes(), 0o600)
        .map_err(|e| e.to_string())
}

/// Recover only an exact journaled bump whose parent is still in main. Returns
/// a composed bump for a disjoint advance; overlapping files require fresh edits.
pub(crate) fn recover(
    git: &dyn GitRunner,
    journal: &Path,
    tip: &str,
    compose: bool,
) -> Result<Option<String>, String> {
    let bytes = match crate::durable::read_text(journal) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let owned: Owned = aterm_json::from_str(&bytes).map_err(|e| e.to_string())?;
    let head = text(git, &["rev-parse", "HEAD"])?;
    if head != owned.commit || head == tip {
        return Ok(None);
    }
    let parents = text(git, &["show", "-s", "--format=%P", &head])?;
    if parents != owned.parent
        || text(git, &["show", "-s", "--format=%s", &head])? != owned.subject
        || paths(git, &owned.parent, &head)? != owned.paths
        || !git_ok(git, &["merge-base", "--is-ancestor", &owned.parent, tip]).is_ok()
        || git_ok(git, &["merge-base", "--is-ancestor", &head, tip]).is_ok()
    {
        return Ok(None);
    }
    if !text(git, &["status", "--porcelain"])?.is_empty() {
        return Err("cannot recover an owned bump in a dirty clone".into());
    }
    let disjoint = touched(git, &owned.parent, tip)?
        .iter()
        .all(|p| !owned.paths.contains(p));
    text(git, &["reset", "--keep", tip])?;
    if compose && disjoint {
        if let Err(why) = text(git, &["cherry-pick", &owned.commit]) {
            text(git, &["cherry-pick", "--abort"])?;
            return Err(format!("the disjoint bump did not compose cleanly: {why}"));
        }
        record(git, journal, &owned.subject, owned.paths)?;
        return text(git, &["rev-parse", "HEAD"]).map(Some);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
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
                assert!(
                    result
                        .unwrap_err()
                        .why
                        .contains("main moved under the bump three times")
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
        println!(
            "bump-baseline after-rejected-write: owned-extra-commits=1 startup-divergence=true"
        );
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
}
