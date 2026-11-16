// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One checkout's alignment with its origin (docs/DESIGN-init-system-2026-10-20.md §4.3, §4.4): the
//! one function `develop.checkout@<path>` and the GitHub item's existing checkouts share.

use crate::model::{Probe, User};
use crate::port::{Cmd, Ran, RunFailed, System};
use crate::repository_progress::{FailureKind, PreservationReason, Progress};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

pub(crate) mod facts;
pub(crate) mod lazy_fetch;

pub(crate) use facts::Facts;
pub(crate) use lazy_fetch::NoLazyFetch;

/// A local read, or one `ls-remote`. A transfer (a fetch, a merge that may fetch) has no
/// wall-clock budget: [`System::run_repository`] bounds it by its progress. An `ls-remote` keeps
/// this one: Git writes nothing until it holds the remote's whole advertisement, which is small,
/// so a progress window would bound it by the same five minutes. A local step that writes a
/// worktree under `GIT_NO_LAZY_FETCH=1` ([`git`]), such as an unfinished checkout's first
/// checkout, keeps it too: it reaches no remote, so its time is the disk's.
const READ_BUDGET: Duration = Duration::from_secs(300);
/// Nothing else runs under a git that changes a ref or a worktree, for this one run: no hook,
/// fsmonitor or automatic maintenance (agh's [`agh::git::lazy_fetch::NO_SIDE_RUNS`], which the
/// lazy-fetch question runs under too), whose detached fork a drain would see holding the run's
/// stdout and refuse its output for ([`System::run_complete_stdout`]).
pub(crate) use agh::git::lazy_fetch::NO_SIDE_RUNS;

/// What a repository's config names that a git command there can start by itself, beyond what
/// [`NO_SIDE_RUNS`] stops: each hook (`hook.<name>.*`) and each
/// filter driver (`filter.<driver>.*`), whose `clean` runs whenever git hashes a worktree file an
/// attributes file assigns it to (a `status` or `diff` of a stat-dirty file, an index rewrite of
/// a racily clean one), in any scope the config reads, the empty name included: what the
/// record of the repository's facts lists of them ([`Facts::programs`]).
#[derive(Debug, Default)]
pub(crate) struct Programs {
    hooks: BTreeSet<String>,
    filters: BTreeSet<String>,
}

impl Programs {
    /// `cmd` with every one turned off for its run, carried in `GIT_CONFIG_*` so no name is
    /// parsed as an option: `hook.<name>.enabled=false`; and for a driver an empty `process`,
    /// which also stops its `clean` on git 2.54 and 2.56 (git takes `clean` only where no
    /// `process` is set), an empty `clean` should a git take it beside one, and
    /// `required=false`, without which a required driver turned off fails the command. So no
    /// attributes file (the worktree's, `.git/info/attributes`, `core.attributesFile`) starts a
    /// driver; a file one would have cleaned is read as it is on disk.
    pub(crate) fn off(&self, cmd: Cmd) -> Cmd {
        let mut off: Vec<(String, &str)> = self
            .hooks
            .iter()
            .map(|name| (format!("hook.{name}.enabled"), "false"))
            .collect();
        for name in &self.filters {
            off.push((format!("filter.{name}.clean"), ""));
            off.push((format!("filter.{name}.smudge"), ""));
            off.push((format!("filter.{name}.process"), ""));
            off.push((format!("filter.{name}.required"), "false"));
        }
        let mut cmd = cmd.env("GIT_CONFIG_COUNT", off.len().to_string());
        for (at, (key, value)) in off.into_iter().enumerate() {
            cmd = cmd
                .env(format!("GIT_CONFIG_KEY_{at}"), key)
                .env(format!("GIT_CONFIG_VALUE_{at}"), value);
        }
        cmd
    }
}

/// The installed git for `user` ([`crate::items::github::git_program_for_user`]: their managed
/// build, else native Git), in `path` when given, with no prompt (`GIT_TERMINAL_PROMPT=0`,
/// and an empty `GIT_ASKPASS`, which keeps a person's `core.askPass` from running), no
/// optional index lock (`GIT_OPTIONAL_LOCKS=0`, so a read never races the person's own git),
/// and no fetch of an object a partial clone lacks (`GIT_NO_LAZY_FETCH=1`, which git 2.45 and
/// later and the May 2024 security releases from 2.39.4 on honour, and an older git ignores,
/// which [`lazy_fetch::of`] tells apart): a read of a missing object fails rather than reaching
/// a remote by itself, outside aterm's credential route. Only a run that must complete a partial
/// clone's worktree, under [`network`], lifts it ([`lazily`]).
pub(crate) fn git(sys: &dyn System, user: &User, path: Option<&Path>) -> Cmd {
    let mut command = Cmd::new(crate::items::github::git_program_for_user(sys, user))
        .as_user(user)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .budget(READ_BUDGET);
    if let Some(path) = path {
        command = command.arg("-C").arg(path);
    }
    command
}

/// `command`, a [`network`] run that writes a worktree, free to fetch what a partial clone
/// lacks (a merge's or a submodule checkout's blobs) through the route it carries.
pub(crate) fn lazily(command: Cmd) -> Cmd {
    command.env("GIT_NO_LAZY_FETCH", "0")
}

/// [`git`] for a run that reaches a remote, or may (a partial clone's merge fetches what it
/// lacks, [`lazily`]): aterm's own credential route rides on it
/// ([`crate::git_credential::pinned`]), so no helper a file names runs for any host, the login
/// keychain's least of all, whatever the person's git settings say.
pub(crate) fn network(sys: &dyn System, user: &User, path: Option<&Path>) -> Cmd {
    crate::git_credential::pinned(
        git(sys, user, path),
        crate::init_github::helper(sys, user).as_ref(),
    )
}

/// Whether the checkout at `path` is an unfinished branch, from its record of facts
/// ([`Facts::unborn`]): an absent HEAD commit is an unfinished branch only when its symbolic
/// branch has no ref. A missing object, detached HEAD or unreadable ref is not an empty
/// checkout.
pub(crate) fn unborn(sys: &dyn System, user: &User, path: &Path) -> Result<Option<String>, String> {
    Facts::of(sys, user, path)?.unborn()
}

/// Preserve all owner work, including ignored files and an index without a commit.
/// This read starts no configured filter, hook or filesystem monitor.
pub(crate) fn virgin_unborn(
    sys: &dyn System,
    user: &User,
    path: &Path,
    branch: &str,
) -> Result<(), String> {
    match unborn_preservation(sys, user, path, branch)? {
        None => Ok(()),
        Some(why) => Err(why),
    }
}

/// Local owner work is a preservation reason; unreadable Git state remains unknown. The branch,
/// the hooks and filters its reads turn off and its upstream are its record's ([`Facts::of`]),
/// checked again after its files are read.
pub(crate) fn unborn_preservation(
    sys: &dyn System,
    user: &User,
    path: &Path,
    branch: &str,
) -> Result<Option<String>, String> {
    let facts = Facts::of(sys, user, path)?;
    if facts.unborn()?.as_deref() != Some(branch) {
        return Ok(Some("the unfinished branch changed; work preserved".into()));
    }
    let git = git(sys, user, Some(path));
    let programs = facts.programs()?;
    for args in [
        vec!["ls-files", "--stage", "-z"],
        vec![
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored",
        ],
    ] {
        let read = sys
            .run_complete_stdout(&programs.off(git.clone().args(NO_SIDE_RUNS).args(args)))
            .map_err(|why| why.to_string())?;
        if !read.success() {
            return Err("cannot inspect the unfinished checkout's index and working files".into());
        }
        if !read.stdout.is_empty() {
            return Ok(Some(
                "the unfinished checkout contains owner work; left untouched".into(),
            ));
        }
    }
    // An upstream it does not name is none; one it names is origin's same branch, once.
    let now = Facts::of(sys, user, path)?;
    for (key, expected) in [
        (format!("branch.{branch}.remote"), "origin".to_owned()),
        (
            format!("branch.{branch}.merge"),
            format!("refs/heads/{branch}"),
        ),
    ] {
        let named = now.values(&key);
        if !named.is_empty() && named != [expected] {
            return Ok(Some(
                "the unfinished branch has another upstream; left untouched".into(),
            ));
        }
    }
    Ok(None)
}

/// Fetch an admitted first commit and create only the still-unborn ordinary branch, its
/// progress published under the checkout's name. The fetch is a transfer bounded by its
/// progress ([`acquisition_transfer`]).
/// The caller owns the repository pass and revalidates source, origin and physical storage.
/// # Errors
/// What [`hydrate_observed`] says.
pub(crate) fn hydrate_unborn(
    sys: &dyn System,
    user: &User,
    path: &Path,
    branch: &str,
    tip: &str,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<(), AcquisitionFailure> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repository");
    let mut progress = Progress::new(name, path.to_owned());
    progress.placed = true;
    let result = hydrate_observed(sys, user, path, branch, tip, current, &mut progress);
    match &result {
        Ok(()) => progress.enter(crate::repository_progress::Phase::Ready),
        Err(failure) => failure.record_progress(&mut progress),
    }
    progress.usable = result.is_ok();
    sys.repository_progress(&progress);
    result
}

/// [`hydrate_unborn`], reporting its stages to `progress`.
/// # Errors
/// Authorization changed, owner work appeared, the fetch failed or stalled (a network failure),
/// the advertised commit moved, or the checkout was refused.
fn hydrate_observed(
    sys: &dyn System,
    user: &User,
    path: &Path,
    branch: &str,
    tip: &str,
    current: &(dyn Fn() -> Result<(), String> + Sync),
    progress: &mut Progress,
) -> Result<(), AcquisitionFailure> {
    use crate::repository_progress::Phase;
    current()?;
    virgin_unborn(sys, user, path, branch)?;
    let programs = Facts::of(sys, user, path)?.programs()?;
    let command = programs.off(network(sys, user, Some(path)).args(NO_SIDE_RUNS));
    current()?;
    progress.enter(Phase::Connecting);
    sys.repository_progress(progress);
    acquisition_transfer(
        sys,
        &command.clone().args([
            "fetch",
            "--progress",
            "--no-recurse-submodules",
            "--no-tags",
            "--",
            "origin",
            "refs/heads/*:refs/remotes/origin/*",
            "refs/tags/*:refs/tags/*",
        ]),
        "cannot fetch the unfinished checkout's first commit",
        progress,
    )?;
    current()?;
    virgin_unborn(sys, user, path, branch)?;
    let reference = format!("refs/remotes/origin/{branch}^{{commit}}");
    let found = sys
        .run_complete_stdout(&git(sys, user, Some(path)).args([
            "rev-parse",
            "--verify",
            &reference,
        ]))
        .map_err(|why| why.to_string())?;
    if !found.success() || found.out().trim() != tip {
        return Err(
            "the advertised first commit changed during fetch; waiting for another pass".into(),
        );
    }
    current()?;
    progress.enter(Phase::Checkout);
    sys.repository_progress(progress);
    // A local step, under READ_BUDGET rather than a progress window: `command` is a [`network`]
    // run, which keeps [`git`]'s `GIT_NO_LAZY_FETCH=1`, so the checkout reads only what the fetch
    // above brought and reaches no remote.
    let target = format!("refs/remotes/origin/{branch}");
    let merged = sys
        .run_complete_stdout(&command.args([
            "checkout",
            "--no-overwrite-ignore",
            "--no-recurse-submodules",
            "-b",
            branch,
            "--track",
            &target,
        ]))
        .map_err(|why| why.to_string())?;
    if !merged.success() {
        return Err("the first checkout was refused; owner work preserved".into());
    }
    current()?;
    let head = sys
        .run_complete_stdout(&git(sys, user, Some(path)).args([
            "rev-parse",
            "--verify",
            "HEAD^{commit}",
        ]))
        .map_err(|why| why.to_string())?;
    if !head.success() || head.out().trim() != tip {
        return Err("the first checkout has not reached its advertised commit".into());
    }
    Ok(())
}

/// Whether git's standard error says its transport could not reach the remote's host (no name
/// resolution, no route, nothing answering), as against a refusal from a host it did reach (no
/// access, no such repository): the one an item stands aside on, offline.
pub(crate) fn unreachable(stderr: &[u8]) -> bool {
    const SAID: [&str; 10] = [
        "Could not resolve host",
        "Resolving timed out",
        "Failed to connect to",
        "Couldn't connect to server",
        "Network is unreachable",
        "No route to host",
        "Connection timed out",
        "Operation timed out",
        "Connection refused",
        "nodename nor servname provided",
    ];
    let said = String::from_utf8_lossy(stderr);
    SAID.iter().any(|marker| said.contains(marker))
}

/// Why a checkout could not be read or moved: the line, which never holds a URL, and whether what
/// failed was reaching origin's host, so an item stands aside offline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unread {
    /// What could not be done.
    pub(crate) why: String,
    /// Origin's host could not be reached ([`fn@unreachable`]).
    pub(crate) offline: bool,
    /// An admitted network command failed, without classifying its remote reason.
    pub(crate) network: bool,
}

impl From<String> for Unread {
    fn from(why: String) -> Self {
        Self {
            why,
            offline: false,
            network: false,
        }
    }
}

impl Unread {
    /// A transfer that produced no result ([`System::run_repository`]): one that stalled is a
    /// network failure, its origin having stopped sending (never offline: the host answered
    /// before it stopped); any other a local one.
    pub(crate) fn unfinished(failed: &RunFailed, why: String) -> Self {
        Self {
            why,
            offline: false,
            network: failed.is_stall(),
        }
    }
}

/// An acquisition error retains whether an admitted network command actually failed.
/// A nonzero Git result alone never proves authentication refusal.
#[derive(Debug)]
pub(crate) enum AcquisitionFailure {
    Local(String),
    Network(String),
}

impl AcquisitionFailure {
    pub(crate) const fn is_network(&self) -> bool {
        matches!(self, Self::Network(_))
    }

    /// A transfer that produced no result ([`System::run_repository`]): one that stalled is a
    /// network failure, its origin having stopped sending; any other a local one.
    pub(crate) fn unfinished(failed: &RunFailed, why: String) -> Self {
        if failed.is_stall() {
            Self::Network(why)
        } else {
            Self::Local(why)
        }
    }

    /// Publish only the typed classification. Error text may contain transport diagnostics.
    pub(crate) fn record_progress(&self, progress: &mut Progress) {
        progress.fail(if self.is_network() {
            FailureKind::Network
        } else {
            FailureKind::Local
        });
    }
}

impl From<String> for AcquisitionFailure {
    fn from(why: String) -> Self {
        Self::Local(why)
    }
}

impl From<&str> for AcquisitionFailure {
    fn from(why: &str) -> Self {
        Self::Local(why.to_owned())
    }
}

impl std::fmt::Display for AcquisitionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(why) | Self::Network(why) => f.write_str(why),
        }
    }
}

/// Read one admitted network command. Its classification never depends on what Git said; the
/// failure's line ends with Git's own reason ([`git_failed`]), or with why the run produced
/// nothing.
/// # Errors
/// The process did not complete, or Git returned a nonzero status.
pub(crate) fn acquisition_read(
    sys: &dyn System,
    command: &Cmd,
    why: &str,
) -> Result<Vec<u8>, AcquisitionFailure> {
    let ran = sys
        .run_complete_stdout(command)
        .map_err(|failed| AcquisitionFailure::Local(format!("{why} ({failed})")))?;
    if !ran.success() {
        return Err(AcquisitionFailure::Network(git_failed(why, &ran)));
    }
    Ok(ran.stdout)
}

/// Run one admitted network transfer ([`System::run_repository`]), bounded by its progress and
/// reporting it to `progress`. One that stalled is a network failure, as is a nonzero Git
/// result, whose line ends with Git's own reason ([`git_failed`]); one that produced nothing for
/// any other reason is a local one ([`AcquisitionFailure::unfinished`]).
/// # Errors
/// The transfer did not complete, or Git returned a nonzero status.
pub(crate) fn acquisition_transfer(
    sys: &dyn System,
    command: &Cmd,
    why: &str,
    progress: &mut Progress,
) -> Result<Ran, AcquisitionFailure> {
    let ran = sys
        .run_repository(command, progress)
        .map_err(|failed| AcquisitionFailure::unfinished(&failed, format!("{why} ({failed})")))?;
    if !ran.success() {
        return Err(AcquisitionFailure::Network(git_failed(why, &ran)));
    }
    Ok(ran)
}

/// The most characters of Git's own words [`git_said`] keeps.
const SAID: usize = 240;

/// `why`, then Git's reason for the failed `ran`: the last line it wrote to standard error
/// ([`git_said`]), else how it ended.
pub(crate) fn git_failed(why: &str, ran: &crate::port::Ran) -> String {
    match git_said(&ran.stderr) {
        Some(said) => format!("{why}: {said}"),
        None => match ran.status {
            Some(status) => format!("{why} (git exited {status}, saying nothing)"),
            None => format!("{why} (git was ended by a signal)"),
        },
    }
}

/// Git's reason, from what it wrote to standard error, as one line for a failure's `why`: the
/// last line it marked `fatal:` or `error:`, else its last line (a progress meter's
/// carriage-return updates count as lines). The line never holds a URL or a credential
/// ([`Unread`]): each URL (`https://<user>:<token>@host/…`) or SSH address Git names, quoted
/// or not, is written `origin`, a path it quotes is cut to its file name, and any word shaped
/// as a GitHub token is dropped. Control characters become spaces, and the line is cut at
/// [`SAID`] characters. `None` where Git wrote nothing but space.
pub(crate) fn git_said(stderr: &[u8]) -> Option<String> {
    const TOKENS: [&str; 6] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"];
    let names_remote = |text: &str| {
        text.contains("://")
            || text
                .split_once('@')
                .is_some_and(|(_, host)| host.contains(':'))
    };
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let line = lines
        .iter()
        .rev()
        .find(|line| line.starts_with("fatal:") || line.starts_with("error:"))
        .or(lines.last())?;
    // The parts between single quotes, Git's quoting of what it names, are the odd ones.
    let mut unquoted = String::with_capacity(line.len());
    for (at, part) in line.split('\'').enumerate() {
        if at % 2 == 0 {
            unquoted.push_str(part);
        } else if names_remote(part) {
            unquoted.push_str("origin");
        } else if part.starts_with('/') {
            let name = Path::new(part).file_name().unwrap_or_default();
            unquoted.push('\'');
            unquoted.push_str(&name.to_string_lossy());
            unquoted.push('\'');
        } else {
            unquoted.push('\'');
            unquoted.push_str(part);
            unquoted.push('\'');
        }
    }
    let said = unquoted
        .split(' ')
        .map(|word| {
            let bare = word.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
            if names_remote(word) {
                "origin".to_owned()
            } else if TOKENS.iter().any(|prefix| bare.starts_with(prefix)) {
                "[token]".to_owned()
            } else {
                word.chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect()
            }
        })
        .collect::<Vec<String>>()
        .join(" ");
    let said = said.trim();
    if said.is_empty() {
        None
    } else if said.chars().count() <= SAID {
        Some(said.to_owned())
    } else {
        Some(format!("{}…", said.chars().take(SAID).collect::<String>()))
    }
}

/// Whether `url` names `github.com/<owner>/<name>` over HTTPS or SSH, with no credential in it
/// (`https://token@github.com/…` is another URL).
pub(crate) fn github_origin(url: &str, owner: &str, name: &str) -> bool {
    github_path(url).is_some_and(|path| path.eq_ignore_ascii_case(&format!("{owner}/{name}")))
}

/// Whether an ordinary Git push URL retains the exact GitHub repository. A public login
/// inserted by configured `pushInsteadOf` is accepted only when it is the independently
/// authenticated writer; passwords, token userinfo, another host and another path refuse.
#[must_use]
pub fn github_writer_origin(url: &str, owner: &str, name: &str, writer: &str) -> bool {
    if github_origin(url, owner, name) {
        return true;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let Some((user, host_path)) = rest.split_once('@') else {
        return false;
    };
    !writer.is_empty()
        && writer
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && user.eq_ignore_ascii_case(writer)
        && github_origin(&format!("https://{host_path}"), owner, name)
}

/// The `<owner>/<name>` a github.com URL names, without `.git`: the host read without regard
/// to case, as DNS reads it, and a trailing `/` or a `.git` in any case dropped, as GitHub
/// serves the same repository at each.
fn github_path(url: &str) -> Option<&str> {
    let url = url.trim().trim_end_matches('/');
    let path = [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
        "ssh://git@github.com:22/",
    ]
    .iter()
    .find_map(|prefix| {
        url.get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .map(|_| &url[prefix.len()..])
    })?;
    Some(
        path.len()
            .checked_sub(".git".len())
            .filter(|&at| {
                path.get(at..)
                    .is_some_and(|tail| tail.eq_ignore_ascii_case(".git"))
            })
            .map_or(path, |at| &path[..at]),
    )
}

/// A remote a checkout's origin may name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Remote {
    /// `github.com/<owner>/<name>`, over HTTPS or SSH.
    Github {
        /// The account or organization.
        owner: String,
        /// The repository.
        name: String,
    },
    /// Exactly this URL or path, read without a trailing `/` (a tree whose `repository` is not
    /// on GitHub, a test's scratch remote).
    Exact(String),
}

impl Remote {
    /// The remote `url` names: a GitHub repository when it is one, else that URL exactly.
    pub(crate) fn of(url: &str) -> Self {
        let url = url.trim().trim_end_matches('/');
        match github_path(url).and_then(|path| path.split_once('/')) {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Self::Github {
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                }
            }
            _ => Self::Exact(url.to_owned()),
        }
    }

    fn admits(&self, url: &str) -> bool {
        match self {
            Self::Github { owner, name } => github_origin(url, owner, name),
            Self::Exact(exact) => url.trim().trim_end_matches('/') == exact,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Github { owner, name } => format!("github.com/{owner}/{name}"),
            Self::Exact(exact) => exact.clone(),
        }
    }
}

/// The remotes a checkout's origin may be: any one of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Origin(pub(crate) Vec<Remote>);

impl Origin {
    fn admits(&self, url: &str) -> bool {
        self.0.iter().any(|remote| remote.admits(url))
    }

    fn describe(&self) -> String {
        let names: Vec<String> = self.0.iter().map(Remote::describe).collect();
        names.join(" or ")
    }
}

/// Existing human-readable detail plus a source-owned safe reason for repository snapshots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Preserved {
    why: String,
    reason: PreservationReason,
}

impl Preserved {
    fn new(reason: PreservationReason, why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            reason,
        }
    }
}

impl From<String> for Preserved {
    fn from(why: String) -> Self {
        Self::new(PreservationReason::Unrecorded, why)
    }
}

impl From<&str> for Preserved {
    fn from(why: &str) -> Self {
        Self::from(why.to_owned())
    }
}

impl std::ops::Deref for Preserved {
    type Target = str;

    fn deref(&self) -> &str {
        &self.why
    }
}

impl std::fmt::Display for Preserved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

/// Where a checkout stands against its origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stands {
    /// On origin's default branch, at its tip.
    Current {
        /// The branch.
        branch: String,
        /// The commit.
        head: String,
    },
    /// Clean, on the default branch, and an ancestor of its fetched tip: a fast-forward.
    Behind {
        /// The branch.
        branch: String,
        /// The tip it would move to.
        tip: String,
        /// How many commits.
        by: u64,
    },
    /// Clean, on the default branch, with no commit of its own, and origin's tip is not here yet: a
    /// fetch tells whether it is a fast-forward.
    Unfetched {
        /// The branch.
        branch: String,
        /// Origin's tip.
        tip: String,
    },
    /// Left as it is, and why (dirty, ahead, diverged, detached, foreign origin, …).
    Kept(Preserved),
}

impl Stands {
    /// The probe this stand reads as: a checkout left as it is is `Ok`, its reason in `now`.
    pub(crate) fn probe(&self) -> Probe {
        match self {
            Self::Current { branch, head } => Probe::Ok {
                now: format!("up to date with origin/{branch} at {}", short(head)),
            },
            Self::Behind { branch, tip, by } => Probe::Change {
                from: format!("{} behind origin/{branch}", commits(*by)),
                to: format!("fast-forwarded to origin/{branch} at {}", short(tip)),
            },
            Self::Unfetched { branch, tip } => Probe::Change {
                from: format!(
                    "origin/{branch} is at {}, which this checkout has not fetched",
                    short(tip)
                ),
                to: format!("fetched, then fast-forwarded to origin/{branch}"),
            },
            Self::Kept(why) => Probe::Ok {
                now: format!("left as it is: {why}"),
            },
        }
    }

    /// The probe this stand reads as when it was judged against origin as last fetched
    /// ([`read_fetched`]): [`Self::probe`], saying so.
    pub(crate) fn probe_as_fetched(&self) -> Probe {
        match self {
            Self::Current { branch, head } => Probe::Ok {
                now: format!(
                    "up to date with origin/{branch} at {}, as last fetched",
                    short(head)
                ),
            },
            Self::Behind { branch, tip, by } => Probe::Change {
                from: format!("{} behind origin/{branch}, as last fetched", commits(*by)),
                to: format!("fast-forwarded to origin/{branch} at {}", short(tip)),
            },
            Self::Unfetched { .. } | Self::Kept(_) => self.probe(),
        }
    }
}

fn short(oid: &str) -> &str {
    oid.get(..12).unwrap_or(oid)
}

fn commits(n: u64) -> String {
    if n == 1 {
        "1 commit".to_owned()
    } else {
        format!("{n} commits")
    }
}

/// Why a read stopped: the checkout is left as it is (`Kept`), or it could not be read.
enum Stop {
    Kept(Preserved),
    Failed(Unread),
}

impl From<String> for Stop {
    fn from(why: String) -> Self {
        Self::Failed(why.into())
    }
}

impl From<Unread> for Stop {
    fn from(unread: Unread) -> Self {
        Self::Failed(unread)
    }
}

/// What the checkout says of itself, read without the network.
struct Local {
    branch: String,
    head: String,
    /// The commits its record's refs name ([`Facts::commits`]): each one is here.
    here: BTreeSet<String>,
}

/// Whether [`local`] reads the worktree's files.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Worktree {
    /// `status` says whether anything there is the person's own: what a read that goes on to
    /// origin, or a judgement of a checkout left as it is, needs.
    Read,
    /// Not read: a later [`local`] reads them before the network is reached.
    Unread,
}

fn run(sys: &dyn System, cmd: &Cmd, what: &str) -> Result<Ran, String> {
    sys.run_complete_stdout(cmd)
        .map_err(|why| format!("cannot run git {what} in the checkout: {why}"))
}

fn stdout(ran: &Ran) -> String {
    String::from_utf8_lossy(&ran.stdout).trim().to_owned()
}

/// One git read that must succeed, its trimmed standard output.
fn read_ok(sys: &dyn System, user: &User, path: &Path, args: &[&str]) -> Result<String, String> {
    let what = args.first().copied().unwrap_or("read");
    let ran = run(
        sys,
        &git(sys, user, Some(path)).args(args.iter().copied()),
        what,
    )?;
    if !ran.success() {
        return Err(format!("git {what} failed in the checkout"));
    }
    Ok(stdout(&ran))
}

/// What the checkout at `path` says of itself, from its record of facts ([`Facts::of`]): the
/// top of a worktree whose one origin `origin` admits, as git reaches it, on a branch with a
/// commit, and, where `worktree` says so, with nothing of the person's own in its files.
fn local(
    sys: &dyn System,
    user: &User,
    path: &Path,
    origin: &Origin,
    worktree: Worktree,
) -> Result<Local, Stop> {
    let facts = Facts::of(sys, user, path)?;
    if !facts.top() {
        return Err(Stop::from("not the top of a git worktree".to_owned()));
    }
    let urls = facts.values("remote.origin.url");
    match urls.as_slice() {
        [] => {
            return Err(Stop::Kept(Preserved::new(
                PreservationReason::MissingOrigin,
                "it has no origin remote",
            )));
        }
        [url] => {
            if !origin.admits(url) {
                return Err(Stop::Kept(Preserved::new(
                    PreservationReason::DifferentOrigin,
                    format!("its origin is not {}", origin.describe()),
                )));
            }
        }
        _ => {
            return Err(Stop::Kept(Preserved::new(
                PreservationReason::MultipleOrigins,
                "origin has more than one URL",
            )));
        }
    }
    let effective = facts.effective_url(sys, user, path)?;
    if !origin.admits(&effective) {
        return Err(Stop::Kept(Preserved::new(
            PreservationReason::RewrittenOrigin,
            format!(
                "git rewrites its origin to a remote that is not {} (url.*.insteadOf)",
                origin.describe()
            ),
        )));
    }
    let branch = match facts.head() {
        facts::Head::Symbolic(reference) => match reference.strip_prefix("refs/heads/") {
            Some(branch) if !branch.is_empty() => branch.to_owned(),
            _ => {
                return Err(Stop::Kept(Preserved::new(
                    PreservationReason::Detached,
                    "HEAD is not on a branch",
                )));
            }
        },
        facts::Head::Detached(_) => {
            return Err(Stop::Kept(Preserved::new(
                PreservationReason::Detached,
                "HEAD is detached",
            )));
        }
    };
    let Some(head) = facts.head_commit().map(str::to_owned) else {
        return Err(Stop::Kept(Preserved::new(
            PreservationReason::NoLocalCommit,
            format!("{branch} has no commit yet"),
        )));
    };
    if worktree == Worktree::Read {
        // Even a status can invoke configured filters or fsmonitor. The same
        // no-program overrides used for dependency reads keep eligibility local.
        let programs = facts.programs()?;
        let status = run(
            sys,
            &programs
                .off(git(sys, user, Some(path)))
                .args(NO_SIDE_RUNS)
                .args([
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=normal",
                    "--ignore-submodules=all",
                ]),
            "status",
        )?;
        if !status.success() {
            return Err(Stop::from("git status failed in the checkout".to_owned()));
        }
        if !stdout(&status).is_empty() {
            return Err(Stop::Kept(Preserved::new(
                PreservationReason::LocalChanges,
                "it has uncommitted changes or untracked files",
            )));
        }
    }
    Ok(Local {
        branch,
        head,
        here: facts.commits(),
    })
}

/// Origin's default branch and its tip (`git ls-remote --symref origin HEAD`): the one
/// network read a probe makes.
fn remote_head(sys: &dyn System, user: &User, path: &Path) -> Result<(String, String), Stop> {
    let ran = run(
        sys,
        &network(sys, user, Some(path)).args(["ls-remote", "--symref", "origin", "HEAD"]),
        "ls-remote",
    )?;
    if !ran.success() {
        let offline = unreachable(&ran.stderr);
        return Err(Stop::Failed(Unread {
            why: git_failed(
                if offline {
                    "cannot reach origin to read its default branch (offline)"
                } else {
                    "cannot read origin's default branch"
                },
                &ran,
            ),
            offline,
            network: true,
        }));
    }
    let (mut branch, mut tip) = (None, None);
    for line in stdout(&ran).lines() {
        let Some((value, name)) = line.split_once('\t') else {
            continue;
        };
        if name != "HEAD" {
            continue;
        }
        if let Some(reference) = value.strip_prefix("ref: refs/heads/") {
            branch = Some(reference.to_owned());
        } else if oid(value) {
            tip = Some(value.to_owned());
        }
    }
    match (branch, tip) {
        (Some(branch), Some(tip)) => Ok((branch, tip)),
        (_, None) => Err(Stop::Kept(Preserved::new(
            PreservationReason::NoRemoteCommit,
            "origin has no commit yet",
        ))),
        (None, Some(_)) => Err(Stop::from(
            "origin did not name its default branch".to_owned(),
        )),
    }
}

/// Why [`read_fetched`] did not judge the checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NotFetched {
    /// The checkout could not be read.
    Unread(Unread),
    /// It holds no record, as last fetched, of origin's default branch or of the tip that
    /// branch names, so whether it is behind cannot be read without the network. `act` is the
    /// git subcommand, run in the checkout, that records it (it asks origin): `aterm init
    /// --develop` would not, since its probe of a checkout that is current fetches nothing.
    Unrecorded {
        /// What is missing.
        why: String,
        /// `remote set-head origin --auto` or `fetch origin`.
        act: &'static str,
    },
}

impl From<String> for NotFetched {
    fn from(why: String) -> Self {
        Self::Unread(why.into())
    }
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn count(sys: &dyn System, user: &User, path: &Path, args: &[&str]) -> Result<u64, String> {
    crate::parse_uint(&read_ok(sys, user, path, args)?)
        .ok_or_else(|| "git rev-list --count answered no number".to_owned())
}

/// How many commits `head` has that `tip` lacks, and `tip` has that `head` lacks: one `git
/// rev-list --left-right --count head...tip`.
fn apart(
    sys: &dyn System,
    user: &User,
    path: &Path,
    head: &str,
    tip: &str,
) -> Result<(u64, u64), String> {
    let counted = read_ok(
        sys,
        user,
        path,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{head}...{tip}"),
        ],
    )?;
    counted
        .split_once('\t')
        .and_then(|(ahead, behind)| Some((crate::parse_uint(ahead)?, crate::parse_uint(behind)?)))
        .ok_or_else(|| "git rev-list --left-right --count answered no numbers".to_owned())
}

/// Where `local` stands against `tip`, origin's `default` branch. `fetched`: a fetch just
/// ran, so a tip not here is a refspec that leaves it out, not a fetch still to make.
fn judge(
    sys: &dyn System,
    user: &User,
    path: &Path,
    local: &Local,
    default: &str,
    tip: &str,
    fetched: bool,
) -> Result<Stands, String> {
    let Local { branch, head, here } = local;
    if branch != default {
        return Ok(Stands::Kept(Preserved::new(
            PreservationReason::DifferentBranch,
            format!("it is on {branch}, not origin's default branch {default}"),
        )));
    }
    if head == tip {
        return Ok(Stands::Current {
            branch: branch.clone(),
            head: head.clone(),
        });
    }
    // A tip a ref of the record names is here; any other is asked of the object store.
    if !here.contains(tip) {
        let found = run(
            sys,
            &git(sys, user, Some(path)).args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{tip}^{{commit}}"),
            ]),
            "rev-parse",
        )?;
        match found.status {
            Some(0) => {}
            Some(1) if fetched => {
                return Err(format!(
                    "origin/{branch} is still not here after the fetch (its fetch refspec leaves it out)"
                ));
            }
            Some(1) => {
                let own = count(
                    sys,
                    user,
                    path,
                    &["rev-list", "--count", "HEAD", "--not", "--remotes=origin"],
                )?;
                return Ok(if own > 0 {
                    Stands::Kept(Preserved::new(
                        PreservationReason::Ahead,
                        format!(
                            "it has {} that origin lacks (as last fetched)",
                            commits(own)
                        ),
                    ))
                } else {
                    Stands::Unfetched {
                        branch: branch.clone(),
                        tip: tip.to_owned(),
                    }
                });
            }
            _ => return Err("cannot read origin's tip in the checkout".into()),
        }
    }
    // Two different commits: `head` is an ancestor of `tip` exactly when it has none of its
    // own, and `tip` of `head` when that has none.
    let (ahead, behind) = apart(sys, user, path, head, tip)?;
    if ahead == 0 {
        return Ok(Stands::Behind {
            branch: branch.clone(),
            tip: tip.to_owned(),
            by: behind,
        });
    }
    if behind == 0 {
        return Ok(Stands::Kept(Preserved::new(
            PreservationReason::Ahead,
            format!("it has {} that origin/{branch} lacks", commits(ahead)),
        )));
    }
    Ok(Stands::Kept(Preserved::new(
        PreservationReason::Diverged,
        format!(
            "it has diverged from origin/{branch} ({} here, {} there)",
            commits(ahead),
            commits(behind)
        ),
    )))
}

/// Why init leaves the checkout at `path` as it is, read without the network: what [`read`] judges
/// before it reaches origin (an origin that is not admitted, a detached HEAD, uncommitted changes
/// or untracked files).
/// # Errors
/// The checkout could not be read (not the top of a worktree, git failed).
pub(crate) fn kept_here(
    sys: &dyn System,
    user: &User,
    path: &Path,
    origin: &Origin,
) -> Result<Option<String>, String> {
    match local(sys, user, path, origin, Worktree::Read) {
        Ok(_) => Ok(None),
        Err(Stop::Kept(why)) => Ok(Some(why.why)),
        Err(Stop::Failed(unread)) => Err(unread.why),
    }
}

/// Where the checkout at `path` stands against `origin`, changing nothing: local reads and one
/// `ls-remote`.
/// # Errors
/// The checkout or its origin could not be read; the line names which, never a URL.
pub(crate) fn read(
    sys: &dyn System,
    user: &User,
    path: &Path,
    origin: &Origin,
) -> Result<Stands, Unread> {
    let stood = local(sys, user, path, origin, Worktree::Read).and_then(|local| {
        let (default, tip) = remote_head(sys, user, path)?;
        judge(sys, user, path, &local, &default, &tip, false).map_err(Stop::from)
    });
    match stood {
        Ok(stands) => Ok(stands),
        Err(Stop::Kept(why)) => Ok(Stands::Kept(why)),
        Err(Stop::Failed(unread)) => Err(unread),
    }
}

/// Where the checkout at `path` stands against `origin` as it last fetched it, with no network
/// read at all (`aterm doctor`'s, design §5, S9): [`read`]'s judgement over
/// `refs/remotes/origin/HEAD` and the branch it names, in place of `ls-remote`.
/// # Errors
/// The checkout could not be read, or it records no default branch of origin's or not its tip
/// ([`NotFetched`]); the line names which, never a URL.
pub(crate) fn read_fetched(
    sys: &dyn System,
    user: &User,
    path: &Path,
    origin: &Origin,
) -> Result<Stands, NotFetched> {
    fetched_stand(sys, user, path, origin, Worktree::Read)
}

/// [`read_fetched`] without reading the worktree's files: what the refresh of a checkout
/// already here reads before [`align_guarded`], whose own read runs `status` before it reaches
/// origin, so a checkout is read for the person's own work once before each network read. A
/// checkout with uncommitted work reads as [`read_fetched`] would read it clean.
/// # Errors
/// As [`read_fetched`].
pub(crate) fn stands_fetched(
    sys: &dyn System,
    user: &User,
    path: &Path,
    origin: &Origin,
) -> Result<Stands, NotFetched> {
    fetched_stand(sys, user, path, origin, Worktree::Unread)
}

fn fetched_stand(
    sys: &dyn System,
    user: &User,
    path: &Path,
    origin: &Origin,
    worktree: Worktree,
) -> Result<Stands, NotFetched> {
    let local = match local(sys, user, path, origin, worktree) {
        Ok(local) => local,
        Err(Stop::Kept(why)) => return Ok(Stands::Kept(why)),
        Err(Stop::Failed(unread)) => return Err(NotFetched::Unread(unread)),
    };
    let (default, tip) = Facts::of(sys, user, path)?.fetched_head()?;
    Ok(judge(sys, user, path, &local, &default, &tip, true)?)
}

/// Whether this same active branch explicitly tracks origin's corresponding
/// branch, by the checkout's record of facts ([`Facts::tracks_origin`]), read again where its
/// files changed; one that cannot be read does not. Automatic default refresh preserves
/// missing, unreadable or different tracking; explicit profile/develop alignment keeps its
/// existing policy.
pub(crate) fn tracks_origin(sys: &dyn System, user: &User, path: &Path, branch: &str) -> bool {
    Facts::of(sys, user, path).is_ok_and(|facts| facts.tracks_origin(branch))
}

/// Bring the checkout at `path` to origin's default branch when [`read`] says it moves: `git fetch
/// origin` (origin alone, no hook), the same judgement over what was fetched, then `git merge
/// --ff-only` of the fetched tip (no autostash, no submodule worktree touched, no hook). The
/// fetch and the merge are transfers, bounded by their progress, not by a wall clock
/// ([`System::run_repository`]).
/// # Errors
/// A read, the fetch or the merge failed, or the fetch or the merge stalled (a network failure);
/// what the merge refused is left as it was.
pub(crate) fn align(
    sys: &dyn System,
    user: &User,
    path: &Path,
    origin: &Origin,
) -> Result<Stands, Unread> {
    align_guarded(sys, user, path, origin, &|| Ok(()))
}

/// [`align`] with current authorization checked before its fetch and its worktree update.
/// # Errors
/// As [`align`], or authorization changed while origin was being read or fetched.
pub(crate) fn align_guarded(
    sys: &dyn System,
    user: &User,
    path: &Path,
    origin: &Origin,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<Stands, Unread> {
    use crate::repository_progress::{Phase, Progress};
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repository");
    let mut progress = Progress::new(name, path.to_owned());
    progress.placed = true;
    // Alignment updates an existing checkout. Its last known usability stands
    // independently of connecting to its origin or fetching new commits.
    progress.usable = true;
    progress.enter(Phase::Checking);
    sys.repository_progress(&progress);
    let result = align_observed(sys, user, path, origin, current, &mut progress);
    match &result {
        Ok(Stands::Kept(kept)) => progress.defer(kept.reason),
        Ok(_) => progress.enter(Phase::Ready),
        Err(unread) => progress.fail(if unread.network {
            FailureKind::Network
        } else {
            FailureKind::Local
        }),
    }
    sys.repository_progress(&progress);
    result
}

fn align_observed(
    sys: &dyn System,
    user: &User,
    path: &Path,
    origin: &Origin,
    current: &(dyn Fn() -> Result<(), String> + Sync),
    progress: &mut crate::repository_progress::Progress,
) -> Result<Stands, Unread> {
    use crate::repository_progress::Phase;
    current().map_err(Unread::from)?;
    let before = read(sys, user, path, origin)?;
    progress.usable = true;
    let default = match &before {
        Stands::Behind { branch, .. } | Stands::Unfetched { branch, .. } => branch.clone(),
        Stands::Current { .. } | Stands::Kept(_) => return Ok(before),
    };
    let programs = Facts::of(sys, user, path)?.programs()?;
    current().map_err(Unread::from)?;
    progress.enter(Phase::Fetching);
    sys.repository_progress(progress);
    let fetch = sys
        .run_repository(
            &programs
                .off(network(sys, user, Some(path)))
                .args(NO_SIDE_RUNS)
                .args(["fetch", "--progress", "--no-recurse-submodules", "origin"]),
            progress,
        )
        .map_err(|failed| {
            Unread::unfinished(
                &failed,
                format!("git fetch origin did not finish ({failed})"),
            )
        })?;
    if !fetch.success() {
        let offline = unreachable(&fetch.stderr);
        return Err(Unread {
            why: git_failed(
                if offline {
                    "cannot reach origin to fetch from it (offline)"
                } else {
                    "git fetch origin failed"
                },
                &fetch,
            ),
            offline,
            network: true,
        });
    }
    let local = match local(sys, user, path, origin, Worktree::Read) {
        Ok(local) => local,
        Err(Stop::Kept(why)) => return Ok(Stands::Kept(why)),
        Err(Stop::Failed(unread)) => return Err(unread),
    };
    let tracking = format!("refs/remotes/origin/{default}");
    let facts = Facts::of(sys, user, path)?;
    let tip = if facts.read_ref(&tracking) {
        facts.commit_of(&tracking).map(str::to_owned)
    } else {
        // Origin's default branch is not the one its recorded HEAD names: asked of git.
        let found = run(
            sys,
            &git(sys, user, Some(path)).args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{tracking}^{{commit}}"),
            ]),
            "rev-parse",
        )?;
        found.success().then(|| stdout(&found))
    };
    let Some(tip) = tip else {
        return Err(format!(
            "origin/{default} is not here after the fetch (its fetch refspec leaves it out)"
        )
        .into());
    };
    let stands = judge(sys, user, path, &local, &default, &tip, true)?;
    let Stands::Behind { .. } = &stands else {
        return Ok(stands);
    };
    // The record as it stands now (read again where a file it came from changed since).
    let programs = Facts::of(sys, user, path)?.programs()?;
    current().map_err(Unread::from)?;
    progress.enter(Phase::Updating);
    sys.repository_progress(progress);
    // A partial clone's merge fetches what it lacks, so it is a transfer too: bounded by its
    // progress (its tree's work while it prints nothing), its stall a network failure.
    let merge = sys
        .run_repository(
            &programs
                .off(lazily(network(sys, user, Some(path))))
                .args(NO_SIDE_RUNS)
                .args([
                    "-c",
                    "submodule.recurse=false",
                    "merge",
                    "--ff-only",
                    "--no-autostash",
                    "--quiet",
                    &tip,
                ]),
            progress,
        )
        .map_err(|failed| {
            Unread::unfinished(
                &failed,
                format!("cannot run git merge in the checkout: {failed}"),
            )
        })?;
    if !merge.success() {
        let said = String::from_utf8_lossy(&merge.stderr);
        let said = said.lines().rev().find(|line| !line.trim().is_empty());
        return Err(format!(
            "git merge --ff-only refused, so the checkout is as it was{}",
            said.map(|line| format!(": {}", line.trim()))
                .unwrap_or_default()
        )
        .into());
    }
    Ok(stands)
}

#[cfg(test)]
pub(crate) mod tests;
