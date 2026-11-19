// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The verifier's remote boundary: GitHub goes through agh, local repositories through git.
//! Pinned reads import an exact observed OID; ordinary main refreshes reuse the checkout's
//! objects. Writes carry the expected ref and stop on an unknown outcome; one that sent nothing
//! (agh says so, or its write started no child: [`Failed::Unstarted`]) may be made again, and
//! one refused with no effect, by agh or by GitHub, for a reason another attempt does not cure
//! ([`Pushed::Refused`]) is not. A push of main or of the journal runs on [`run_mutation`]: its
//! work window's end, or an interrupt its caller reads, asks agh to stop, and its owner is kept
//! through agh's settlement, never killed. Neither a failed agh command nor an unrecognised URL
//! falls back to git.
//! The reads every host makes (`engine`'s main refresh) build everywhere; the writes, the commit
//! import and the deadline runners serve `verify land` and `verify watch` alone and build where
//! they do (unix).

use aterm_json::{Value, json};
use aterm_uds::spawnfd::Command;
use std::ffi::OsString;
use std::path::Path;
use std::process::Output;
#[cfg(unix)]
use std::time::{Duration, Instant};

/// The window one write gets: the longest credential grant agh issues
/// ([`agh::git::GRANT_MAX`]), beyond agh's own bounds on its write, the 60 s no-progress window
/// agh gives each git child of a push that names no `--timeout` and agh's patience with a push
/// that moved no remote ref ([`agh::git::GIT_BUDGET`]). agh opens its write's durable intent before its
/// re-verification of the pushed history and records a settlement when it ends, so agh ends its
/// own write: a write this tool killed would leave that intent open with no settlement at all,
/// refusing every later atomic ref write of the repository from this Mac. A push of main
/// (`land`'s `Wire::write`) has this work window from its own start, and the journal's pushes
/// share one from the first admitted ([`run_mutation`]): its end asks agh to stop and keeps the
/// owner through its settlement, so neither this window nor agh's per-child bound is a deadline
/// on the whole write's cleanup. A push agh is asked to stop, or ends for want of progress
/// (that window), once it may have sent its pack is settled Unknown (one whose transfer
/// stalled only where agh's re-read does not find it landed), which keeps the intent open and
/// refusing those writes until it is reconciled: once its agh and git are gone
/// (`agh::Journal::closable`), the next atomic write reads its refs from origin and closes it
/// only where they read wholly as before or wholly as after the write ([`IntentWait`]).
#[cfg(unix)]
pub(crate) const WRITE: Duration = agh::git::GRANT_MAX;
#[cfg(unix)]
const _: () = assert!(WRITE.as_secs() > agh::git::GIT_BUDGET.as_secs());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tool {
    Git,
    Agh,
}

/// How a remote step is run: [`run`], or [`run_unless`].
pub(crate) type Runner<'a> = dyn Fn(Tool, &[OsString]) -> Result<Output, String> + 'a;

/// How a write is run: [`run_mutation`] in production.
#[cfg(unix)]
pub(crate) type Writer<'a> = dyn Fn(Tool, &[OsString]) -> Result<Output, Failed> + 'a;

/// Why a remote step gave no output: whether its child started, which decides what a write
/// that gave none means.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failed {
    /// No child started, so nothing was sent: the write was refused before it started (the run
    /// took an interrupt first, or a push's work window or admission had ended:
    /// [`run_mutation`]), or its command could not be made or started. A push whose write ends
    /// so is one that sent nothing ([`Pushed::Unsent`]), never one of unknown outcome.
    Unstarted(String),
    /// Its child started and gave no whole output: its output could not be read whole, its
    /// step's own hang detector ended it, or its work window or an interrupt asked it to stop. A
    /// write that ends so has an unknown outcome.
    Ended(String),
}

#[cfg(unix)]
impl Failed {
    /// The same failure, its reason rewritten by `say`.
    pub(crate) fn map(self, say: impl FnOnce(String) -> String) -> Self {
        match self {
            Self::Unstarted(why) => Self::Unstarted(say(why)),
            Self::Ended(why) => Self::Ended(say(why)),
        }
    }

    /// The reason alone, for a caller that decides nothing by whether the child started.
    pub(crate) fn why(self) -> String {
        match self {
            Self::Unstarted(why) | Self::Ended(why) => why,
        }
    }
}

/// The caller's explicit ownership policy for a mutation, separate from remote discovery.
#[cfg(unix)]
pub(crate) struct WriteRunner<'a>(pub(crate) &'a Writer<'a>);

#[cfg(unix)]
#[path = "remote_mutation.rs"]
mod mutation;
#[cfg(unix)]
pub(crate) use mutation::run as run_mutation;

#[cfg(unix)]
pub(crate) fn run(
    root: &Path,
    git: &dyn Fn(&Path) -> Command,
    tool: Tool,
    args: &[OsString],
    until: Instant,
) -> Result<Output, String> {
    let mut cmd = command(root, git, tool, args)?;
    aterm_uds::deadline::output_until_complete_stdout(&mut cmd, until)
        .map_err(|e| format!("{tool:?} remote operation: {e}"))
}

/// [`run`], ended once `stopped` reads `true` (read at least every
/// [`aterm_uds::deadline::STOP_TICK`]): an ended step publishes no output and is an error.
#[cfg(unix)]
pub(crate) fn run_unless(
    root: &Path,
    git: &dyn Fn(&Path) -> Command,
    tool: Tool,
    args: &[OsString],
    until: Instant,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, String> {
    let mut cmd = command(root, git, tool, args)?;
    aterm_uds::deadline::output_until_complete_stdout_from_unless(
        &mut cmd,
        aterm_uds::spawnfd::Stdio::null(),
        until,
        stopped,
    )
    .map_err(|e| format!("{tool:?} remote operation: {e}"))
}

/// The command [`run`] runs, for a caller that waits on it its own way.
pub(crate) fn command(
    root: &Path,
    git: &dyn Fn(&Path) -> Command,
    tool: Tool,
    args: &[OsString],
) -> Result<Command, String> {
    let mut cmd = match tool {
        Tool::Git => {
            let mut cmd = git(root);
            // Local discovery and snapshot reads answer for this checkout, even
            // when the caller inherited another repository's Git environment.
            for (key, _) in std::env::vars_os() {
                if key.as_encoded_bytes().starts_with(b"GIT_") {
                    cmd.env_remove(key);
                }
            }
            cmd.env("GIT_NO_REPLACE_OBJECTS", "1")
                .env("GIT_NO_LAZY_FETCH", "1")
                .env("GIT_TERMINAL_PROMPT", "0");
            cmd
        }
        Tool::Agh => agh_command(root, false)?,
    };
    cmd.args(args);
    Ok(cmd)
}

fn agh_command(root: &Path, owned: bool) -> Result<Command, String> {
    #[cfg(target_os = "linux")]
    let image = std::path::PathBuf::from("/proc/self/exe");
    #[cfg(target_os = "macos")]
    let image = crate::remote_host::image()?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let image = std::env::current_exe()
        .map_err(|e| format!("cannot name the remote transport host: {e}"))?;
    let mut cmd = aterm_uds::spawnfd::command(image);
    cmd.arg(crate::remote_cli::FLAG);
    if owned {
        cmd.arg("owned");
    }
    cmd.arg(std::process::id().to_string()).current_dir(root);
    Ok(cmd)
}

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Local(String),
    Github(String),
}

fn kind(url: &str) -> Result<Kind, String> {
    let tail = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"));
    if let Some(tail) = tail {
        let tail = tail.strip_suffix('/').unwrap_or(tail);
        let slug = tail.strip_suffix(".git").unwrap_or(tail);
        let parts: Vec<_> = slug.split('/').collect();
        if parts.len() == 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && *part != "."
                    && *part != ".."
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            })
        {
            return Ok(Kind::Github(slug.to_ascii_lowercase()));
        }
    } else if !url.is_empty()
        && !url.starts_with('-')
        && !past_drive(url).contains(':')
        && !url.chars().any(char::is_control)
    {
        return Ok(Kind::Local(url.to_owned()));
    }
    // A remote URL can contain credentials; never include it in a diagnostic.
    Err("remote must name an unambiguous GitHub OWNER/REPO or a local repository path".into())
}

/// The part of a local repository's spelling in which a colon makes git read it as `host:path`,
/// a remote it reaches over ssh: all of it.
#[cfg(not(windows))]
const fn past_drive(url: &str) -> &str {
    url
}

/// The part of a local repository's spelling in which a colon makes git read it as `host:path`,
/// a remote it reaches over ssh: on Windows all but the drive an absolute path starts with
/// (`C:\…`, `C:/…`), which git reads as a local path. A drive with no separator after it
/// (`C:repo`, relative to that drive's working directory) is left whole, so it is refused.
#[cfg(windows)]
fn past_drive(url: &str) -> &str {
    match url.as_bytes() {
        [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic() => &url[2..],
        _ => url,
    }
}

fn words(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// A changed URL or an insteadOf rewrite cannot turn a local operation into network traffic.
fn local_words(args: &[&str]) -> Vec<OsString> {
    let mut command = words(&[
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.file.allow=always",
    ]);
    command.extend(words(args));
    command
}

fn why(tool: Tool, out: &Output) -> String {
    format!(
        "{tool:?} remote operation exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

fn success(tool: Tool, out: Output) -> Result<Vec<u8>, String> {
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(why(tool, &out))
    }
}

#[derive(Clone, Copy)]
enum ReadStep {
    FetchUrl,
    PushUrl,
    GithubRef,
    GithubMainSnapshot,
    GithubSnapshot,
    LocalRef,
    LocalSnapshot,
    CommitInspection,
}

impl ReadStep {
    const fn context(self) -> &'static str {
        match self {
            Self::FetchUrl => "remote fetch URL discovery",
            Self::PushUrl => "remote push URL discovery",
            Self::GithubRef => "GitHub ref resolution",
            Self::GithubMainSnapshot => "GitHub main snapshot import",
            Self::GithubSnapshot => "GitHub exact snapshot import",
            Self::LocalRef => "local ref resolution",
            Self::LocalSnapshot => "local exact snapshot import",
            Self::CommitInspection => "local commit inspection",
        }
    }
}

/// Name the semantic read without adding its arguments or private input to the error.
fn read_step<T>(step: ReadStep, read: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    read().map_err(|why| format!("{}: {why}", step.context()))
}

pub(crate) struct Remote<'a> {
    root: &'a Path,
    name: &'a str,
    kind: Kind,
    run: &'a Runner<'a>,
}

#[cfg(unix)]
pub(crate) enum Pushed {
    Done,
    Rejected(String),
    /// Another atomic batch prevented this attempt from sending anything.
    Blocked {
        intent: agh::IntentId,
        why: String,
    },
    /// Nothing was sent, so the same write may be made again: its write started no child
    /// ([`Failed::Unstarted`]: the run took an interrupt first, its work window or admission had
    /// ended, a GitHub push's input file could not be written, or git or agh could not be made or
    /// started, so no intent was opened), or
    /// agh could not run the write, says that it sent nothing, and settled its own intent closed
    /// (a network, server or rate failure before the send, or a stop agh took before it
    /// dispatched git: [`unsent`]).
    Unsent(String),
    /// The write was refused with no effect, for a reason another attempt does not cure
    /// ([`refused_with_no_effect`]): by agh before it sent anything (no credential or a 401, no
    /// permission, input or usage it refuses, its own gate), or by GitHub, which rejected the push
    /// it received (a branch rule, push protection, a token without the workflow scope). Nothing
    /// was written and no intent is left open, so the push is not made again and leaves nothing
    /// to settle.
    Refused(String),
}

/// How a wait on another writer's open atomic intent ended ([`Remote::wait_for_intent`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntentWait {
    /// The intent is closed (settled, found or retracted), so it stops no write.
    Closed,
    /// The intent is open and its writer, agh and its git child alike, is gone
    /// (`agh::Journal::closable`). That permits the next atomic write to read the intent's
    /// recorded refs from origin before it begins; it closes the intent only where every ref
    /// reads as before its write (retracted) or every one as after it (found), and holds it
    /// where they are mixed or cannot be read, or where that write's agh does not judge the
    /// writer gone (its boot or process start unread). agh refuses that write with the same
    /// diagnostic in all three cases ([`blocking_intent`]), so a later write the intent still
    /// refuses cannot tell origin's settled answer from one a later attempt may change.
    Reconcilable,
    /// The intent is open, records no writer (`writer` is null: an agh from before writers were
    /// recorded began it), and is settled with an unknown outcome, so its agh finished that
    /// write. agh reconciles only a recorded writer's intent from origin, so no wait and no
    /// later write closes it; once origin shows what that write did, `agh intent retract`
    /// does.
    Held,
}

/// The outcome of a push whose write ([`Writer`]) gave no output: one that started no child sent
/// nothing ([`Pushed::Unsent`]); one that started has an unknown outcome, an error.
#[cfg(unix)]
fn unanswered(failed: Failed) -> Result<Pushed, String> {
    match failed {
        Failed::Unstarted(why) => Ok(Pushed::Unsent(why)),
        Failed::Ended(why) => Err(why),
    }
}

/// agh's one machine diagnostic of a failed command: the fields a caller decides by.
#[cfg(unix)]
#[derive(serde::Deserialize)]
struct Refusal {
    v: u8,
    exit: u8,
    kind: String,
    sub: Option<String>,
    side_effect: String,
    repo: Option<String>,
    intent: Option<String>,
}

/// Read the machine diagnostic, never its human `why` or suggested command: `None` unless
/// stderr holds exactly one, for this exit.
#[cfg(unix)]
fn refusal(out: &Output) -> Option<Refusal> {
    #[derive(serde::Deserialize)]
    struct Diagnostic {
        error: Refusal,
    }
    let mut records = out
        .stderr
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|line| line.starts_with(b"{") || line.starts_with(b"["))
        .map(aterm_json::from_slice::<Diagnostic>);
    let error = records.next()?.ok()?.error;
    if records.next().is_some() {
        return None;
    }
    (error.v == 1 && out.status.code() == Some(i32::from(error.exit))).then_some(error)
}

/// The foreign intent whose open entry refused this write before it sent anything. Unknown
/// outcomes without this exact pre-send refusal remain fatal, including older agh diagnostics.
#[cfg(unix)]
fn blocking_intent(out: &Output, repo: &str) -> Option<agh::IntentId> {
    let error = refusal(out)?;
    if error.exit == 13
        && error.kind == "outcome_unknown"
        && error.sub.as_deref() == Some("intent_open")
        && error.side_effect == "none"
        && error.repo.as_deref() == Some(repo)
    {
        agh::IntentId::parse(error.intent.as_deref()?)
    } else {
        None
    }
}

/// Whether agh's diagnostic says the write could not run and sent nothing: one of its kinds a
/// later attempt may cure (could not run, a rate limit, a server failure, or a stop agh took
/// before it dispatched git: a signal sent to agh itself, or its cancellation watcher's own
/// stop; one this run asked for through the mutation's control is the capture's damage,
/// [`run_mutation`], and never read here), with no side effect, for this repository where it
/// names one. agh settles such a write's intent itself (never sent), so the journal holds
/// nothing open for it. Another kind with no effect is a refusal ([`refused_with_no_effect`]);
/// any other failure keeps its own meaning.
#[cfg(unix)]
fn unsent(out: &Output, repo: &str) -> bool {
    refusal(out).is_some_and(|error| {
        matches!(
            (error.exit, error.kind.as_str()),
            (3, "could_not_run") | (11, "rate_limited") | (12, "server") | (130, "interrupted")
        ) && error.side_effect == "none"
            && error.repo.as_deref().is_none_or(|named| named == repo)
    })
}

/// Whether agh's diagnostic is a refusal with no effect that another attempt does not cure: no
/// side effect (nothing was written, though the push may have been sent), for this repository
/// where it names one, of any kind but the four [`unsent`] makes again and an unknown outcome
/// (exit 13, which agh never settles as not sent, and whose pre-send intent refusal is
/// [`blocking_intent`]'s). agh refuses so itself before it opens an intent (its login binding,
/// route, permission and input checks), or settles the one it opened closed, never sent or
/// refused, GitHub's own rejection of the push it received included (a `[remote rejected]` rule,
/// the workflow scope), so nothing is left open. An expected-ref conflict (exit 9) is read
/// before this, as a rejection.
#[cfg(unix)]
fn refused_with_no_effect(out: &Output, repo: &str) -> bool {
    refusal(out).is_some_and(|error| {
        !matches!(
            error.kind.as_str(),
            "could_not_run" | "rate_limited" | "server" | "interrupted" | "outcome_unknown"
        ) && error.side_effect == "none"
            && error.repo.as_deref().is_none_or(|named| named == repo)
    })
}

impl<'a> Remote<'a> {
    pub(crate) fn open(root: &'a Path, name: &'a str, run: &'a Runner<'a>) -> Result<Self, String> {
        let url = |push| {
            let step = if push {
                ReadStep::PushUrl
            } else {
                ReadStep::FetchUrl
            };
            read_step(step, || {
                let mut args = words(&["remote", "get-url", "--all"]);
                if push {
                    args.push("--push".into());
                }
                args.push(name.into());
                let out = run(Tool::Git, &args)?;
                // git may echo a malformed configured URL on failure. Keep this read's error local.
                if !out.status.success() {
                    return Err("cannot read the remote's expanded URL".to_owned());
                }
                let text =
                    std::str::from_utf8(&out.stdout).map_err(|_| "remote URL is not UTF-8")?;
                let lines: Vec<_> = text.lines().collect();
                if lines.len() != 1 {
                    return Err("remote must have exactly one fetch URL and one push URL".into());
                }
                kind(lines[0])
            })
        };
        let kind = url(false)?;
        if kind != url(true)? {
            return Err("remote fetch and push URLs must address the same repository".into());
        }
        Ok(Self {
            root,
            name,
            kind,
            run,
        })
    }

    /// Reject a source agh cannot push before the caller spends time judging it.
    /// Local Git remotes do not impose agh's complete-history requirement.
    #[cfg(unix)]
    pub(crate) fn admit_push_source(&self) -> Result<(), String> {
        if matches!(self.kind, Kind::Local(_)) {
            return Ok(());
        }
        let stdout = success(
            Tool::Git,
            (self.run)(Tool::Git, &words(&["rev-parse", "--is-shallow-repository"]))?,
        )?;
        agh::git::require_complete_source(&stdout).map_err(|error| error.to_string())
    }

    /// Refresh ordinary live main through agh's incremental snapshot operation: the commit
    /// this refresh's own fetch wrote, which agh prints. Its ref, `refs/agh/fetched-main`, is
    /// never read back: every worktree of the clone shares it, and a sibling's fetch that read
    /// an older main can overwrite it after this one's write, so a read of it can see main go
    /// backwards. Pinned provenance reads still use [`Self::fetch`]. A failed refresh never
    /// turns a previous local snapshot into a successful current observation.
    pub(crate) fn refresh_main(&self) -> Result<String, String> {
        let Kind::Github(slug) = &self.kind else {
            return self
                .fetch("refs/heads/main")?
                .ok_or_else(|| "remote main is absent".into());
        };
        const INTO: &str = "refs/agh/fetched-main";
        let mut args = words(&["repo", "fetch-main", slug]);
        args.push(self.root.as_os_str().to_owned());
        let out = read_step(ReadStep::GithubMainSnapshot, || {
            success(Tool::Agh, (self.run)(Tool::Agh, &args)?)
        })?;
        let value: Value =
            aterm_json::from_slice(&out).map_err(|e| format!("agh fetch-main: {e}"))?;
        if value["ref"].as_str() != Some(INTO) {
            return Err("agh's main snapshot names another ref".into());
        }
        let tip = value["oid"]
            .as_str()
            .ok_or("agh's main snapshot names no commit")?;
        if !aterm_land_trailer::object_id(tip) {
            return Err("agh's main snapshot is not one exact object ID".into());
        }
        let kind = read_step(ReadStep::CommitInspection, || {
            success(
                Tool::Git,
                (self.run)(Tool::Git, &words(&["cat-file", "-t", tip]))?,
            )
        })?;
        if kind != b"commit\n" {
            return Err("agh's main snapshot is not a commit object".into());
        }
        Ok(tip.into())
    }

    /// Import the exact commit a ref names, or report an absent ref. No branch/index is moved.
    pub(crate) fn fetch(&self, reference: &str) -> Result<Option<String>, String> {
        match &self.kind {
            Kind::Local(_) => {
                let out = read_step(ReadStep::LocalRef, || {
                    success(
                        Tool::Git,
                        (self.run)(
                            Tool::Git,
                            &local_words(&["ls-remote", self.name, reference]),
                        )?,
                    )
                })?;
                let text = std::str::from_utf8(&out).map_err(|_| "remote refs are not UTF-8")?;
                let mut found = text
                    .lines()
                    .filter_map(|line| line.split_once('\t'))
                    .filter(|(_, name)| *name == reference);
                let Some((sha, _)) = found.next() else {
                    return Ok(None);
                };
                if found.next().is_some() || !aterm_land_trailer::object_id(sha) {
                    return Err("remote did not return one exact commit ID".into());
                }
                read_step(ReadStep::LocalSnapshot, || {
                    success(
                        Tool::Git,
                        (self.run)(
                            Tool::Git,
                            &local_words(&["fetch", "--quiet", "--no-tags", self.name, sha]),
                        )?,
                    )
                })?;
                Ok(Some(sha.to_owned()))
            }
            Kind::Github(slug) => {
                if !reference.starts_with("refs/") {
                    return Err("remote ref must start with refs/".into());
                }
                let out = read_step(ReadStep::GithubRef, || {
                    success(
                        Tool::Agh,
                        (self.run)(
                            Tool::Agh,
                            &words(&["repo", "refs", slug, "--ref", reference]),
                        )?,
                    )
                })?;
                let value: Value =
                    aterm_json::from_slice(&out).map_err(|e| format!("agh refs: {e}"))?;
                let value = value.get("answer").unwrap_or(&value);
                let rows = value
                    .as_array()
                    .ok_or("agh did not return a complete ref array")?;
                let mut found = rows
                    .iter()
                    .filter(|row| row["ref"].as_str() == Some(reference));
                let Some(row) = found.next() else {
                    return Ok(None);
                };
                let sha = row["oid"].as_str().ok_or("agh ref has no object ID")?;
                if found.next().is_some() || !aterm_land_trailer::object_id(sha) {
                    return Err("agh did not return one exact commit ID".into());
                }
                // Import the immutable observation without publishing a shared cache ref:
                // sibling readers may import different snapshots at the same time.
                self.agh_commit(slug, sha, None)?;
                Ok(Some(sha.to_owned()))
            }
        }
    }

    /// Import the exact commit `sha` and what it reaches, publishing it at `into` alone: no
    /// branch, index or checkout moves. Unlike [`Self::fetch`], `sha` need be no ref's tip, as a
    /// superproject's gitlink can pin any commit the remote holds. A local remote is asked over
    /// the file transport, GitHub through agh.
    #[cfg(unix)]
    pub(crate) fn import_commit(&self, sha: &str, into: &str) -> Result<(), String> {
        if !aterm_land_trailer::object_id(sha) {
            return Err("a commit import needs one exact object ID".into());
        }
        match &self.kind {
            Kind::Local(_) => {
                let wanted = format!("+{sha}:{into}");
                read_step(ReadStep::LocalSnapshot, || {
                    success(
                        Tool::Git,
                        (self.run)(
                            Tool::Git,
                            &local_words(&[
                                "fetch",
                                "--quiet",
                                "--no-tags",
                                "--no-write-fetch-head",
                                "--recurse-submodules=no",
                                self.name,
                                &wanted,
                            ]),
                        )?,
                    )
                })?;
                self.require_commit(sha)
            }
            Kind::Github(slug) => self.agh_commit(slug, sha, Some(into)),
        }
    }

    /// Import commit `sha` from GitHub's `slug`, optionally publishing the requested ref,
    /// then require that the object arrived as a commit.
    fn agh_commit(&self, slug: &str, sha: &str, into: Option<&str>) -> Result<(), String> {
        let dir = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
        let input = dir.path().join("fetch.json");
        let (flag, payload) = match into {
            Some(into) => (
                "--input",
                json!([{"source":sha,"into":into,"expected_oid":sha}]),
            ),
            None => ("--objects", json!([sha])),
        };
        std::fs::write(&input, payload.to_string()).map_err(|e| e.to_string())?;
        let mut args = words(&["repo", "fetch", slug]);
        args.extend([
            self.root.as_os_str().to_owned(),
            flag.into(),
            input.into_os_string(),
        ]);
        read_step(ReadStep::GithubSnapshot, || {
            success(Tool::Agh, (self.run)(Tool::Agh, &args)?)
        })?;
        self.require_commit(sha)
    }

    /// `sha` is a commit object in this repository.
    fn require_commit(&self, sha: &str) -> Result<(), String> {
        let object = read_step(ReadStep::CommitInspection, || {
            success(
                Tool::Git,
                (self.run)(Tool::Git, &words(&["cat-file", "-t", sha]))?,
            )
        })?;
        if object == b"commit\n" {
            Ok(())
        } else {
            Err(format!("{sha} is not a commit object"))
        }
    }

    /// Publish exact local objects, expected ref included, as one write. agh's write makes
    /// every check its dry run makes (routing, the source graph, the expected ref) before it
    /// sends, so no rehearsal comes first: each one would repeat agh's strict re-verification of
    /// the pushed history while the expected ref is free to move. A local repository's push
    /// leases the expected ref too, so a ref moved anywhere else, back to an ancestor of `after`
    /// included (a fast-forward plain git would take), refuses it. An unknown outcome stops the
    /// caller; only a typed refusal which sent nothing permits observation of the blocking
    /// intent ([`Pushed::Blocked`]) or the same write again ([`Pushed::Unsent`]), as does a
    /// write that started no child ([`Failed::Unstarted`]), a GitHub push whose input file could
    /// not be written included. Any other typed refusal agh says had no effect (its own
    /// authentication, permission, input or usage refusal, or GitHub's rejection of the push it
    /// received) is [`Pushed::Refused`]: neither made again nor of unknown outcome.
    ///
    /// Publish using an explicitly supplied mutation owner (`write`, [`run_mutation`] in
    /// production); discovery continues to use the remote's reader, so no read's deadline ends
    /// the push. No command text is used to guess which lifetime a step needs. The push names no
    /// `--timeout`, so agh bounds each git child of its write by its 60 s no-progress window in
    /// place of a wall bound: it ends one whose process tree is still that long, or whose HTTP
    /// transfer moves less than a byte a second that long (a remote that holds the connection
    /// open and never answers, which the tree cannot show), so a stalled push ends inside agh
    /// long before the owner's work window would ask it to stop; once its pack may have gone,
    /// agh never pushes it again: a push a re-read finds landed is [`Pushed::Done`], and any
    /// other is settled Unknown, its intent open until it is reconciled, and is an error of
    /// unknown outcome here.
    #[cfg(unix)]
    pub(crate) fn push_with(
        &self,
        write: WriteRunner<'_>,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<Pushed, String> {
        self.push_from(write, &aterm_tempfile::tempdir, reference, before, after)
    }

    /// [`Self::push_with`], a GitHub push's input file written in the directory `scratch` makes:
    /// one that cannot be made, or an input that cannot be written in it, is a write that started
    /// no child ([`Failed::Unstarted`]), so the push sent nothing ([`Pushed::Unsent`]).
    #[cfg(unix)]
    fn push_from(
        &self,
        write: WriteRunner<'_>,
        scratch: &dyn Fn() -> std::io::Result<aterm_tempfile::TempDir>,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<Pushed, String> {
        match &self.kind {
            Kind::Local(_) => {
                let destination = format!("{after}:{reference}");
                let lease = format!("--force-with-lease={reference}:{}", before.unwrap_or(""));
                let out = match (write.0)(
                    Tool::Git,
                    &local_words(&["push", "--porcelain", &lease, self.name, &destination]),
                ) {
                    Ok(out) => out,
                    Err(failed) => return unanswered(failed),
                };
                Ok(if out.status.success() {
                    Pushed::Done
                } else {
                    Pushed::Rejected(format!(
                        "git push {} {destination}: {}",
                        self.name,
                        String::from_utf8_lossy(&out.stderr).trim()
                    ))
                })
            }
            Kind::Github(slug) => {
                let unwritten = |error: std::io::Error| {
                    unanswered(Failed::Unstarted(format!(
                        "Agh remote write: its input could not be written: {error}; nothing was \
                         sent"
                    )))
                };
                let dir = match scratch() {
                    Ok(dir) => dir,
                    Err(error) => return unwritten(error),
                };
                let input = dir.path().join("push.json");
                let before = before.unwrap_or("0000000000000000000000000000000000000000");
                if let Err(error) = std::fs::write(
                    &input,
                    json!([{"ref":reference,"before":before,"after":after}]).to_string(),
                ) {
                    return unwritten(error);
                }
                let mut args = words(&["repo", "push", slug]);
                args.extend([
                    self.root.as_os_str().to_owned(),
                    "--input".into(),
                    input.into_os_string(),
                ]);
                let out = match (write.0)(Tool::Agh, &args) {
                    Ok(out) => out,
                    Err(failed) => return unanswered(failed),
                };
                if out.status.success() {
                    return Ok(Pushed::Done);
                }
                let why = why(Tool::Agh, &out);
                if out.status.code() == Some(9) {
                    Ok(Pushed::Rejected(why))
                } else if let Some(intent) = blocking_intent(&out, slug) {
                    Ok(Pushed::Blocked { intent, why })
                } else if unsent(&out, slug) {
                    Ok(Pushed::Unsent(why))
                } else if refused_with_no_effect(&out, slug) {
                    Ok(Pushed::Refused(why))
                } else if out.status.code() == Some(13) {
                    Err(format!(
                        "{why}; outcome unknown: reconcile agh's intent before another write"
                    ))
                } else {
                    Err(why)
                }
            }
        }
    }

    /// Observe only the exact foreign journal entry which refused our pre-send attempt, until
    /// it is closed, its dead writer can be reconciled by the next atomic write (`closable`),
    /// when the push is made again, or it is held for good: which of the three ended the wait
    /// ([`IntentWait`]). The caller owns the deadline; neither journal reads nor superseding
    /// writers extend it.
    #[cfg(unix)]
    pub(crate) fn wait_for_intent(
        &self,
        intent: &agh::IntentId,
        pause: &dyn Fn() -> Result<(), String>,
    ) -> Result<IntentWait, String> {
        /// Whether the document records a writer: `Some(false)` for `"writer": null`, `None`
        /// where the agh that printed it prints no `writer` at all.
        fn recorded<'de, D: serde::Deserializer<'de>>(writer: D) -> Result<Option<bool>, D::Error> {
            <Option<serde::de::IgnoredAny> as serde::Deserialize>::deserialize(writer)
                .map(|writer| Some(writer.is_some()))
        }
        let Kind::Github(slug) = &self.kind else {
            return Err("a local remote has no agh intent".into());
        };
        let intent = intent.as_str();
        loop {
            let args = words(&["intent", "show", slug, intent, "--format", "json"]);
            let bytes = success(Tool::Agh, (self.run)(Tool::Agh, &args)?)?;
            #[derive(serde::Deserialize)]
            struct Intent {
                id: String,
                repo: String,
                op: String,
                key: String,
                open: bool,
                /// agh's next atomic write can reconcile it from origin (`agh::Journal::closable`).
                #[serde(default)]
                closable: bool,
                /// Whether it records its writer ([`recorded`]).
                #[serde(default, deserialize_with = "recorded")]
                writer: Option<bool>,
                /// How its writer settled it, `None` before that.
                #[serde(default)]
                settled: Option<String>,
            }
            let doc: Intent = aterm_json::from_slice(&bytes)
                .map_err(|_| "agh intent returned invalid JSON".to_owned())?;
            if doc.id != intent
                || doc.repo != *slug
                || doc.op != "ref.update-atomic"
                || doc.key != "atomic refs"
            {
                return Err("agh intent identity does not match the refused atomic push".into());
            }
            if !doc.open {
                return Ok(IntentWait::Closed);
            }
            if doc.closable {
                return Ok(IntentWait::Reconcilable);
            }
            if doc.writer == Some(false) && doc.settled.is_some() {
                return Ok(IntentWait::Held);
            }
            pause()?;
        }
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
