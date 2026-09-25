// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bare-repo race proofs for the build-number ledger claim (release spec §2),
//! and for where it lands: the release commit on the published commit, main
//! taking it as a fast-forward or a merge (owner ruling R2, 2026-09-23).
//!
//! REAL git against a local bare-repo origin — no mocks of git semantics: the
//! claim's fast-forward push either is or is not a compare-and-swap, and only
//! git can say. The injectable [`ledger::GitRunner`] seam is used solely to
//! land a rival's push at the exact moment between the claimant's fetch and
//! its own push — the race window itself, made deterministic.

// The release crate is a binary on purpose (the spec's §9 file plan has no
// lib.rs — nothing outside the cut tool may link this code), so the
// integration tests compile the modules under test directly into the test
// crate. `ledger` and `changelog` cross-reference through `crate::`, hence
// both are mounted even though only `ledger` is exercised here.
#[path = "../src/changelog.rs"]
#[allow(dead_code)] // mounted only because ledger cross-references crate::changelog
mod changelog;
#[path = "../src/ledger.rs"]
#[allow(dead_code)] // test mount: the claim/parse surface is exercised, not every helper
mod ledger;

use std::cell::{Cell, RefCell};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ledger::{ClaimPlan, GitCli, GitRunner, LEDGER_FLOOR, RunOut};

/// One frozen "clock" for every test: claims take `now` as an input (spec §2
/// computes `n = max(last + 1, unix_now)`), so freezing it makes every minted
/// number an exact-equality assertion instead of a range check.
const NOW: u64 = 1_790_000_000;

/// The committed seed (spec §2) re-spelled in the ONE version scheme — the
/// whole-file equality asserts below hang off these exact bytes. The real
/// `RELEASES.ledger` keeps its pre-cut-over two-component lines verbatim
/// (append-only history is never edited); `parse_takes_last_record_and_names_
/// malformed_lines` covers that mixed-history file directly.
const SEED_LEDGER: &str = "# aterm release ledger — append-only; one line per claimed build number. Never edit or reuse.\n1783354739 0.25.0\n";

/// Minimal changelog: the claim rolls it on both sides, and its "cut elsewhere"
/// abort reads origin/main:CHANGELOG.md.
const SEED_CHANGELOG: &str = "# Changelog\n\n## [Unreleased]\n\n### Added\n- **Something real** — an entry.\n\n## [0.25.0] - 2026-07-06\n\n- old notes.\n";

/// The roll date every claim here uses.
const DATE: &str = "2026-09-23";

// --------------------------------------------------------------------------
// fixture: bare origin + a rival clone (the "other machine") + the claimant's
// working clone
// --------------------------------------------------------------------------

struct Fixture {
    root: PathBuf,
    origin: PathBuf,
    rival: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        // Distinct per test name + pid: tests run in parallel threads.
        let root = env::temp_dir().join(format!("aterm-ledger-race-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let origin = root.join("origin.git");
        run(
            &root,
            "git",
            &["init", "-q", "--bare", "-b", "main", "origin.git"],
        );
        let rival = root.join("rival");
        run(
            &root,
            "git",
            &["clone", "-q", origin.to_str().unwrap(), "rival"],
        );
        config_identity(&rival);
        // A clone of an EMPTY repo names its unborn branch from local config
        // (init.defaultBranch), which this machine may set to anything —
        // force main so the seed push matches the claim's hardcoded refs.
        run(&rival, "git", &["symbolic-ref", "HEAD", "refs/heads/main"]);
        fs::write(rival.join("RELEASES.ledger"), SEED_LEDGER).unwrap();
        fs::write(rival.join("CHANGELOG.md"), SEED_CHANGELOG).unwrap();
        run(&rival, "git", &["add", "-A"]);
        run(&rival, "git", &["commit", "-q", "-m", "seed"]);
        run(&rival, "git", &["push", "-q", "-u", "origin", "main"]);
        let work = root.join("work");
        run(
            &root,
            "git",
            &["clone", "-q", origin.to_str().unwrap(), "work"],
        );
        config_identity(&work);
        Fixture {
            root,
            origin,
            rival,
            work,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Run a command, assert success, return stdout. Setup/assertion plumbing
/// only — the code under test never goes through here.
fn run(cwd: &Path, prog: &str, args: &[&str]) -> String {
    let out = Command::new(prog)
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
    run(
        repo,
        "git",
        &["config", "user.email", "ship-test@aterm.invalid"],
    );
    run(repo, "git", &["config", "user.name", "aterm ship test"]);
    run(repo, "git", &["config", "commit.gpgsign", "false"]);
    run(repo, "git", &["config", "tag.gpgsign", "false"]);
}

/// Read a file straight out of the BARE origin — the ground truth every
/// preservation assert compares against (never a clone's view of it).
fn origin_file(fix: &Fixture, path: &str) -> String {
    run(&fix.origin, "git", &["show", &format!("main:{path}")])
}

fn origin_subjects(fix: &Fixture) -> String {
    run(&fix.origin, "git", &["log", "--format=%s", "main"])
}

/// The rival ("another machine") appends a ledger line and pushes — the write
/// a real race winner performs.
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

/// The rival lands a FULL claim of the version (ledger line + rolled
/// changelog section) — the "cut elsewhere" shape the retry must refuse.
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

// --------------------------------------------------------------------------
// the race hook: a GitRunner that fires a callback right before each push
// --------------------------------------------------------------------------

/// Wraps the production [`GitCli`] and invokes `on_push(attempt_no)` before
/// forwarding each `push` — everything else passes straight through to real
/// git. This is the deterministic stand-in for "the rival's push lands first".
struct HookRunner<F: FnMut(u32)> {
    inner: GitCli,
    pushes: Cell<u32>,
    on_push: RefCell<F>,
}

impl<F: FnMut(u32)> HookRunner<F> {
    fn new(work: &Path, on_push: F) -> Self {
        HookRunner {
            inner: GitCli::new(work),
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
}

/// The rival pushes a peer's CODE and a changelog entry after the publish — the
/// shape that must neither block a cut nor leak into it.
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

/// Drive one claim of `version` from the published commit = the work clone's
/// HEAD, with the real changelog rolls. Returns the outcome and how many
/// attempts the claim made (each builds both commits afresh).
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
    let res = ledger::claim(git, work, &plan, &|published, main| {
        attempts.set(attempts.get() + 1);
        changelog::claim_changelogs(published, main, version, DATE)
    });
    (res, attempts.get())
}

/// A lost/aborted claim must leave NOTHING behind (spec §2: "abort with tree
/// reset clean — nothing burned"): the checkout clean and back on the published
/// commit, and no release commit on origin.
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

// --------------------------------------------------------------------------
// parser + number rule (pure)
// --------------------------------------------------------------------------

#[test]
fn parse_takes_last_record_and_names_malformed_lines() {
    let good = "# comment\n1783354739 0.25.0\n1783918101 0.26.0\n";
    let t = ledger::tail(good).unwrap();
    assert_eq!((t.build, t.version.as_str()), (1_783_918_101, "0.26.0"));

    // The REAL post-cut-over file: pre-cut-over history keeps its retired
    // two-component versions verbatim (append-only, never edited) and a
    // three-component line is appended on top. The ledger grammar is column
    // 1 — the build number — so column 2's shape is never parsed, and the
    // mixed file must read back with its exact three-component tail.
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

    // Every malformed shape aborts and names its 1-based line — blank lines
    // included: the ledger is append-only, so ANY unparseable edit is treated
    // as corruption, never skipped over.
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
    // Below the v0.25 seed floor entirely → refuse to mint.
    assert!(ledger::next_build(5, 10).is_err());
}

/// One version scheme: canonical `MAJOR.MINOR.PATCH`. The retired
/// two-component spelling is refused like any other malformed shape — a claim
/// may never mint a build number for a version no client can select.
#[test]
fn claim_rejects_non_canonical_three_component_versions() {
    // Shape-checked before any git runs, so a dead path is fine as the repo.
    let git = GitCli::new("/nonexistent");
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
        let err = ledger::claim(&git, Path::new("/nonexistent"), &plan, &|s, m| {
            Ok((s.to_string(), m.to_string()))
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("MAJOR.MINOR.PATCH"), "{bad:?} → {err}");
    }
}

// --------------------------------------------------------------------------
// claim protocol against real git
// --------------------------------------------------------------------------

#[test]
fn happy_path_claims_appends_and_verifies() {
    let fix = Fixture::new("happy");
    let source = head_of(&fix.work);
    let git = GitCli::new(&fix.work);
    let (res, attempts) = do_claim(&git, &fix.work, "0.26.0", false);
    let c = res.unwrap();
    assert_eq!(c.build, NOW);
    assert_eq!(c.ledger_line, "1790000000 0.26.0");
    assert_eq!(attempts, 1);
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
    // The roll landed in the SAME single commit, and only the claim's two files moved.
    assert!(origin_file(&fix, "CHANGELOG.md").contains(&format!(
        "## [Unreleased]\n\n## [0.26.0] - {DATE}\n\n### Added\n- **Something real**"
    )));
    assert_eq!(
        moved(&fix.work, &source, &c.commit),
        "CHANGELOG.md RELEASES.ledger"
    );
}

/// THE RULING (R2): peers pushed code after the publish. The release commit is
/// still the published commit plus the claim's two files; main takes it by a
/// merge whose tree is its own tip plus the same two files; the peer's code and
/// its unreleased changelog entry stay on main, out of the release.
#[test]
fn a_published_commit_behind_main_is_released_and_merged() {
    let fix = Fixture::new("behind");
    let source = head_of(&fix.work);
    rival_peer_push(&fix);
    run(&fix.work, "git", &["fetch", "-q", "origin", "main"]);
    let tip = run(&fix.work, "git", &["rev-parse", "origin/main"])
        .trim()
        .to_string();
    let git = GitCli::new(&fix.work);
    let (res, attempts) = do_claim(&git, &fix.work, "0.26.0", false);
    let c = res.expect("a peer's push is not a refusal");
    assert_eq!(attempts, 1);

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
    // claim's two files against the tip — the shape pre-push admits as a claim.
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

/// A failure while BUILDING main's commit — after the release commit exists,
/// before anything is pushed — leaves nothing: the checkout is back on the
/// published commit, clean, and origin never saw a claim. (Without the reset the
/// unpushed release commit would be left checked out.)
#[test]
fn a_failure_building_mains_commit_leaves_the_checkout_on_the_published_commit() {
    struct FailingIndex(GitCli);
    impl GitRunner for FailingIndex {
        fn git(&self, args: &[&str]) -> ledger::Result<RunOut> {
            if args.first() == Some(&"update-index") {
                return Ok(RunOut {
                    status: 128,
                    stdout: Vec::new(),
                    stderr: b"fatal: injected update-index failure".to_vec(),
                });
            }
            self.0.git(args)
        }
    }
    let fix = Fixture::new("landing-fails");
    let source = head_of(&fix.work);
    rival_peer_push(&fix);
    let (res, attempts) = do_claim(
        &FailingIndex(GitCli::new(&fix.work)),
        &fix.work,
        "0.26.0",
        false,
    );
    let err = res.unwrap_err().to_string();
    assert!(err.contains("injected update-index failure"), "{err}");
    assert_eq!(attempts, 1);
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
    let git = GitCli::new(&fix.work);
    let plan = ClaimPlan {
        version: "0.26.0",
        now: NOW,
        allow_existing_section: false,
        max_attempts: 5,
        source: &tip,
    };
    let err = ledger::claim(&git, &fix.work, &plan, &|_, _| panic!("nothing is rolled"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("is not the published commit"), "{err}");
    assert!(!origin_subjects(&fix).contains("release: v0.26.0"));
}

#[test]
fn cas_loser_rebuilds_from_origin_with_higher_n() {
    let fix = Fixture::new("cas");
    let source = head_of(&fix.work);
    let winner = "1790000123 0.99.0"; // > the loser's first n → forces a bigger retry n
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_append(&fix, winner);
        }
    });
    let (res, attempts) = do_claim(&runner, &fix.work, "0.26.0", false);
    let c = res.unwrap();
    // Loser retried with a strictly-higher number (winner's tail + 1).
    assert_eq!(c.build, 1_790_000_124);
    assert_eq!(attempts, 2);
    assert_eq!(runner.pushes.get(), 2);
    // THE core preservation proof, whole file byte-exact: seed, then the
    // winner's line untouched, then ours as the tail — the rebuild from origin's
    // blobs can never clobber the winner (spec decision 3; the reset-soft design
    // verifiably did).
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}{winner}\n1790000124 0.26.0\n")
    );
    // The winner moved main past the published commit, so the retry lands by a
    // merge — and the release commit is still the published commit + two files.
    assert_ne!(c.landed, c.commit);
    assert_eq!(parents(&fix.work, &c.commit), vec![source.clone()]);
    assert!(origin_subjects(&fix).contains("release: v0.26.0 (build 1790000124)"));
}

#[test]
fn cas_loser_may_remint_same_n_while_clock_leads() {
    let fix = Fixture::new("cas-clock");
    // Winner's number is BELOW the loser's clock-derived n: the retry lands
    // max(tail+1, now) = now again — same number, still strictly above the
    // new tail, and never published anywhere in between. Documents the
    // max(…, now) subtlety of spec §2.
    let winner = "1789999000 0.98.0";
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_append(&fix, winner);
        }
    });
    let (res, attempts) = do_claim(&runner, &fix.work, "0.26.0", false);
    let c = res.unwrap();
    assert_eq!(c.build, NOW);
    assert_eq!(attempts, 2);
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}{winner}\n1790000000 0.26.0\n")
    );
}

#[test]
fn retry_cap_aborts_clean_with_nothing_burned() {
    let fix = Fixture::new("cap");
    let source = head_of(&fix.work);
    // A rival lands a fresh push before EVERY attempt — the claimant must
    // lose all 5 rounds, then stop cleanly.
    let runner = HookRunner::new(&fix.work, |attempt| {
        rival_append(&fix, &format!("17900002{attempt:02} 0.9{attempt}.0"));
    });
    let (res, attempts) = do_claim(&runner, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("5 times"), "{err}");
    assert_eq!(runner.pushes.get(), 5);
    assert_eq!(attempts, 5);
    assert_clean_at(&fix, &source);
    // Every winner line survived, byte-exact, in order.
    let mut expected = SEED_LEDGER.to_string();
    for a in 1..=5u32 {
        expected.push_str(&format!("17900002{a:02} 0.9{a}.0\n"));
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
    // The recut path (spec §5): the version's section ALREADY sits on origin
    // (rolled by the earlier wedged claim); a fresh claim for the same version
    // must ride past it instead of aborting, and leave main's roll as it is.
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_land_section(&fix, "1790000300 0.26.0", "0.26.0");
        }
    });
    let (res, _) = do_claim(&runner, &fix.work, "0.26.0", true);
    let c = res.unwrap();
    assert_eq!(c.build, 1_790_000_301); // fresh, strictly above the wedged claim
    // The wedged cut's artifacts survive byte-exact next to the new claim.
    let remote = origin_file(&fix, "RELEASES.ledger");
    assert!(remote.contains("1790000300 0.26.0\n"));
    assert!(remote.ends_with("1790000301 0.26.0\n"));
    let main_changelog = origin_file(&fix, "CHANGELOG.md");
    assert!(main_changelog.contains("## [0.26.0] - 2026-01-01"));
    assert_eq!(
        main_changelog.matches("## [0.26.0]").count(),
        1,
        "main is never rolled twice: {main_changelog}"
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
    let git = GitCli::new(&fix.work);
    let (res, attempts) = do_claim(&git, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("line 3") && err.contains("not a u64"), "{err}");
    // Parse failure precedes the roll, the commit and the push — nothing happened.
    assert_eq!(attempts, 0);
    assert_clean_at(&fix, &source);
}

#[test]
fn offline_fetch_fails_closed() {
    let fix = Fixture::new("offline");
    let gone = fix.root.join("nonexistent.git");
    run(
        &fix.work,
        "git",
        &["remote", "set-url", "origin", gone.to_str().unwrap()],
    );
    let git = GitCli::new(&fix.work);
    let (res, attempts) = do_claim(&git, &fix.work, "0.26.0", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("no offline cuts"), "{err}");
    assert_eq!(attempts, 0, "no offline claim may roll anything");
}
