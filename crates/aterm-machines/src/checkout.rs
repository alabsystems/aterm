// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One checkout's alignment with its origin (docs/DESIGN-init-system-2026-10-20.md §4.3, §4.4): the
//! one function `develop.checkout@<path>` and the GitHub item's existing checkouts share.

use crate::model::{Probe, User};
use crate::port::{Cmd, Ran, System};
use std::path::Path;
use std::time::Duration;

/// A local read, or one `ls-remote`.
const READ_BUDGET: Duration = Duration::from_secs(300);
/// `git fetch origin` of a large history.
const FETCH_BUDGET: Duration = Duration::from_secs(1800);
/// Nothing else runs under a git that changes a ref or a worktree, for this one run (no
/// configuration is written): no repository hook (a hook directory that does not exist), and no
/// automatic maintenance. A merge or a fetch otherwise ends by starting `git maintenance run
/// --auto --detach`, whose fork can still hold the run's stdout in its process group when git
/// exits, and a drain that sees it refuses the run's output ([`System::run_complete_stdout`]).
pub(crate) const NO_SIDE_RUNS: [&str; 4] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "maintenance.auto=false",
];

/// The installed git ([`crate::items::github::git_program`]: the Command Line Tools', or a
/// selected Xcode's) as `user`, in `path` when given, with no prompt (`GIT_TERMINAL_PROMPT=0`)
/// and no optional index lock (`GIT_OPTIONAL_LOCKS=0`, so a read never races the person's own
/// git).
pub(crate) fn git(sys: &dyn System, user: &User, path: Option<&Path>) -> Cmd {
    let mut command = Cmd::new(crate::items::github::git_program(sys))
        .as_user(user)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .budget(READ_BUDGET);
    if let Some(path) = path {
        command = command.arg("-C").arg(path);
    }
    command
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
}

impl From<String> for Unread {
    fn from(why: String) -> Self {
        Self {
            why,
            offline: false,
        }
    }
}

/// Whether `url` names `github.com/<owner>/<name>` over HTTPS or SSH, with no credential in it
/// (`https://token@github.com/…` is another URL).
pub(crate) fn github_origin(url: &str, owner: &str, name: &str) -> bool {
    github_path(url).is_some_and(|path| path.eq_ignore_ascii_case(&format!("{owner}/{name}")))
}

/// The `<owner>/<name>` a github.com URL names, without `.git`.
fn github_path(url: &str) -> Option<&str> {
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com:22/"))?;
    Some(path.strip_suffix(".git").unwrap_or(path))
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
    /// Exactly this URL or path (a tree whose `repository` is not on GitHub, a test's scratch
    /// remote).
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
            Self::Exact(exact) => url == exact,
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
    Kept(String),
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

    /// Whether an [`align`] would fetch.
    pub(crate) const fn moves(&self) -> bool {
        matches!(self, Self::Behind { .. } | Self::Unfetched { .. })
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
    Kept(String),
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

fn local(sys: &dyn System, user: &User, path: &Path, origin: &Origin) -> Result<Local, Stop> {
    if read_ok(sys, user, path, &["rev-parse", "--is-inside-work-tree"])? != "true"
        || !read_ok(sys, user, path, &["rev-parse", "--show-prefix"])?.is_empty()
    {
        return Err(Stop::from("not the top of a git worktree".to_owned()));
    }
    let urls = run(
        sys,
        &git(sys, user, Some(path)).args(["config", "--get-all", "remote.origin.url"]),
        "config",
    )?;
    match urls.status {
        Some(0) => {}
        Some(1) => return Err(Stop::Kept("it has no origin remote".into())),
        _ => return Err(Stop::from("cannot read the checkout's origin".to_owned())),
    }
    let urls = stdout(&urls);
    let urls: Vec<&str> = urls.lines().collect();
    if urls.len() != 1 {
        return Err(Stop::Kept("origin has more than one URL".into()));
    }
    if !origin.admits(urls[0]) {
        return Err(Stop::Kept(format!(
            "its origin is not {}",
            origin.describe()
        )));
    }
    let effective = read_ok(sys, user, path, &["ls-remote", "--get-url", "origin"])?;
    if !origin.admits(&effective) {
        return Err(Stop::Kept(format!(
            "git rewrites its origin to a remote that is not {} (url.*.insteadOf)",
            origin.describe()
        )));
    }
    let symbolic = run(
        sys,
        &git(sys, user, Some(path)).args(["symbolic-ref", "--quiet", "HEAD"]),
        "symbolic-ref",
    )?;
    let branch = match symbolic.status {
        Some(0) => {
            let reference = stdout(&symbolic);
            match reference.strip_prefix("refs/heads/") {
                Some(branch) if !branch.is_empty() => branch.to_owned(),
                _ => return Err(Stop::Kept("HEAD is not on a branch".into())),
            }
        }
        Some(1) => return Err(Stop::Kept("HEAD is detached".into())),
        _ => return Err(Stop::from("cannot read the checkout's HEAD".to_owned())),
    };
    let head = run(
        sys,
        &git(sys, user, Some(path)).args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]),
        "rev-parse",
    )?;
    let head = match head.status {
        Some(0) => stdout(&head),
        Some(1) => return Err(Stop::Kept(format!("{branch} has no commit yet"))),
        _ => return Err(Stop::from("cannot read the checkout's HEAD".to_owned())),
    };
    let status = read_ok(
        sys,
        user,
        path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=all",
        ],
    )?;
    if !status.is_empty() {
        return Err(Stop::Kept(
            "it has uncommitted changes or untracked files".into(),
        ));
    }
    Ok(Local { branch, head })
}

/// Origin's default branch and its tip (`git ls-remote --symref origin HEAD`): the one
/// network read a probe makes.
fn remote_head(sys: &dyn System, user: &User, path: &Path) -> Result<(String, String), Stop> {
    let ran = run(
        sys,
        &git(sys, user, Some(path)).args(["ls-remote", "--symref", "origin", "HEAD"]),
        "ls-remote",
    )?;
    if !ran.success() {
        return Err(Stop::Failed(if unreachable(&ran.stderr) {
            Unread {
                why: "cannot reach origin to read its default branch (offline)".into(),
                offline: true,
            }
        } else {
            "cannot read origin's default branch (no access to origin, or no such repository)"
                .to_owned()
                .into()
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
        (_, None) => Err(Stop::Kept("origin has no commit yet".into())),
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

/// Origin's default branch and its tip as this checkout last fetched them
/// (`refs/remotes/origin/HEAD`, which a clone writes, and the branch it names): no network read.
fn fetched_head(
    sys: &dyn System,
    user: &User,
    path: &Path,
) -> Result<(String, String), NotFetched> {
    let symbolic = run(
        sys,
        &git(sys, user, Some(path)).args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]),
        "symbolic-ref",
    )?;
    let default = match symbolic.status {
        Some(0) => match stdout(&symbolic).strip_prefix("refs/remotes/origin/") {
            Some(branch) if !branch.is_empty() => branch.to_owned(),
            _ => {
                return Err(NotFetched::from(
                    "refs/remotes/origin/HEAD names no branch of origin's".to_owned(),
                ));
            }
        },
        Some(1) => {
            return Err(NotFetched::Unrecorded {
                why: "this checkout records no default branch of origin's \
                      (refs/remotes/origin/HEAD), so whether it is behind cannot be read \
                      without the network"
                    .to_owned(),
                act: "remote set-head origin --auto",
            });
        }
        _ => {
            return Err(NotFetched::from(
                "cannot read the checkout's refs/remotes/origin/HEAD".to_owned(),
            ));
        }
    };
    let tip = run(
        sys,
        &git(sys, user, Some(path)).args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{default}^{{commit}}"),
        ]),
        "rev-parse",
    )?;
    match tip.status {
        Some(0) if oid(&stdout(&tip)) => Ok((default, stdout(&tip))),
        Some(1) => Err(NotFetched::Unrecorded {
            why: format!(
                "refs/remotes/origin/HEAD names origin/{default}, which this checkout does not \
                 hold, so whether it is behind cannot be read without the network"
            ),
            act: "fetch origin",
        }),
        _ => Err(NotFetched::from(format!(
            "cannot read origin/{default} in the checkout"
        ))),
    }
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn count(sys: &dyn System, user: &User, path: &Path, args: &[&str]) -> Result<u64, String> {
    read_ok(sys, user, path, args)?
        .parse()
        .map_err(|_| "git rev-list --count answered no number".to_owned())
}

/// Whether `ancestor` is an ancestor of `of` (`git merge-base --is-ancestor`).
fn is_ancestor(
    sys: &dyn System,
    user: &User,
    path: &Path,
    ancestor: &str,
    of: &str,
) -> Result<bool, String> {
    let ran = run(
        sys,
        &git(sys, user, Some(path)).args(["merge-base", "--is-ancestor", ancestor, of]),
        "merge-base",
    )?;
    match ran.status {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err("git merge-base failed in the checkout".into()),
    }
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
    let Local { branch, head } = local;
    if branch != default {
        return Ok(Stands::Kept(format!(
            "it is on {branch}, not origin's default branch {default}"
        )));
    }
    if head == tip {
        return Ok(Stands::Current {
            branch: branch.clone(),
            head: head.clone(),
        });
    }
    let here = run(
        sys,
        &git(sys, user, Some(path)).args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{tip}^{{commit}}"),
        ]),
        "rev-parse",
    )?;
    match here.status {
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
                Stands::Kept(format!(
                    "it has {} that origin lacks (as last fetched)",
                    commits(own)
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
    if is_ancestor(sys, user, path, head, tip)? {
        let by = count(
            sys,
            user,
            path,
            &["rev-list", "--count", &format!("{head}..{tip}")],
        )?;
        return Ok(Stands::Behind {
            branch: branch.clone(),
            tip: tip.to_owned(),
            by,
        });
    }
    let ahead = count(
        sys,
        user,
        path,
        &["rev-list", "--count", &format!("{tip}..{head}")],
    )?;
    if is_ancestor(sys, user, path, tip, head)? {
        return Ok(Stands::Kept(format!(
            "it has {} that origin/{branch} lacks",
            commits(ahead)
        )));
    }
    let behind = count(
        sys,
        user,
        path,
        &["rev-list", "--count", &format!("{head}..{tip}")],
    )?;
    Ok(Stands::Kept(format!(
        "it has diverged from origin/{branch} ({} here, {} there)",
        commits(ahead),
        commits(behind)
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
    match local(sys, user, path, origin) {
        Ok(_) => Ok(None),
        Err(Stop::Kept(why)) => Ok(Some(why)),
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
    let stood = local(sys, user, path, origin).and_then(|local| {
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
    let local = match local(sys, user, path, origin) {
        Ok(local) => local,
        Err(Stop::Kept(why)) => return Ok(Stands::Kept(why)),
        Err(Stop::Failed(unread)) => return Err(NotFetched::Unread(unread)),
    };
    let (default, tip) = fetched_head(sys, user, path)?;
    Ok(judge(sys, user, path, &local, &default, &tip, true)?)
}

/// Bring the checkout at `path` to origin's default branch when [`read`] says it moves: `git fetch
/// origin` (origin alone, no hook), the same judgement over what was fetched, then `git merge
/// --ff-only` of the fetched tip (no autostash, no submodule worktree touched, no hook).
/// # Errors
/// A read, the fetch or the merge failed; what the merge refused is left as it was.
pub(crate) fn align(
    sys: &dyn System,
    user: &User,
    path: &Path,
    origin: &Origin,
) -> Result<Stands, Unread> {
    let before = read(sys, user, path, origin)?;
    let default = match &before {
        Stands::Behind { branch, .. } | Stands::Unfetched { branch, .. } => branch.clone(),
        Stands::Current { .. } | Stands::Kept(_) => return Ok(before),
    };
    let fetch = run(
        sys,
        &git(sys, user, Some(path))
            .budget(FETCH_BUDGET)
            .args(NO_SIDE_RUNS)
            .args(["fetch", "--quiet", "--no-recurse-submodules", "origin"]),
        "fetch",
    )?;
    if !fetch.success() {
        return Err(if unreachable(&fetch.stderr) {
            Unread {
                why: "cannot reach origin to fetch from it (offline)".into(),
                offline: true,
            }
        } else {
            "git fetch origin failed (no access to origin, or no such repository)"
                .to_owned()
                .into()
        });
    }
    let local = match local(sys, user, path, origin) {
        Ok(local) => local,
        Err(Stop::Kept(why)) => return Ok(Stands::Kept(why)),
        Err(Stop::Failed(unread)) => return Err(unread),
    };
    let tracking = run(
        sys,
        &git(sys, user, Some(path)).args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{default}^{{commit}}"),
        ]),
        "rev-parse",
    )?;
    if !tracking.success() {
        return Err(format!(
            "origin/{default} is not here after the fetch (its fetch refspec leaves it out)"
        )
        .into());
    }
    let tip = stdout(&tracking);
    let stands = judge(sys, user, path, &local, &default, &tip, true)?;
    let Stands::Behind { .. } = &stands else {
        return Ok(stands);
    };
    let merge = run(
        sys,
        &git(sys, user, Some(path)).args(NO_SIDE_RUNS).args([
            "-c",
            "submodule.recurse=false",
            "merge",
            "--ff-only",
            "--no-autostash",
            "--quiet",
            &tip,
        ]),
        "merge",
    )?;
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
