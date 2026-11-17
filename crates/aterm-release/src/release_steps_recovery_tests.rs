// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The launcher's custody recovery must run before candidate movement, without asking an
//! engine or changing a dry run. Every remote here is a real private local Git repository.

use super::*;

fn git(repo: &Path, args: &[&str]) -> String {
    crate::ledger::git_ok(&GitCli::new(repo), args)
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned()
}

struct Fixture {
    _root: aterm_tempfile::TempDir,
    repo: PathBuf,
    bare: PathBuf,
    peer: PathBuf,
    real: Real,
    base: String,
    bump: String,
    journal: PathBuf,
    custody: Vec<u8>,
}

impl Fixture {
    fn new(dry: bool) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let repo = root.path().join("source");
        crate::ledger::testing::init(
            &repo,
            &[
                ("user.name", "Andrew Yates"),
                ("user.email", "fixture@example.invalid"),
            ],
        );
        std::fs::write(repo.join("version"), "0.5.0\n").unwrap();
        git(&repo, &["add", "version"]);
        git(&repo, &["commit", "-qm", "published base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let bare = root.path().join("origin.git");
        git(
            &repo,
            &[
                "clone",
                "-q",
                "--bare",
                repo.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        let peer = root.path().join("peer");
        git(
            &repo,
            &[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                peer.to_str().unwrap(),
            ],
        );
        crate::ledger::testing::config(
            &peer,
            &[
                ("user.name", "Andrew Yates"),
                ("user.email", "fixture@example.invalid"),
            ],
        );
        git(
            &repo,
            &[
                "config",
                "remote.origin.url",
                "https://github.com/fixture/app.git",
            ],
        );
        git(
            &repo,
            &["config", "remote.fixture.url", bare.to_str().unwrap()],
        );
        git(&repo, &["update-ref", "refs/remotes/origin/main", &base]);
        std::fs::write(repo.join("version"), "0.6.0\n").unwrap();
        let subject = "release: 0.6.0 — the version and the lockfile move together";
        git(&repo, &["commit", "-qam", subject]);
        let bump = git(&repo, &["rev-parse", "HEAD"]);
        // Scratch logs avoid a real run's host-state cleanup; scratch_logs controls the
        // actual production dry-run branch independently of the log directory's placement.
        let mut real =
            Real::new(&repo, Some(&root.path().join("absent-engine")), true, false).unwrap();
        real.scratch_logs = dry;
        real.source_origin = |repo| {
            let bare = git(repo, &["config", "--get", "remote.fixture.url"]);
            Ok(crate::github_remote::claim_fixture(repo, Path::new(&bare)))
        };
        let journal = real.release.join("bump-claim.json");
        crate::bump_claim::record(
            &GitCli::new(&repo),
            &journal,
            subject,
            vec!["version".into()],
        )
        .unwrap();
        let custody = std::fs::read(&journal).unwrap();
        Self {
            _root: root,
            repo,
            bare,
            peer,
            real,
            base,
            bump,
            journal,
            custody,
        }
    }

    fn advance(&self) -> String {
        std::fs::write(self.peer.join("landed"), "independent main change\n").unwrap();
        git(&self.peer, &["add", "landed"]);
        git(&self.peer, &["commit", "-qm", "main advances"]);
        git(&self.peer, &["push", "-q", "origin", "main"]);
        git(&self.peer, &["rev-parse", "HEAD"])
    }

    fn publish_and_advance(&self) -> String {
        git(
            &self.repo,
            &[
                "push",
                "-q",
                self.bare.to_str().unwrap(),
                "HEAD:refs/heads/main",
            ],
        );
        git(&self.peer, &["fetch", "-q", "origin", "main"]);
        git(&self.peer, &["reset", "-q", "--hard", "FETCH_HEAD"]);
        self.advance()
    }
}

#[test]
fn an_owned_bump_is_recovered_before_the_frontend_moves_to_its_candidate() {
    let mut f = Fixture::new(false);
    let tip = f.advance();
    let release_journal = f.real.release.join("release.toml");
    std::fs::write(&release_journal, "an unfinished after-cut remains here\n").unwrap();
    // Negative control: candidate movement alone is the old frontend's refusal.
    assert!(f.real.main_to(&f.base).is_err());
    assert_eq!(git(&f.repo, &["rev-parse", "origin/main"]), f.base);

    f.real.prepare_clone().unwrap();

    assert_eq!(
        git(&f.repo, &["rev-parse", "HEAD"]),
        f.base,
        "only the owned bump was undone"
    );
    assert_eq!(
        git(&f.repo, &["rev-parse", "origin/main"]),
        tip,
        "authority is freshly observed"
    );
    assert_eq!(f.real.prefetched_origin.as_ref().unwrap().main, tip);
    f.real.main_to(&tip).unwrap();
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), tip);
    assert_eq!(std::fs::read(&f.journal).unwrap(), f.custody);
    assert_eq!(
        std::fs::read_to_string(&release_journal).unwrap(),
        "an unfinished after-cut remains here\n"
    );
    assert!(f.real.origin_main.is_none(), "recovery asks no engine");
    assert_eq!(
        git(&f.repo, &["cat-file", "-t", &f.bump]),
        "commit",
        "the retained custody commit remains available"
    );
    assert_eq!(
        git(&f.bare, &["rev-parse", "refs/heads/main"]),
        tip,
        "recovery wrote no remote ref"
    );
}

#[test]
fn a_bump_already_on_origin_is_retained_without_undoing_the_published_claim() {
    let mut f = Fixture::new(false);
    git(
        &f.repo,
        &[
            "push",
            "-q",
            f.bare.to_str().unwrap(),
            "HEAD:refs/heads/main",
        ],
    );
    f.real.prepare_clone().unwrap();
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), f.bump);
    assert_eq!(git(&f.repo, &["rev-parse", "origin/main"]), f.bump);
    assert_eq!(std::fs::read(&f.journal).unwrap(), f.custody);
}

#[test]
fn original_history_retains_a_published_bump_hidden_by_local_rewrites() {
    for rewrite in ["replacement", "graft"] {
        let mut f = Fixture::new(false);
        let tip = f.publish_and_advance();
        crate::github_remote::claim_fixture(&f.repo, &f.bare)
            .refresh_main()
            .unwrap();
        if rewrite == "replacement" {
            let tree = git(&f.repo, &["rev-parse", &format!("{tip}^{{tree}}")]);
            let substitute = git(
                &f.repo,
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    &f.base,
                    "-m",
                    "hide the published bump",
                ],
            );
            git(&f.repo, &["replace", &tip, &substitute]);
        } else {
            std::fs::write(
                f.repo.join(".git/info/grafts"),
                format!("{tip} {}\n", f.base),
            )
            .unwrap();
        }
        let runner = GitCli::new(&f.repo);
        assert_eq!(
            runner
                .git(&["merge-base", "--is-ancestor", &f.bump, &tip])
                .unwrap()
                .status,
            1,
            "CONTROL: ordinary local history hides the published bump"
        );
        assert_eq!(
            runner
                .git_original(&["merge-base", "--is-ancestor", &f.bump, &tip])
                .unwrap()
                .status,
            0,
            "original history still contains the bump"
        );
        f.real.prepare_clone().unwrap();
        assert_eq!(
            git(&f.repo, &["rev-parse", "HEAD"]),
            f.bump,
            "{rewrite}: published history must not be undone"
        );
        assert_eq!(std::fs::read(&f.journal).unwrap(), f.custody);
        assert_eq!(git(&f.bare, &["rev-parse", "refs/heads/main"]), tip);
    }
}

#[test]
fn original_history_recovers_an_unpublished_bump_despite_a_local_shallow_cutoff() {
    let mut f = Fixture::new(false);
    let tip = f.advance();
    crate::github_remote::claim_fixture(&f.repo, &f.bare)
        .refresh_main()
        .unwrap();
    std::fs::write(f.repo.join(".git/shallow"), format!("{tip}\n")).unwrap();
    let runner = GitCli::new(&f.repo);
    assert_eq!(
        runner
            .git(&["merge-base", "--is-ancestor", &f.base, &tip])
            .unwrap()
            .status,
        1,
        "CONTROL: the local cutoff hides the parent origin really holds"
    );
    assert_eq!(
        runner
            .git_original(&["merge-base", "--is-ancestor", &f.base, &tip])
            .unwrap()
            .status,
        0
    );
    f.real.prepare_clone().unwrap();
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), f.base);
    assert_eq!(std::fs::read(&f.journal).unwrap(), f.custody);
    assert_eq!(git(&f.bare, &["rev-parse", "refs/heads/main"]), tip);
}

#[test]
fn unread_original_membership_never_means_an_unpublished_bump() {
    struct Unread<'a> {
        actual: GitCli,
        bump: &'a str,
        status: Option<i32>,
    }
    impl GitRunner for Unread<'_> {
        fn git(&self, args: &[&str]) -> crate::ledger::Result<crate::ledger::RunOut> {
            self.actual.git(args)
        }
        fn git_original(&self, args: &[&str]) -> crate::ledger::Result<crate::ledger::RunOut> {
            if args.get(2).copied() == Some(self.bump) {
                return match self.status {
                    Some(status) => Ok(crate::ledger::RunOut {
                        status,
                        stdout: Vec::new(),
                        stderr: b"injected original-history refusal".to_vec(),
                    }),
                    None => Err(crate::ledger::Error::new(
                        "injected original-history I/O failure",
                    )),
                };
            }
            self.actual.git_original(args)
        }
    }
    let f = Fixture::new(false);
    let tip = f.publish_and_advance();
    crate::github_remote::claim_fixture(&f.repo, &f.bare)
        .refresh_main()
        .unwrap();
    for status in [None, Some(-1), Some(2), Some(128)] {
        let unread = Unread {
            actual: GitCli::new(&f.repo),
            bump: &f.bump,
            status,
        };
        let why = crate::bump_claim::recover_for_selection(&unread, &f.journal, &tip).unwrap_err();
        assert!(why.contains("original"), "{status:?}: {why}");
        assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), f.bump, "{status:?}");
        assert_eq!(std::fs::read(&f.journal).unwrap(), f.custody);
        assert_eq!(git(&f.bare, &["rev-parse", "refs/heads/main"]), tip);
    }
}

#[test]
fn dry_preparation_foreign_history_and_dirty_work_never_recover_a_bump() {
    for mode in [
        "dry",
        "foreign-child",
        "foreign-branch",
        "dirty",
        "missing-custody",
        "wrong-subject",
        "wrong-paths",
        "malformed-custody",
    ] {
        let mut f = Fixture::new(mode == "dry");
        let tip = f.advance();
        if mode == "foreign-child" {
            std::fs::write(f.repo.join("foreign"), "someone else's work\n").unwrap();
            git(&f.repo, &["add", "foreign"]);
            git(&f.repo, &["commit", "-qm", "foreign child"]);
        } else if mode == "foreign-branch" {
            git(&f.repo, &["checkout", "-qb", "foreign"]);
        } else if mode == "dirty" {
            std::fs::write(f.repo.join("version"), "uncommitted user version\n").unwrap();
        } else if mode == "missing-custody" {
            std::fs::remove_file(&f.journal).unwrap();
        } else if matches!(mode, "wrong-subject" | "wrong-paths") {
            let mut record: aterm_json::Value = aterm_json::from_slice(&f.custody).unwrap();
            let object = record.as_object_mut().unwrap();
            if mode == "wrong-subject" {
                object.insert("subject".into(), "a forged claim".into());
            } else {
                object.insert("paths".into(), aterm_json::json!(["foreign"]));
            }
            std::fs::write(&f.journal, aterm_json::to_vec(&record).unwrap()).unwrap();
        } else if mode == "malformed-custody" {
            std::fs::write(&f.journal, "{ incomplete").unwrap();
        }
        let head = git(&f.repo, &["rev-parse", "HEAD"]);
        let version = std::fs::read(f.repo.join("version")).unwrap();
        let journal = std::fs::read(&f.journal).ok();
        if !matches!(mode, "wrong-subject" | "wrong-paths") {
            f.real.source_origin = |_| panic!("this state must not read source origin");
        }
        let result = f.real.prepare_clone();
        assert_eq!(
            result.is_err(),
            matches!(mode, "dirty" | "malformed-custody"),
            "{mode}: {result:?}"
        );
        assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), head, "{mode}");
        assert_eq!(
            std::fs::read(f.repo.join("version")).unwrap(),
            version,
            "{mode}"
        );
        assert_eq!(std::fs::read(&f.journal).ok(), journal, "{mode}");
        assert_eq!(
            git(&f.bare, &["rev-parse", "refs/heads/main"]),
            tip,
            "{mode}"
        );
        if mode != "dry" && mode != "dirty" {
            assert!(
                f.real.main_to(&f.base).is_err(),
                "{mode}: foreign history must still refuse"
            );
        }
    }
}
