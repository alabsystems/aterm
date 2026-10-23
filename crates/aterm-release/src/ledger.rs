// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build-number ledger: parse and append `RELEASES.ledger` (`#` comments; records are `<build_number> <version>`; a malformed non-comment line refuses with its line number), compute `n = max(last + 1, unix_now)`, and run the claim in two halves:

use aterm_uds::spawnfd::{Command, command};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::changelog;

/// How an operator runs a release: the one spelling every remedy names (`release::RELEASE_COMMAND` is this).
pub const RELEASE_COMMAND: &str = "targo --unverified ship release";

/// The ledger's repo-root-relative path.
pub const LEDGER_FILE: &str = "RELEASES.ledger";

/// The v0.25 seed build number (the last committer-epoch-derived build published).
pub const LEDGER_FLOOR: u64 = 1_783_354_739;

/// Total push attempts before the claim stops: the first push plus four rebuild-and-retry rounds.
pub const MAX_CLAIM_ATTEMPTS: u32 = 5;

/// One release-pipeline failure, formatted for the operator where it happens (what failed, why, what to do next).
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

/// The build and package stages return `Result<_, String>`; this lets the pipeline `?`
impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Captured output of one runner invocation.
pub struct RunOut {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOut {
    pub fn success(&self) -> bool {
        self.status == 0
    }

    /// Lossy UTF-8 view of stdout, fine for shas, refs and listings.
    pub fn stdout_utf8(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_utf8(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// The one seam between the release tool and `git`: argv in, captured output out.
pub trait GitRunner {
    fn git(&self, args: &[&str]) -> Result<RunOut>;

    /// `git <args>` with `input` on its stdin, for the objects-only writes ([`commit_with`]: `hash-object --stdin`, `mktree -z`).
    fn git_input(&self, args: &[&str], input: &[u8]) -> Result<RunOut> {
        let _ = input;
        Err(Error::new(format!(
            "git {}: this runner feeds git no input",
            args.join(" ")
        )))
    }
}

/// Run `cmd` (a `git` with its leading arguments) with `args` and `input` on its stdin, captured as [`RunOut`].
pub(crate) fn run_with_input(mut cmd: Command, args: &[&str], input: &[u8]) -> Result<RunOut> {
    use aterm_uds::spawnfd::Stdio;
    use std::io::Write as _;
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

/// Production runner: `git -C <repo> <args…>` with captured output, stdin empty and no terminal prompt, a credential helper of its that waits for a person ended ([`crate::credential_wait`]).
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
        use aterm_uds::spawnfd::Stdio;
        let mut git = command("git");
        let child = crate::credential_wait::ask_no_one(&mut git)
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::new(format!("failed to spawn git {}: {e}", args.join(" "))))?;
        let watch = crate::credential_wait::Watch::enter(child.id());
        let out = child
            .wait_with_output()
            .map_err(|e| Error::new(format!("wait for git {}: {e}", args.join(" "))))?;
        drop(watch);
        Ok(RunOut {
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    fn git_input(&self, args: &[&str], input: &[u8]) -> Result<RunOut> {
        let mut cmd = command("git");
        crate::credential_wait::ask_no_one(&mut cmd)
            .arg("-C")
            .arg(&self.repo);
        run_with_input(cmd, args, input)
    }
}

/// The `-c` pairs on every git that writes a tree (a checkout, an archive; a clone's config keeps them), so it writes the commit's bytes whatever the releaser's `core.autocrlf`, `core.eol` or global attributes file says.
pub(crate) const BYTE_EXACT: [&str; 6] = [
    "-c",
    "core.autocrlf=false",
    "-c",
    "core.eol=lf",
    "-c",
    "core.attributesFile=/dev/null",
];

/// `git -C <repo>` that answers for that repository's objects as recorded and nothing else: every inherited `GIT_*` variable is removed (`GIT_DIR`, `GIT_OBJECT_DIRECTORY`, `GIT_GRAFT_FILE` … would answer for another repository or rewrite this one), no system or global config is read, replace refs are ignored, nothing is fetched lazily or prompts, no optional lock is taken, and stdin is empty.
pub fn object_git(repo: &Path) -> Command {
    let mut cmd = command("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&key);
        }
    }
    cmd.env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(repo)
        .stdin(aterm_uds::spawnfd::Stdio::null());
    crate::credential_wait::ask_no_one(&mut cmd);
    cmd
}

/// Run a git command and require success; the error carries the argv and git's trimmed stderr.
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

/// Parse the full ledger: `#` lines are comments; every other line, blank ones included, must be two whitespace-separated fields with a u64 first, or the parse refuses with its 1-based line number.
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

/// The last record.
pub fn tail(text: &str) -> Result<Record> {
    parse(text)?.pop().ok_or_else(|| {
        Error::new(format!(
            "{LEDGER_FILE} has no records — it must contain at least the v0.25 seed line"
        ))
    })
}

/// `n = max(last + 1, unix_now)`, required above [`LEDGER_FLOOR`].
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

/// Inputs to one claim.
pub struct ClaimPlan<'a> {
    /// Release version, e.g. "0.2.0" (`[workspace.package] version`, always `MAJOR.MINOR.0`).
    pub version: &'a str,
    /// Unix seconds, read once by the caller.
    pub now: u64,
    /// Main already carries this version's `## [X.Y.Z]` changelog section, rolled there by an earlier claim whose cut wedged, so a lost race onto such a tip is this cut's own history.
    pub allow_existing_section: bool,
    /// Normally [`MAX_CLAIM_ATTEMPTS`]; a knob so tests can prove the cap.
    pub max_attempts: u32,
    /// The published commit (full sha) the release is cut from: on `origin/main`, the release commit's one parent, and where the cut tree sits until the claim lands.
    pub source: &'a str,
}

/// The ref that keeps a reservation's release commit safe from gc until [`claim`] puts it on main.
pub const RESERVED_REF: &str = "refs/aterm-release/reserved";

/// A release's identity, fixed by [`reserve`] before anything is built from it: what the binary is stamped with, what `verify release` judges, and, once [`claim`] lands it, what ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reserved {
    /// `n`: `max(origin tail + 1, now)` when reserved.
    pub build: u64,
    /// The release commit C (full sha): the published commit plus `line` appended to its ledger and its rolled changelog, nothing else.
    pub commit: String,
    /// The ledger line C appends, e.g. "1783918101 0.2.0".
    pub line: String,
}

/// A verified claim: the number is on origin/main, tail-checked byte for byte, and safe to bake into artifacts.
#[derive(Debug)]
pub struct Claim {
    pub build: u64,
    /// The release commit (full sha), the artifacts' source; the cut tree is left on it, detached.
    pub commit: String,
    /// The commit that landed on origin/main: the release commit itself when main had not moved past the published commit, else a merge of it onto main's tip.
    pub landed: String,
    /// The exact ledger line appended, e.g. "1783918101 0.2.0".
    pub ledger_line: String,
}

/// The changelog texts a claim writes, from `(published commit's, main tip's)` CHANGELOG.md: `(release commit's, main's)`.
pub type ClaimChangelogs<'a> = &'a dyn Fn(&str, &str) -> Result<(String, String)>;

/// Reserve the release's identity before anything is built: read origin's ledger tail, `n = max(tail + 1, plan.now)`, and make the release commit C (`plan.source` plus `n version` appended to its ledger and its rolled changelog, `changelogs`' first half) from git objects ([`commit_with`]), with no worktree, index or push.
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

/// Claim a reservation: main takes the release commit `reserved.commit` whatever its tip is, as a fast-forward when main has not moved past the published commit, else as a merge onto the tip (the tip's tree plus the same ledger line and the shipped notes moved out of `[Unreleased]`, `changelogs`' second half).
pub fn claim(
    git: &dyn GitRunner,
    worktree: &Path,
    plan: &ClaimPlan<'_>,
    reserved: &Reserved,
    changelogs: ClaimChangelogs<'_>,
) -> Result<Claim> {
    check_version_shape(plan.version)?;

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

    let mut base = show_origin_ledger(git)?;
    let mut attempt = 0u32;
    let landed = loop {
        attempt += 1;
        let tail_build = tail(&base)?.build;
        if tail_build >= n {
            return Err(Error::new(format!(
                "origin's ledger tail is build {tail_build}, at or past the reserved build {n} \
                 — another release claimed past this one while it was built and verified. \
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

        let push = git.git(&["push", "origin", &format!("{landed}:refs/heads/main")])?;
        if push.success() {
            break landed;
        }
        let push_err = push.stderr_utf8().trim().to_string();

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

        git_ok(git, &["fetch", "origin", "main"])?;

        if attempt >= plan.max_attempts {
            return Err(Error::new(format!(
                "ledger claim lost the push race {attempt} times — aborting with the \
                 checkout on the published commit; nothing was burned (losing this many CAS \
                 rounds needs a human look). last rejection: {push_err}"
            )));
        }

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

        base = show_origin_ledger(git)?;
    };

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
    let checkout = ["checkout", "-q", "--detach", release];
    git_ok(git, &[&BYTE_EXACT[..], &checkout].concat()).map_err(|e| unjournaled(e.to_string()))?;
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

/// `source` is a commit of `origin/main`: a claim lands on main.
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

/// The reservation is this claim's release commit: its one parent is the published commit,
/// its ledger is the published commit's plus the version's line, its changelog is the
/// published commit's rolled for the version ([`changelog::release_changelog`], on the
/// date its section names, or as it stands when it already carries the section), and
/// nothing else moved.
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

/// A commit made from git objects alone, with no worktree or index: `base`'s root tree with each top-level `files` entry's bytes replaced (each must already be a blob there), committed with `parents` and `message` under the repository's configured identity.
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
        let hash = ["hash-object", "-w", "--stdin", path.as_str()];
        let blob = git.git_input(&[&BYTE_EXACT[..], &hash].concat(), text.as_bytes())?;
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

/// `git show <rev>:<path>`, strict UTF-8: the bytes are re-used as a base to append to or
/// roll, so a lossy decode could silently rewrite them.
fn show_utf8(git: &dyn GitRunner, rev: &str, path: &str) -> Result<String> {
    let spec = format!("{rev}:{path}");
    let out = git_ok(git, &["show", &spec])?;
    String::from_utf8(out.stdout).map_err(|_| Error::new(format!("{spec} is not valid UTF-8")))
}

/// `git show origin/main:RELEASES.ledger`: the blob the claim's push compares and swaps against.
pub fn show_origin_ledger(git: &dyn GitRunner) -> Result<String> {
    show_utf8(git, "origin/main", LEDGER_FILE)
}

/// The release version is spliced into a tag name, a changelog heading, the DMG asset name and the ledger grammar, so it must be canonical `MAJOR.MINOR.PATCH` (a release is the workspace `MAJOR.MINOR.0`; see `VERSIONING.md`): three non-empty ASCII-digit components, no leading zero unless the component is `"0"`, so one version has one spelling. cli.rs applies the same check to version arguments up front.
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

/// Fixture plumbing every test binary shares: the `git` a fixture's own setup runs, the runner tests hand the code under test, and a fresh repository with its config.
#[cfg(test)]
pub(crate) mod testing {
    use std::fs;
    use std::path::{Path, PathBuf};

    pub(crate) use aterm_uds::fixture_git::git;

    /// The [`GitRunner`](super::GitRunner) a test hands the code under test:
    /// [`GitCli`](super::GitCli)'s `git -C <repo> <args…>`, run by [`git`] (the direct git,
    /// isolated from this machine) rather than the trampolined `git` on PATH.
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

    /// Append to the config of the fresh repository at `repo`, in one write, what one
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
            let (section, name) = key
                .split_once('.')
                .filter(|(_, name)| !name.contains('.'))
                .expect("a section.name key, no subsection");
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
