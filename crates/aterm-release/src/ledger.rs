// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build-number ledger (release spec §2): parse/append `RELEASES.ledger`
//! (`#` comments ignored; records are `<build_number> <version>`; malformed
//! non-comment lines abort with their line number), compute
//! `n = max(last + 1, unix_now)`, and run the claim protocol — a fast-forward
//! `git push` is a compare-and-swap on the ledger tail, with the
//! reset-hard-and-regenerate retry (max 5) on rejection and the mandatory
//! post-push verification that the remote tail is byte-exactly ours. Only a
//! verified claim is ever stamped into an artifact.
//!
//! This module also hosts the crate-wide plumbing every pipeline stage shares
//! (`Error`, the injectable [`GitRunner`] seam, [`git_ok`]): the spec's file
//! plan (§9) gives the crate no separate utility module, and the claim is the
//! one stage every other stage feeds, so the shared pieces live with it.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::changelog;

/// The ledger's repo-root-relative path. Committed and append-only: one line
/// per claimed build number, never edited, never reused (spec §2).
pub const LEDGER_FILE: &str = "RELEASES.ledger";

/// The v0.25 seed build number (the last committer-epoch-derived build ever
/// published). Every minted `n` must be strictly above it — below-floor
/// numbers would be invisible to the deployed fleet's `floor.toml` ratchets
/// and would reorder the About window's "built" date.
pub const LEDGER_FLOOR: u64 = 1_783_354_739;

/// Total push attempts before the claim aborts (spec §2 step 4: "retry
/// (max 5)"): the first push plus four regenerate-and-retry rounds. Losing
/// five straight CAS races on a single-operator repo means something is
/// systemically wrong — stop and let the operator look.
pub const MAX_CLAIM_ATTEMPTS: u32 = 5;

/// One release-pipeline failure, formatted for the operator at the failure
/// site (what failed + why + what to do next). A single stringly type is
/// deliberate: nothing upstream ever matches on failure *kind* — every error
/// is terminal for the cut and is printed verbatim, so a variant enum would
/// be dead weight.
#[derive(Debug)]
pub struct Error(pub String);

impl Error {
    pub fn new(msg: impl Into<String>) -> Self {
        Error(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// The build/package stages (buildplan/bundle/sign/dmg) return
/// `Result<_, String>` — this lets the pipeline `?` them straight into the
/// crate-wide error without a per-site `map_err`.
impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Captured output of one runner invocation. `status` is the exit code, with
/// signal-death mapped to -1 (still non-zero, so it can never read as
/// success).
pub struct RunOut {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOut {
    pub fn success(&self) -> bool {
        self.status == 0
    }

    /// Lossy UTF-8 view of stdout — fine for shas/refs/ls-remote listings.
    /// The one byte-exactness-critical read (the ledger blob) goes through
    /// [`show_origin_ledger`], which refuses non-UTF-8 instead of mangling it.
    pub fn stdout_utf8(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_utf8(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// The ONE seam between the release tool and `git`: argv in, captured output
/// out. Production uses [`GitCli`] (real `git -C <repo>`); the race tests wrap
/// it with a hook that lands a rival's push between this process's fetch and
/// push — driving REAL git against local bare-repo fixtures instead of
/// mocking git semantics (which is exactly what a compare-and-swap proof must
/// not do).
pub trait GitRunner {
    fn git(&self, args: &[&str]) -> Result<RunOut>;
}

/// Production runner: `git -C <repo> <args…>` with captured output.
pub struct GitCli {
    repo: PathBuf,
}

impl GitCli {
    pub fn new(repo: impl Into<PathBuf>) -> Self {
        GitCli { repo: repo.into() }
    }
}

impl GitRunner for GitCli {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .map_err(|e| Error::new(format!("failed to spawn git {}: {e}", args.join(" "))))?;
        Ok(RunOut {
            // Signal-death has no exit code; -1 keeps it unambiguously a failure.
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

/// Run a git command and REQUIRE success; the error carries the argv and the
/// trimmed stderr so a failed cut prints the actual git diagnostic, not a
/// generic "git failed".
pub fn git_ok(git: &dyn GitRunner, args: &[&str]) -> Result<RunOut> {
    let out = git.git(args)?;
    if !out.success() {
        return Err(Error::new(format!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            out.status,
            out.stderr_utf8().trim()
        )));
    }
    Ok(out)
}

/// `git rev-parse <refname>`, trimmed.
pub fn rev_parse(git: &dyn GitRunner, refname: &str) -> Result<String> {
    Ok(git_ok(git, &["rev-parse", refname])?
        .stdout_utf8()
        .trim()
        .to_string())
}

/// One ledger record: `<build_number> <version>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub build: u64,
    pub version: String,
}

/// Parse the full ledger. Grammar (spec §2): lines starting with `#` are
/// comments; every other line must be exactly two whitespace-separated fields
/// with a u64 first field. ANY malformed non-comment line — blank lines
/// included — aborts with its 1-based line number: the ledger is the ordering
/// root of the whole update fleet, so an edit we cannot fully parse is an edit
/// we must not build on.
pub fn parse(text: &str) -> Result<Vec<Record>> {
    let mut records = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        if raw.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = raw.split_whitespace().collect();
        if fields.len() != 2 {
            return Err(Error::new(format!(
                "{LEDGER_FILE} line {line_no}: malformed record {raw:?} — expected \
                 \"<build_number> <version>\" (the ledger is append-only; never edit it)"
            )));
        }
        let build: u64 = fields[0].parse().map_err(|_| {
            Error::new(format!(
                "{LEDGER_FILE} line {line_no}: build number {:?} is not a u64",
                fields[0]
            ))
        })?;
        records.push(Record {
            build,
            version: fields[1].to_string(),
        });
    }
    Ok(records)
}

/// The last record — the parser contract of spec §2 ("last non-comment line,
/// first field as u64"). An empty ledger is an error: the committed file is
/// seeded with the v0.25 line, so "no records" means the file was gutted.
pub fn tail(text: &str) -> Result<Record> {
    parse(text)?.pop().ok_or_else(|| {
        Error::new(format!(
            "{LEDGER_FILE} has no records — it must contain at least the v0.25 seed line"
        ))
    })
}

/// Number rule (spec §2): `n = max(last + 1, unix_now)`, asserted above the
/// v0.25 floor. `max(…, now)` keeps n epoch-scale forever (About's "built"
/// date stays sane, dev builds on the committer-epoch fallback stay on the
/// same ordering scale); `last + 1` keeps n monotonic even under a backwards
/// clock.
pub fn next_build(last: u64, now: u64) -> Result<u64> {
    let n = last.saturating_add(1).max(now);
    if n <= LEDGER_FLOOR {
        return Err(Error::new(format!(
            "computed build number {n} is not above the v0.25 floor {LEDGER_FLOOR} — \
             both the ledger tail and this machine's clock are below the last published \
             build; refusing to mint a non-monotonic number"
        )));
    }
    Ok(n)
}

/// Inputs to one claim. `now` is injected (not read inside) so the race tests
/// are deterministic and so every retry reuses ONE clock reading — retries
/// derive monotonicity from the ledger tail, never from time moving.
pub struct ClaimPlan<'a> {
    /// Release version being cut, e.g. "0.2.0" (canonical MAJOR.MINOR.PATCH —
    /// the workspace version with DEV reset to 0).
    pub version: &'a str,
    /// Unix seconds, read once by the caller.
    pub now: u64,
    /// The recut path (spec §5) legitimately re-claims a version whose
    /// `## [X.Y.Z]` changelog section already sits on origin (rolled by the
    /// earlier wedged cut) — it sets this to skip the section half of the
    /// "cut elsewhere" abort. The remote-TAG abort always applies: a tag
    /// means the version was fully published somewhere.
    pub allow_existing_section: bool,
    /// Normally [`MAX_CLAIM_ATTEMPTS`]; a knob so tests can prove the cap.
    pub max_attempts: u32,
}

/// A verified claim: the number is on origin/main, tail-checked byte-exactly,
/// and safe to bake into artifacts.
#[derive(Debug)]
pub struct Claim {
    pub build: u64,
    /// The release commit's full sha (== origin/main at verification time).
    pub commit: String,
    /// The exact ledger line we appended, e.g. "1783918101 0.2.0".
    pub ledger_line: String,
}

/// The claim protocol (spec §2, steps 1-5). Runs BEFORE the expensive build —
/// n is baked into the binary, so the number must be settled first; a lost
/// race here costs seconds.
///
/// `regenerate` produces the rest of the release commit's content for a given
/// n (the changelog roll; the workspace version is the operator's bump and is
/// never rewritten by a cut)
/// and returns the repo-relative paths it wrote, which are staged alongside
/// the ledger. It is re-run from scratch on every retry because the retry
/// resets hard to origin/main and rebuilds the WHOLE commit from origin's
/// blobs — the reset-soft alternative was rejected for verifiably clobbering
/// the winner's ledger line (spec decision 3).
pub fn claim(
    git: &dyn GitRunner,
    worktree: &Path,
    plan: &ClaimPlan<'_>,
    regenerate: &mut dyn FnMut(u64) -> Result<Vec<String>>,
) -> Result<Claim> {
    check_version_shape(plan.version)?;

    // Step 1: fetch, and require HEAD == origin/main. Fail closed if offline —
    // an offline "claim" would be a local fiction another machine could race.
    git_ok(git, &["fetch", "origin", "main"]).map_err(|e| {
        Error::new(format!(
            "cannot reach origin (no offline cuts — the ledger claim IS the push): {e}"
        ))
    })?;
    let head = rev_parse(git, "HEAD")?;
    let origin_tip = rev_parse(git, "origin/main")?;
    if head != origin_tip {
        return Err(Error::new(format!(
            "HEAD ({head}) != origin/main ({origin_tip}) — pull first; a claim must be \
             a fast-forward append on the current remote tip"
        )));
    }

    // Step 2: read the tail from ORIGIN's blob (not the worktree file) — the
    // blob is what the fast-forward push CASes against, and re-reading it on
    // every retry is what preserves a race winner's line byte-exactly.
    let mut base = show_origin_ledger(git)?;
    let mut n = next_build(tail(&base)?.build, plan.now)?;

    let mut attempt = 0u32;
    loop {
        attempt += 1;

        // Step 3: ONE commit — regenerated content for THIS n, plus our
        // ledger line appended to origin's exact blob bytes.
        let mut paths = regenerate(n)?;
        let line = format!("{n} {}", plan.version);
        let mut ledger_out = base.clone();
        if !ledger_out.is_empty() && !ledger_out.ends_with('\n') {
            ledger_out.push('\n');
        }
        ledger_out.push_str(&line);
        ledger_out.push('\n');
        let ledger_path = worktree.join(LEDGER_FILE);
        fs::write(&ledger_path, &ledger_out)
            .map_err(|e| Error::new(format!("cannot write {}: {e}", ledger_path.display())))?;
        if !paths.iter().any(|p| p == LEDGER_FILE) {
            paths.push(LEDGER_FILE.to_string());
        }
        let mut add: Vec<&str> = vec!["add", "--"];
        add.extend(paths.iter().map(String::as_str));
        git_ok(git, &add)?;
        let msg = format!("release: v{} (build {n})", plan.version);
        git_ok(git, &["commit", "-q", "-m", &msg])?;

        // Step 4: the push IS the compare-and-swap — a fast-forward succeeds
        // for exactly one appender per remote tip.
        let push = git.git(&["push", "origin", "main"])?;
        if push.success() {
            break;
        }
        let push_err = push.stderr_utf8().trim().to_string();

        // Drop OUR commit and realign on the last-KNOWN origin tip FIRST — a
        // pure local operation that no network state can fail. Doing it
        // before anything that CAN fail (the recovery fetch below) keeps the
        // abort invariant ("tree clean, nothing burned") unconditional: the
        // unpushed release commit must never be stranded on local main, where
        // the next cut's "pull first" gate cannot fix an ahead-of-origin
        // branch and no message would tell the operator to reset by hand.
        git_ok(git, &["reset", "--hard", "origin/main"])?;

        // Only a non-fast-forward rejection is a lost CAS race — the one
        // failure regenerating the commit can fix. Anything else (expired
        // credentials, branch protection, network death mid-push) would fail
        // identically on every round; abort NOW with git's own diagnostic
        // instead of burning five regenerate rounds against an unmoved origin
        // and then misreporting a "push race" on a single-operator repo.
        let lost_race = push_err.contains("non-fast-forward")
            || push_err.contains("fetch first")
            || push_err.contains("[rejected]");
        if !lost_race {
            return Err(Error::new(format!(
                "git push origin main failed (NOT a fast-forward rejection — retrying \
                 cannot help); aborting with the tree reset clean to origin/main, \
                 nothing burned: {push_err}"
            )));
        }

        // Someone else won this tip: fetch the winner's truth and realign on
        // it (reset --hard, spec decision 3) before deciding whether to retry.
        git_ok(git, &["fetch", "origin", "main"])?;
        git_ok(git, &["reset", "--hard", "origin/main"])?;

        if attempt >= plan.max_attempts {
            return Err(Error::new(format!(
                "ledger claim lost the push race {attempt} times — aborting with the \
                 tree reset clean to origin/main; nothing was burned (losing this many \
                 CAS rounds on a single-operator repo needs a human look). last \
                 rejection: {push_err}"
            )));
        }

        // Same-version-cut-elsewhere abort: if origin now carries tag vX.Y.Z
        // or a "## [X.Y.Z]" changelog section, this version is being (or was) cut
        // on another machine — racing it with a second number would publish
        // two artifacts claiming one version.
        let tag_ref = format!("refs/tags/v{}", plan.version);
        let tags = git_ok(git, &["ls-remote", "--tags", "origin", &tag_ref])?;
        if !tags.stdout_utf8().trim().is_empty() {
            return Err(Error::new(format!(
                "v{} cut elsewhere: origin already has tag v{}",
                plan.version, plan.version
            )));
        }
        if !plan.allow_existing_section {
            let spec = format!("origin/main:{}", changelog::CHANGELOG_FILE);
            let cl = git_ok(git, &["show", &spec])?;
            if changelog::has_section(&cl.stdout_utf8(), plan.version) {
                return Err(Error::new(format!(
                    "v{} cut elsewhere: origin/main already has a \"## [{}]\" changelog \
                     section",
                    plan.version, plan.version
                )));
            }
        }

        // Retry: recompute from the winner's tail — strictly higher than what
        // is now on origin, same single `now` reading.
        base = show_origin_ledger(git)?;
        n = next_build(tail(&base)?.build, plan.now)?;
    }

    // Step 5: verify the append LANDED as ours — a successful push exit code
    // is git's claim; the artifact stamp requires the remote bytes. Fresh
    // fetch, tip identity, and byte-exact tail equality.
    git_ok(git, &["fetch", "origin", "main"])?;
    let head = rev_parse(git, "HEAD")?;
    let origin_tip = rev_parse(git, "origin/main")?;
    let line = format!("{n} {}", plan.version);
    if head != origin_tip {
        return Err(Error::new(format!(
            "post-push verify failed: origin/main ({origin_tip}) != HEAD ({head}) — the \
             claim may have landed but cannot be verified; re-run `cargo ship cut` (a \
             fresh claim will mint a fresh number; gaps are normal)"
        )));
    }
    let remote = show_origin_ledger(git)?;
    let remote_tail = remote.lines().last().unwrap_or("");
    if remote_tail != line {
        return Err(Error::new(format!(
            "post-push verify failed: remote ledger tail {remote_tail:?} is not \
             byte-exactly our line {line:?} — refusing to stamp an unverified claim"
        )));
    }
    Ok(Claim {
        build: n,
        commit: head,
        ledger_line: line,
    })
}

/// `git show origin/main:RELEASES.ledger`, strict UTF-8: the same bytes are
/// re-used as the append base, so a lossy decode could silently rewrite the
/// winner's line — refuse instead.
fn show_origin_ledger(git: &dyn GitRunner) -> Result<String> {
    let spec = format!("origin/main:{LEDGER_FILE}");
    let out = git_ok(git, &["show", &spec])?;
    String::from_utf8(out.stdout)
        .map_err(|_| Error::new(format!("origin/main:{LEDGER_FILE} is not valid UTF-8")))
}

/// The release version is spliced into a tag name, a changelog heading, the
/// DMG asset name and the ledger grammar — reject anything that is not
/// exactly three canonical numeric components before it can poison those
/// greps. There is ONE version scheme: `MAJOR.MINOR.PATCH` (the workspace
/// `MAJOR.MINOR.0` with DEV reset to 0 — see `VERSIONING.md`).
///
/// Canonical means non-empty, ASCII digits only, and no leading zero unless
/// the component IS `"0"`: one version must have exactly ONE spelling, or two
/// tags could share a numeric order. Public: cli.rs applies the same shape
/// check to `--set-version` / `--abandon` / `verify vX.Y.Z` arguments up
/// front.
pub fn check_version_shape(version: &str) -> Result<()> {
    let parts: Vec<&str> = version.split('.').collect();
    let ok = parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        });
    if !ok {
        return Err(Error::new(format!(
            "version {version:?} is not canonical MAJOR.MINOR.PATCH (e.g. \"0.2.0\")"
        )));
    }
    Ok(())
}
