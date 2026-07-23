// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bare-repo race proofs for the build-number ledger claim (release spec §2).
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

use ledger::{ClaimPlan, Error, GitCli, GitRunner, LEDGER_FLOOR, RunOut};

/// One frozen "clock" for every test: claims take `now` as an input (spec §2
/// computes `n = max(last + 1, unix_now)`), so freezing it makes every minted
/// number an exact-equality assertion instead of a range check.
const NOW: u64 = 1_790_000_000;

/// Byte-exact copy of the committed seed (spec §2) — the whole-file equality
/// asserts below hang off these exact bytes.
const SEED_LEDGER: &str = "# aterm release ledger — append-only; one line per claimed build number. Never edit or reuse.\n1783354739 0.25\n";

/// Minimal changelog: the claim's "cut elsewhere" abort reads
/// origin/main:CHANGELOG.md, so the fixture repo must carry one.
const SEED_CHANGELOG: &str = "# Changelog\n\n## [Unreleased]\n\n### Added\n- **Something real** — an entry.\n\n## [0.25] - 2026-07-06\n\n- old notes.\n";

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

/// Drive one claim with a stand-in `regenerate` that writes a BUMP file
/// mentioning the n it was called with (so the asserts can prove the retry
/// regenerated content for the NEW number, not reused attempt 1's). Returns
/// the claim outcome plus every n regenerate saw.
fn do_claim(
    git: &dyn GitRunner,
    work: &Path,
    version: &str,
    allow_existing_section: bool,
) -> (ledger::Result<ledger::Claim>, Vec<u64>) {
    let mut ns = Vec::new();
    let plan = ClaimPlan {
        version,
        now: NOW,
        allow_existing_section,
        max_attempts: 5,
    };
    let mut regen = |n: u64| {
        ns.push(n);
        fs::write(work.join("BUMP"), format!("{version} build {n}\n"))
            .map_err(|e| Error::new(e.to_string()))?;
        Ok(vec!["BUMP".to_string()])
    };
    let res = ledger::claim(git, work, &plan, &mut regen);
    (res, ns)
}

/// A lost/aborted claim must leave NOTHING behind (spec §2: "abort with tree
/// reset clean — nothing burned").
fn assert_clean_at_origin_tip(fix: &Fixture) {
    assert!(
        run(&fix.work, "git", &["status", "--porcelain"])
            .trim()
            .is_empty(),
        "working tree not clean after abort"
    );
    assert!(
        !fix.work.join("BUMP").exists(),
        "regenerated content survived the abort"
    );
    let head = run(&fix.work, "git", &["rev-parse", "HEAD"]);
    let tip = run(&fix.origin, "git", &["rev-parse", "main"]);
    assert_eq!(
        head.trim(),
        tip.trim(),
        "HEAD not reset to the real origin tip"
    );
    assert!(
        !origin_subjects(fix).contains("release: v0.26"),
        "an aborted claim must never land a release commit on origin"
    );
}

// --------------------------------------------------------------------------
// parser + number rule (pure)
// --------------------------------------------------------------------------

#[test]
fn parse_takes_last_record_and_names_malformed_lines() {
    let good = "# comment\n1783354739 0.25\n1783918101 0.26\n";
    let t = ledger::tail(good).unwrap();
    assert_eq!((t.build, t.version.as_str()), (1_783_918_101, "0.26"));

    // Every malformed shape aborts and names its 1-based line — blank lines
    // included: the ledger is append-only, so ANY unparseable edit is treated
    // as corruption, never skipped over.
    let cases: [(&str, usize); 4] = [
        ("# c\n1783354739 0.25\n\n1783918101 0.26\n", 3), // blank line
        ("1783354739\n", 1),                              // one field
        ("1783354739 0.25 extra\n", 1),                   // three fields
        ("abc 0.25\n", 1),                                // non-numeric build
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
    // Below the v0.25 floor entirely → refuse to mint.
    assert!(ledger::next_build(5, 10).is_err());
}

#[test]
fn claim_rejects_non_major_minor_versions() {
    // Shape-checked before any git runs, so a dead path is fine as the repo.
    let git = GitCli::new("/nonexistent");
    for bad in ["0.26.1", "v0.26", "0.2x", "26", ""] {
        let plan = ClaimPlan {
            version: bad,
            now: NOW,
            allow_existing_section: false,
            max_attempts: 5,
        };
        let err = ledger::claim(&git, Path::new("/nonexistent"), &plan, &mut |_| Ok(vec![]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("MAJOR.MINOR"), "{bad:?} → {err}");
    }
}

// --------------------------------------------------------------------------
// claim protocol against real git
// --------------------------------------------------------------------------

#[test]
fn happy_path_claims_appends_and_verifies() {
    let fix = Fixture::new("happy");
    let git = GitCli::new(&fix.work);
    let (res, ns) = do_claim(&git, &fix.work, "0.26", false);
    let c = res.unwrap();
    assert_eq!(c.build, NOW);
    assert_eq!(c.ledger_line, "1790000000 0.26");
    assert_eq!(ns, vec![NOW]);
    // The verified claim commit is on origin, message per spec §1.
    assert!(origin_subjects(&fix).contains("release: v0.26 (build 1790000000)"));
    assert_eq!(
        c.commit,
        run(&fix.work, "git", &["rev-parse", "HEAD"]).trim()
    );
    // Whole-file byte equality: seed untouched, our line appended.
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}1790000000 0.26\n")
    );
    // The regenerated content landed in the SAME single commit.
    assert_eq!(origin_file(&fix, "BUMP"), "0.26 build 1790000000\n");
}

#[test]
fn stale_head_aborts_with_pull_first() {
    let fix = Fixture::new("stale");
    rival_append(&fix, "1790000042 0.99"); // origin moves before the claim starts
    let git = GitCli::new(&fix.work);
    let (res, ns) = do_claim(&git, &fix.work, "0.26", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("pull first"), "{err}");
    // Gate fired before anything was generated or committed.
    assert!(ns.is_empty());
    assert!(!fix.work.join("BUMP").exists());
}

#[test]
fn cas_loser_regenerates_from_origin_with_higher_n() {
    let fix = Fixture::new("cas");
    let winner = "1790000123 0.99"; // > the loser's first n → forces a bigger retry n
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_append(&fix, winner);
        }
    });
    let (res, ns) = do_claim(&runner, &fix.work, "0.26", false);
    let c = res.unwrap();
    // Loser retried with a strictly-higher number (winner's tail + 1).
    assert_eq!(c.build, 1_790_000_124);
    assert_eq!(ns, vec![1_790_000_000, 1_790_000_124]);
    assert_eq!(runner.pushes.get(), 2);
    // THE core preservation proof, whole file byte-exact: seed, then the
    // winner's line untouched, then ours as the tail — the reset-hard +
    // regenerate-from-origin's-blobs retry can never clobber the winner
    // (spec decision 3; the reset-soft design verifiably did).
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}{winner}\n1790000124 0.26\n")
    );
    // The commit content was REGENERATED for the retry's n, not reused.
    assert_eq!(origin_file(&fix, "BUMP"), "0.26 build 1790000124\n");
    assert!(origin_subjects(&fix).contains("release: v0.26 (build 1790000124)"));
}

#[test]
fn cas_loser_may_remint_same_n_while_clock_leads() {
    let fix = Fixture::new("cas-clock");
    // Winner's number is BELOW the loser's clock-derived n: the retry lands
    // max(tail+1, now) = now again — same number, still strictly above the
    // new tail, and never published anywhere in between. Documents the
    // max(…, now) subtlety of spec §2.
    let winner = "1789999000 0.98";
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_append(&fix, winner);
        }
    });
    let (res, ns) = do_claim(&runner, &fix.work, "0.26", false);
    let c = res.unwrap();
    assert_eq!(c.build, NOW);
    assert_eq!(ns, vec![NOW, NOW]);
    assert_eq!(
        origin_file(&fix, "RELEASES.ledger"),
        format!("{SEED_LEDGER}{winner}\n1790000000 0.26\n")
    );
}

#[test]
fn retry_cap_aborts_clean_with_nothing_burned() {
    let fix = Fixture::new("cap");
    // A rival lands a fresh push before EVERY attempt — the claimant must
    // lose all 5 rounds, then stop cleanly.
    let runner = HookRunner::new(&fix.work, |attempt| {
        rival_append(&fix, &format!("17900002{attempt:02} 0.9{attempt}"));
    });
    let (res, ns) = do_claim(&runner, &fix.work, "0.26", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("5 times"), "{err}");
    assert_eq!(runner.pushes.get(), 5);
    assert_eq!(ns.len(), 5);
    assert_clean_at_origin_tip(&fix);
    // Every winner line survived, byte-exact, in order.
    let mut expected = SEED_LEDGER.to_string();
    for a in 1..=5u32 {
        expected.push_str(&format!("17900002{a:02} 0.9{a}\n"));
    }
    assert_eq!(origin_file(&fix, "RELEASES.ledger"), expected);
}

#[test]
fn remote_tag_means_version_cut_elsewhere() {
    let fix = Fixture::new("tag-elsewhere");
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_append(&fix, "1790000300 0.26");
            run(&fix.rival, "git", &["tag", "v0.26"]);
            run(&fix.rival, "git", &["push", "-q", "origin", "v0.26"]);
        }
    });
    let (res, _) = do_claim(&runner, &fix.work, "0.26", false);
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("cut elsewhere") && err.contains("tag v0.26"),
        "{err}"
    );
    assert_eq!(
        runner.pushes.get(),
        1,
        "must abort, not retry, on a same-version tag"
    );
    assert_clean_at_origin_tip(&fix);
}

#[test]
fn remote_changelog_section_means_version_cut_elsewhere() {
    let fix = Fixture::new("section-elsewhere");
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_land_section(&fix, "1790000300 0.26", "0.26");
        }
    });
    let (res, _) = do_claim(&runner, &fix.work, "0.26", false);
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("cut elsewhere") && err.contains("changelog"),
        "{err}"
    );
    assert_clean_at_origin_tip(&fix);
}

#[test]
fn recut_flag_claims_fresh_n_past_existing_section() {
    let fix = Fixture::new("recut");
    // The recut path (spec §5): the version's section ALREADY sits on origin
    // (rolled by the earlier wedged cut); a fresh claim for the same version
    // must ride past it instead of aborting.
    let runner = HookRunner::new(&fix.work, |attempt| {
        if attempt == 1 {
            rival_land_section(&fix, "1790000300 0.26", "0.26");
        }
    });
    let (res, _) = do_claim(&runner, &fix.work, "0.26", true);
    let c = res.unwrap();
    assert_eq!(c.build, 1_790_000_301); // fresh, strictly above the wedged claim
    // The wedged cut's artifacts survive byte-exact next to the new claim.
    let remote = origin_file(&fix, "RELEASES.ledger");
    assert!(remote.contains("1790000300 0.26\n"));
    assert!(remote.ends_with("1790000301 0.26\n"));
    assert!(origin_file(&fix, "CHANGELOG.md").contains("## [0.26] - 2026-01-01"));
}

#[test]
fn malformed_remote_ledger_aborts_before_any_commit() {
    let fix = Fixture::new("malformed");
    // Corrupt the ledger on origin (line 3 non-numeric), then bring the
    // claimant to the tip so the corruption — not the tip gate — is what fires.
    rival_append(&fix, "not-a-number 0.26");
    run(&fix.work, "git", &["fetch", "-q", "origin", "main"]);
    run(&fix.work, "git", &["reset", "-q", "--hard", "origin/main"]);
    let git = GitCli::new(&fix.work);
    let (res, ns) = do_claim(&git, &fix.work, "0.26", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("line 3") && err.contains("not a u64"), "{err}");
    // Parse failure precedes regenerate/commit/push — nothing happened.
    assert!(ns.is_empty());
    assert_clean_at_origin_tip(&fix);
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
    let (res, ns) = do_claim(&git, &fix.work, "0.26", false);
    let err = res.unwrap_err().to_string();
    assert!(err.contains("no offline cuts"), "{err}");
    assert!(ns.is_empty(), "no offline claim may generate anything");
}
