// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bare-repo race proofs for the build-number ledger claim and where it lands: the release
//! commit on the published commit, main taking it as a fast-forward or a merge. The claim
//! lands a reservation ([`ledger::reserve`]): number and release commit are fixed before the
//! build, so a lost race never re-numbers, and a tail that reached the reservation refuses
//! with nothing burned.
//!
//! Real git against a local bare origin: whether the claim's fast-forward push is a
//! compare-and-swap only git can say. The [`ledger::GitRunner`] seam only lands a rival's push
//! between the claimant's fetch and its own push, making the race window deterministic; the
//! runner underneath is always [`ledger::testing::Git`].

use crate::{changelog, ledger};

use aterm_uds::spawnfd::command;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};

use ledger::testing::Git;
use ledger::{ClaimPlan, GitRunner, LEDGER_FLOOR, RunOut};

/// One frozen clock: claims take `now` as an input (`n = max(last + 1, now)`), so every minted
/// number is an exact-equality assertion.
const NOW: u64 = 1_790_000_000;

/// The committed seed in the three-component scheme; the whole-file equality asserts hang off
/// these bytes. The real `RELEASES.ledger` keeps its two-component history lines verbatim;
/// `parse_takes_last_record_and_names_malformed_lines` covers that mixed file.
const SEED_LEDGER: &str = "# aterm release ledger — append-only; one line per claimed build number. Never edit or reuse.\n1783354739 0.25.0\n";

/// Minimal changelog: the claim rolls it on both sides, and its "cut elsewhere"
/// abort reads origin/main:CHANGELOG.md.
const SEED_CHANGELOG: &str = "# Changelog\n\n## [Unreleased]\n\n### Added\n- **Something real** — an entry.\n\n## [0.25.0] - 2026-07-06\n\n- old notes.\n";

/// The roll date every claim here uses.
const DATE: &str = "2026-09-23";

// --- fixture: bare origin, a rival clone (the other machine), the claimant's clone ---

struct Fixture {
    root: aterm_tempfile::TempDir,
    origin: PathBuf,
    rival: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-ledger-race-{name}-"))
            .tempdir()
            .unwrap();
        let root = dir.path();
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
        // A clone of an empty repo names its unborn branch from local config
        // (init.defaultBranch): force main so the seed push matches the claim's refs.
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
        Fixture {
            root: dir,
            origin,
            rival,
            work,
        }
    }
}

/// Run a command, assert success, return stdout. Setup only: the code under test never comes
/// through here.
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

/// Fixture repos must commit regardless of this machine's global git config:
/// pin identity and disable signing (a gpg prompt would hang the test).
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

/// A file read from the bare origin, the ground truth every preservation assert compares with.
fn origin_file(fix: &Fixture, path: &str) -> String {
    run(&fix.origin, "git", &["show", &format!("main:{path}")])
}

fn origin_subjects(fix: &Fixture) -> String {
    run(&fix.origin, "git", &["log", "--format=%s", "main"])
}

/// The rival (another machine) appends a ledger line and pushes, as a real race winner does.
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

/// The rival lands a full claim of the version (ledger line and rolled section): the "cut
/// elsewhere" shape the retry must refuse.
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

// --- the race hook: a GitRunner that fires a callback before each push -----

/// Wraps [`Git`] and calls `on_push(attempt_no)` before forwarding each `push`: the
/// deterministic stand-in for "the rival's push lands first".
struct HookRunner<F: FnMut(u32)> {
    inner: Git,
    pushes: Cell<u32>,
    on_push: RefCell<F>,
}

impl<F: FnMut(u32)> HookRunner<F> {
    fn new(work: &Path, on_push: F) -> Self {
        HookRunner {
            inner: Git::new(work),
            pushes: Cell::new(0),
            on_push: RefCell::new(on_push),
        }
    }
}

impl<F: FnMut(u32)> GitRunner for HookRunner<F> {
    fn git(&self, args: &[&str]) -> ledger::Result<RunOut> {
        if args.first() == Some(&"push") {
            let n = self.pushes.get() + 1;
            self.pushes.set(n);
            (self.on_push.borrow_mut())(n);
        }
        self.inner.git(args)
    }

    fn git_input(&self, args: &[&str], input: &[u8]) -> ledger::Result<RunOut> {
        self.inner.git_input(args, input)
    }
}

/// The rival pushes a peer's code and a changelog entry after the publish: the shape that must
/// neither block a cut nor leak into it.
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

/// Drive one release of `version` from the published commit = the work clone's
/// HEAD, with the real changelog rolls: reserve its identity, then claim it.
/// Returns the outcome and how many times the rolls were asked (once by the
/// reservation, once per landing built on a tip main moved to).
fn do_claim(
    git: &dyn GitRunner,
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
    };
    let attempts = Cell::new(0u32);
    let rolls = |published: &str, main: &str| {
        attempts.set(attempts.get() + 1);
        changelog::claim_changelogs(published, main, version, DATE)
    };
    let res = ledger::reserve(git, &plan, &rolls)
        .and_then(|reserved| ledger::claim(git, work, &plan, &reserved, &rolls));
    (res, attempts.get())
}

/// A lost or aborted claim leaves nothing behind: the checkout clean on the published commit,
/// no release commit on origin.
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

// --- parser + number rule (pure) -------------------------------------------

#[test]
fn parse_takes_last_record_and_names_malformed_lines() {
    let good = "# comment\n1783354739 0.25.0\n1783918101 0.26.0\n";
    let t = ledger::tail(good).unwrap();
    assert_eq!((t.build, t.version.as_str()), (1_783_918_101, "0.26.0"));

    // The real file keeps its two-component history lines verbatim beneath three-component
    // ones. The grammar parses only column 1, the build number, so the mixed file reads back
    // with its exact tail.
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

    // Every malformed shape aborts naming its 1-based line, blank lines included: in an
    // append-only file any unparseable edit is corruption, never skipped.
    let cases: [(&str, usize); 4] = [
        ("# c\n1783354739 0.25.0\n\n1783918101 0.26.0\n", 3), // blank line
        ("1783354739\n", 1),                                  // one field
        ("1783354739 0.25.0 extra\n", 1),                     // three fields
        ("abc 0.25.0\n", 1),                                  // non-numeric build
    ];
    for (text, line) in cases {
        let err = ledger::tail(text).unwrap_err().to_string();
        assert!(err.contains(&format!("line {line}")), "{text:?} → {err}");
    }

    // Comments-only = gutted seed = error, not "start from zero".
    assert!(ledger::tail("# only comments\n").is_err());
}

#[test]
fn next_build_is_monotonic_and_floored() {
    // Normal steady state: the clock leads the tail.
    assert_eq!(ledger::next_build(LEDGER_FLOOR, NOW).unwrap(), NOW);
    // Backwards clock: tail + 1 keeps monotonicity.
    assert_eq!(ledger::next_build(NOW + 5, NOW).unwrap(), NOW + 6);
    // Exactly at the floor is still above it by +1.
    assert_eq!(
        ledger::next_build(LEDGER_FLOOR, 0).unwrap(),
        LEDGER_FLOOR + 1
    );
    // Below the seed floor: refuse to mint.
    assert!(ledger::next_build(5, 10).is_err());
}

/// One version scheme, canonical `MAJOR.MINOR.PATCH`: a claim never mints a build number for a
/// version no client can select.
#[test]
fn claim_rejects_non_canonical_three_component_versions() {
    // Shape-checked before any git runs, so a dead path is fine as the repo.
    let git = Git::new("/nonexistent");
    let reserved = ledger::Reserved {
        build: NOW,
        commit: "0000000000000000000000000000000000000002".into(),
        line: format!("{NOW} 0.26.0"),
    };
    for bad in [
        "0.26",     // the retired two-component scheme
        "v0.26.1",  // tag spelling, not a version
        "0.2.x",    // non-numeric component
        "0.26.1.2", // four components
        "01.2.3",   // leading zero
        "0..1",     // empty component
        "26",       // one component
        "",
    ] {
        let plan = ClaimPlan {
            version: bad,
            now: NOW,
            allow_existing_section: false,
            max_attempts: 5,
            source: "0000000000000000000000000000000000000001",
        };
        let rolls = |s: &str, m: &str| Ok((s.to_string(), m.to_string()));
        for err in [
            ledger::reserve(&git, &plan, &rolls)
                .unwrap_err()
                .to_string(),
            ledger::claim(&git, Path::new("/nonexistent"), &plan, &reserved, &rolls)
                .unwrap_err()
                .to_string(),
        ] {
            assert!(err.contains("MAJOR.MINOR.PATCH"), "{bad:?} → {err}");
        }
    }
}

// --- claim protocol against real git ---------------------------------------

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
    // Main had not moved past the published commit: it takes the release commit
    // itself, as a fast-forward — one commit, the published commit its parent.
    assert_eq!(c.landed, c.commit);
    assert_eq!(parents(&fix.work, &c.commit), vec![source.clone()]);
    assert_eq!(c.commit, head_of(&fix.work), "the checkout sits on it");
    assert!(origin_subjects(&fix).contains("release: v0.26.0 (build 1790000000)"));
    // Whole-file byte equality: seed untouched, our line appended.
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1790000000 0.26.0\n")
    );
    // The roll landed in the same commit, and only the claim's two files moved.
    assert!(origin_file(&fix, "CHANGELOG.md").contains(&format!(
        "## [Unreleased]\n\n## [0.26.0] - {DATE}\n\n### Added\n- **Something real**"
    )));
    assert_eq!(
        moved(&fix.work, &source, &c.commit),
        "CHANGELOG.md RELEASES.ledger"
    );
}

/// Peers pushed code after the publish: the release commit is still the published commit plus
/// the claim's two files, and main takes it by a merge whose tree is its tip plus the same two
/// files. The peer's code and unreleased changelog entry stay on main, out of the release.
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

    // The release commit: the published commit + CHANGELOG.md + RELEASES.ledger.
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

    // Main's commit: a merge of the release commit onto the tip, moving only the
    // claim's two files against the tip — bookkeeping, no code.
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

    // The notes that shipped are the published commit's; the peer's entry is
    // still unreleased on main, under its heading.
    let main_changelog = origin_file(&fix, "CHANGELOG.md");
    assert!(
        main_changelog.contains(&format!(
            "## [Unreleased]\n\n### Added\n\n- **A peer's entry** — not in the published commit.\n\n## [0.26.0] - {DATE}\n\n### Added\n- **Something real**"
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

/// A failure building main's commit, after the release commit exists and before any push,
/// leaves the checkout clean on the published commit (the claim builds from objects and moves
/// it only once the push is verified), and origin never saw a claim.
#[test]
fn a_failure_building_mains_commit_leaves_the_checkout_on_the_published_commit() {
    struct FailingLanding(Git);
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
    let (res, attempts) = do_claim(
        &FailingLanding(Git::new(&fix.work)),
        &fix.work,
        "0.26.0",
        false,
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("injected commit-tree failure"), "{err}");
    assert_eq!(attempts, 2, "the reservation's roll, then the landing's");
    assert_clean_at(&fix, &source);
}

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
    };
    // The reservation is made from objects, wherever the checkout sits; the claim
    // refuses a checkout that is not on the published commit, before anything.
    let rolls = |s: &str, m: &str| changelog::claim_changelogs(s, m, "0.26.0", DATE);
    let reserved = ledger::reserve(&git, &plan, &rolls).expect("reserved from objects");
    let err = ledger::claim(&git, &fix.work, &plan, &reserved, &|_, _| {
        panic!("nothing is rolled")
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("is not the published commit"), "{err}");
    assert!(!origin_subjects(&fix).contains("release: v0.26.0"));
}

/// Build once: the reserved number is the one its bytes carry, so a lost race whose winner took
/// a number at or past it is never re-numbered. The claim refuses with nothing burned and names
/// the re-run that reserves anew.
#[test]
fn a_reservation_the_tail_reached_refuses_and_burns_nothing() {
    let fix = Fixture::new("stale");
    let source = head_of(&fix.work);
    let winner = "1790000123 0.99.0"; // past the reserved n
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_append(&fix, winner);
        }
    });
    let (res, _) = do_claim(&runner, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("at or past the reserved build 1790000000")
            && err.contains("nothing was burned")
            && err.contains("reserves a fresh number"),
        "{err}"
    );
    assert_eq!(runner.pushes.get(), 1, "never a push under another number");
    assert_clean_at(&fix, &source);
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}{winner}\n"),
        "the winner's line, and nothing of ours"
    );
}

/// The re-run reserves anew from the winner's tail: a fresh, strictly higher
/// number and a release commit of its own, landed by a merge.
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

#[test]
fn a_lost_race_below_the_reservation_lands_it_with_its_own_number() {
    let fix = Fixture::new("cas-clock");
    // The winner's number is below the reservation's clock-derived n: the retry rebuilds only
    // the landing on the winner's tip and lands the same number, still above the new tail.
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

#[test]
fn retry_cap_aborts_clean_with_nothing_burned() {
    let fix = Fixture::new("cap");
    let source = head_of(&fix.work);
    // A rival lands a push below the reserved number before every attempt: all 5 rounds are
    // lost, then the claim stops cleanly.
    let runner = HookRunner::new(&fix.work, |attempt| {
        rival_append(&fix, &format!("17899990{attempt:02} 0.9{attempt}.0"));
    });
    let (res, attempts) = do_claim(&runner, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("5 times"), "{err}");
    assert_eq!(runner.pushes.get(), 5);
    assert_eq!(attempts, 5, "the reservation's roll, then four landings");
    assert_clean_at(&fix, &source);
    // Every winner line survived, byte-exact, in order.
    let mut expected = SEED_LEDGER.to_string();
    for a in 1..=5u32 {
        expected.push_str(&format!("17899990{a:02} 0.9{a}.0\n"));
    }
    assert_eq!(origin_file(&fix, "RELEASES.ledger"), expected);
}

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

#[test]
fn recut_flag_claims_fresh_n_past_existing_section() {
    let fix = Fixture::new("recut");
    // Recut: the version's section already sits on origin, rolled by a wedged claim with a lower
    // number; a fresh claim rides past it instead of aborting and leaves main's roll as it is.
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_land_section(&fix, "1789999300 0.26.0", "0.26.0");
        }
    });
    let (res, _) = do_claim(&runner, &fix.work, "0.26.0", true);
    let c = res.unwrap();
    assert_eq!(c.build, NOW); // the reservation's, strictly above the wedged claim
    // The wedged cut's artifacts survive byte-exact next to the new claim.
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

/// A recut from main's tip: an abandoned claim rolled the section onto main, so the published
/// commit already carries `## [0.26.0]`. The reservation takes that changelog as it stands and
/// moves only the ledger, and the claim lands it as a fast-forward.
#[test]
fn a_recut_from_a_source_carrying_the_section_moves_only_the_ledger() {
    let fix = Fixture::new("recut-tip");
    rival_land_section(&fix, "1789999300 0.26.0", "0.26.0");
    run(&fix.work, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.work, "git", &["reset", "-q", "--hard", "origin/main"]);
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
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1789999300 0.26.0\n1790000000 0.26.0\n")
    );
}

/// The claim takes only the release commit its reservation writes: one whose
/// changelog is not the published commit's roll, or that moves any other file, is
/// refused before anything is pushed — nothing claimed.
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
    };
    let rolls = |s: &str, m: &str| changelog::claim_changelogs(s, m, "0.26.0", DATE);
    let reserved = ledger::reserve(&git, &plan, &rolls).expect("reserved");
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
            "is not the published commit's rolled",
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
    ] {
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let forged = ledger::commit_with(&git, &source, &files, &[&source], "forged").unwrap();
        let forged = ledger::Reserved {
            commit: forged,
            ..reserved.clone()
        };
        let err = ledger::claim(&git, &fix.work, &plan, &forged, &rolls)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(why) && err.contains("nothing was claimed"),
            "{why}: {err}"
        );
    }
    // A stray file beside the claim's two: the release commit's tree carries it.
    fs::write(fix.work.join("stray.rs"), "fn stray() {}\n").unwrap();
    run(&fix.work, "git", &["add", "stray.rs"]);
    run(&fix.work, "git", &["commit", "-q", "-m", "stray"]);
    let with_stray = head_of(&fix.work);
    run(&fix.work, "git", &["reset", "-q", "--hard", &source]);
    let forged = ledger::commit_with(
        &git,
        &with_stray,
        &[("CHANGELOG.md", &rolled), ("RELEASES.ledger", &ledger_text)],
        &[&source],
        "forged",
    )
    .unwrap();
    let err = ledger::claim(
        &git,
        &fix.work,
        &plan,
        &ledger::Reserved {
            commit: forged,
            ..reserved
        },
        &rolls,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("moves [\"stray.rs\"]"), "{err}");
    assert_clean_at(&fix, &source);
}

/// A claim that landed and then failed a post-push step (putting the cut tree on the release
/// commit) holds no cut journal, so the refusal names the re-run, not `--abandon`: the number
/// may be spent, nothing was signed or published, and the re-run lands a fresh number above it.
#[test]
fn a_claim_that_landed_unjournaled_names_the_re_run_not_an_abandon() {
    struct FailingCheckout(Git);
    impl GitRunner for FailingCheckout {
        fn git(&self, args: &[&str]) -> ledger::Result<RunOut> {
            if args.first() == Some(&"checkout") {
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
    let (res, _) = do_claim(
        &FailingCheckout(Git::new(&fix.work)),
        &fix.work,
        "0.26.0",
        false,
    );
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("injected checkout failure")
            && err.contains("may have LANDED")
            && err.contains("nothing to withdraw")
            && err.contains("reserves a fresh number")
            && !err.contains("--abandon"),
        "{err}"
    );
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1790000000 0.26.0\n"),
        "the claim landed"
    );
    // The re-run: build 1790000000 is spent; a fresh reservation lands above it, as
    // a recut of the version main now carries.
    let (res, _) = do_claim(&Git::new(&fix.work), &fix.work, "0.26.0", true);
    let c = res.expect("the re-run claims anew");
    assert_eq!(c.build, 1_790_000_001);
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1790000000 0.26.0\n1790000001 0.26.0\n")
    );
}

#[test]
fn malformed_remote_ledger_aborts_before_any_commit() {
    let fix = Fixture::new("malformed");
    // Corrupt the ledger on origin (line 3 non-numeric), then bring the claimant
    // to the tip so the published commit is the corrupt one's.
    rival_append(&fix, "not-a-number 0.26.0");
    run(&fix.work, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.work, "git", &["reset", "-q", "--hard", "origin/main"]);
    let source = head_of(&fix.work);
    let git = Git::new(&fix.work);
    let (res, attempts) = do_claim(&git, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("line 3") && err.contains("not a u64"), "{err}");
    // The parse precedes the roll, the reservation and the push.
    assert_eq!(attempts, 0);
    assert_clean_at(&fix, &source);
}

#[test]
fn offline_fetch_fails_closed() {
    let fix = Fixture::new("offline");
    let gone = fix.root.path().join("nonexistent.git");
    run(
        &fix.work,
        "git",
        &["remote", "set-url", "origin", gone.to_str().unwrap()],
    );
    let git = Git::new(&fix.work);
    let (res, attempts) = do_claim(&git, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("no offline cuts"), "{err}");
    assert_eq!(attempts, 0, "no offline claim may roll anything");
}
