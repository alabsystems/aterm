// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build-number ledger (release spec §2): parse/append `RELEASES.ledger`
//! (`#` comments ignored; records are `<build_number> <version>`; malformed
//! non-comment lines abort with their line number), compute
//! `n = max(last + 1, unix_now)`, and run the claim protocol in its two halves:
//!
//! * [`reserve`] fixes the release's identity BEFORE anything is built from it:
//!   `n` from origin's ledger tail and the clock, and the RELEASE commit C — the
//!   published commit plus the ledger line and the rolled changelog, nothing
//!   else — made from git objects, touching no worktree and no index, pushed
//!   nowhere. The binary is built and MEASURED from C carrying `n`, so the bytes
//!   MEASURE judged are the bytes that ship; and a release that refuses anywhere
//!   before the claim burns no number, because nothing was claimed.
//! * [`claim`] lands that reservation: main takes C (a fast-forward, or a merge
//!   onto a tip peers moved), and that `git push` is a compare-and-swap on the
//!   ledger tail, with the rebuild-the-landing retry (max 5) on rejection and the
//!   mandatory post-push verification that the remote tail is byte-exactly ours.
//!   A reservation whose `n` the tail has reached in the meantime — another
//!   release claimed past it — is never re-numbered: its bytes carry `n`, so it
//!   refuses, nothing burned, and the release reserves again and builds again.
//!
//! Only a verified claim is ever SHIPPED: a reserved `n` reaches an unsigned
//! candidate and its measurement, and nothing is signed for it until [`claim`]
//! has landed it.
//!
//! This module also hosts the crate-wide plumbing every pipeline stage shares
//! (`Error`, the injectable [`GitRunner`] seam, [`git_ok`]): the spec's file
//! plan (§9) gives the crate no separate utility module, and the claim is the
//! one stage every other stage feeds, so the shared pieces live with it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::changelog;

/// How an operator runs a release — the ONE spelling every remedy names
/// (`release::RELEASE_COMMAND` is this). Here, with the crate-wide plumbing,
/// because the claim's remedies name it and the changelog's test binary mounts
/// this module without the release machine.
pub const RELEASE_COMMAND: &str = "targo --unverified ship release";

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

    /// `git <args>` with `input` on its stdin: the objects-only writes
    /// ([`commit_with`]: `hash-object --stdin`, `mktree -z`). A runner that
    /// cannot feed git refuses, naming the command — never a write that read
    /// nothing.
    fn git_input(&self, args: &[&str], input: &[u8]) -> Result<RunOut> {
        let _ = input;
        Err(Error::new(format!(
            "git {}: this runner feeds git no input",
            args.join(" ")
        )))
    }
}

/// Run `cmd` (a `git` with its leading arguments) with `args` and `input` on its
/// stdin, captured as [`RunOut`]. The stdin is written from a thread, so git's
/// output never fills a pipe the writer waits behind.
pub(crate) fn run_with_input(mut cmd: Command, args: &[&str], input: &[u8]) -> Result<RunOut> {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = cmd
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::new(format!("failed to spawn git {}: {e}", args.join(" "))))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::new(format!("git {}: no stdin", args.join(" "))))?;
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child
        .wait_with_output()
        .map_err(|e| Error::new(format!("wait for git {}: {e}", args.join(" "))))?;
    writer
        .join()
        .map_err(|_| Error::new(format!("git {}: the input writer panicked", args.join(" "))))?
        .map_err(|e| Error::new(format!("feed git {}: {e}", args.join(" "))))?;
    Ok(RunOut {
        status: out.status.code().unwrap_or(-1),
        stdout: out.stdout,
        stderr: out.stderr,
    })
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

    fn git_input(&self, args: &[&str], input: &[u8]) -> Result<RunOut> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.repo);
        run_with_input(cmd, args, input)
    }
}

/// `git -C <repo>` that answers for that repository's objects as they are recorded and
/// for nothing else: every `GIT_*` variable the caller's shell set is removed
/// (`GIT_DIR`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`,
/// `GIT_REPLACE_REF_BASE`, `GIT_GRAFT_FILE`, `GIT_SHALLOW_FILE` … would each answer for
/// another repository or rewrite this one), no system or global config is read,
/// replace refs are ignored, nothing is fetched lazily, nothing prompts, no optional
/// lock is taken, and stdin is empty.
///
/// Replace refs matter most: under `git replace <a> <b>`, `git cat-file --batch`
/// answers a request for `<a>` with `<b>`'s bytes under a header naming `<a>`
/// (MEASURED 2026-10-09), so a reader of what a commit holds that honoured them would
/// take another object's bytes for it. The ONE git for reading what a commit holds:
/// the release snapshot ([`crate::snapshot`]) and the Trust lane
/// ([`crate::trust_oblig::TrustGit`]) both spawn it.
pub fn object_git(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&key);
        }
    }
    cmd.env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(repo)
        .stdin(std::process::Stdio::null());
    cmd
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
    /// Release version being cut, e.g. "0.2.0" (`[workspace.package] version`,
    /// always `MAJOR.MINOR.0`).
    pub version: &'a str,
    /// Unix seconds, read once by the caller.
    pub now: u64,
    /// Main ALREADY carries this version's `## [X.Y.Z]` changelog section —
    /// rolled there by an earlier claim whose cut wedged — so a lost race onto
    /// such a tip is this cut's own history, not a cut elsewhere. The remote-TAG
    /// abort always applies: a tag means the version was fully published.
    pub allow_existing_section: bool,
    /// Normally [`MAX_CLAIM_ATTEMPTS`]; a knob so tests can prove the cap.
    pub max_attempts: u32,
    /// The published commit (full sha) the release is cut from: on `origin/main`,
    /// the release commit's one parent, and where the worktree [`claim`] runs in
    /// — the cut tree — sits until the claim lands.
    pub source: &'a str,
}

/// The ref that keeps a reservation's release commit reachable (git's gc keeps
/// what a ref names) while the release builds, measures and publishes, before
/// [`claim`] puts it on main.
pub const RESERVED_REF: &str = "refs/aterm-release/reserved";

/// A release's identity, fixed before anything is built from it ([`reserve`]):
/// what the binary is stamped with, what MEASURE judges, and — once [`claim`]
/// lands it — what ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reserved {
    /// `n`: `max(origin tail + 1, now)` when reserved.
    pub build: u64,
    /// The RELEASE commit C (full sha): the published commit plus `line` appended
    /// to ITS ledger and its rolled changelog, nothing else. Made from objects,
    /// pushed nowhere until the claim.
    pub commit: String,
    /// The ledger line C appends, e.g. "1783918101 0.2.0".
    pub line: String,
}

/// A verified claim: the number is on origin/main, tail-checked byte-exactly,
/// and safe to bake into artifacts.
#[derive(Debug)]
pub struct Claim {
    pub build: u64,
    /// The RELEASE commit (full sha): the published commit plus the ledger line
    /// and the rolled changelog, and nothing else. The artifacts are built from
    /// here; the worktree the claim ran in (the cut tree) is left on it, detached.
    pub commit: String,
    /// The commit that landed on origin/main: the release commit itself when main
    /// had not moved past the published commit, else a merge of it onto main's
    /// tip ([`landing_commit`]).
    pub landed: String,
    /// The exact ledger line we appended, e.g. "1783918101 0.2.0".
    pub ledger_line: String,
}

/// The changelog texts a claim writes, from `(published commit's, main tip's)`
/// CHANGELOG.md: `(release commit's, main's)`. See `changelog::claim_changelogs`.
pub type ClaimChangelogs<'a> = &'a dyn Fn(&str, &str) -> Result<(String, String)>;

/// RESERVE the release's identity (spec §2, steps 1-3, before anything is built):
/// read origin's ledger tail, `n = max(tail + 1, plan.now)`, and make the RELEASE
/// commit C — `plan.source` plus `n version` appended to ITS ledger and its rolled
/// changelog (`changelogs`' first half), nothing else — from git objects
/// ([`commit_with`]): no worktree, no index, no push. [`RESERVED_REF`] holds C so
/// git keeps it while the release builds and measures. Nothing is claimed: a
/// release that refuses from here to [`claim`] burns no number.
///
/// Fails closed offline — `n` read from a stale ledger would be a fiction the
/// claim then refuses; a source that is not on `origin/main`; and every failure
/// of the ledger's parse, the changelog roll or git.
pub fn reserve(
    git: &dyn GitRunner,
    plan: &ClaimPlan<'_>,
    changelogs: ClaimChangelogs<'_>,
) -> Result<Reserved> {
    check_version_shape(plan.version)?;
    git_ok(git, &["fetch", "origin", "main"]).map_err(|e| {
        Error::new(format!(
            "cannot reach origin (no offline cuts — the reservation reads origin's ledger \
             tail): {e}"
        ))
    })?;
    source_on_main(git, plan.source)?;
    let source_ledger = show_utf8(git, plan.source, LEDGER_FILE)?;
    let source_changelog = show_utf8(git, plan.source, changelog::CHANGELOG_FILE)?;
    let n = next_build(tail(&show_origin_ledger(git)?)?.build, plan.now)?;
    let main_changelog = show_utf8(git, "origin/main", changelog::CHANGELOG_FILE)?;
    let (release_changelog, _) = changelogs(&source_changelog, &main_changelog)?;
    let line = format!("{n} {}", plan.version);
    let commit = commit_with(
        git,
        plan.source,
        &[
            (changelog::CHANGELOG_FILE, &release_changelog),
            (LEDGER_FILE, &appended(&source_ledger, &line)),
        ],
        &[plan.source],
        &format!("release: v{} (build {n})", plan.version),
    )?;
    git_ok(git, &["update-ref", RESERVED_REF, &commit])?;
    Ok(Reserved {
        build: n,
        commit,
        line,
    })
}

/// CLAIM a reservation (spec §2, steps 4-5): main takes the release commit
/// `reserved.commit` whatever its tip is — as a fast-forward when main has not
/// moved past the published commit, else as a merge onto the tip whose tree is
/// the tip's plus the same ledger line and the shipped notes moved out of
/// `[Unreleased]` (`changelogs`' second half, [`commit_with`]) — and the push of
/// that commit is the compare-and-swap (owner ruling R2: peers' pushes since the
/// publish stay on main and out of the release).
///
/// THE NUMBER IS THE RESERVATION'S, ALWAYS. The binary built and measured from
/// C carries `reserved.build`, so the claim never re-numbers: before every push
/// origin's tail must be below it, and a tail at or past it — another release
/// claimed past ours while this one was built and measured — refuses with nothing
/// claimed and nothing burned (the release reserves again and builds again). A
/// lost race to anything below it rebuilds only the landing, from the winner's
/// ledger, and pushes again.
///
/// `worktree` is the cut tree: at the published commit, untouched until the claim
/// has landed and been verified, then put on C. Every abort leaves it where it was.
pub fn claim(
    git: &dyn GitRunner,
    worktree: &Path,
    plan: &ClaimPlan<'_>,
    reserved: &Reserved,
    changelogs: ClaimChangelogs<'_>,
) -> Result<Claim> {
    check_version_shape(plan.version)?;

    // Step 4a: fetch, and require HEAD == the published commit, on origin/main.
    // Fail closed if offline — an offline "claim" would be a local fiction
    // another machine could race.
    git_ok(git, &["fetch", "origin", "main"]).map_err(|e| {
        Error::new(format!(
            "cannot reach origin (no offline cuts — the ledger claim IS the push): {e}"
        ))
    })?;
    let head = rev_parse(git, "HEAD")?;
    if head != plan.source {
        return Err(Error::new(format!(
            "HEAD ({head}) is not the published commit ({}) — the release commit is built \
             on it, so the checkout must sit exactly there",
            plan.source
        )));
    }
    source_on_main(git, plan.source)?;
    reservation_is_the_claims(git, plan, reserved)?;
    let source_changelog = show_utf8(git, plan.source, changelog::CHANGELOG_FILE)?;
    let (n, release, line) = (
        reserved.build,
        reserved.commit.as_str(),
        reserved.line.as_str(),
    );

    // Step 4b: read the tail from ORIGIN's blob (not a worktree file) — the blob
    // is what the push CASes against, and re-reading it on every retry is what
    // preserves a race winner's line byte-exactly.
    let mut base = show_origin_ledger(git)?;
    let mut attempt = 0u32;
    let landed = loop {
        attempt += 1;
        let tail_build = tail(&base)?.build;
        if tail_build >= n {
            return Err(Error::new(format!(
                "origin's ledger tail is build {tail_build}, at or past the reserved build {n} \
                 — another release claimed past this one while it was built and measured. \
                 Nothing was claimed and nothing was burned; the reserved bytes carry {n}, \
                 so they are never claimed under another number: run `{}` again, which \
                 reserves a fresh number and builds and measures it",
                RELEASE_COMMAND
            )));
        }
        let tip = rev_parse(git, "origin/main")?;
        let landed = if tip == plan.source {
            release.to_string()
        } else {
            let main_changelog = show_utf8(git, "origin/main", changelog::CHANGELOG_FILE)?;
            let (_, landing_changelog) = changelogs(&source_changelog, &main_changelog)?;
            commit_with(
                git,
                &tip,
                &[
                    (changelog::CHANGELOG_FILE, &landing_changelog),
                    (LEDGER_FILE, &appended(&base, line)),
                ],
                &[&tip, release],
                &format!(
                    "release: v{} (build {n}) lands on main — cut from the published {}",
                    plan.version,
                    plan.source.get(..12).unwrap_or(plan.source)
                ),
            )?
        };

        // Step 4c: the push IS the compare-and-swap — a fast-forward of the tip
        // succeeds for exactly one appender per remote tip.
        let push = git.git(&["push", "origin", &format!("{landed}:refs/heads/main")])?;
        if push.success() {
            break landed;
        }
        let push_err = push.stderr_utf8().trim().to_string();

        // Only a non-fast-forward rejection is a lost CAS race — the one
        // failure rebuilding the landing can fix. Anything else (expired
        // credentials, branch protection, network death mid-push) would fail
        // identically on every round; abort NOW with git's own diagnostic
        // instead of burning five rounds against an unmoved origin and then
        // misreporting a "push race" on a single-operator repo.
        let lost_race = push_err.contains("non-fast-forward")
            || push_err.contains("fetch first")
            || push_err.contains("[rejected]");
        if !lost_race {
            return Err(Error::new(format!(
                "git push origin main failed (NOT a fast-forward rejection — retrying \
                 cannot help); aborting with the checkout on the published commit, nothing \
                 burned: {push_err}"
            )));
        }

        // Someone else won this tip: fetch the winner's truth before deciding
        // whether to retry.
        git_ok(git, &["fetch", "origin", "main"])?;

        if attempt >= plan.max_attempts {
            return Err(Error::new(format!(
                "ledger claim lost the push race {attempt} times — aborting with the \
                 checkout on the published commit; nothing was burned (losing this many CAS \
                 rounds needs a human look). last rejection: {push_err}"
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
            let cl = show_utf8(git, "origin/main", changelog::CHANGELOG_FILE)?;
            if changelog::has_section(&cl, plan.version) {
                return Err(Error::new(format!(
                    "v{} cut elsewhere: origin/main already has a \"## [{}]\" changelog \
                     section",
                    plan.version, plan.version
                )));
            }
        }

        // Retry from the winner's ledger: the landing is rebuilt on its tip; the
        // number stays the reservation's, checked against the new tail above.
        base = show_origin_ledger(git)?;
    };

    // Step 5: verify the append LANDED as ours — a successful push exit code
    // is git's claim; shipping requires the remote bytes. Fresh fetch, tip
    // identity, and byte-exact tail equality. Every failure from here on may follow
    // a claim that LANDED, before any cut journal holds it: nothing is signed,
    // tagged or published yet, so there is nothing to withdraw (`--abandon` needs a
    // journal), and the remedy is the re-run that reserves anew.
    let unjournaled = |why: String| {
        Error::new(format!(
            "post-push verify failed: {why} — the claim of build {n} may have LANDED on \
             origin, and no cut journal holds it. Nothing was signed, tagged or published \
             under it, so there is nothing to withdraw: run `{}` again. If it landed, build \
             {n} is spent (a gap in the ledger, which is normal) and the release reserves a \
             fresh number and builds and measures it; if it did not, the cut claims {n} again",
            RELEASE_COMMAND
        ))
    };
    git_ok(git, &["fetch", "origin", "main"]).map_err(|e| unjournaled(e.to_string()))?;
    let origin_tip = rev_parse(git, "origin/main").map_err(|e| unjournaled(e.to_string()))?;
    if origin_tip != landed {
        return Err(unjournaled(format!(
            "origin/main ({origin_tip}) is not the pushed claim ({landed})"
        )));
    }
    let remote = show_origin_ledger(git).map_err(|e| unjournaled(e.to_string()))?;
    let remote_tail = remote.lines().last().unwrap_or("");
    if remote_tail != line {
        return Err(unjournaled(format!(
            "the remote ledger tail {remote_tail:?} is not byte-exactly our line {line:?}"
        )));
    }
    // The cut tree onto the release commit, where everything after the claim reads
    // (and where a resume puts it back); the reservation's ref goes, since main
    // keeps the commit now.
    git_ok(git, &["checkout", "-q", "--detach", release])
        .map_err(|e| unjournaled(e.to_string()))?;
    let head = rev_parse(git, "HEAD").map_err(|e| unjournaled(e.to_string()))?;
    if head != release {
        return Err(unjournaled(format!(
            "HEAD of {} ({head}) is not the release commit ({release})",
            worktree.display()
        )));
    }
    let _ = git.git(&["update-ref", "-d", RESERVED_REF, release]);
    Ok(Claim {
        build: n,
        commit: release.to_string(),
        landed,
        ledger_line: line.to_string(),
    })
}

/// `source` is a commit of `origin/main` — a claim lands on main.
fn source_on_main(git: &dyn GitRunner, source: &str) -> Result<()> {
    if git
        .git(&["merge-base", "--is-ancestor", source, "origin/main"])?
        .success()
    {
        return Ok(());
    }
    Err(Error::new(format!(
        "the published commit {source} is not on origin/main — a claim lands on main"
    )))
}

/// The reservation is a release commit OF THIS CLAIM: its one parent the published
/// commit, its version's line the tail of its ledger, and nothing moved against the
/// published commit but what the reservation writes — its ledger the published
/// commit's plus that line, and its changelog exactly the published commit's rolled
/// for the version ([`changelog::release_changelog`], on the date its section names),
/// which is the published commit's own when that already carries the section (a
/// recut from main's tip after an earlier claim rolled it there): then the ledger is
/// all that moved.
fn reservation_is_the_claims(
    git: &dyn GitRunner,
    plan: &ClaimPlan<'_>,
    reserved: &Reserved,
) -> Result<()> {
    let refuse = |why: String| {
        Err(Error::new(format!(
            "the reserved release commit {} is not this claim's: {why} — nothing was claimed; \
             run `{}` again, which reserves anew",
            reserved.commit, RELEASE_COMMAND
        )))
    };
    let parents = rev_trimmed(git, &["rev-list", "--parents", "-n", "1", &reserved.commit])?;
    let parents: Vec<&str> = parents.split_whitespace().skip(1).collect();
    if parents != [plan.source] {
        return refuse(format!(
            "its parents are {parents:?}, not the published {}",
            plan.source
        ));
    }
    if reserved.line != format!("{} {}", reserved.build, plan.version) {
        return refuse(format!(
            "its line {:?} is not build {} of v{}",
            reserved.line, reserved.build, plan.version
        ));
    }
    let ledger = show_utf8(git, &reserved.commit, LEDGER_FILE)?;
    if ledger != appended(&show_utf8(git, plan.source, LEDGER_FILE)?, &reserved.line) {
        return refuse(format!(
            "its ledger is not the published commit's plus {:?}",
            reserved.line
        ));
    }
    let released = show_utf8(git, &reserved.commit, changelog::CHANGELOG_FILE)?;
    // The roll's date is the reservation's own, read back: a claim may run days later.
    let date = changelog::section_date(&released, plan.version);
    let source_changelog = show_utf8(git, plan.source, changelog::CHANGELOG_FILE)?;
    let expected =
        changelog::release_changelog(&source_changelog, plan.version, date.unwrap_or(""))?;
    if released != expected {
        return refuse(match date {
            None => format!(
                "its {} carries no \"## [{}] - <date>\" section",
                changelog::CHANGELOG_FILE,
                plan.version
            ),
            Some(_) => format!(
                "its {} is not the published commit's rolled for v{}",
                changelog::CHANGELOG_FILE,
                plan.version
            ),
        });
    }
    let moved = rev_trimmed(git, &["diff", "--name-only", plan.source, &reserved.commit])?;
    let stray: Vec<&str> = moved
        .lines()
        .filter(|path| *path != changelog::CHANGELOG_FILE && *path != LEDGER_FILE)
        .collect();
    if !stray.is_empty() {
        return refuse(format!("it moves {stray:?} against the published commit"));
    }
    Ok(())
}

/// A commit made from git objects alone — no worktree, no index: `base`'s root
/// tree with each top-level `files` entry's bytes replaced (each must already be
/// a blob there), committed with `parents` and `message` under this repository's
/// configured identity. The blobs are hashed as `git add` would hash them at
/// their paths (`--path`), so the bytes match a commit made in a checkout.
///
/// # Errors
/// A `base` with no such top-level blob, and any git failure.
pub fn commit_with(
    git: &dyn GitRunner,
    base: &str,
    files: &[(&str, &str)],
    parents: &[&str],
    message: &str,
) -> Result<String> {
    let listed = git_ok(git, &["ls-tree", "-z", &format!("{base}^{{tree}}")])?.stdout;
    let mut entries: Vec<Vec<u8>> = listed
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    for (name, text) in files {
        let path = format!("--path={name}");
        let blob = git.git_input(
            &["hash-object", "-w", "--stdin", path.as_str()],
            text.as_bytes(),
        )?;
        if !blob.success() {
            return Err(Error::new(format!(
                "git hash-object {name} failed (exit {}): {}",
                blob.status,
                blob.stderr_utf8().trim()
            )));
        }
        let blob = blob.stdout_utf8().trim().to_string();
        let at = entries
            .iter()
            .position(|e| {
                e.starts_with(b"100644 blob ")
                    && e.split(|b| *b == b'\t').nth(1) == Some(name.as_bytes())
            })
            .ok_or_else(|| {
                Error::new(format!("{base} holds no top-level file {name} to replace"))
            })?;
        entries[at] = format!("100644 blob {blob}\t{name}").into_bytes();
    }
    let mut listing = Vec::new();
    for entry in &entries {
        listing.extend_from_slice(entry);
        listing.push(0);
    }
    let tree = git.git_input(&["mktree", "-z"], &listing)?;
    if !tree.success() {
        return Err(Error::new(format!(
            "git mktree failed (exit {}): {}",
            tree.status,
            tree.stderr_utf8().trim()
        )));
    }
    let tree = tree.stdout_utf8().trim().to_string();
    let mut args = vec!["commit-tree", tree.as_str()];
    for parent in parents {
        args.extend(["-p", parent]);
    }
    args.extend(["-m", message]);
    rev_trimmed(git, &args)
}

fn rev_trimmed(git: &dyn GitRunner, args: &[&str]) -> Result<String> {
    Ok(git_ok(git, args)?.stdout_utf8().trim().to_string())
}

/// `base` with `line` appended as its last record.
fn appended(base: &str, line: &str) -> String {
    let mut out = base.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(line);
    out.push('\n');
    out
}

/// `git show <rev>:<path>`, strict UTF-8: the same bytes are re-used as a base
/// to append to or roll, so a lossy decode could silently rewrite them — refuse
/// instead.
fn show_utf8(git: &dyn GitRunner, rev: &str, path: &str) -> Result<String> {
    let spec = format!("{rev}:{path}");
    let out = git_ok(git, &["show", &spec])?;
    String::from_utf8(out.stdout).map_err(|_| Error::new(format!("{spec} is not valid UTF-8")))
}

/// `git show origin/main:RELEASES.ledger` — the blob the claim's push CASes
/// against.
pub fn show_origin_ledger(git: &dyn GitRunner) -> Result<String> {
    show_utf8(git, "origin/main", LEDGER_FILE)
}

/// The release version is spliced into a tag name, a changelog heading, the
/// DMG asset name and the ledger grammar — reject anything that is not
/// exactly three canonical numeric components before it can poison those
/// greps. There is ONE version scheme: `MAJOR.MINOR.PATCH` (a release is the
/// workspace `MAJOR.MINOR.0` as written — see `VERSIONING.md`).
///
/// Canonical means non-empty, ASCII digits only, and no leading zero unless
/// the component IS `"0"`: one version must have exactly ONE spelling, or two
/// tags could share a numeric order. Public: cli.rs applies the same shape
/// check to `--abandon` / `verify vX.Y.Z` arguments up front.
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

/// Fixture plumbing every test binary shares: the `git` a fixture's own setup and
/// reads run, the runner tests hand the code under test, a fresh repository with
/// its identity, and stand-in executables.
#[cfg(test)]
pub(crate) mod testing {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;

    /// The executable a `git` from PATH ends up running, resolved once per process.
    ///
    /// WHY (MEASURED on a 14-core M-series Mac): macOS's `/usr/bin/git`
    /// is xcode-select's trampoline, which finds the developer directory and execs
    /// the git it names on every call: 8.3 ms of CPU per `rev-parse HEAD` against
    /// 2.7 ms for that git run directly, 200 in a row, and 23 ms against 7.8 ms with
    /// fourteen at once; the `it` binary ran 4,936 git processes through it. It is
    /// the same file with the same argv; the trampoline adds only the SDK variables
    /// (`SDKROOT`, `CPATH`, `LIBRARY_PATH`, `MANPATH`), which git does not read.
    /// Where the `git` on PATH is no trampoline (Linux, a Homebrew git first) this
    /// is that git.
    pub(crate) fn git_path() -> &'static Path {
        static GIT: OnceLock<PathBuf> = OnceLock::new();
        GIT.get_or_init(|| {
            let on_path = std::env::var_os("PATH")
                .and_then(|path| {
                    std::env::split_paths(&path)
                        .map(|dir| dir.join("git"))
                        .find(|candidate| candidate.is_file())
                })
                .unwrap_or_else(|| PathBuf::from("git"));
            if !cfg!(target_os = "macos") || on_path != Path::new("/usr/bin/git") {
                return on_path;
            }
            // What the trampoline asks (`xcrun --find`), so the same git.
            Command::new("/usr/bin/xcrun")
                .args(["--find", "git"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
                .filter(|found| found.is_file())
                .unwrap_or(on_path)
        })
    }

    /// `git`, for a fixture's own setup and reads ([`git_path`]).
    pub(crate) fn git() -> Command {
        Command::new(git_path())
    }

    /// The [`GitRunner`](super::GitRunner) a test hands the code under test:
    /// [`GitCli`](super::GitCli)'s `git -C <repo> <args…>`, its output and exit read as `GitCli`
    /// reads them, run by [`git_path`] rather than the `git` on PATH: the same executable without
    /// the trampoline in front of it (MEASURED: it takes the `it` binary's trampolined git
    /// processes from 2,818 to 109). `GitCli` itself still runs wherever the code under test builds
    /// its own: the cut tree, a receipt store, the cutter binary the handoff tests drive.
    pub(crate) struct Git {
        repo: PathBuf,
    }

    impl Git {
        pub(crate) fn new(repo: impl Into<PathBuf>) -> Self {
            Git { repo: repo.into() }
        }
    }

    impl super::GitRunner for Git {
        fn git(&self, args: &[&str]) -> super::Result<super::RunOut> {
            let out = git()
                .arg("-C")
                .arg(&self.repo)
                .args(args)
                .output()
                .map_err(|e| {
                    super::Error::new(format!("failed to spawn git {}: {e}", args.join(" ")))
                })?;
            Ok(super::RunOut {
                status: out.status.code().unwrap_or(-1),
                stdout: out.stdout,
                stderr: out.stderr,
            })
        }

        fn git_input(&self, args: &[&str], input: &[u8]) -> super::Result<super::RunOut> {
            let mut cmd = git();
            cmd.arg("-C").arg(&self.repo);
            super::run_with_input(cmd, args, input)
        }
    }

    /// `git init -q -b main <dir>`, then [`config`] there.
    ///
    /// # Panics
    /// When git fails: a fixture without its repository decides nothing.
    pub(crate) fn init(dir: &Path, pairs: &[(&str, &str)]) {
        let out = git()
            .args(["init", "-q", "-b", "main"])
            .arg(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git init {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        config(dir, pairs);
    }

    /// What one `git config <key> <value>` per pair leaves in the config of a fresh
    /// init, clone or bare repository at `repo`, written in one append: each section
    /// once, in the order its first key comes, its keys in order — where git puts a
    /// key whose section the file does not hold yet, and the next of that section's
    /// keys after it (checked byte for byte against git 2.50). One
    /// write, not a process per pair. A value is written bare, so one git would quote
    /// is refused.
    ///
    /// Then `maintenance.auto = false`, which every fixture takes: each commit, fetch
    /// and push into it started a `git maintenance run --auto` that ran no task in a
    /// repository this small (`gc --auto` waits for thousands of loose objects or
    /// fifty packs) — 540 of the `it` binary's 6,496 git processes, not one of
    /// which started a child. A bare origin takes it with `&[]`: its
    /// pushes ran one each.
    ///
    /// # Panics
    /// On a key that is not `section.name`, a section the file already holds, a value
    /// git would quote, or a config that cannot be written.
    pub(crate) fn config(repo: &Path, pairs: &[(&str, &str)]) {
        let file = if repo.join(".git").is_dir() {
            repo.join(".git/config")
        } else {
            repo.join("config")
        };
        let mut text = fs::read_to_string(&file).unwrap();
        let mut sections: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
        for &(key, value) in pairs.iter().chain([&("maintenance.auto", "false")]) {
            // `remote.origin.url` goes under `[remote "origin"]`, not `[remote]`.
            let (section, name) = key
                .split_once('.')
                .filter(|(_, name)| !name.contains('.'))
                .expect("a section.name key, no subsection");
            // git quotes a value with an outer space, `#` or `;`, and escapes a
            // quote, a backslash, a newline and a tab (git 2.50).
            assert!(
                !value.is_empty()
                    && value.trim() == value
                    && !value.contains(['"', '\\', '#', ';', '\n', '\t']),
                "{key} = {value:?} is a value git would quote"
            );
            match sections.iter_mut().find(|(s, _)| *s == section) {
                Some((_, keys)) => keys.push((name, value)),
                None => sections.push((section, vec![(name, value)])),
            }
        }
        for (section, keys) in sections {
            assert!(
                !text.contains(&format!("[{section}]")),
                "[{section}] already in {}",
                file.display()
            );
            text.push_str(&format!("[{section}]\n"));
            for (name, value) in keys {
                text.push_str(&format!("\t{name} = {value}\n"));
            }
        }
        fs::write(&file, text).unwrap();
    }
}
