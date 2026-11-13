// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bare-repo race proofs for the build-number ledger claim.

use crate::{changelog, ledger};

use aterm_uds::spawnfd::command;
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use crate::publisher_origin::Git;
use crate::push_hook::HookRunner;
use ledger::testing::Git as LocalGit;
use ledger::{ClaimPlan, GitRunner, LEDGER_FLOOR, RunOut};

const NOW: u64 = 1_790_000_000;

const SEED_LEDGER: &str = "# aterm release ledger — append-only; one line per claimed build number. Never edit or reuse.\n1783354739 0.25.0\n";

const SEED_CHANGELOG: &str = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- **Something real** — an entry.\n\n## [0.25.0] - 2026-07-06\n\n- old notes.\n";

const DATE: &str = "2026-09-23";

struct Fixture {
    root: aterm_tempfile::TempDir,
    origin: PathBuf,
    rival: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        static BUILT: crate::template::Template<()> = crate::template::Template::new();
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-ledger-race-{name}-"))
            .suffix(".noindex")
            .tempdir()
            .unwrap();
        let root = dir.path();
        BUILT.lay(root, build);
        Fixture {
            origin: root.join("origin.git"),
            rival: root.join("rival"),
            work: root.join("work"),
            root: dir,
        }
    }
}

fn build(root: &Path) {
    let origin = root.join("origin.git");
    run(
        root,
        "git",
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    ledger::testing::config(&origin, &[]);
    let rival = root.join("rival");
    run(
        root,
        "git",
        &["clone", "-q", origin.to_str().unwrap(), "rival"],
    );
    config_identity(&rival);
    run(&rival, "git", &["symbolic-ref", "HEAD", "refs/heads/main"]);
    fs::write(rival.join("RELEASES.ledger"), SEED_LEDGER).unwrap();
    fs::write(rival.join("CHANGELOG.md"), SEED_CHANGELOG).unwrap();
    run(&rival, "git", &["add", "-A"]);
    run(&rival, "git", &["commit", "-q", "-m", "seed"]);
    run(&rival, "git", &["push", "-q", "-u", "origin", "main"]);
    let work = root.join("work");
    run(
        root,
        "git",
        &["clone", "-q", origin.to_str().unwrap(), "work"],
    );
    config_identity(&work);
}

fn run(cwd: &Path, prog: &str, args: &[&str]) -> String {
    let mut command = if prog == "git" {
        ledger::testing::git()
    } else {
        command(prog)
    };
    let out = command
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn {prog} {args:?}: {e}"));
    assert!(
        out.status.success(),
        "{prog} {args:?} in {} failed:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn config_identity(repo: &Path) {
    ledger::testing::config(
        repo,
        &[
            ("user.email", "ship-test@aterm.invalid"),
            ("user.name", "aterm ship test"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
        ],
    );
}

fn origin_file(fix: &Fixture, path: &str) -> String {
    run(&fix.origin, "git", &["show", &format!("main:{path}")])
}

fn origin_subjects(fix: &Fixture) -> String {
    run(&fix.origin, "git", &["log", "--format=%s", "main"])
}

fn rival_append(fix: &Fixture, line: &str) {
    run(&fix.rival, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.rival, "git", &["reset", "-q", "--hard", "origin/main"]);
    let p = fix.rival.join("RELEASES.ledger");
    let mut t = fs::read_to_string(&p).unwrap();
    if !t.ends_with('\n') {
        t.push('\n');
    }
    t.push_str(line);
    t.push('\n');
    fs::write(&p, t).unwrap();
    run(
        &fix.rival,
        "git",
        &["commit", "-q", "-am", &format!("rival: {line}")],
    );
    run(&fix.rival, "git", &["push", "-q", "origin", "main"]);
}

fn rival_land_section(fix: &Fixture, line: &str, version: &str) {
    run(&fix.rival, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.rival, "git", &["reset", "-q", "--hard", "origin/main"]);
    let lp = fix.rival.join("RELEASES.ledger");
    let mut t = fs::read_to_string(&lp).unwrap();
    t.push_str(line);
    t.push('\n');
    fs::write(&lp, t).unwrap();
    let cp = fix.rival.join("CHANGELOG.md");
    let mut c = fs::read_to_string(&cp).unwrap();
    c.push_str(&format!(
        "\n## [{version}] - 2026-01-01\n\n- cut elsewhere.\n"
    ));
    fs::write(&cp, c).unwrap();
    run(
        &fix.rival,
        "git",
        &["commit", "-q", "-am", "rival: full claim"],
    );
    run(&fix.rival, "git", &["push", "-q", "origin", "main"]);
}

fn rival_peer_push(fix: &Fixture) {
    run(&fix.rival, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.rival, "git", &["reset", "-q", "--hard", "origin/main"]);
    fs::write(fix.rival.join("peer.rs"), "fn peer() {}\n").unwrap();
    let cp = fix.rival.join("CHANGELOG.md");
    let c = fs::read_to_string(&cp).unwrap().replace(
        "### Added\n",
        "### Added\n- **A peer's entry** — not in the published commit.\n",
    );
    fs::write(&cp, c).unwrap();
    run(&fix.rival, "git", &["add", "-A"]);
    run(
        &fix.rival,
        "git",
        &["commit", "-q", "-m", "feat: a peer's code"],
    );
    run(&fix.rival, "git", &["push", "-q", "origin", "main"]);
}

#[cfg(unix)]
fn do_claim<T: GitRunner + ledger::Origin>(
    git: &T,
    work: &Path,
    version: &str,
    allow_existing_section: bool,
) -> (ledger::Result<ledger::Claim>, u32) {
    do_claim_with_origin(git, git, work, version, allow_existing_section)
}

#[cfg(unix)]
fn do_claim_with_origin(
    git: &dyn GitRunner,
    origin: &dyn ledger::Origin,
    work: &Path,
    version: &str,
    allow_existing_section: bool,
) -> (ledger::Result<ledger::Claim>, u32) {
    let source = run(work, "git", &["rev-parse", "HEAD"]).trim().to_string();
    let plan = ClaimPlan {
        version,
        now: NOW,
        allow_existing_section,
        max_attempts: 5,
        source: &source,
        harness_base: None,
    };
    let git = Commits {
        inner: git,
        made: Cell::new(0),
    };
    let res = ledger::reserve(&git, origin, &plan, DATE)
        .and_then(|reserved| ledger::claim(&git, origin, work, &plan, &reserved));
    (res, git.made.get())
}

/// A runner that counts the commits the claim makes: the reservation's, then one per landing.
struct Commits<'a> {
    inner: &'a dyn GitRunner,
    made: Cell<u32>,
}

impl GitRunner for Commits<'_> {
    fn git(&self, args: &[&str]) -> ledger::Result<RunOut> {
        if args.contains(&"commit-tree") {
            self.made.set(self.made.get() + 1);
        }
        self.inner.git(args)
    }

    fn git_read(&self, args: &[&str]) -> ledger::Result<RunOut> {
        self.inner.git_read(args)
    }

    fn git_input(&self, args: &[&str], input: &[u8]) -> ledger::Result<RunOut> {
        self.inner.git_input(args, input)
    }
}

fn assert_clean_at(fix: &Fixture, source: &str) {
    assert!(
        run(&fix.work, "git", &["status", "--porcelain"])
            .trim()
            .is_empty(),
        "working tree not clean after abort"
    );
    let head = run(&fix.work, "git", &["rev-parse", "HEAD"]);
    assert_eq!(
        head.trim(),
        source,
        "HEAD not reset to the published commit"
    );
    assert!(
        !origin_subjects(fix).contains("release: v0.26.0"),
        "an aborted claim must never land a release commit on origin"
    );
}

fn head_of(repo: &Path) -> String {
    run(repo, "git", &["rev-parse", "HEAD"]).trim().to_string()
}

fn parents(repo: &Path, commit: &str) -> Vec<String> {
    run(repo, "git", &["rev-list", "--parents", "-n", "1", commit])
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn moved(repo: &Path, from: &str, to: &str) -> String {
    run(repo, "git", &["diff", "--name-only", from, to])
        .trim()
        .replace('\n', " ")
}

#[test]
fn parse_takes_last_record_and_names_malformed_lines() {
    let good = "# comment\n1783354739 0.25.0\n1783918101 0.26.0\n";
    let t = ledger::tail(good).unwrap();
    assert_eq!((t.build, t.version.as_str()), (1_783_918_101, "0.26.0"));

    let mixed = "# comment\n1784869524 0.61\n1790000000 0.2.0\n";
    let t = ledger::tail(mixed).unwrap();
    assert_eq!((t.build, t.version.as_str()), (1_790_000_000, "0.2.0"));
    let records = ledger::parse(mixed).unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| (record.build, record.version.as_str()))
            .collect::<Vec<_>>(),
        [(1_784_869_524, "0.61"), (1_790_000_000, "0.2.0")],
        "retired history stays readable; it is just never a cut's source"
    );

    let cases: [(&str, usize); 4] = [
        ("# c\n1783354739 0.25.0\n\n1783918101 0.26.0\n", 3),
        ("1783354739\n", 1),
        ("1783354739 0.25.0 extra\n", 1),
        ("abc 0.25.0\n", 1),
    ];
    for (text, line) in cases {
        let err = ledger::tail(text).unwrap_err().to_string();
        assert!(err.contains(&format!("line {line}")), "{text:?} → {err}");
    }

    assert!(ledger::tail("# only comments\n").is_err());
}

#[test]
fn next_build_is_monotonic_and_floored() {
    assert_eq!(ledger::next_build(LEDGER_FLOOR, NOW).unwrap(), NOW);
    assert_eq!(ledger::next_build(NOW + 5, NOW).unwrap(), NOW + 6);
    assert_eq!(
        ledger::next_build(LEDGER_FLOOR, 0).unwrap(),
        LEDGER_FLOOR + 1
    );
    assert!(ledger::next_build(5, 10).is_err());
}

#[test]
fn claim_rejects_non_canonical_three_component_versions() {
    let git = LocalGit::new("/nonexistent");
    let origin = crate::publisher_script::Script::new(Vec::new());
    let reserved = ledger::Reserved {
        build: NOW,
        commit: "0000000000000000000000000000000000000002".into(),
        line: format!("{NOW} 0.26.0"),
    };
    for bad in [
        "0.26", "v0.26.1", "0.2.x", "0.26.1.2", "01.2.3", "0..1", "26", "",
    ] {
        let plan = ClaimPlan {
            version: bad,
            now: NOW,
            allow_existing_section: false,
            max_attempts: 5,
            source: "0000000000000000000000000000000000000001",
            harness_base: None,
        };
        for err in [
            ledger::reserve(&git, &origin, &plan, DATE)
                .unwrap_err()
                .to_string(),
            ledger::claim(&git, &origin, Path::new("/nonexistent"), &plan, &reserved)
                .unwrap_err()
                .to_string(),
        ] {
            assert!(err.contains("MAJOR.MINOR.PATCH"), "{bad:?} → {err}");
        }
    }
}

#[cfg(unix)]
#[test]
fn happy_path_claims_appends_and_verifies() {
    let fix = Fixture::new("happy");
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let (res, attempts) = do_claim(&git, &fix.work, "0.26.0", false);
    let c = res.unwrap();
    assert_eq!(c.build, NOW);
    assert_eq!(c.ledger_line, "1790000000 0.26.0");
    assert_eq!(
        attempts, 1,
        "the reservation's roll; a fast-forward builds no landing"
    );
    assert!(
        run(&fix.work, "git", &["for-each-ref", ledger::RESERVED_REF])
            .trim()
            .is_empty(),
        "the reservation's ref goes once main keeps the commit"
    );
    assert_eq!(c.landed, c.commit);
    assert_eq!(parents(&fix.work, &c.commit), vec![source.clone()]);
    assert_eq!(c.commit, head_of(&fix.work), "the checkout sits on it");
    assert!(origin_subjects(&fix).contains("release: v0.26.0 (build 1790000000)"));
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1790000000 0.26.0\n")
    );
    assert!(origin_file(&fix, "CHANGELOG.md").contains(&format!(
        "## [Unreleased]\n\n## [0.26.0] - {DATE}\n\n### Added\n\n- **Something real**"
    )));
    assert_eq!(
        moved(&fix.work, &source, &c.commit),
        "CHANGELOG.md RELEASES.ledger"
    );
}

#[cfg(unix)]
#[test]
fn a_published_commit_behind_main_is_released_and_merged() {
    let fix = Fixture::new("behind");
    let source = head_of(&fix.work);
    rival_peer_push(&fix);
    run(&fix.work, "git", &["fetch", "-q", "origin", "main"]);
    let tip = run(&fix.work, "git", &["rev-parse", "origin/main"])
        .trim()
        .to_string();
    let git = Git::new(&fix.work);
    let (res, attempts) = do_claim(&git, &fix.work, "0.26.0", false);
    let c = res.expect("a peer's push is not a refusal");
    assert_eq!(attempts, 2, "the reservation's roll, then the landing's");

    assert_eq!(parents(&fix.work, &c.commit), vec![source.clone()]);
    assert_eq!(
        moved(&fix.work, &source, &c.commit),
        "CHANGELOG.md RELEASES.ledger"
    );
    assert_eq!(
        c.commit,
        head_of(&fix.work),
        "the checkout sits on the release commit"
    );
    assert!(
        run(&fix.work, "git", &["status", "--porcelain"])
            .trim()
            .is_empty(),
        "building main's commit left the worktree and index exactly the release commit's"
    );
    assert!(
        !fix.work.join("peer.rs").exists(),
        "the peer's code is not in the build"
    );

    assert_ne!(c.landed, c.commit);
    assert_eq!(
        parents(&fix.work, &c.landed),
        vec![tip.clone(), c.commit.clone()]
    );
    assert_eq!(
        moved(&fix.work, &tip, &c.landed),
        "CHANGELOG.md RELEASES.ledger"
    );
    assert_eq!(
        run(&fix.origin, "git", &["rev-parse", "main"]).trim(),
        c.landed
    );
    assert_eq!(
        origin_file(&fix, "peer.rs"),
        "fn peer() {}\n",
        "the peer's code stays on main"
    );
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1790000000 0.26.0\n")
    );

    let main_changelog = origin_file(&fix, "CHANGELOG.md");
    assert!(
        main_changelog.contains(&format!(
            "## [Unreleased]\n\n### Added\n\n- **A peer's entry** — not in the published commit.\n\n## [0.26.0] - {DATE}\n\n### Added\n\n- **Something real**"
        )),
        "{main_changelog}"
    );
    let released = run(
        &fix.work,
        "git",
        &["show", &format!("{}:CHANGELOG.md", c.commit)],
    );
    assert!(!released.contains("A peer's entry"), "{released}");
}

#[cfg(unix)]
#[test]
fn a_failure_building_mains_commit_leaves_the_checkout_on_the_published_commit() {
    struct FailingLanding(LocalGit);
    impl GitRunner for FailingLanding {
        fn git(&self, args: &[&str]) -> ledger::Result<RunOut> {
            let parents = args.iter().filter(|a| **a == "-p").count();
            if args.first() == Some(&"commit-tree") && parents == 2 {
                return Ok(RunOut {
                    status: 128,
                    stdout: Vec::new(),
                    stderr: b"fatal: injected commit-tree failure".to_vec(),
                });
            }
            self.0.git(args)
        }

        fn git_input(&self, args: &[&str], input: &[u8]) -> ledger::Result<RunOut> {
            self.0.git_input(args, input)
        }
    }
    let fix = Fixture::new("landing-fails");
    let source = head_of(&fix.work);
    rival_peer_push(&fix);
    let origin = Git::new(&fix.work);
    let (res, attempts) = do_claim_with_origin(
        &FailingLanding(LocalGit::new(&fix.work)),
        &origin,
        &fix.work,
        "0.26.0",
        false,
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("injected commit-tree failure"), "{err}");
    assert_eq!(attempts, 2, "the reservation's roll, then the landing's");
    assert_clean_at(&fix, &source);
}

#[cfg(unix)]
#[test]
fn a_checkout_off_the_published_commit_is_refused_before_anything() {
    let fix = Fixture::new("off-source");
    rival_peer_push(&fix);
    run(&fix.work, "git", &["fetch", "-q", "origin", "main"]);
    let tip = run(&fix.work, "git", &["rev-parse", "origin/main"])
        .trim()
        .to_string();
    let git = Git::new(&fix.work);
    let plan = ClaimPlan {
        version: "0.26.0",
        now: NOW,
        allow_existing_section: false,
        max_attempts: 5,
        source: &tip,
        harness_base: None,
    };
    let reserved = ledger::reserve(&git, &git, &plan, DATE).expect("reserved from objects");
    ledger::claim(&git, &git, &fix.work, &plan, &reserved).unwrap_err();
    assert!(!origin_subjects(&fix).contains("release: v0.26.0"));
}

#[cfg(unix)]
#[test]
fn a_reservation_the_tail_reached_refuses_and_burns_nothing() {
    for (name, winner) in [
        ("stale-at", format!("{NOW} 0.99.0")),
        ("stale-past", format!("{} 0.99.0", NOW + 123)),
    ] {
        let winner = winner.as_str();
        let fix = Fixture::new(name);
        let source = head_of(&fix.work);
        let runner = HookRunner::new(&fix.work, |attempt| {
            if attempt == 1 {
                rival_append(&fix, winner);
            }
        });
        let (res, _) = do_claim(&runner, &fix.work, "0.26.0", false);
        res.unwrap_err();
        assert_eq!(
            runner.pushes.get(),
            1,
            "{winner}: never a push under another number"
        );
        assert_clean_at(&fix, &source);
        assert_eq!(
            origin_file(&fix, "RELEASES.ledger"),
            format!("{SEED_LEDGER}{winner}\n"),
            "{winner}: the winner's line, and nothing of ours"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_fresh_reservation_after_a_stale_one_lands_above_the_winner() {
    let fix = Fixture::new("stale-again");
    let source = head_of(&fix.work);
    let winner = "1790000123 0.99.0";
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_append(&fix, winner);
        }
    });
    let (stale, _) = do_claim(&runner, &fix.work, "0.26.0", false);
    stale.unwrap_err();
    let (res, _) = do_claim(&Git::new(&fix.work), &fix.work, "0.26.0", false);
    let c = res.expect("a fresh reservation lands");
    assert_eq!(c.build, 1_790_000_124);
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}{winner}\n1790000124 0.26.0\n")
    );
    assert_ne!(c.landed, c.commit);
    assert_eq!(parents(&fix.work, &c.commit), vec![source]);
}

#[cfg(unix)]
#[test]
fn a_lost_race_below_the_reservation_lands_it_with_its_own_number() {
    let fix = Fixture::new("cas-clock");
    let winner = "1789999000 0.98.0";
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_append(&fix, winner);
        }
    });
    let (res, attempts) = do_claim(&runner, &fix.work, "0.26.0", false);
    let c = res.unwrap();
    assert_eq!(c.build, NOW);
    assert_eq!(
        attempts, 2,
        "the reservation's roll, then the retry's landing"
    );
    assert_eq!(runner.pushes.get(), 2);
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}{winner}\n1790000000 0.26.0\n")
    );
    assert_ne!(c.landed, c.commit, "the winner moved main: a merge");
}

#[cfg(unix)]
#[test]
fn retry_cap_aborts_clean_with_nothing_burned() {
    let fix = Fixture::new("cap");
    let source = head_of(&fix.work);
    let runner = HookRunner::new(&fix.work, |attempt| {
        rival_append(&fix, &format!("17899990{attempt:02} 0.9{attempt}.0"));
    });
    let (res, attempts) = do_claim(&runner, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("5 times"), "{err}");
    assert_eq!(runner.pushes.get(), 5);
    assert_eq!(attempts, 5, "the reservation's roll, then four landings");
    assert_clean_at(&fix, &source);
    let mut expected = SEED_LEDGER.to_string();
    for a in 1..=5u32 {
        expected.push_str(&format!("17899990{a:02} 0.9{a}.0\n"));
    }
    assert_eq!(origin_file(&fix, "RELEASES.ledger"), expected);
}

#[cfg(unix)]
#[test]
fn remote_tag_means_version_cut_elsewhere() {
    let fix = Fixture::new("tag-elsewhere");
    let source = head_of(&fix.work);
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_append(&fix, "1790000300 0.26.0");
            run(&fix.rival, "git", &["tag", "v0.26.0"]);
            run(&fix.rival, "git", &["push", "-q", "origin", "v0.26.0"]);
        }
    });
    let (res, _) = do_claim(&runner, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("cut elsewhere") && err.contains("tag v0.26.0"),
        "{err}"
    );
    assert_eq!(
        runner.pushes.get(),
        1,
        "must abort, not retry, on a same-version tag"
    );
    assert_clean_at(&fix, &source);
}

#[cfg(unix)]
#[test]
fn remote_changelog_section_means_version_cut_elsewhere() {
    let fix = Fixture::new("section-elsewhere");
    let source = head_of(&fix.work);
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_land_section(&fix, "1790000300 0.26.0", "0.26.0");
        }
    });
    let (res, _) = do_claim(&runner, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("cut elsewhere") && err.contains("changelog"),
        "{err}"
    );
    assert_clean_at(&fix, &source);
}

#[cfg(unix)]
#[test]
fn recut_flag_claims_fresh_n_past_existing_section() {
    let fix = Fixture::new("recut");
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_land_section(&fix, "1789999300 0.26.0", "0.26.0");
        }
    });
    let (res, _) = do_claim(&runner, &fix.work, "0.26.0", true);
    let c = res.unwrap();
    assert_eq!(c.build, NOW);
    let remote = origin_file(&fix, "RELEASES.ledger");
    assert!(remote.contains("1789999300 0.26.0\n"));
    assert!(remote.ends_with("1790000000 0.26.0\n"));
    let main_changelog = origin_file(&fix, "CHANGELOG.md");
    assert!(main_changelog.contains("## [0.26.0] - 2026-01-01"));
    assert_eq!(
        main_changelog.matches("## [0.26.0]").count(),
        1,
        "main is never rolled twice: {main_changelog}"
    );
}

#[cfg(unix)]
#[test]
fn a_recut_from_a_source_carrying_the_section_moves_only_the_ledger() {
    let fix = Fixture::new("recut-tip");
    rival_land_section(&fix, "1789999300 0.26.0", "0.26.0");
    // A recut's entry files belong to the next window: it deletes none (T-R4).
    rival_files(
        &fix,
        &[("changes/fixed/next.md", Some("- **Next.** Later.\n"))],
    );
    sync_work(&fix);
    let source = head_of(&fix.work);
    let rolled = origin_file(&fix, "CHANGELOG.md");
    let (res, attempts) = do_claim(&Git::new(&fix.work), &fix.work, "0.26.0", true);
    let c = res.expect("a source that already carries the section is claimed");
    assert_eq!(
        attempts, 1,
        "the reservation's roll; a fast-forward builds no landing"
    );
    assert_eq!(c.build, NOW);
    assert_eq!(c.landed, c.commit, "main had not moved: a fast-forward");
    assert_eq!(parents(&fix.work, &c.commit), vec![source.clone()]);
    assert_eq!(moved(&fix.work, &source, &c.commit), "RELEASES.ledger");
    assert_eq!(
        origin_file(&fix, "CHANGELOG.md"),
        rolled,
        "the section stands as it was"
    );
    assert_eq!(
        origin_file(&fix, "changes/fixed/next.md"),
        "- **Next.** Later.\n",
        "a recut deletes no entry file"
    );
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1789999300 0.26.0\n1790000000 0.26.0\n")
    );
}

#[cfg(unix)]
#[test]
fn a_reservation_that_is_not_the_claims_is_refused_before_the_push() {
    let fix = Fixture::new("not-the-claims");
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let plan = ClaimPlan {
        version: "0.26.0",
        now: NOW,
        allow_existing_section: false,
        max_attempts: 5,
        source: &source,
        harness_base: None,
    };
    let reserved = ledger::reserve(&git, &git, &plan, DATE).expect("reserved");
    let ledger_text = run(
        &fix.work,
        "git",
        &["show", &format!("{}:RELEASES.ledger", reserved.commit)],
    );
    let rolled = run(
        &fix.work,
        "git",
        &["show", &format!("{}:CHANGELOG.md", reserved.commit)],
    );
    for (files, why) in [
        (
            vec![
                (
                    "CHANGELOG.md",
                    rolled.replace("Something real", "Something else"),
                ),
                ("RELEASES.ledger", ledger_text.clone()),
            ],
            "is not the published commit's window assembled and rolled",
        ),
        (
            vec![
                ("CHANGELOG.md", SEED_CHANGELOG.to_string()),
                ("RELEASES.ledger", ledger_text.clone()),
            ],
            "carries no \"## [0.26.0] - <date>\" section",
        ),
        (
            vec![
                ("CHANGELOG.md", rolled.clone()),
                ("RELEASES.ledger", SEED_LEDGER.to_string()),
            ],
            "its ledger is not the published commit's plus",
        ),
        (
            vec![
                ("CHANGELOG.md", rolled.clone()),
                // Preserve the reserved tail while rewriting an old record.
                (
                    "RELEASES.ledger",
                    ledger_text.replacen("0.25.0", "0.24.0", 1),
                ),
            ],
            "its ledger is not the published commit's plus",
        ),
    ] {
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let forged = ledger::commit_with(&git, &source, &files, &[], &[&source], "forged").unwrap();
        let forged = ledger::Reserved {
            commit: forged,
            ..reserved.clone()
        };
        let err = ledger::claim(&git, &git, &fix.work, &plan, &forged)
            .unwrap_err()
            .to_string();
        assert!(err.contains(why), "{why}: {err}");
    }
    fs::write(fix.work.join("stray.rs"), "fn stray() {}\n").unwrap();
    run(&fix.work, "git", &["add", "stray.rs"]);
    run(&fix.work, "git", &["commit", "-q", "-m", "stray"]);
    let with_stray = head_of(&fix.work);
    run(&fix.work, "git", &["reset", "-q", "--hard", &source]);
    let forged = ledger::commit_with(
        &git,
        &with_stray,
        &[("CHANGELOG.md", &rolled), ("RELEASES.ledger", &ledger_text)],
        &[],
        &[&source],
        "forged",
    )
    .unwrap();
    let err = ledger::claim(
        &git,
        &git,
        &fix.work,
        &plan,
        &ledger::Reserved {
            commit: forged,
            ..reserved
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("\nA\tstray.rs"), "{err}");
    assert_clean_at(&fix, &source);
}

#[cfg(unix)]
#[test]
fn a_claim_that_landed_unjournaled_is_spent_and_the_re_run_claims_anew() {
    struct FailingCheckout(LocalGit);
    impl GitRunner for FailingCheckout {
        fn git(&self, args: &[&str]) -> ledger::Result<RunOut> {
            if args.contains(&"checkout") {
                return Ok(RunOut {
                    status: 128,
                    stdout: Vec::new(),
                    stderr: b"fatal: injected checkout failure".to_vec(),
                });
            }
            self.0.git(args)
        }

        fn git_input(&self, args: &[&str], input: &[u8]) -> ledger::Result<RunOut> {
            self.0.git_input(args, input)
        }
    }
    let fix = Fixture::new("landed-unjournaled");
    let origin = Git::new(&fix.work);
    let (res, _) = do_claim_with_origin(
        &FailingCheckout(LocalGit::new(&fix.work)),
        &origin,
        &fix.work,
        "0.26.0",
        false,
    );
    res.unwrap_err();
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1790000000 0.26.0\n"),
        "the claim landed"
    );
    let (res, _) = do_claim(&Git::new(&fix.work), &fix.work, "0.26.0", true);
    let c = res.expect("the re-run claims anew");
    assert_eq!(c.build, 1_790_000_001);
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1790000000 0.26.0\n1790000001 0.26.0\n")
    );
}

#[cfg(unix)]
#[test]
fn claim_refuses_a_rewritten_historical_record_on_newer_main() {
    let fix = Fixture::new("rewritten-history");
    rival_append(&fix, "1783354740 0.25.0");
    run(&fix.work, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.work, "git", &["reset", "-q", "--hard", "origin/main"]);
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let plan = ClaimPlan {
        version: "0.26.0",
        now: NOW,
        allow_existing_section: false,
        max_attempts: 5,
        source: &source,
        harness_base: None,
    };
    let reserved = ledger::reserve(&git, &git, &plan, DATE).unwrap();
    let original = fs::read_to_string(fix.rival.join("RELEASES.ledger")).unwrap();
    let tampered = original.replacen("0.25.0", "0.24.0", 1);
    assert_eq!(original.lines().last(), tampered.lines().last());
    fs::write(fix.rival.join("RELEASES.ledger"), &tampered).unwrap();
    run(
        &fix.rival,
        "git",
        &["commit", "-q", "-am", "rewrite historical record"],
    );
    run(&fix.rival, "git", &["push", "-q", "origin", "main"]);
    let before = head_of(&fix.rival);
    // NEGATIVE CONTROL: without the prefix proof, claim succeeds and appends
    // its reservation to this well-formed but rewritten ledger.
    assert!(ledger::claim(&git, &git, &fix.work, &plan, &reserved).is_err());
    assert_eq!(
        run(&fix.origin, "git", &["rev-parse", "main"]).trim(),
        before
    );
    assert_eq!(origin_file(&fix, "RELEASES.ledger"), tampered);
    assert_eq!(head_of(&fix.work), source);
}

#[cfg(unix)]
#[test]
fn malformed_remote_ledger_aborts_before_any_commit() {
    let fix = Fixture::new("malformed");
    rival_append(&fix, "not-a-number 0.26.0");
    run(&fix.work, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.work, "git", &["reset", "-q", "--hard", "origin/main"]);
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let (res, attempts) = do_claim(&git, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("line 3"), "{err}");
    assert_eq!(attempts, 0);
    assert_clean_at(&fix, &source);
}

#[cfg(unix)]
#[test]
fn offline_fetch_fails_closed() {
    let fix = Fixture::new("offline");
    let git = Git::new(&fix.work);
    fs::rename(&fix.origin, fix.root.path().join("offline.git")).unwrap();
    let (res, attempts) = do_claim(&git, &fix.work, "0.26.0", false);
    res.unwrap_err();
    assert_eq!(attempts, 0, "no offline claim may roll anything");
}

#[cfg(unix)]
#[test]
fn an_uncertain_native_claim_never_retries_or_rereads_as_a_race() {
    struct LostReply {
        inner: Git,
        refreshes: Cell<u32>,
        writes: Cell<u32>,
    }
    impl ledger::Origin for LostReply {
        fn refresh_main(&self) -> std::result::Result<String, String> {
            self.refreshes.set(self.refreshes.get() + 1);
            ledger::Origin::refresh_main(&self.inner)
        }
        fn get(&self, _: &str) -> std::result::Result<Option<String>, String> {
            panic!("unknown is not a conflict/tag probe")
        }
        fn import(&self, _: &str) -> std::result::Result<(), String> {
            panic!("unexpected import")
        }
        fn peel(&self, _: &str) -> std::result::Result<Option<String>, String> {
            panic!("unexpected peel")
        }
        fn tag_object(&self, _: &str) -> std::result::Result<Vec<u8>, String> {
            panic!("unexpected tag")
        }
        fn write(
            &self,
            reference: &str,
            before: Option<&str>,
            after: &str,
        ) -> std::result::Result<ledger::RefWrite, String> {
            self.writes.set(self.writes.get() + 1);
            assert_eq!(
                ledger::Origin::write(&self.inner, reference, before, after)?,
                ledger::RefWrite::Written
            );
            // The real native backend accepted the exact main update; this
            // fixture withholds only its reply. Journal persistence is tested
            // separately at the real native adapter's intent boundary.
            Err("outcome unknown; side_effect=unknown; [rejected] non-fast-forward".into())
        }
        fn delete(&self, _: &str, _: &str) -> std::result::Result<ledger::RefWrite, String> {
            panic!("unexpected delete")
        }
        fn control_transaction(
            &self,
            _: ledger::ControlRefTransaction,
        ) -> std::result::Result<ledger::RefWrite, String> {
            panic!("unexpected control")
        }
    }
    let fix = Fixture::new("unknown-reply");
    let source = head_of(&fix.work);
    let origin = LostReply {
        inner: Git::new(&fix.work),
        refreshes: Cell::new(0),
        writes: Cell::new(0),
    };
    let (result, _) = do_claim_with_origin(&origin.inner, &origin, &fix.work, "0.26.0", false);
    let error = result.unwrap_err().to_string();
    assert!(error.contains("outcome unknown"), "{error}");
    assert_eq!(origin.writes.get(), 1, "no repeated mutation");
    assert_eq!(
        origin.refreshes.get(),
        2,
        "only reservation and initial claim snapshots, no post-read"
    );
    assert_eq!(
        head_of(&fix.work),
        source,
        "no successful claim/detach is reported"
    );
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1790000000 0.26.0\n"),
        "the native write really landed despite the withheld reply"
    );
}

/// Commit `files` in the rival clone (`None` deletes the path) and push them to main.
fn rival_files(fix: &Fixture, files: &[(&str, Option<&str>)]) {
    run(&fix.rival, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.rival, "git", &["reset", "-q", "--hard", "origin/main"]);
    for (path, text) in files {
        let full = fix.rival.join(path);
        match text {
            Some(text) => {
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(&full, text).unwrap();
                run(&fix.rival, "git", &["add", "--", path]);
            }
            None => {
                run(&fix.rival, "git", &["rm", "-q", "--", path]);
            }
        }
    }
    run(
        &fix.rival,
        "git",
        &["commit", "-q", "-m", "rival: entry files"],
    );
    run(&fix.rival, "git", &["push", "-q", "origin", "main"]);
}

/// Put the work clone on origin's main: the published commit a release is cut from.
fn sync_work(fix: &Fixture) {
    run(&fix.work, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.work, "git", &["reset", "-q", "--hard", "origin/main"]);
}

fn name_status(repo: &Path, from: &str, to: &str) -> String {
    run(
        repo,
        "git",
        &["diff", "--name-status", "--no-renames", from, to],
    )
}

fn rolled_section(repo: &Path, commit: &str) -> String {
    let text = run(repo, "git", &["show", &format!("{commit}:CHANGELOG.md")]);
    changelog::rolled_body(&text, "0.26.0").unwrap()
}

/// T-R2. THE RELEASE COMMIT ROLLS THE ENTRY FILES AND DELETES THEM: the published commit's raw
/// entry and both files ship in `[0.26.0]` in kind order, the release commit holds no
/// `changes/`, and against the published commit it modifies the two claim files and deletes the
/// two entry files, nothing else.
#[cfg(unix)]
#[test]
fn the_release_commit_rolls_the_fragments_and_deletes_them() {
    let fix = Fixture::new("fragments");
    rival_files(
        &fix,
        &[
            ("changes/added/b.md", Some("- **B.** Added by a file.\n")),
            ("changes/fixed/a.md", Some("- **A.** Fixed by a file.\n")),
        ],
    );
    sync_work(&fix);
    let source = head_of(&fix.work);
    let (res, _) = do_claim(&Git::new(&fix.work), &fix.work, "0.26.0", false);
    let c = res.unwrap();
    assert_eq!(c.landed, c.commit, "a fast-forward");
    assert_eq!(
        rolled_section(&fix.work, &c.commit),
        "### Added\n\n- **Something real** — an entry.\n\n- **B.** Added by a file.\n\n\
         ### Fixed\n\n- **A.** Fixed by a file."
    );
    assert_eq!(
        name_status(&fix.work, &source, &c.commit),
        "M\tCHANGELOG.md\nM\tRELEASES.ledger\nD\tchanges/added/b.md\nD\tchanges/fixed/a.md\n"
    );
    assert!(
        run(&fix.work, "git", &["ls-tree", &c.commit, "changes"])
            .trim()
            .is_empty(),
        "an emptied changes/ goes with its files"
    );
    // NEGATIVE CONTROL: without files the release commit moves only the two claim files.
    let fix = Fixture::new("no-fragments");
    let source = head_of(&fix.work);
    let (res, _) = do_claim(&Git::new(&fix.work), &fix.work, "0.26.0", false);
    assert_eq!(
        name_status(&fix.work, &source, &res.unwrap().commit),
        "M\tCHANGELOG.md\nM\tRELEASES.ledger\n"
    );
}

/// T-R3. A LANDING ON A MOVED MAIN DELETES ONLY WHAT SHIPPED: after the publish main gains a
/// peer's file, rewrites a shipped one and deletes another; the landing deletes only the
/// shipped file main still holds unchanged, keeps the peer's and the rewritten one (which ship
/// next), and main records the release commit's section byte for byte under an empty
/// `[Unreleased]`.
#[cfg(unix)]
#[test]
fn a_landing_on_moved_main_deletes_only_what_shipped() {
    let fix = Fixture::new("moved-fragments");
    rival_files(
        &fix,
        &[
            ("changes/fixed/a.md", Some("- **A.** Shipped.\n")),
            ("changes/fixed/b.md", Some("- **B.** Shipped.\n")),
            ("changes/fixed/c.md", Some("- **C.** Shipped.\n")),
        ],
    );
    sync_work(&fix);
    rival_files(
        &fix,
        &[
            ("changes/fixed/p.md", Some("- **P.** A peer's.\n")),
            (
                "changes/fixed/a.md",
                Some("- **A.** Reworded after the publish.\n"),
            ),
            ("changes/fixed/c.md", None),
        ],
    );
    let tip = run(&fix.rival, "git", &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let (res, attempts) = do_claim(&Git::new(&fix.work), &fix.work, "0.26.0", false);
    let c = res.unwrap();
    assert_eq!(attempts, 2, "the reservation, then the landing");
    assert_eq!(
        parents(&fix.work, &c.landed),
        vec![tip.clone(), c.commit.clone()]
    );
    assert_eq!(
        name_status(&fix.work, &tip, &c.landed),
        "M\tCHANGELOG.md\nM\tRELEASES.ledger\nD\tchanges/fixed/b.md\n"
    );
    assert_eq!(
        origin_file(&fix, "changes/fixed/a.md"),
        "- **A.** Reworded after the publish.\n"
    );
    assert_eq!(
        origin_file(&fix, "changes/fixed/p.md"),
        "- **P.** A peer's.\n"
    );
    assert_eq!(
        rolled_section(&fix.work, &c.landed),
        rolled_section(&fix.work, &c.commit)
    );
    assert!(rolled_section(&fix.work, &c.commit).contains("- **A.** Shipped."));
    assert!(
        origin_file(&fix, "CHANGELOG.md").contains("## [Unreleased]\n\n## [0.26.0] - "),
        "main's [Unreleased] is the empty scaffold"
    );
}

/// T-R6. A CLAIM LANDS THE RESERVATION DAY'S DATE: the claim takes no date of its own, so a
/// landing made after midnight records the release commit's `## [0.26.0] - <date>`.
#[cfg(unix)]
#[test]
fn a_claim_after_midnight_lands_the_reserve_days_date() {
    let fix = Fixture::new("midnight");
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let plan = ClaimPlan {
        version: "0.26.0",
        now: NOW,
        allow_existing_section: false,
        max_attempts: 5,
        source: &source,
        harness_base: None,
    };
    let reserved = ledger::reserve(&git, &git, &plan, "2026-09-23").unwrap();
    rival_peer_push(&fix);
    let c = ledger::claim(&git, &git, &fix.work, &plan, &reserved).unwrap();
    assert_ne!(c.landed, c.commit);
    let landed = origin_file(&fix, "CHANGELOG.md");
    assert!(landed.contains("## [0.26.0] - 2026-09-23\n"), "{landed}");
    assert_eq!(landed.matches("## [0.26.0]").count(), 1);
}

/// THE LANDING NEVER JUDGES MAIN'S TIP: after the publish main gains text under `[Unreleased]`
/// no release would accept and odd files under `changes/` (an executable entry, one nested a
/// level too deep); the claim still lands, main records the release commit's section byte for
/// byte, and the odd text and files stay for the dev check and the next cut.
#[cfg(unix)]
#[test]
fn a_landing_on_a_main_the_release_would_refuse_still_lands_exactly() {
    let fix = Fixture::new("odd-main");
    rival_files(&fix, &[("changes/fixed/a.md", Some("- **A.** Shipped.\n"))]);
    sync_work(&fix);
    run(&fix.rival, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.rival, "git", &["reset", "-q", "--hard", "origin/main"]);
    let cp = fix.rival.join("CHANGELOG.md");
    let odd = fs::read_to_string(&cp).unwrap().replace(
        "## [Unreleased]\n\n",
        "## [Unreleased]\n\n- A raw line, no heading.\n\n",
    );
    fs::write(&cp, odd).unwrap();
    fs::create_dir_all(fix.rival.join("changes/fixed/deep")).unwrap();
    fs::write(
        fix.rival.join("changes/fixed/deep/n.md"),
        "- **N.** Nested.\n",
    )
    .unwrap();
    fs::write(
        fix.rival.join("changes/fixed/x.md"),
        "- **X.** Executable.\n",
    )
    .unwrap();
    run(&fix.rival, "git", &["add", "-A"]);
    run(
        &fix.rival,
        "git",
        &["update-index", "--chmod=+x", "changes/fixed/x.md"],
    );
    run(
        &fix.rival,
        "git",
        &["commit", "-q", "-m", "rival: odd window"],
    );
    run(&fix.rival, "git", &["push", "-q", "origin", "main"]);
    let tip = run(&fix.rival, "git", &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let (res, _) = do_claim(&Git::new(&fix.work), &fix.work, "0.26.0", false);
    let c = res.expect("the landing never judges main's tip");
    assert_eq!(
        rolled_section(&fix.work, &c.landed),
        rolled_section(&fix.work, &c.commit)
    );
    assert_eq!(
        name_status(&fix.work, &tip, &c.landed),
        "M\tCHANGELOG.md\nM\tRELEASES.ledger\nD\tchanges/fixed/a.md\n"
    );
    let main = origin_file(&fix, "CHANGELOG.md");
    assert!(
        main.contains("## [Unreleased]\n\n- A raw line, no heading.\n\n## [0.26.0]"),
        "{main}"
    );
    assert_eq!(
        origin_file(&fix, "changes/fixed/deep/n.md"),
        "- **N.** Nested.\n"
    );
    assert!(
        run(
            &fix.origin,
            "git",
            &["ls-tree", "main", "changes/fixed/x.md"]
        )
        .starts_with("100755 "),
        "the executable file stays as it was"
    );
}

/// T-R5. A RESERVATION THAT SHIPS OTHER FILES THAN IT DELETES IS REFUSED BEFORE THE PUSH, by the
/// tree rule: a release commit with the right roll that keeps one of the published commit's
/// files; one that deletes a file and edits another; and a recut's that deletes a file. Each
/// names what it moved. NEGATIVE CONTROL: the honest reservation claims and deletes both files.
#[cfg(unix)]
#[test]
fn a_reservation_that_ships_other_files_than_it_deletes_is_refused() {
    let fix = Fixture::new("forged-fragments");
    rival_files(
        &fix,
        &[
            ("changes/fixed/a.md", Some("- **A.** One.\n")),
            ("changes/fixed/b.md", Some("- **B.** Two.\n")),
        ],
    );
    sync_work(&fix);
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let plan = ClaimPlan {
        version: "0.26.0",
        now: NOW,
        allow_existing_section: false,
        max_attempts: 5,
        source: &source,
        harness_base: None,
    };
    let reserved = ledger::reserve(&git, &git, &plan, DATE).unwrap();
    let show = |path: &str| {
        run(
            &fix.work,
            "git",
            &["show", &format!("{}:{path}", reserved.commit)],
        )
    };
    let (rolled, ledger_text) = (show("CHANGELOG.md"), show("RELEASES.ledger"));
    let oid = |path: &str| {
        run(
            &fix.work,
            "git",
            &["rev-parse", &format!("{source}:{path}")],
        )
        .trim()
        .to_string()
    };
    let a = oid("changes/fixed/a.md");
    let files = [
        ("CHANGELOG.md", rolled.as_str()),
        ("RELEASES.ledger", ledger_text.as_str()),
    ];
    let keeps_one = ledger::commit_with(
        &git,
        &source,
        &files,
        &[("changes/fixed/a.md", a.as_str())],
        &[&source],
        "forged: keeps b.md",
    )
    .unwrap();
    // One file deleted, the other edited: made in the work clone, then left.
    run(
        &fix.work,
        "git",
        &["checkout", "-q", "--detach", &keeps_one],
    );
    fs::write(fix.work.join("changes/fixed/b.md"), "- **B.** Edited.\n").unwrap();
    run(
        &fix.work,
        "git",
        &["commit", "-q", "-am", "forged: edits b.md"],
    );
    let edited = run(&fix.work, "git", &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_string();
    let edits_one = run(
        &fix.work,
        "git",
        &[
            "commit-tree",
            &edited,
            "-p",
            &source,
            "-m",
            "forged: edits b.md",
        ],
    )
    .trim()
    .to_string();
    run(&fix.work, "git", &["checkout", "-q", &source]);
    run(&fix.work, "git", &["checkout", "-q", "-B", "main", &source]);
    for (forged, moved) in [
        (
            keeps_one,
            "M\tCHANGELOG.md\nM\tRELEASES.ledger\nD\tchanges/fixed/a.md",
        ),
        (
            edits_one,
            "M\tCHANGELOG.md\nM\tRELEASES.ledger\nD\tchanges/fixed/a.md\nM\tchanges/fixed/b.md",
        ),
    ] {
        let err = ledger::claim(
            &git,
            &git,
            &fix.work,
            &plan,
            &ledger::Reserved {
                commit: forged,
                ..reserved.clone()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("its tree is not"), "{err}");
        assert!(err.contains(moved), "{moved}: {err}");
        assert_clean_at(&fix, &source);
    }
    // A recut ships its section as it stands and deletes no file.
    let recut_text = rolled.clone();
    let fix_recut = Fixture::new("forged-recut");
    rival_files(
        &fix_recut,
        &[
            ("CHANGELOG.md", Some(recut_text.as_str())),
            ("changes/fixed/a.md", Some("- **A.** Next window.\n")),
        ],
    );
    sync_work(&fix_recut);
    let recut_source = head_of(&fix_recut.work);
    let recut_git = Git::new(&fix_recut.work);
    let recut_plan = ClaimPlan {
        source: &recut_source,
        allow_existing_section: true,
        ..plan
    };
    let recut = ledger::reserve(&recut_git, &recut_git, &recut_plan, DATE).unwrap();
    assert_eq!(
        name_status(&fix_recut.work, &recut_source, &recut.commit),
        "M\tRELEASES.ledger\n"
    );
    let recut_ledger = run(
        &fix_recut.work,
        "git",
        &["show", &format!("{}:RELEASES.ledger", recut.commit)],
    );
    let a_next = run(
        &fix_recut.work,
        "git",
        &["rev-parse", &format!("{recut_source}:changes/fixed/a.md")],
    )
    .trim()
    .to_string();
    let deletes = ledger::commit_with(
        &recut_git,
        &recut_source,
        &[("RELEASES.ledger", recut_ledger.as_str())],
        &[("changes/fixed/a.md", a_next.as_str())],
        &[&recut_source],
        "forged: a recut that deletes",
    )
    .unwrap();
    let err = ledger::claim(
        &recut_git,
        &recut_git,
        &fix_recut.work,
        &recut_plan,
        &ledger::Reserved {
            commit: deletes,
            ..recut
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("D\tchanges/fixed/a.md"), "{err}");

    let c = ledger::claim(&git, &git, &fix.work, &plan, &reserved).expect("NEGATIVE CONTROL");
    assert_eq!(
        name_status(&fix.work, &source, &c.commit),
        "M\tCHANGELOG.md\nM\tRELEASES.ledger\nD\tchanges/fixed/a.md\nD\tchanges/fixed/b.md\n"
    );
}

/// T-R9. THE DRY RUN PRINTS THE ROLL THE RESERVATION WRITES: [`ledger::release_roll`], what a
/// dry run's words count, is byte for byte the release commit's changelog and exactly the files
/// it deletes, and its words count the section's entries and those files.
#[cfg(unix)]
#[test]
fn the_dry_run_prints_the_roll_the_reservation_writes() {
    let fix = Fixture::new("dry-roll");
    rival_files(
        &fix,
        &[
            ("changes/fixed/a.md", Some("- **A.** One.\n")),
            ("changes/security/s.md", Some("- **S.** Two.\n")),
        ],
    );
    sync_work(&fix);
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let plan = ClaimPlan {
        version: "0.26.0",
        now: NOW,
        allow_existing_section: false,
        max_attempts: 5,
        source: &source,
        harness_base: None,
    };
    let (text, removals) = ledger::release_roll(&git, &plan, DATE).unwrap();
    let reserved = ledger::reserve(&git, &git, &plan, DATE).unwrap();
    assert_eq!(
        text,
        run(
            &fix.work,
            "git",
            &["show", &format!("{}:CHANGELOG.md", reserved.commit)]
        )
    );
    let deleted: Vec<String> = name_status(&fix.work, &source, &reserved.commit)
        .lines()
        .filter_map(|row| row.strip_prefix("D\t").map(str::to_string))
        .collect();
    assert_eq!(
        removals
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>(),
        deleted
    );
    assert_eq!(
        changelog::roll_words(
            changelog::note_entries(&text, "0.26.0").len(),
            removals.len()
        ),
        "its roll ships 3 entries and deletes 2 entry files"
    );
}

/// T-R8. EVERY CUT MODE SHIPS THE RELEASE COMMIT'S ROLLED SECTION: the notes a cut writes come
/// from the release commit's objects ([`crate::publish::release_body_of`]), so a fresh window's
/// and a recut's release commits both ship their `[0.26.0]` section, whatever the working tree
/// holds. NEGATIVE CONTROL: the published commit itself has no such section to ship.
#[cfg(unix)]
#[test]
fn every_cut_mode_ships_the_release_commits_rolled_section() {
    let fix = Fixture::new("notes-from-objects");
    rival_files(&fix, &[("changes/fixed/a.md", Some("- **A.** Shipped.\n"))]);
    sync_work(&fix);
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let plan = ClaimPlan {
        version: "0.26.0",
        now: NOW,
        allow_existing_section: false,
        max_attempts: 5,
        source: &source,
        harness_base: None,
    };
    let fresh = ledger::reserve(&git, &git, &plan, DATE).unwrap();
    let body = crate::publish::release_body_of(&git, &fresh.commit, "0.26.0").unwrap();
    assert_eq!(body, rolled_section(&fix.work, &fresh.commit));
    assert!(body.contains("- **A.** Shipped."), "{body}");
    // The working tree still holds the unrolled window: it is never read.
    assert!(
        fs::read_to_string(fix.work.join("CHANGELOG.md"))
            .unwrap()
            .contains("## [Unreleased]\n\n### Added")
    );
    crate::publish::release_body_of(&git, &source, "0.26.0").unwrap_err();

    let fix = Fixture::new("notes-from-objects-recut");
    rival_land_section(&fix, "1789999300 0.26.0", "0.26.0");
    sync_work(&fix);
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let recut = ledger::reserve(
        &git,
        &git,
        &ClaimPlan {
            source: &source,
            ..plan
        },
        DATE,
    )
    .unwrap();
    assert_eq!(
        crate::publish::release_body_of(&git, &recut.commit, "0.26.0").unwrap(),
        "- cut elsewhere."
    );
}

/// THE LANDING REMOVES A FILE THAT CARRIES A RAW ENTRY THE RELEASE SHIPPED (the drain of
/// entries written before fragments): between the publish and the claim, main moves the
/// published commit's raw `[Unreleased]` entry into a file, word for word or re-wrapped. The
/// release shipped that entry, so the landing deletes the file; left on main, the next release's
/// step 1 would refuse it as a repeat of v0.26.0. A peer's new file stays.
/// NEGATIVE CONTROL: a file with other words stays and ships next.
#[cfg(unix)]
#[test]
fn a_raw_entry_moved_into_a_file_after_the_publish_is_removed_by_the_landing() {
    for (label, moved) in [
        ("verbatim", "- **Something real** — an entry.\n"),
        ("rewrapped", "- **Something real** —\n  an entry.\n"),
    ] {
        let fix = Fixture::new(&format!("raw-moved-{label}"));
        rival_files(
            &fix,
            &[
                (
                    "CHANGELOG.md",
                    Some(
                        &SEED_CHANGELOG
                            .replace("### Added\n\n- **Something real** — an entry.\n\n", ""),
                    ),
                ),
                ("changes/added/s9-real.md", Some(moved)),
                (
                    "changes/fixed/s9-peer.md",
                    Some("- **Peer.** Other words.\n"),
                ),
            ],
        );
        let (res, _) = do_claim(&Git::new(&fix.work), &fix.work, "0.26.0", false);
        let c = res.unwrap();
        assert_ne!(c.landed, c.commit, "{label}: a landing on moved main");
        assert_eq!(
            rolled_section(&fix.work, &c.landed),
            rolled_section(&fix.work, &c.commit),
            "{label}"
        );
        assert!(
            run(
                &fix.origin,
                "git",
                &["ls-tree", "main", "changes/added/s9-real.md"]
            )
            .trim()
            .is_empty(),
            "{label}: the shipped entry's file goes"
        );
        assert_eq!(
            origin_file(&fix, "changes/fixed/s9-peer.md"),
            "- **Peer.** Other words.\n",
            "{label}"
        );
    }
}

/// The claude harness's version file declaring 0.`minor`.
fn harness_file(minor: u16) -> String {
    format!(
        "//! A reader.\n\n{}\n",
        crate::harness_versions::version_line((0, minor))
    )
}

/// T-R10/T-R11. THE RESERVATION DERIVES THE HARNESS ANNOUNCEMENT AND THE CLAIM HOLDS THE RELEASE
/// COMMIT TO IT: with the claude harness raised since the tagged v0.25.0, the release commit's
/// section carries the derived line first under `### Changed`, and the claim lands it. A release
/// commit whose announcement was dropped, names another version, or counts from a release newer
/// than the window's is refused before the push.
/// NEGATIVE CONTROL: the same base with no raised version derives no line.
#[cfg(unix)]
#[test]
fn the_derived_harness_announcement_leads_changed_and_the_claim_holds_it() {
    const PHASE: &str = "crates/aterm-phase/src/phase.rs";
    let fix = Fixture::new("harness-announcement");
    rival_files(&fix, &[(PHASE, Some(&harness_file(1)))]);
    sync_work(&fix);
    let released = head_of(&fix.work);
    run(&fix.work, "git", &["tag", "v0.25.0", &released]);
    rival_files(&fix, &[(PHASE, Some(&harness_file(2)))]);
    sync_work(&fix);
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let unraised = ClaimPlan {
        version: "0.26.0",
        now: NOW,
        allow_existing_section: false,
        max_attempts: 5,
        source: &released,
        harness_base: Some(("0.25.0", &released)),
    };
    let quiet = ledger::reserve(&git, &git, &unraised, DATE).unwrap();
    assert!(
        !rolled_section(&fix.work, &quiet.commit).contains("Harness versions"),
        "no raised version, no announcement"
    );
    let plan = ClaimPlan {
        source: &source,
        ..unraised
    };
    let reserved = ledger::reserve(&git, &git, &plan, DATE).unwrap();
    let line = "- **Harness versions since v0.25.0: `alab claude harness 0.2`.** Each harness's \
                minor moves once per release in which it changed.";
    assert_eq!(
        rolled_section(&fix.work, &reserved.commit),
        format!("### Added\n\n- **Something real** — an entry.\n\n### Changed\n\n{line}")
    );
    let show = |path: &str| {
        run(
            &fix.work,
            "git",
            &["show", &format!("{}:{path}", reserved.commit)],
        )
    };
    let (rolled, ledger_text) = (show("CHANGELOG.md"), show("RELEASES.ledger"));
    for (text, why) in [
        (
            rolled.replace(&format!("### Changed\n\n{line}\n\n"), ""),
            "is not the published commit's window assembled and rolled",
        ),
        (
            rolled.replace("harness 0.2`", "harness 0.3`"),
            "is not the published commit's window assembled and rolled",
        ),
        (
            rolled.replace("since v0.25.0", "since v0.30.0"),
            "counts from v0.30.0, newer than the window's v0.25.0",
        ),
    ] {
        let forged = ledger::commit_with(
            &git,
            &source,
            &[("CHANGELOG.md", &text), ("RELEASES.ledger", &ledger_text)],
            &[],
            &[&source],
            "forged announcement",
        )
        .unwrap();
        let err = ledger::claim(
            &git,
            &git,
            &fix.work,
            &plan,
            &ledger::Reserved {
                commit: forged,
                ..reserved.clone()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(why), "{why}: {err}");
    }
    let c = ledger::claim(&git, &git, &fix.work, &plan, &reserved).unwrap();
    assert!(origin_file(&fix, "CHANGELOG.md").contains(line));
    assert_eq!(c.landed, c.commit);
}

/// THE RELEASE PAYS THE HARNESS DEBT IT CREATES, IN THE COMMIT THAT CREATES IT. Main moved past
/// the published commit: a peer changed the claude harness's source under the version the
/// release ships, and changed the muse harness's with its version raised. The landing merge is
/// what puts the release commit, and so its tag, in main's history; the landing check then
/// counts from that tag. The merge raises claude's version in its version file (keeping the
/// peer's code), leaves muse (already above the release's) and agy (unchanged) alone, and the
/// landing check on the merge, with the tag public, owes nothing.
/// NEGATIVE CONTROLS: before the release lands, main owes nothing (the debt is the release's
/// doing); and the same merge without the raised version file owes exactly claude's.
#[cfg(unix)]
#[test]
fn a_release_landing_on_moved_main_leaves_no_harness_debt() {
    const PHASE: &str = "crates/aterm-phase/src/phase.rs";
    const AGY: &str = "crates/aterm-phase/src/agy.rs";
    const MUSE: &str = "crates/aterm-phase/src/muse.rs";
    let manifest =
        |version: &str| format!("[workspace]\n[workspace.package]\nversion = \"{version}\"\n");
    let judged = |fix: &Fixture, head: &str| {
        aterm_verify::harness_debt::judge(
            &fix.work,
            head,
            &std::env::var_os("PATH").unwrap_or_default(),
        )
        .unwrap()
    };
    let fix = Fixture::new("harness-landing");
    rival_files(
        &fix,
        &[
            ("Cargo.toml", Some(&manifest("0.25.0"))),
            (PHASE, Some(&harness_file(1))),
            (AGY, Some(&harness_file(4))),
            (MUSE, Some(&harness_file(7))),
        ],
    );
    sync_work(&fix);
    let released = head_of(&fix.work);
    run(&fix.work, "git", &["tag", "v0.25.0", &released]);
    // The window v0.25.0 opened: claude changed and was raised once, and the version opened.
    rival_files(
        &fix,
        &[
            ("Cargo.toml", Some(&manifest("0.26.0"))),
            (PHASE, Some(&harness_file(2))),
        ],
    );
    sync_work(&fix);
    let source = head_of(&fix.work);
    // Main moves on while the release is built from `source`.
    let peer_phase = format!("{}fn peer() {{}}\n", harness_file(2));
    let peer_muse = format!("{}fn peer() {{}}\n", harness_file(8));
    rival_files(
        &fix,
        &[(PHASE, Some(&peer_phase)), (MUSE, Some(&peer_muse))],
    );
    run(&fix.work, "git", &["fetch", "-q", "origin", "main"]);
    let tip = run(&fix.work, "git", &["rev-parse", "origin/main"])
        .trim()
        .to_string();
    assert!(
        matches!(
            judged(&fix, &tip),
            aterm_verify::harness_debt::Judgement::Clear { ref version, .. } if version == "0.25.0"
        ),
        "before the release lands, main owes nothing since v0.25.0"
    );

    let git = Git::new(&fix.work);
    let plan = ClaimPlan {
        version: "0.26.0",
        now: NOW,
        allow_existing_section: false,
        max_attempts: 5,
        source: &source,
        harness_base: Some(("0.25.0", &released)),
    };
    let reserved = ledger::reserve(&git, &git, &plan, DATE).unwrap();
    let c = ledger::claim(&git, &git, &fix.work, &plan, &reserved).unwrap();
    assert_ne!(
        c.landed, c.commit,
        "main moved, so the release lands as a merge"
    );
    let raised: Vec<String> = c
        .harness
        .iter()
        .map(|o| {
            format!(
                "{} {}.{} -> {}.{}",
                o.harness.cli(),
                o.at.0,
                o.at.1,
                o.to.0,
                o.to.1
            )
        })
        .collect();
    assert_eq!(raised, ["claude 0.2 -> 0.3"]);
    assert_eq!(
        moved(&fix.work, &tip, &c.landed),
        format!("CHANGELOG.md RELEASES.ledger {PHASE}"),
        "the merge moves main's tree by the claim's two files and the one owed version file"
    );
    assert_eq!(
        origin_file(&fix, PHASE),
        format!("{}fn peer() {{}}\n", harness_file(3)),
        "the version is raised in main's own text, which keeps the peer's code"
    );
    assert_eq!(origin_file(&fix, MUSE), peer_muse);
    assert_eq!(origin_file(&fix, AGY), harness_file(4));
    assert!(
        run(&fix.work, "git", &["log", "-1", "--format=%B", &c.landed])
            .contains("written here: alab claude harness 0.3."),
        "the merge says what it raised"
    );

    // The release is public: its tag is on the release commit, which the merge put in main.
    run(&fix.work, "git", &["tag", "v0.26.0", &c.commit]);
    match judged(&fix, &c.landed) {
        aterm_verify::harness_debt::Judgement::Clear { version, baseline } => {
            assert_eq!(
                (version.as_str(), baseline.as_str()),
                ("0.26.0", c.commit.as_str())
            );
        }
        owed => panic!("the next landing owes a harness version: {owed:?}"),
    }

    // The same merge as the claim made it before it raised the versions: main's tree, the
    // rolled changelog and the ledger line, and nothing else.
    let landed_file =
        |path: &str| run(&fix.work, "git", &["show", &format!("{}:{path}", c.landed)]);
    let bare = ledger::commit_with(
        &git,
        &tip,
        &[
            ("CHANGELOG.md", &landed_file("CHANGELOG.md")),
            ("RELEASES.ledger", &landed_file("RELEASES.ledger")),
        ],
        &[],
        &[&tip, &c.commit],
        "a landing that raises nothing",
    )
    .unwrap();
    match judged(&fix, &bare) {
        aterm_verify::harness_debt::Judgement::Owed { debts, .. } => {
            let lines: Vec<String> = debts.iter().map(|debt| debt.line()).collect();
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert!(
                lines[0].starts_with("alab claude harness 0.2 ") && lines[0].contains(PHASE),
                "{lines:?}"
            );
        }
        clear => panic!("a landing that raises nothing owed nothing: {clear:?}"),
    }
}

/// A release that main fast-forwards to changes no harness version: nothing on main is past
/// the release commit.
#[cfg(unix)]
#[test]
fn a_fast_forward_landing_raises_no_harness_version() {
    const PHASE: &str = "crates/aterm-phase/src/phase.rs";
    let fix = Fixture::new("harness-fast-forward");
    rival_files(&fix, &[(PHASE, Some(&harness_file(1)))]);
    sync_work(&fix);
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let (res, _) = do_claim(&git, &fix.work, "0.26.0", false);
    let c = res.unwrap();
    assert_eq!(c.landed, c.commit);
    assert!(c.harness.is_empty());
    assert_eq!(
        moved(&fix.work, &source, &c.landed),
        "CHANGELOG.md RELEASES.ledger"
    );
}
