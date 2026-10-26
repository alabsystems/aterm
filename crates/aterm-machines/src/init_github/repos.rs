// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clone missing GitHub repositories into `~` and fast-forward clean worktrees that are behind.

use super::{Github, auth_id, auth_ready, get};
use crate::checkout::{Origin, Stands, git, github_origin as origin_matches};
use crate::github_pages::Page;
use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{Cmd, Scratch, Secret, System};
use aterm_capacity::admit::gib;
use aterm_capacity::reading::Volume;
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

const GIT: &str = crate::checkout::GIT;
const PAGE_SIZE: usize = 30;
const CLONE_BUDGET: Duration = Duration::from_secs(1800);

pub(super) fn item(user: &User, config: &Github, recorded: Vec<PathBuf>) -> Box<dyn Item> {
    Box::new(Repositories {
        user: user.clone(),
        config: Github {
            account: config.account.clone(),
            owner: config.owner.clone(),
            token_file: config.token_file.clone(),
            token_key: config.token_key.clone(),
        },
        recorded,
    })
}

struct Repositories {
    user: User,
    config: Github,
    recorded: Vec<PathBuf>,
}

struct Scan {
    all: usize,
    missing: Vec<Repo>,
    existing: Vec<(Repo, PathBuf, Stands)>,
    recorded: usize,
}

const NAMED: usize = 4;

impl Scan {
    fn moving(&self) -> usize {
        self.existing
            .iter()
            .filter(|(_, _, stands)| stands.moves())
            .count()
    }

    fn kept(&self) -> Option<String> {
        let kept: Vec<String> = self
            .existing
            .iter()
            .filter_map(|(repo, _, stands)| match stands {
                Stands::Kept(why) => Some(format!("~/{}: {why}", repo.name)),
                _ => None,
            })
            .collect();
        if kept.is_empty() {
            return None;
        }
        let more = kept.len().saturating_sub(NAMED);
        let mut line = kept.into_iter().take(NAMED).collect::<Vec<_>>().join("; ");
        if more > 0 {
            line.push_str(&format!("; and {more} more"));
        }
        Some(line)
    }
}

impl Item for Repositories {
    fn id(&self) -> String {
        format!("init.github.repositories@{}", self.user.name)
    }
    fn family(&self) -> &'static str {
        "github"
    }
    fn title(&self) -> String {
        format!(
            "Every accessible github.com/{} repository cloned into ~",
            self.config.owner
        )
    }
    fn after(&self) -> Vec<String> {
        vec![auth_id(&self.user)]
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let token = match auth_ready(sys, &self.user, &self.config) {
            Err(why) => return Probe::Unusable { why },
            Ok(None) => return Probe::Waits {
                on: auth_id(&self.user),
                why: "enumerate private and public repositories after GitHub authentication is configured".into(),
            },
            Ok(Some(token)) => token,
        };
        match self.scan(sys, &token) {
            Err(why) => Probe::Unusable { why },
            Ok(scan) if scan.missing.is_empty() && scan.moving() == 0 => {
                let mut now = format!("{} matching worktrees present", scan.all);
                if scan.recorded > 0 {
                    now.push_str(&format!(
                        "; {} recorded in [develop], which the develop items align",
                        scan.recorded
                    ));
                }
                match scan.kept() {
                    Some(kept) => now.push_str(&format!("; left as they are: {kept}")),
                    None => now.push_str("; the others up to date with GitHub"),
                }
                Probe::Ok { now }
            }
            Ok(scan) => Probe::Change {
                from: format!(
                    "{} of {} repositories missing from ~{}; {} clean worktree(s) behind GitHub",
                    scan.missing.len(),
                    scan.all,
                    unfit(sys, &self.user, &scan.missing),
                    scan.moving()
                ),
                to: "clone missing repositories with full history; fast-forward each clean \
                     worktree that is behind; preserve every other worktree"
                    .into(),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let Some(token) = auth_ready(sys, &self.user, &self.config)? else {
            return Err("GitHub authentication changed; no repositories cloned".into());
        };
        let scan = self.scan(sys, &token)?;
        let home = Path::new(&self.user.home);
        let mut left = Vec::new();
        for repo in scan.missing {
            if let Some(name) = no_room(sys.volume(home).as_ref(), &repo) {
                left.push(name);
                continue;
            }
            clone_one(sys, &self.user, &repo.owner, &repo)?;
        }
        let mut failed = Vec::new();
        for (repo, path, stands) in scan.existing {
            if !stands.moves() {
                continue;
            }
            if let Err(unread) = crate::checkout::align(sys, &self.user, &path, &repo.origin()) {
                failed.push(format!("~/{}: {}", repo.name, unread.why));
            }
        }
        failed.extend(short_of_room(sys.volume(home).as_ref(), &left, INIT_AGAIN));
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }
}

/// How many of `missing` the disk holding `user`'s home has no room for now ([`no_room`]), as
/// the plan adds it; empty when every one fits or the volume cannot be read.
fn unfit(sys: &dyn System, user: &User, missing: &[Repo]) -> String {
    let volume = sys.volume(Path::new(&user.home));
    match missing
        .iter()
        .filter(|repo| no_room(volume.as_ref(), repo).is_some())
        .count()
    {
        0 => String::new(),
        unfit => format!(", {unfit} of them with no room on the disk now"),
    }
}

impl Repositories {
    fn scan(&self, sys: &dyn System, token: &Secret) -> Result<Scan, String> {
        admit_home(sys, &self.user)?;
        let all = enumerate(sys, &self.user, &self.config, token)?;
        let (missing, existing) = partition(sys, &self.user, &all)?;
        let mut aligned = Vec::new();
        let mut recorded = 0;
        for (repo, path) in existing {
            if self.recorded.contains(&path) {
                recorded += 1;
                continue;
            }
            let stands = crate::checkout::read(sys, &self.user, &path, &repo.origin())
                .unwrap_or_else(|unread| Stands::Kept(unread.why));
            aligned.push((repo, path, stands));
        }
        Ok(Scan {
            all: all.len(),
            missing,
            existing: aligned,
            recorded,
        })
    }
}

pub(super) fn aterm_checkouts(
    sys: &dyn System,
    user: &User,
    config: &Github,
    token: &Secret,
) -> Result<Vec<PathBuf>, String> {
    admit_home(sys, user)?;
    let all = enumerate(sys, user, config, token)?;
    let (_, existing) = partition(sys, user, &all)?;
    Ok(existing
        .into_iter()
        .filter(|(_, path)| {
            crate::init_develop::workspace(sys, user, path).is_ok_and(|found| found.is_some())
        })
        .map(|(_, path)| path)
        .collect())
}

pub(super) fn missing(sys: &dyn System, user: &User, all: &[Repo]) -> Result<Vec<Repo>, String> {
    partition(sys, user, all).map(|(missing, _)| missing)
}

type Present = Vec<(Repo, PathBuf)>;

fn partition(sys: &dyn System, user: &User, all: &[Repo]) -> Result<(Vec<Repo>, Present), String> {
    admit_home(sys, user)?;
    let home = Path::new(&user.home);
    let mut missing = Vec::new();
    let mut existing = Vec::new();
    for repo in all {
        let path = home.join(&repo.name);
        match sys.stat(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => missing.push(repo.clone()),
            Ok(stat) if stat.dir && !stat.symlink => {
                verify_existing(sys, user, &repo.owner, repo, &path)?;
                existing.push((repo.clone(), path));
            }
            Ok(_) => {
                return Err(format!(
                    "~/{} is occupied or linked; left untouched",
                    repo.name
                ));
            }
            Err(_) => return Err(format!("cannot inspect ~/{}; left untouched", repo.name)),
        }
    }
    Ok((missing, existing))
}

pub(super) fn admit_home(sys: &dyn System, user: &User) -> Result<(), String> {
    let home = Path::new(&user.home);
    let stat = sys
        .stat(home)
        .map_err(|_| "cannot inspect the repository destination home")?;
    if !home.is_absolute() || !stat.dir || stat.symlink || stat.uid != user.uid {
        return Err("repository destination must be this user's own home directory".into());
    }
    if !git_installed(sys) {
        return Err(NO_GIT.into());
    }
    Ok(())
}

/// Why [`admit_home`] refuses a Mac without Command Line Tools git.
pub(super) const NO_GIT: &str = "Command Line Tools git is not installed; no repositories changed";

/// Whether Command Line Tools git, which every clone runs, is installed.
pub(super) fn git_installed(sys: &dyn System) -> bool {
    sys.stat(Path::new(GIT)).is_ok_and(|stat| !stat.dir)
}

/// The room a clone takes per byte of GitHub's `size`: 54 repositories GitHub sized at 10 GiB
/// took 29 GiB cloned (2026-10-25), one of them 4.7 times its size.
const CLONE_ROOM_PER_SIZE: u64 = 5;

/// What a pass that left repositories for room ([`short_of_room`]) tells the person to do once
/// there is room, as `aterm init` says it.
pub(super) const INIT_AGAIN: &str = "run aterm init again";

/// `repo` as a pass that leaves it uncloned names it, when its clone would take `volume` below
/// the free space the window's disk watch keeps on it ([`Volume::floor`]); `None` when it fits,
/// or when the volume could not be read. A size GitHub gives as `0` (an empty repository, or one
/// it has not sized yet) counts as 1 KiB, so no clone runs on a volume already at its floor.
pub(super) fn no_room(volume: Option<&Volume>, repo: &Repo) -> Option<String> {
    let volume = volume?;
    let need = repo
        .size_kib
        .max(1)
        .saturating_mul(1024)
        .saturating_mul(CLONE_ROOM_PER_SIZE);
    if need <= volume.above_floor() {
        return None;
    }
    Some(if need >= aterm_capacity::floor::GIB / 10 {
        format!("~/{} (about {})", repo.name, gib(need))
    } else {
        format!("~/{}", repo.name)
    })
}

/// What a pass that left `left` uncloned for room ([`no_room`]) says, with `volume`'s free space,
/// the free space it keeps and `then`, what to do once there is room; `None` when it left none.
pub(super) fn short_of_room(
    volume: Option<&Volume>,
    left: &[String],
    then: &str,
) -> Option<String> {
    if left.is_empty() {
        return None;
    }
    let disk = volume.map_or_else(String::new, |volume| {
        format!(
            "; {} has {} free and keeps {} free",
            volume.mount.display(),
            gib(volume.free),
            gib(volume.floor)
        )
    });
    let count = match left.len() {
        1 => "1 repository".to_owned(),
        n => format!("{n} repositories"),
    };
    Some(format!(
        "{count} not cloned, for want of disk space: {}{disk}; free space, then {then}",
        left.join(", ")
    ))
}

#[derive(Clone, Debug)]
pub(super) struct Repo {
    pub(super) owner: String,
    pub(super) name: String,
    pub(super) default_branch: Option<String>,
    /// GitHub's `size` for it in KiB (`0` where GitHub gives none, or has not sized it yet),
    /// which [`no_room`] estimates its clone's room from.
    pub(super) size_kib: u64,
}

impl Repo {
    fn origin(&self) -> Origin {
        Origin(vec![crate::checkout::Remote::Github {
            owner: self.owner.clone(),
            name: self.name.clone(),
        }])
    }
}

fn component(value: &str, owner: bool) -> bool {
    !value.is_empty()
        && value.len() <= (if owner { 39 } else { 100 })
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || (!owner && matches!(b, b'_' | b'.')))
}

fn branch(value: &str) -> bool {
    !value.is_empty()
        && value != "@"
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.ends_with('.')
        && !value.contains("..")
        && !value.contains("@{")
        && !value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
        && value
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

fn text(sys: &dyn System, cmd: &Cmd, why: &str) -> Result<String, String> {
    let ran = sys.run_complete_stdout(cmd).map_err(|_| why.to_owned())?;
    if !ran.success() {
        return Err(why.into());
    }
    String::from_utf8(ran.stdout).map_err(|_| why.to_owned())
}

fn enumerate(
    sys: &dyn System,
    user: &User,
    config: &Github,
    token: &Secret,
) -> Result<Vec<Repo>, String> {
    if !component(&config.owner, true) || !component(&config.account, true) {
        return Err("invalid configured GitHub owner or account".into());
    }
    let base = if config.owner.eq_ignore_ascii_case(&config.account) {
        "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc"
            .to_owned()
    } else {
        let (owner, _) = get(sys, user, token, &format!("/users/{}", config.owner))?;
        if owner
            .get("login")
            .and_then(Value::as_str)
            .is_none_or(|login| !login.eq_ignore_ascii_case(&config.owner))
            || owner.get("type").and_then(Value::as_str) != Some("Organization")
        {
            return Err("a different repository owner must be a GitHub organization; personal private repositories cannot be enumerated completely this way".into());
        }
        format!(
            "/orgs/{}/repos?type=all&per_page=30&sort=full_name&direction=asc",
            config.owner
        )
    };
    enumerate_pages(&base, Some(&config.owner), |endpoint| {
        get(sys, user, token, endpoint).map_err(String::from)
    })
}

pub(super) fn enumerate_pages(
    base: &str,
    owner: Option<&str>,
    api: impl FnMut(&str) -> Result<Page, String>,
) -> Result<Vec<Repo>, String> {
    let values = crate::github_pages::all(crate::github_pages::API, base, PAGE_SIZE, api)?;
    let mut repositories = Vec::new();
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for value in &values {
        let repo = repo(value, owner)?;
        let id = value
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| *id != 0)
            .ok_or("repository identity is missing")?;
        if !ids.insert(id) || !names.insert(repo.name.to_ascii_lowercase()) {
            return Err("duplicate or colliding repository destinations in GitHub inventory; no repositories changed".into());
        }
        repositories.push(repo);
    }
    repositories.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });
    Ok(repositories)
}

fn repo(value: &Value, expected_owner: Option<&str>) -> Result<Repo, String> {
    let owner = value
        .get("owner")
        .and_then(|owner| owner.get("login"))
        .and_then(Value::as_str)
        .filter(|owner| component(owner, true))
        .ok_or("invalid GitHub repository owner")?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| component(name, false))
        .ok_or("invalid GitHub repository name")?;
    if expected_owner.is_some_and(|expected| !owner.eq_ignore_ascii_case(expected))
        || value
            .get("full_name")
            .and_then(Value::as_str)
            .is_none_or(|full| !full.eq_ignore_ascii_case(&format!("{owner}/{name}")))
        || value.get("private").and_then(Value::as_bool).is_none()
        || value
            .get("clone_url")
            .and_then(Value::as_str)
            .is_none_or(|url| !origin_matches(url, owner, name))
    {
        return Err(
            "GitHub repository identity or visibility does not match the configured owner".into(),
        );
    }
    let default_branch = match value.get("default_branch") {
        Some(Value::Null) => None,
        Some(Value::String(value)) if branch(value) => Some(value.clone()),
        _ => return Err("invalid or missing GitHub default branch".into()),
    };
    Ok(Repo {
        owner: owner.into(),
        name: name.into(),
        default_branch,
        size_kib: value.get("size").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn git_read(sys: &dyn System, user: &User, path: &Path, args: &[&str]) -> Result<String, String> {
    text(
        sys,
        &git(user, Some(path)).args(args.iter().copied()),
        "cannot verify the existing Git worktree; left untouched",
    )
}

fn verify_existing(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
    path: &Path,
) -> Result<(), String> {
    if git_read(sys, user, path, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true"
        || !git_read(sys, user, path, &["rev-parse", "--show-prefix"])?
            .trim()
            .is_empty()
        || !Path::new(git_read(sys, user, path, &["rev-parse", "--show-toplevel"])?.trim())
            .is_absolute()
    {
        return Err(format!(
            "~/{} is not a top-level Git worktree; left untouched",
            repo.name
        ));
    }
    let origin = git_read(
        sys,
        user,
        path,
        &["config", "--get-all", "remote.origin.url"],
    )?;
    let origins = origin.lines().collect::<Vec<_>>();
    if origins.len() != 1 || !origin_matches(origins[0], owner, &repo.name) {
        return Err(format!(
            "~/{} has a different or credential-bearing GitHub origin; left untouched",
            repo.name
        ));
    }
    Ok(())
}

fn verify_integrity(sys: &dyn System, user: &User, path: &Path) -> Result<(), String> {
    if git_read(sys, user, path, &["rev-parse", "--is-shallow-repository"])?.trim() != "false" {
        return Err("the new clone is shallow; not published".into());
    }
    let partial = sys
        .run_complete_stdout(&git(user, Some(path)).args([
            "config",
            "--get-regexp",
            "^(extensions\\.partialclone|remote\\..*\\.(promisor|partialclonefilter))$",
        ]))
        .map_err(|_| "cannot inspect partial-clone configuration")?;
    if partial.status != Some(1) || !partial.stdout.is_empty() || !partial.stderr.is_empty() {
        return Err(
            "the new clone has partial or unreadable clone configuration; not published".into(),
        );
    }
    git_read(
        sys,
        user,
        path,
        &["fsck", "--connectivity-only", "--no-dangling"],
    )?;
    Ok(())
}

#[derive(Debug)]
struct Remote {
    head: Option<String>,
    branch: Option<String>,
    refs: BTreeMap<String, String>,
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn remote(text: &str) -> Result<Remote, String> {
    let (mut head, mut default, mut refs) = (None, None, BTreeMap::new());
    for line in text.lines() {
        let (value, name) = line
            .split_once('\t')
            .ok_or("unrecognized remote Git reference")?;
        if let Some(reference) = value.strip_prefix("ref: ") {
            if name != "HEAD"
                || !reference.starts_with("refs/heads/")
                || !branch(&reference[11..])
                || default.replace(reference[11..].into()).is_some()
            {
                return Err("invalid remote default branch".into());
            }
        } else if !oid(value) {
            return Err("invalid remote Git object identity".into());
        } else if name == "HEAD" {
            if head.replace(value.into()).is_some() {
                return Err("duplicate remote HEAD".into());
            }
        } else {
            let name = name.strip_suffix("^{}").unwrap_or(name);
            if !name.starts_with("refs/heads/") && !name.starts_with("refs/tags/") {
                return Err("unexpected remote Git reference".into());
            }
            if !branch(
                name.strip_prefix("refs/heads/")
                    .or_else(|| name.strip_prefix("refs/tags/"))
                    .unwrap_or(""),
            ) {
                return Err("invalid remote Git reference name".into());
            }
            if line.ends_with("^{}") {
                continue;
            }
            if refs.insert(name.into(), value.into()).is_some() {
                return Err("duplicate remote Git reference".into());
            }
        }
    }
    Ok(Remote {
        head,
        branch: default,
        refs,
    })
}

fn admit_endpoint(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
    url: &str,
) -> Result<String, String> {
    let expanded = text(
        sys,
        &git(user, None).args(["ls-remote", "--get-url", "--", url]),
        "cannot inspect Git's effective GitHub repository URL",
    )?;
    let mut lines = expanded.lines();
    let effective = lines.next().unwrap_or_default();
    if !origin_matches(effective, owner, &repo.name) || lines.next().is_some() {
        return Err(
            "Git's effective URL does not name the configured GitHub repository; no clone started"
                .into(),
        );
    }
    Ok(effective.to_owned())
}

struct CloneGuard<'a> {
    account: &'a str,
    current: &'a dyn Fn() -> Result<(), String>,
}

fn network_git(user: &User, endpoint: &str, guard: Option<&CloneGuard<'_>>) -> Cmd {
    let cmd = git(user, None);
    let Some(guard) = guard else { return cmd };
    // Command-line config only. The exact effective URL outranks less-specific
    // credential settings and clears every inherited helper for this request.
    cmd.env("GIT_ASKPASS", "").args([
        "-c".to_owned(),
        format!("credential.{endpoint}.helper="),
        "-c".to_owned(),
        format!("credential.{endpoint}.helper=osxkeychain"),
        "-c".to_owned(),
        format!("credential.{endpoint}.username={}", guard.account),
        "-c".to_owned(),
        format!("credential.{endpoint}.useHttpPath=false"),
    ])
}

fn clone_endpoint(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
    url: &str,
    guard: Option<&CloneGuard<'_>>,
) -> Result<String, String> {
    let endpoint = admit_endpoint(sys, user, owner, repo, url)?;
    if guard.is_some()
        && !endpoint
            .to_ascii_lowercase()
            .starts_with("https://github.com/")
    {
        return Err("the selected vault's captured credential requires a GitHub HTTPS endpoint; no clone started".into());
    }
    Ok(endpoint)
}

pub(super) fn clone_one_guarded(
    sys: &dyn System,
    user: &User,
    repo: &Repo,
    account: &str,
    current: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    clone_one_with(
        sys,
        user,
        &repo.owner,
        repo,
        Some(&CloneGuard { account, current }),
    )
}

pub(super) fn clone_one(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
) -> Result<(), String> {
    clone_one_with(sys, user, owner, repo, None)
}

fn clone_one_with(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
    guard: Option<&CloneGuard<'_>>,
) -> Result<(), String> {
    let current = || guard.map_or(Ok(()), |guard| (guard.current)());
    current()?;
    let home = Path::new(&user.home);
    let destination = home.join(&repo.name);
    match sys.stat(&destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        _ => {
            return Err(format!(
                "~/{} appeared before cloning; left untouched",
                repo.name
            ));
        }
    }
    let url = format!("https://github.com/{owner}/{}.git", repo.name);
    let endpoint = clone_endpoint(sys, user, owner, repo, &url, guard)?;
    current()?;
    let source = text(
        sys,
        &network_git(user, &endpoint, guard).args([
            "ls-remote",
            "--symref",
            "--",
            &url,
            "HEAD",
            "refs/heads/*",
            "refs/tags/*",
        ]),
        "cannot read the GitHub repository's complete remote references",
    )?;
    let source = remote(&source)?;
    if let Some(head) = &source.head {
        let expected = repo
            .default_branch
            .as_ref()
            .ok_or("nonempty GitHub repository has no default branch")?;
        if source.branch.as_ref() != Some(expected)
            || source.refs.get(&format!("refs/heads/{expected}")) != Some(head)
        {
            return Err(
                "GitHub's default branch changed during enumeration; run init again".into(),
            );
        }
    } else if !source.refs.is_empty() {
        return Err("GitHub repository has references but no usable default HEAD".into());
    } else if source
        .branch
        .as_ref()
        .is_some_and(|branch| repo.default_branch.as_ref() != Some(branch))
    {
        return Err("the empty GitHub repository's default branch changed; run init again".into());
    }
    let endpoint = clone_endpoint(sys, user, owner, repo, &url, guard)?;
    current()?;
    let mut staging = Scratch::new_in(sys, home, "github-clone")
        .map_err(|_| "cannot allocate private repository staging in this home")?;
    let clone = staging.path().join("repo");
    let command = network_git(user, &endpoint, guard)
        .budget(CLONE_BUDGET)
        .args([
            "clone",
            "--no-single-branch",
            "--tags",
            "--origin",
            "origin",
            "--template=",
            "--",
            &url,
        ])
        .arg(&clone);
    text(
        sys,
        &command,
        "GitHub clone failed; existing destination left untouched",
    )?;
    verify_existing(sys, user, owner, repo, &clone)?;
    let alternates = git_read(
        sys,
        user,
        &clone,
        &["rev-parse", "--git-path", "objects/info/alternates"],
    )?;
    let alternates = Path::new(alternates.trim());
    let alternates = if alternates.is_absolute() {
        alternates.to_owned()
    } else {
        clone.join(alternates)
    };
    if !matches!(sys.stat(&alternates), Err(error) if error.kind() == io::ErrorKind::NotFound) {
        return Err("the new clone depends on an alternate object store; not published".into());
    }
    verify_new(sys, user, repo, &clone, &source)?;
    current()?;
    sys.rename_noreplace(&clone, &destination).map_err(|_| {
        format!(
            "cannot publish ~/{} without replacing an existing destination; left untouched",
            repo.name
        )
    })?;
    staging.close().map_err(
        |_| "repository published, but its private empty staging directory could not be removed",
    )?;
    Ok(())
}

fn verify_new(
    sys: &dyn System,
    user: &User,
    repo: &Repo,
    clone: &Path,
    source: &Remote,
) -> Result<(), String> {
    verify_integrity(sys, user, clone)?;
    let head = sys
        .run_complete_stdout(&git(user, Some(clone)).args([
            "rev-parse",
            "--verify",
            "HEAD^{commit}",
        ]))
        .map_err(|_| "cannot verify cloned HEAD")?;
    match &source.head {
        Some(expected)
            if head.success()
                && std::str::from_utf8(&head.stdout)
                    .ok()
                    .is_some_and(|value| value.trim() == expected) => {}
        None if matches!(head.status, Some(1 | 128)) && head.stdout.is_empty() => {
            if let Some(branch) = &repo.default_branch {
                text(
                    sys,
                    &git(user, Some(clone)).args([
                        "symbolic-ref",
                        "HEAD",
                        &format!("refs/heads/{branch}"),
                    ]),
                    "cannot establish the empty repository's default branch",
                )?;
            }
        }
        _ => {
            return Err(
                "cloned HEAD does not match the advertised GitHub commit or empty state".into(),
            );
        }
    }
    let listed = git_read(
        sys,
        user,
        clone,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
    )?;
    let mut refs = BTreeMap::new();
    for line in listed.lines() {
        let (oid, reference) = line
            .split_once(' ')
            .ok_or("unrecognized cloned Git reference")?;
        if !self::oid(oid) || refs.insert(reference.to_owned(), oid.to_owned()).is_some() {
            return Err("invalid cloned Git reference".into());
        }
    }
    let mut wanted = BTreeMap::new();
    for (reference, oid) in &source.refs {
        let target = reference.strip_prefix("refs/heads/").map_or_else(
            || reference.clone(),
            |name| format!("refs/remotes/origin/{name}"),
        );
        wanted.insert(target, oid.clone());
    }
    if let (Some(branch), Some(head)) = (&repo.default_branch, &source.head) {
        wanted.insert(format!("refs/heads/{branch}"), head.clone());
        wanted.insert("refs/remotes/origin/HEAD".into(), head.clone());
    }
    if refs != wanted {
        return Err(
            "the full set of cloned branches and tags does not match GitHub; run init again".into(),
        );
    }
    let symbolic = git_read(sys, user, clone, &["symbolic-ref", "--quiet", "HEAD"])?;
    if let Some(branch) = &repo.default_branch
        && symbolic.trim() != format!("refs/heads/{branch}")
    {
        return Err("the cloned checkout is not on GitHub's default branch".into());
    }
    if let (Some(branch), Some(_)) = (&repo.default_branch, &source.head) {
        let origin_head = git_read(
            sys,
            user,
            clone,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        )?;
        let upstream_remote = git_read(
            sys,
            user,
            clone,
            &["config", "--get", &format!("branch.{branch}.remote")],
        )?;
        let upstream_ref = git_read(
            sys,
            user,
            clone,
            &["config", "--get", &format!("branch.{branch}.merge")],
        )?;
        if origin_head.trim() != format!("refs/remotes/origin/{branch}")
            || upstream_remote.trim() != "origin"
            || upstream_ref.trim() != format!("refs/heads/{branch}")
        {
            return Err(
                "the cloned default branch does not track GitHub's advertised branch".into(),
            );
        }
    }
    if !git_read(
        sys,
        user,
        clone,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(
            "the new clone has incomplete checkout or unexpected worktree changes; not published"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::git_credential::{GITHUB, script};
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, GithubResponse, Live, Ran, Stat};
    use std::path::PathBuf;

    const KEYCHAIN: &str = "fixture_keychain_token_only";
    const BASE: &str =
        "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc";
    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    fn directory(uid: u32) -> Stat {
        Stat {
            uid,
            gid: 20,
            mode: 0o700,
            dir: true,
            symlink: false,
            len: 0,
        }
    }

    fn config() -> Github {
        Github {
            account: "alice".into(),
            owner: "alice".into(),
            token_file: "/unread-credential-source".into(),
            token_key: None,
        }
    }

    fn token() -> Secret {
        Secret::new(KEYCHAIN.as_bytes()).unwrap()
    }

    fn fixture() -> (Fake, User, Github) {
        let mut sys = Fake::root();
        let who = user("alice", 501);
        sys.euid = who.uid;
        sys.stat_of(&who.home, directory(who.uid));
        sys.file(GIT, b"synthetic git");
        script::configured(&sys, &who, "alice");
        script::holds(&sys, Some(&who), GITHUB, "alice", KEYCHAIN);
        sys.on_github_api(&who, "/user", json(r#"{"login":"alice"}"#));
        (sys, who, config())
    }

    fn value(name: &str, id: u64, private: bool) -> Value {
        aterm_json::json!({
            "id": id, "name": name, "full_name": format!("alice/{name}"),
            "owner": {"login": "alice"}, "private": private,
            "clone_url": format!("https://github.com/alice/{name}.git"),
            "default_branch": "main", "size": 0,
        })
    }

    fn json(body: &str) -> Result<GithubResponse, String> {
        Ok(GithubResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.as_bytes().to_vec(),
        })
    }

    fn page(values: Vec<Value>, links: Option<String>) -> Result<GithubResponse, String> {
        let mut answer = json(&aterm_json::to_string(&values).unwrap())?;
        answer
            .headers
            .extend(links.map(|links| ("Link".to_owned(), links)));
        Ok(answer)
    }

    #[test]
    fn pagination_includes_private_empty_and_every_page_without_any_mutation() {
        let (sys, who, config) = fixture();
        let first = (0..30)
            .map(|i| value(&format!("repo{i:03}"), i + 1, i % 2 == 0))
            .collect();
        let links = format!(
            "<https://api.github.com{BASE}&page=2>; rel=\"next\", <https://api.github.com{BASE}&page=2>; rel=\"last\""
        );
        sys.on_github_api(&who, &format!("{BASE}&page=1"), page(first, Some(links)));
        let mut empty = value("zz-empty-private", 101, true);
        empty
            .as_object_mut()
            .unwrap()
            .insert("default_branch".into(), Value::Null);
        sys.on_github_api(&who, &format!("{BASE}&page=2"), page(vec![empty], None));
        let repos = enumerate(&sys, &who, &config, &token()).unwrap();
        assert_eq!(repos.len(), 31);
        assert_eq!(repos.last().unwrap().default_branch, None);
        assert_eq!(sys.calls().len(), 2);
        assert!(
            sys.calls()
                .iter()
                .all(|call| call.contains(" agh GET ") && !call.contains("clone"))
        );

        let (sys, who, config) = fixture();
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(
                (0..30)
                    .map(|i| value(&format!("repo{i:03}"), i + 1, false))
                    .collect(),
                None,
            ),
        );
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=2"),
            page(
                vec![],
                Some(format!(
                    "<https://api.github.com{BASE}&page=1>; rel=\"prev\", <https://api.github.com{BASE}&page=1>; rel=\"last\""
                )),
            ),
        );
        assert_eq!(enumerate(&sys, &who, &config, &token()).unwrap().len(), 30);
    }

    #[test]
    fn malformed_foreign_duplicate_or_incomplete_pages_never_start_cloning() {
        for variant in 0..5 {
            let (sys, who, config) = fixture();
            let mut values = vec![value("one", 1, true)];
            let mut links = None;
            match variant {
                0 => {
                    values[0].as_object_mut().unwrap().remove("default_branch");
                }
                1 => {
                    values[0]
                        .as_object_mut()
                        .unwrap()
                        .insert("name".into(), Value::String("../escape".into()));
                }
                2 => {
                    values.push(value("ONE", 2, false));
                }
                3 => {
                    links = Some("<https://elsewhere.invalid/repos?page=2>; rel=\"next\"".into());
                }
                4 => {
                    values = (0..30)
                        .map(|i| value(&format!("repo{i:03}"), i + 1, false))
                        .collect();
                    sys.on_github_api(
                        &who,
                        &format!("{BASE}&page=2"),
                        page(vec![value("omitted", 101, true)], None),
                    );
                }
                _ => unreachable!(),
            }
            sys.on_github_api(&who, &format!("{BASE}&page=1"), page(values, links));
            assert!(enumerate(&sys, &who, &config, &token()).is_err());
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains("scratch") && !call.contains(" clone "))
            );
        }
        let (sys, who, config) = fixture();
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            Err("GitHub's answer was cut short".into()),
        );
        assert!(enumerate(&sys, &who, &config, &token()).is_err());
    }

    #[test]
    fn differing_personal_owner_is_refused_and_organization_scope_is_explicit() {
        let (sys, who, mut config) = fixture();
        config.owner = "other".into();
        sys.on_github_api(
            &who,
            "/users/other",
            json(r#"{"login":"other","type":"User"}"#),
        );
        assert!(
            enumerate(&sys, &who, &config, &token())
                .unwrap_err()
                .contains("organization")
        );
        let (sys, who, mut config) = fixture();
        config.owner = "team".into();
        sys.on_github_api(
            &who,
            "/users/team",
            json(r#"{"login":"team","type":"Organization"}"#),
        );
        sys.on_github_api(
            &who,
            "/orgs/team/repos?type=all&per_page=30&sort=full_name&direction=asc&page=1",
            page(vec![], None),
        );
        assert!(enumerate(&sys, &who, &config, &token()).unwrap().is_empty());
    }

    fn script_existing(sys: &Fake, who: &User, path: &Path, name: &str) {
        for (args, output) in [
            (
                vec!["rev-parse", "--is-inside-work-tree"],
                "true\n".to_owned(),
            ),
            (vec!["rev-parse", "--show-prefix"], String::new()),
            (
                vec!["rev-parse", "--show-toplevel"],
                format!("{}\n", path.display()),
            ),
            (
                vec!["config", "--get-all", "remote.origin.url"],
                format!("https://github.com/alice/{name}.git\n"),
            ),
            (
                vec!["rev-parse", "--is-shallow-repository"],
                "false\n".into(),
            ),
            (
                vec!["fsck", "--connectivity-only", "--no-dangling"],
                String::new(),
            ),
        ] {
            sys.on(&git(who, Some(path)).args(args).display(), Ran::ok(&output));
        }
        sys.on(
            &git(who, Some(path))
                .args([
                    "config",
                    "--get-regexp",
                    "^(extensions\\.partialclone|remote\\..*\\.(promisor|partialclonefilter))$",
                ])
                .display(),
            Ran::exit(1, ""),
        );
    }

    fn script_read(sys: &Fake, who: &User, path: &Path, name: &str, status: &str) {
        for (args, output) in [
            (
                vec!["ls-remote", "--get-url", "origin"],
                format!("https://github.com/alice/{name}.git\n"),
            ),
            (
                vec!["symbolic-ref", "--quiet", "HEAD"],
                "refs/heads/main\n".to_owned(),
            ),
            (
                vec!["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
                format!("{A}\n"),
            ),
            (
                vec![
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=normal",
                    "--ignore-submodules=all",
                ],
                status.to_owned(),
            ),
        ] {
            sys.on(&git(who, Some(path)).args(args).display(), Ran::ok(&output));
        }
    }

    #[test]
    fn a_clean_worktree_behind_github_is_fast_forwarded_and_a_recorded_one_left_to_develop() {
        use std::sync::{Arc, Mutex};
        for behind in [true, false] {
            let (sys, who, config) = fixture();
            let home = Path::new(&who.home);
            let (moving, dirty, recorded) =
                (home.join("moving"), home.join("dirty"), home.join("aterm"));
            for (path, name) in [(&moving, "moving"), (&dirty, "dirty"), (&recorded, "aterm")] {
                sys.stat_of(path.to_str().unwrap(), directory(who.uid));
                script_existing(&sys, &who, path, name);
            }
            script_read(&sys, &who, &dirty, "dirty", " M README\0");
            sys.on(
                &git(&who, Some(&dirty))
                    .args(["ls-remote", "--symref", "origin", "HEAD"])
                    .display(),
                Ran::ok(&format!("ref: refs/heads/main\tHEAD\n{B}\tHEAD\n")),
            );
            script_read(&sys, &who, &moving, "moving", "");
            let state = Arc::new(Mutex::new((if behind { A } else { B }.to_owned(), false)));
            let model = {
                let (state, moving) = (Arc::clone(&state), moving.clone());
                move |_: &Fake, cmd: &Cmd| -> Option<Ran> {
                    let args: Vec<String> = cmd
                        .args
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect();
                    if args.get(1).map(PathBuf::from) != Some(moving.clone()) {
                        return None;
                    }
                    let mut state = state.lock().unwrap();
                    let (head, fetched) = (state.0.clone(), state.1);
                    Some(match args[2..].join(" ").as_str() {
                        "rev-parse --verify --quiet HEAD^{commit}" => Ran::ok(&format!("{head}\n")),
                        "ls-remote --symref origin HEAD" => {
                            Ran::ok(&format!("ref: refs/heads/main\tHEAD\n{B}\tHEAD\n"))
                        }
                        line if line == format!("rev-parse --verify --quiet {B}^{{commit}}") => {
                            if fetched {
                                Ran::ok(&format!("{B}\n"))
                            } else {
                                Ran::exit(1, "")
                            }
                        }
                        "rev-list --count HEAD --not --remotes=origin" => Ran::ok("0\n"),
                        "-c core.hooksPath=/dev/null -c maintenance.auto=false fetch --quiet --no-recurse-submodules origin" =>
                        {
                            state.1 = true;
                            Ran::ok("")
                        }
                        "rev-parse --verify --quiet refs/remotes/origin/main^{commit}" => {
                            Ran::ok(&format!("{B}\n"))
                        }
                        line if line == format!("merge-base --is-ancestor {A} {B}") => Ran::ok(""),
                        line if line == format!("rev-list --count {A}..{B}") => Ran::ok("1\n"),
                        line if line
                            == format!(
                                "-c core.hooksPath=/dev/null -c maintenance.auto=false -c submodule.recurse=false merge --ff-only --no-autostash --quiet {B}"
                            ) =>
                        {
                            state.0 = B.to_owned();
                            Ran::ok("")
                        }
                        _ => return None,
                    })
                }
            };
            sys.simulate(model);
            sys.on_github_api(
                &who,
                &format!("{BASE}&page=1"),
                page(
                    vec![
                        value("aterm", 1, true),
                        value("dirty", 2, true),
                        value("moving", 3, true),
                    ],
                    None,
                ),
            );
            let probe = item(&who, &config, vec![recorded.clone()])
                .probe(&sys, &ctx(std::slice::from_ref(&who)));
            let rows = engine::run(
                &[item(&who, &config, vec![recorded.clone()])],
                Mode::Apply,
                &sys,
                &ctx(std::slice::from_ref(&who)),
                &mut NoJournal,
            );
            let calls = sys.calls();
            if behind {
                assert!(
                    matches!(&probe, Probe::Change { from, .. } if from.contains("1 clean worktree(s) behind GitHub")),
                    "{probe:?}"
                );
                assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
                assert_eq!(state.lock().unwrap().0, B);
                assert_eq!(
                    calls.iter().filter(|call| call.contains(" fetch ")).count(),
                    1
                );
                assert_eq!(
                    calls
                        .iter()
                        .filter(|call| call.contains(" merge --ff-only "))
                        .count(),
                    1
                );
            } else {
                assert!(matches!(&probe, Probe::Ok { .. }), "{probe:?}");
                assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
                assert!(
                    !calls
                        .iter()
                        .any(|call| call.contains(" fetch ") || call.contains(" merge "))
                );
            }
            let after = item(&who, &config, vec![recorded.clone()])
                .probe(&sys, &ctx(std::slice::from_ref(&who)));
            assert!(
                matches!(&after, Probe::Ok { now } if now.contains("1 recorded in [develop]")
                    && now.contains("~/dirty: it has uncommitted changes")),
                "{after:?}"
            );
            let recorded_reads: Vec<&String> = calls
                .iter()
                .filter(|call| call.contains(&format!("-C {} ", recorded.display())))
                .collect();
            assert!(
                recorded_reads
                    .iter()
                    .all(|call| call.contains(" rev-parse --is-inside-work-tree")
                        || call.contains(" rev-parse --show-prefix")
                        || call.contains(" rev-parse --show-toplevel")
                        || call.contains(" config --get-all remote.origin.url")),
                "{recorded_reads:?}"
            );
            for word in [
                "reset", "stash", "rebase", "checkout", "switch", "pull", "push",
            ] {
                assert!(
                    !calls.iter().any(|call| call.contains(&format!(" {word} "))
                        || call.ends_with(&format!(" {word}"))),
                    "{word}: {calls:?}"
                );
            }
        }
    }

    const GIB: u64 = aterm_capacity::floor::GIB;

    /// A data volume of 100 GiB with `free` bytes free and a floor of 8 GiB.
    fn volume(free: u64) -> aterm_capacity::reading::Volume {
        aterm_capacity::reading::Volume {
            mount: PathBuf::from("/System/Volumes/Data"),
            total: 100 * GIB,
            free,
            floor: 8 * GIB,
            room: 8 * GIB,
        }
    }

    fn sized(name: &str, id: u64, size_kib: u64) -> Value {
        let mut value = value(name, id, true);
        value
            .as_object_mut()
            .unwrap()
            .insert("size".into(), aterm_json::json!(size_kib));
        value
    }

    /// A clone runs while five times GitHub's size for it leaves the disk watch's floor free, to
    /// the byte; one that would not is named and left; an unread volume runs every clone.
    #[test]
    fn a_clone_runs_only_while_it_leaves_the_disk_watchs_floor_free() {
        let repo = |size_kib| Repo {
            owner: "alice".into(),
            name: "big".into(),
            default_branch: Some("main".into()),
            size_kib,
        };
        assert_eq!(no_room(None, &repo(u64::MAX)), None);
        assert_eq!(short_of_room(None, &[], INIT_AGAIN), None);

        // GitHub's 0 (empty, or not sized yet) still needs room above the floor.
        assert_eq!(no_room(Some(&volume(8 * GIB + 5 * 1024)), &repo(0)), None);
        assert_eq!(
            no_room(Some(&volume(8 * GIB + 5 * 1024 - 1)), &repo(0)).as_deref(),
            Some("~/big")
        );
        assert_eq!(
            no_room(Some(&volume(3 * GIB)), &repo(0)).as_deref(),
            Some("~/big")
        );

        let volume = volume(8 * GIB + 5 * 1000 * 1024);
        assert_eq!(no_room(Some(&volume), &repo(1000)), None);
        let left = [
            no_room(Some(&volume), &repo(1001)).unwrap(),
            no_room(Some(&volume), &repo(2 * 1024 * 1024)).unwrap(),
        ];
        assert_eq!(left, ["~/big", "~/big (about 10 GiB)"]);
        assert_eq!(short_of_room(Some(&volume), &[], INIT_AGAIN), None);
        assert_eq!(
            short_of_room(Some(&volume), &left, "try again").unwrap(),
            "2 repositories not cloned, for want of disk space: ~/big, ~/big (about 10 GiB); \
             /System/Volumes/Data has 8 GiB free and keeps 8 GiB free; free space, then try again"
        );
    }

    /// The plan says how many missing repositories the disk has no room for; a pass leaves
    /// out each of them, still clones the next one that fits, and ends naming the one it left.
    #[test]
    fn a_repository_the_disk_has_no_room_for_is_left_and_the_rest_cloned() {
        let (mut sys, who, config) = fixture();
        sys.volume = Some(volume(12 * GIB));
        let (_, _, clone_key) = script_clone(&sys, &who, false);
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(
                vec![sized("huge", 1, 2 * 1024 * 1024), value("new", 2, true)],
                None,
            ),
        );
        let ctx = ctx(std::slice::from_ref(&who));
        match item(&who, &config, Vec::new()).probe(&sys, &ctx) {
            Probe::Change { from, .. } => assert_eq!(
                from,
                "2 of 2 repositories missing from ~, 1 of them with no room on the disk now; 0 \
                 clean worktree(s) behind GitHub"
            ),
            other => panic!("{other:?}"),
        }
        let why = item(&who, &config, Vec::new())
            .apply(&sys, &ctx)
            .unwrap_err();
        assert_eq!(
            why,
            "1 repository not cloned, for want of disk space: ~/huge (about 10 GiB); \
             /System/Volumes/Data has 12 GiB free and keeps 8 GiB free; free space, then run \
             aterm init again"
        );
        let calls = sys.calls();
        assert_eq!(calls.iter().filter(|call| **call == clone_key).count(), 1);
        assert!(
            calls.iter().all(|call| !call.contains("alice/huge")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_worktree_whose_origin_cannot_be_read_never_stops_a_clone() {
        for (stderr, offline) in [
            (
                "git@github.com: Permission denied (publickey).\nfatal: Could not read from \
                 remote repository.\n",
                false,
            ),
            (
                "fatal: unable to access 'https://github.com/alice/present.git/': Could not \
                 resolve host: github.com\n",
                true,
            ),
        ] {
            let (sys, who, config) = fixture();
            let present = Path::new(&who.home).join("present");
            sys.stat_of(present.to_str().unwrap(), directory(who.uid));
            script_existing(&sys, &who, &present, "present");
            script_read(&sys, &who, &present, "present", "");
            sys.on(
                &git(&who, Some(&present))
                    .args(["ls-remote", "--symref", "origin", "HEAD"])
                    .display(),
                Ran::exit(128, stderr),
            );
            let (_, _, clone_key) = script_clone(&sys, &who, false);
            sys.on_github_api(
                &who,
                &format!("{BASE}&page=1"),
                page(vec![value("new", 1, true), value("present", 2, true)], None),
            );
            let origin = Origin(vec![crate::checkout::Remote::Github {
                owner: "alice".into(),
                name: "present".into(),
            }]);
            let unread = crate::checkout::read(&sys, &who, &present, &origin).unwrap_err();
            assert_eq!(unread.offline, offline);
            let ctx = ctx(std::slice::from_ref(&who));
            let item = item(&who, &config, Vec::new());
            match item.probe(&sys, &ctx) {
                Probe::Change { from, .. } => {
                    assert!(from.starts_with("1 of 2 repositories missing"), "{from}");
                }
                other => panic!("{other:?}"),
            }
            let scan = Repositories {
                user: who.clone(),
                config: config.clone(),
                recorded: Vec::new(),
            }
            .scan(&sys, &token())
            .unwrap();
            assert!(
                scan.kept().is_some_and(
                    |kept| kept.starts_with("~/present: ") && kept.contains(&unread.why)
                ),
                "{:?}",
                scan.kept()
            );
            item.apply(&sys, &ctx).unwrap();
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| **call == clone_key)
                    .count(),
                1
            );
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains(" fetch ") && !call.contains(" merge ")),
                "{:?}",
                sys.calls()
            );
        }
    }

    #[test]
    fn the_profile_s_develop_checkouts_are_left_to_the_develop_items() {
        for develop in [true, false] {
            let (sys, who, _) = fixture();
            let aterm = Path::new(&who.home).join("aterm");
            sys.stat_of(aterm.to_str().unwrap(), directory(who.uid));
            script_existing(&sys, &who, &aterm, "aterm");
            script_read(&sys, &who, &aterm, "aterm", "");
            let remote_head = git(&who, Some(&aterm))
                .args(["ls-remote", "--symref", "origin", "HEAD"])
                .display();
            sys.on(
                &remote_head,
                Ran::ok(&format!("ref: refs/heads/main\tHEAD\n{A}\tHEAD\n")),
            );
            sys.on_github_api(
                &who,
                &format!("{BASE}&page=1"),
                page(vec![value("aterm", 1, true)], None),
            );
            let mut profile = "[github]\naccount = \"alice\"\nowner = \"alice\"\ntoken_file = \
                               \"/unread-credential-source\"\n"
                .to_owned();
            if develop {
                profile.push_str("\n[develop]\ncheckouts = [\"~/aterm\"]\n");
            }
            sys.write_user_file(
                &who,
                &Path::new(&who.home).join(".config/aterm/init.toml"),
                profile.as_bytes(),
                0o600,
            )
            .unwrap();
            let ctx = ctx(std::slice::from_ref(&who));
            let items = crate::init_github::items(&sys, &ctx, None).unwrap();
            let repositories = items
                .iter()
                .find(|item| item.id().starts_with("init.github.repositories@"))
                .unwrap();
            let probe = repositories.probe(&sys, &ctx);
            let read = sys.calls().contains(&remote_head);
            if develop {
                assert!(
                    matches!(&probe, Probe::Ok { now } if now.contains("1 recorded in [develop]")),
                    "{probe:?}"
                );
                assert!(!read, "{:?}", sys.calls());
            } else {
                assert!(
                    matches!(&probe, Probe::Ok { now } if !now.contains("[develop]")),
                    "{probe:?}"
                );
                assert!(read, "{:?}", sys.calls());
            }
        }
    }

    #[test]
    fn plan_and_check_probe_do_not_clone_or_change_existing_branches_or_files() {
        let (sys, who, config) = fixture();
        let home = Path::new(&who.home);
        let existing = home.join("existing");
        sys.stat_of(existing.to_str().unwrap(), directory(who.uid));
        sys.file(
            existing.join("dirty.txt").to_str().unwrap(),
            b"owner changes",
        );
        script_existing(&sys, &who, &existing, "existing");
        script_read(&sys, &who, &existing, "existing", "?? dirty.txt\0");
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(
                vec![value("existing", 1, true), value("missing", 2, true)],
                None,
            ),
        );
        let rows = engine::run(
            &[item(&who, &config, Vec::new())],
            Mode::Plan,
            &sys,
            &ctx(std::slice::from_ref(&who)),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Planned);
        assert_eq!(
            sys.read_file(&existing.join("dirty.txt")).unwrap(),
            b"owner changes"
        );
        assert!(sys.calls().iter().all(|call| !call.contains("scratch")
            && !call.contains(" clone ")
            && !call.contains("rename")
            && !call.contains("checkout")
            && !call.contains("fetch")
            && !call.contains("pull")
            && !call.contains("reset")));
    }

    #[test]
    fn occupied_symlink_and_mismatched_origins_are_preserved_and_never_fetched() {
        for kind in 0..3 {
            let (sys, who, config) = fixture();
            let path = Path::new(&who.home).join("one");
            match kind {
                0 => {
                    sys.file(path.to_str().unwrap(), b"owner file");
                }
                1 => {
                    sys.link(path.to_str().unwrap(), "/elsewhere");
                }
                2 => {
                    sys.stat_of(path.to_str().unwrap(), directory(who.uid));
                    let cmd =
                        git(&who, Some(&path)).args(["config", "--get-all", "remote.origin.url"]);
                    sys.on(
                        &cmd.display(),
                        Ran::ok("https://private-token@github.com/alice/one.git\n"),
                    );
                    script_existing(&sys, &who, &path, "one");
                }
                _ => unreachable!(),
            }
            sys.on_github_api(
                &who,
                &format!("{BASE}&page=1"),
                page(vec![value("one", 1, true)], None),
            );
            let row = engine::run(
                &[item(&who, &config, Vec::new())],
                Mode::Apply,
                &sys,
                &ctx(std::slice::from_ref(&who)),
                &mut NoJournal,
            )
            .remove(0);
            assert_eq!(row.outcome, Outcome::Unknown);
            assert!(!row.why.unwrap().contains("private-token"));
            assert!(sys.calls().iter().all(|call| !call.contains("scratch")
                && !call.contains(" clone ")
                && !call.contains("rename")
                && !call.contains("fetch")));
            assert!(sys.stat(&path).is_ok());
        }
    }

    fn script_clone(sys: &Fake, who: &User, empty: bool) -> (Repo, PathBuf, String) {
        script_clone_with(sys, who, empty, None)
    }

    fn script_clone_with(
        sys: &Fake,
        who: &User,
        empty: bool,
        guard: Option<&CloneGuard<'_>>,
    ) -> (Repo, PathBuf, String) {
        let repo = Repo {
            owner: "alice".into(),
            name: "new".into(),
            default_branch: Some("main".into()),
            size_kib: 0,
        };
        let url = "https://github.com/alice/new.git";
        sys.on(
            &git(who, None)
                .args(["ls-remote", "--get-url", "--", url])
                .display(),
            Ran::ok(url),
        );
        let remote = if empty {
            String::new()
        } else {
            format!(
                "ref: refs/heads/main\tHEAD\n{A}\tHEAD\n{A}\trefs/heads/main\n{B}\trefs/heads/topic\n{B}\trefs/tags/v1\n"
            )
        };
        let command = network_git(who, url, guard).args([
            "ls-remote",
            "--symref",
            "--",
            url,
            "HEAD",
            "refs/heads/*",
            "refs/tags/*",
        ]);
        sys.on(&command.display(), Ran::ok(&remote));
        let staged = Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
        let command = network_git(who, url, guard)
            .budget(CLONE_BUDGET)
            .args([
                "clone",
                "--no-single-branch",
                "--tags",
                "--origin",
                "origin",
                "--template=",
                "--",
                url,
            ])
            .arg(&staged);
        let clone_key = command.display();
        sys.on(&clone_key, Ran::ok(""));
        sys.on_stat(&clone_key, &staged, directory(who.uid));
        sys.on_write(&clone_key, &staged.join(".git/fixture"), b"complete clone");
        script_existing(sys, who, &staged, "new");
        sys.on(
            &git(who, Some(&staged))
                .args(["rev-parse", "--git-path", "objects/info/alternates"])
                .display(),
            Ran::ok(".git/objects/info/alternates\n"),
        );
        let head = git(who, Some(&staged)).args(["rev-parse", "--verify", "HEAD^{commit}"]);
        sys.on(
            &head.display(),
            if empty {
                Ran::exit(128, "unborn")
            } else {
                Ran::ok(A)
            },
        );
        let refs = if empty {
            String::new()
        } else {
            format!(
                "{A} refs/heads/main\n{A} refs/remotes/origin/HEAD\n{A} refs/remotes/origin/main\n{B} refs/remotes/origin/topic\n{B} refs/tags/v1\n"
            )
        };
        sys.on(
            &git(who, Some(&staged))
                .args(["for-each-ref", "--format=%(objectname) %(refname)"])
                .display(),
            Ran::ok(&refs),
        );
        sys.on(
            &git(who, Some(&staged))
                .args(["symbolic-ref", "HEAD", "refs/heads/main"])
                .display(),
            Ran::ok(""),
        );
        sys.on(
            &git(who, Some(&staged))
                .args(["symbolic-ref", "--quiet", "HEAD"])
                .display(),
            Ran::ok("refs/heads/main\n"),
        );
        sys.on(
            &git(who, Some(&staged))
                .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
                .display(),
            Ran::ok("refs/remotes/origin/main\n"),
        );
        sys.on(
            &git(who, Some(&staged))
                .args(["config", "--get", "branch.main.remote"])
                .display(),
            Ran::ok("origin\n"),
        );
        sys.on(
            &git(who, Some(&staged))
                .args(["config", "--get", "branch.main.merge"])
                .display(),
            Ran::ok("refs/heads/main\n"),
        );
        sys.on(
            &git(who, Some(&staged))
                .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
                .display(),
            Ran::ok(""),
        );
        (repo, staged, clone_key)
    }

    #[test]
    fn full_and_empty_new_clones_are_verified_then_published_without_credentials_in_origin() {
        for empty in [false, true] {
            let (sys, who, _) = fixture();
            let (repo, staged, _) = script_clone(&sys, &who, empty);
            clone_one(&sys, &who, "alice", &repo).unwrap();
            let final_path = Path::new(&who.home).join("new");
            assert_eq!(
                sys.read_file(&final_path.join(".git/fixture")).unwrap(),
                b"complete clone"
            );
            assert!(sys.stat(&staged).is_err());
            let calls = sys.calls();
            let fsck = calls
                .iter()
                .position(|call| call.contains("fsck --connectivity-only"))
                .unwrap();
            let published = calls
                .iter()
                .position(|call| call.starts_with("rename-noreplace "))
                .unwrap();
            assert!(fsck < published);
            assert_eq!(
                calls.iter().filter(|call| call.contains(" clone ")).count(),
                1
            );
        }
        for endpoint in [
            "git@github.com:alice/new.git",
            "ssh://git@github.com:22/alice/new.git",
        ] {
            let (sys, who, _) = fixture();
            let key = git(&who, None)
                .args([
                    "ls-remote",
                    "--get-url",
                    "--",
                    "https://github.com/alice/new.git",
                ])
                .display();
            sys.on(&key, Ran::ok(endpoint)).on(&key, Ran::ok(endpoint));
            let (repo, _, _) = script_clone(&sys, &who, false);
            clone_one(&sys, &who, "alice", &repo).unwrap();
            assert!(sys.stat(&Path::new(&who.home).join("new")).unwrap().dir);
        }
    }

    #[test]
    fn guarded_clones_pin_the_snapshot_helper_and_refuse_newer_versions() {
        for changed in [false, true] {
            let (sys, who, _) = fixture();
            let current = || {
                if changed && sys.calls().iter().any(|call| call.contains(" clone ")) {
                    Err("injected newer vault version".into())
                } else {
                    Ok(())
                }
            };
            let guard = CloneGuard {
                account: "captured-account",
                current: &current,
            };
            let (repo, _staged, _) = script_clone_with(&sys, &who, false, Some(&guard));
            let result = clone_one_guarded(&sys, &who, &repo, guard.account, &current);
            assert_eq!(result.is_ok(), !changed, "{result:?}");
            let final_path = Path::new(&who.home).join("new");
            assert_eq!(sys.stat(&final_path).is_ok(), !changed);
            let calls = sys.calls();
            let network: Vec<_> = calls
                .iter()
                .filter(|call| call.contains("ls-remote --symref") || call.contains(" clone "))
                .collect();
            assert_eq!(network.len(), 2, "{calls:?}");
            assert!(
                network
                    .iter()
                    .all(|call| call.contains(".helper=osxkeychain")
                        && call.contains(".username=captured-account")
                        && call.contains(".useHttpPath=false"))
            );
            assert!(calls.iter().all(|call| !call.contains("--global")));
            assert_eq!(
                calls
                    .iter()
                    .any(|call| call.starts_with("rename-noreplace ")),
                !changed
            );
        }
    }

    #[test]
    fn guarded_clones_refuse_ssh_rewrites_before_network_work() {
        for endpoint in [
            "git@github.com:alice/new.git",
            "ssh://git@github.com/alice/new.git",
        ] {
            let (sys, who, _) = fixture();
            let repo = Repo {
                owner: "alice".into(),
                name: "new".into(),
                default_branch: Some("main".into()),
                size_kib: 0,
            };
            let url = "https://github.com/alice/new.git";
            sys.on(
                &git(&who, None)
                    .args(["ls-remote", "--get-url", "--", url])
                    .display(),
                Ran::ok(endpoint),
            );
            let why = clone_one_guarded(&sys, &who, &repo, "alice", &|| Ok(())).unwrap_err();
            assert!(why.contains("HTTPS"), "{why}");
            assert!(sys.calls().iter().all(|call| !call.contains("--symref")
                && !call.contains(" clone ")
                && !call.contains("scratch")));
        }
    }

    #[test]
    fn failure_or_a_destination_created_during_clone_never_overwrites_owner_data() {
        for race in [false, true] {
            let (sys, who, _) = fixture();
            let staged =
                Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
            if !race {
                sys.on(
                    &git(&who, Some(&staged))
                        .args(["fsck", "--connectivity-only", "--no-dangling"])
                        .display(),
                    Ran::exit(1, "failure includes secret-like bytes that must not escape"),
                );
            }
            let (repo, _, clone_key) = script_clone(&sys, &who, false);
            let destination = Path::new(&who.home).join("new");
            if race {
                sys.on_stat(&clone_key, &destination, directory(who.uid));
                sys.on_write(&clone_key, &destination.join("owner.txt"), b"racing owner");
            }
            let error = clone_one(&sys, &who, "alice", &repo).unwrap_err();
            assert!(!error.contains("secret-like"));
            assert!(sys.stat(&staged).is_err());
            if race {
                assert_eq!(
                    sys.read_file(&destination.join("owner.txt")).unwrap(),
                    b"racing owner"
                );
                assert!(sys.read_file(&destination.join(".git/fixture")).is_err());
            } else {
                assert!(sys.stat(&destination).is_err());
                assert!(
                    sys.calls()
                        .iter()
                        .all(|call| !call.starts_with("rename-noreplace "))
                );
            }
        }
        for endpoint in [
            "https://mirror.invalid/alice/new.git",
            "https://github.com/other/new.git",
            "https://private-token@github.com/alice/new.git",
            "file:///owner/repository",
            "https://github.com/alice/new.git\nhttps://elsewhere.invalid/new.git",
            "",
        ] {
            let (sys, who, _) = fixture();
            let key = git(&who, None)
                .args([
                    "ls-remote",
                    "--get-url",
                    "--",
                    "https://github.com/alice/new.git",
                ])
                .display();
            sys.on(&key, Ran::ok(endpoint));
            let (repo, _, _) = script_clone(&sys, &who, false);
            let error = clone_one(&sys, &who, "alice", &repo).unwrap_err();
            assert!(!error.contains(endpoint) || endpoint.is_empty());
            assert!(sys.calls().iter().all(|call| !call.contains("--symref")
                && !call.contains(" clone ")
                && !call.contains("scratch allocated")));
        }
        let (sys, who, _) = fixture();
        let key = git(&who, None)
            .args([
                "ls-remote",
                "--get-url",
                "--",
                "https://github.com/alice/new.git",
            ])
            .display();
        sys.on(&key, Ran::ok("https://github.com/alice/new.git"))
            .on(&key, Ran::ok("https://mirror.invalid/alice/new.git"));
        let (repo, _, _) = script_clone(&sys, &who, false);
        assert!(clone_one(&sys, &who, "alice", &repo).is_err());
        assert!(sys.calls().iter().any(|call| call.contains("--symref")));
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains(" clone ") && !call.contains("scratch allocated"))
        );
    }

    #[test]
    fn matching_real_dirty_worktree_on_an_alternate_branch_is_only_read() {
        crate::items::github::need_clt_git();
        let sys = Live::new();
        let home = Scratch::new(&sys, "github-repo-fixture").unwrap();
        let mut who = user("fixture", sys.euid());
        who.home = home.path().to_string_lossy().into_owned();
        let path = home.path().join("existing");
        text(
            &sys,
            &git(&who, None)
                .args(["init", "--template=", "--initial-branch=main"])
                .arg(&path),
            "fixture init",
        )
        .unwrap();
        text(
            &sys,
            // A commit starts automatic maintenance too, whose fork would hold this drain's
            // pipe past git's exit on a loaded machine.
            &git(&who, Some(&path))
                .args(crate::checkout::NO_SIDE_RUNS)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "fixture",
                ]),
            "fixture commit",
        )
        .unwrap();
        text(
            &sys,
            &git(&who, Some(&path)).args([
                "remote",
                "add",
                "origin",
                "git@github.com:alice/existing.git",
            ]),
            "fixture remote",
        )
        .unwrap();
        text(
            &sys,
            &git(&who, Some(&path)).args(["switch", "-c", "topic"]),
            "fixture branch",
        )
        .unwrap();
        sys.write_file(&path.join("dirty.txt"), b"local owner edits", 0o600)
            .unwrap();
        let repo = Repo {
            owner: "alice".into(),
            name: "existing".into(),
            default_branch: Some("main".into()),
            size_kib: 0,
        };
        verify_existing(&sys, &who, "alice", &repo, &path).unwrap();
        assert_eq!(
            sys.read_file(&path.join("dirty.txt")).unwrap(),
            b"local owner edits"
        );
        assert_eq!(
            git_read(&sys, &who, &path, &["symbolic-ref", "--quiet", "HEAD"])
                .unwrap()
                .trim(),
            "refs/heads/topic"
        );
    }

    #[test]
    fn deliberately_shallow_or_partial_matching_existing_checkout_is_preserved_without_integrity_probes()
     {
        let (sys, who, _) = fixture();
        let path = Path::new(&who.home).join("existing");
        script_existing(&sys, &who, &path, "existing");
        sys.refuse_complete_stdout(
            &git(&who, Some(&path))
                .args(["rev-parse", "--is-shallow-repository"])
                .display(),
            "history intentionally unavailable",
        );
        let repo = Repo {
            owner: "alice".into(),
            name: "existing".into(),
            default_branch: Some("main".into()),
            size_kib: 0,
        };
        verify_existing(&sys, &who, "alice", &repo, &path).unwrap();
        assert!(sys.calls().iter().all(|call| !call.contains("shallow")
            && !call.contains("partialclone")
            && !call.contains("fsck")
            && !call.contains("fetch")));
    }

    #[test]
    fn dirty_new_checkout_or_incorrect_tracking_is_never_published() {
        for (args, bad) in [
            (
                vec!["status", "--porcelain=v1", "-z", "--untracked-files=all"],
                " D missing.txt\0",
            ),
            (
                vec!["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
                "refs/remotes/origin/other\n",
            ),
            (vec!["config", "--get", "branch.main.remote"], "other\n"),
            (
                vec!["config", "--get", "branch.main.merge"],
                "refs/heads/other\n",
            ),
        ] {
            let (sys, who, _) = fixture();
            let staged =
                Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
            sys.on(&git(&who, Some(&staged)).args(args).display(), Ran::ok(bad));
            let (repo, _, _) = script_clone(&sys, &who, false);
            assert!(clone_one(&sys, &who, "alice", &repo).is_err());
            assert!(sys.stat(&Path::new(&who.home).join("new")).is_err());
            assert!(sys.stat(&staged).is_err());
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.starts_with("rename-noreplace "))
            );
        }
    }
}
