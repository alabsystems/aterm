// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Preserve matching local worktrees and publish only verified, full new clones.
//! Clones are sequential: at most one clone/network transfer is active, with a
//! thirty-minute bound per clone. No existing worktree is fetched, checked out or
//! rewritten. GitHub pagination and every structured git read require complete EOF.

use super::{Github, auth_id, auth_ready, gh};
use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{Cmd, Scratch, System};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::time::Duration;

const GIT: &str = crate::items::github::CLT_GIT;
// Full repository objects contain many URL templates. Smaller pages leave room
// under the strict capture bound without jq/header ordering assumptions.
const PAGE_SIZE: usize = 30;
const API_BUDGET: Duration = Duration::from_secs(120);
const GIT_READ_BUDGET: Duration = Duration::from_secs(300);
const CLONE_BUDGET: Duration = Duration::from_secs(1800);

/// One reconciliation item for every repository owned by the configured account/org.
pub(super) fn item(user: &User, config: &Github) -> Box<dyn Item> {
    Box::new(Repositories {
        user: user.clone(),
        config: Github {
            account: config.account.clone(),
            owner: config.owner.clone(),
            token_file: config.token_file.clone(),
            token_key: config.token_key.clone(),
        },
    })
}

struct Repositories {
    user: User,
    config: Github,
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
        match auth_ready(sys, &self.user, &self.config) {
            Err(why) => return Probe::Unusable { why },
            Ok(false) => return Probe::Waits {
                on: auth_id(&self.user),
                why: "enumerate private and public repositories after GitHub authentication is configured".into(),
            },
            Ok(true) => {}
        }
        match self.scan(sys) {
            Err(why) => Probe::Unusable { why },
            Ok((all, missing)) if missing.is_empty() => Probe::Ok {
                now: format!(
                    "{} matching worktrees present; existing history, branches and changes preserved",
                    all.len()
                ),
            },
            Ok((all, missing)) => Probe::Change {
                from: format!(
                    "{} of {} repositories missing from ~",
                    missing.len(),
                    all.len()
                ),
                to:
                    "clone missing repositories with full history; preserve every existing worktree"
                        .into(),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if !auth_ready(sys, &self.user, &self.config)? {
            return Err("GitHub authentication changed; no repositories cloned".into());
        }
        let (_, missing) = self.scan(sys)?;
        for repo in missing {
            clone_one(sys, &self.user, &self.config.owner, &repo)?;
        }
        Ok(())
    }
}

impl Repositories {
    fn scan(&self, sys: &dyn System) -> Result<(Vec<Repo>, Vec<Repo>), String> {
        let home = Path::new(&self.user.home);
        let stat = sys
            .stat(home)
            .map_err(|_| "cannot inspect the repository destination home")?;
        if !home.is_absolute() || !stat.dir || stat.symlink || stat.uid != self.user.uid {
            return Err("repository destination must be this user's own home directory".into());
        }
        if !sys.stat(Path::new(GIT)).is_ok_and(|stat| !stat.dir) {
            return Err("Command Line Tools git is not installed; no repositories changed".into());
        }
        let all = enumerate(sys, &self.user, &self.config)?;
        let mut missing = Vec::new();
        for repo in &all {
            let path = home.join(&repo.name);
            match sys.stat(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => missing.push(repo.clone()),
                Ok(stat) if stat.dir && !stat.symlink => {
                    verify_existing(sys, &self.user, &self.config.owner, repo, &path)?
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
        Ok((all, missing))
    }
}

#[derive(Clone, Debug)]
struct Repo {
    name: String,
    default_branch: Option<String>,
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

fn gh_command(sys: &dyn System, user: &User) -> Result<Cmd, String> {
    Ok(super::gh_cmd(&gh(sys)?, user).budget(API_BUDGET))
}

fn api(sys: &dyn System, user: &User, endpoint: &str) -> Result<String, String> {
    let cmd = gh_command(sys, user)?.args([
        "api",
        "--hostname",
        "github.com",
        "--method",
        "GET",
        "--include",
        "-H",
        "Accept: application/vnd.github+json",
        endpoint,
    ]);
    text(sys, &cmd, "GitHub repository enumeration failed")
}

fn text(sys: &dyn System, cmd: &Cmd, why: &str) -> Result<String, String> {
    let ran = sys.run_complete_stdout(cmd).map_err(|_| why.to_owned())?;
    if !ran.success() {
        return Err(why.into());
    }
    String::from_utf8(ran.stdout).map_err(|_| why.to_owned())
}

fn response(text: &str) -> Result<(Value, BTreeMap<String, String>), String> {
    let normalized = text.replace("\r\n", "\n");
    let (headers, body) = normalized
        .split_once("\n\n")
        .ok_or("incomplete GitHub HTTP response")?;
    let mut lines = headers.lines();
    let status = lines
        .next()
        .ok_or("missing GitHub HTTP status")?
        .split_whitespace()
        .collect::<Vec<_>>();
    if status.len() < 2 || !status[0].starts_with("HTTP/") || status[1] != "200" {
        return Err("GitHub repository listing did not return HTTP 200".into());
    }
    let mut fields = BTreeMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or("unrecognized GitHub response header")?;
        let name = name.to_ascii_lowercase();
        // Only pagination headers affect this reader. Repeated unrelated headers
        // (e.g. Set-Cookie) are legal; repeated Link fields are ambiguous here.
        if name == "link" && fields.insert(name, value.trim().into()).is_some() {
            return Err("ambiguous GitHub pagination headers".into());
        }
    }
    let value =
        aterm_json::from_str(body).map_err(|_| "unrecognized GitHub repository response")?;
    Ok((value, fields))
}

fn enumerate(sys: &dyn System, user: &User, config: &Github) -> Result<Vec<Repo>, String> {
    if !component(&config.owner, true) || !component(&config.account, true) {
        return Err("invalid configured GitHub owner or account".into());
    }
    let base = if config.owner.eq_ignore_ascii_case(&config.account) {
        "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc"
            .to_owned()
    } else {
        let (owner, _) = response(&api(sys, user, &format!("/users/{}", config.owner))?)?;
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
    let mut repositories = Vec::new();
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut last = None;
    for page in 1..=10_000 {
        let endpoint = format!("{base}&page={page}");
        let (value, headers) = response(&api(sys, user, &endpoint)?)?;
        let values = value
            .as_array()
            .ok_or("GitHub repository page is not an array")?;
        if values.len() > PAGE_SIZE {
            return Err("GitHub repository page exceeds its requested bound".into());
        }
        let (next, advertised_last) = pagination(headers.get("link"), &base, page)?;
        if let Some(advertised) = advertised_last {
            if advertised < page || last.is_some_and(|last| last != advertised) {
                return Err("GitHub pagination changed during enumeration; run init again".into());
            }
            last = Some(advertised);
        }
        for value in values {
            let repo = repo(value, &config.owner)?;
            let id = value
                .get("id")
                .and_then(Value::as_u64)
                .filter(|id| *id != 0)
                .ok_or("repository identity is missing")?;
            if !ids.insert(id) || !names.insert(repo.name.to_ascii_lowercase()) {
                return Err("duplicate repositories in GitHub pagination; run init again".into());
            }
            repositories.push(repo);
        }
        if next {
            if values.is_empty() {
                return Err("empty GitHub page advertises more repositories".into());
            }
            continue;
        }
        if last.is_some_and(|last| page < last) {
            return Err("GitHub pagination ended before its advertised final page".into());
        }
        if values.len() == PAGE_SIZE {
            // Exactly one full final page is valid. Confirm the next page is empty
            // rather than treating a missing Link header as evidence of completion.
            let (tail, tail_headers) =
                response(&api(sys, user, &format!("{base}&page={}", page + 1))?)?;
            let (tail_next, tail_last) = pagination(tail_headers.get("link"), &base, page + 1)?;
            if tail.as_array().is_none_or(|tail| !tail.is_empty())
                || tail_next
                || tail_last.is_some_and(|last| last != page)
            {
                return Err("GitHub pagination omitted additional repositories".into());
            }
        }
        repositories.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
        return Ok(repositories);
    }
    Err("GitHub repository pagination exceeded its explicit bound".into())
}

fn pagination(
    link: Option<&String>,
    base: &str,
    page: usize,
) -> Result<(bool, Option<usize>), String> {
    let Some(link) = link else {
        return Ok((false, None));
    };
    let (expected_path, expected_query) = base
        .split_once('?')
        .ok_or("invalid repository listing endpoint")?;
    let expected: BTreeMap<_, _> = expected_query
        .split('&')
        .map(|part| part.split_once('=').ok_or("invalid listing query"))
        .collect::<Result<_, _>>()?;
    let mut relations = BTreeSet::new();
    let (mut next, mut last) = (false, None);
    for entry in link.split(',') {
        let (target, relation) = entry
            .trim()
            .split_once(';')
            .ok_or("unrecognized GitHub pagination link")?;
        let url = target
            .strip_prefix('<')
            .and_then(|url| url.strip_suffix('>'))
            .and_then(|url| url.strip_prefix("https://api.github.com"))
            .ok_or("GitHub pagination points outside the expected API")?;
        let (path, query) = url
            .split_once('?')
            .ok_or("GitHub pagination has no query")?;
        let mut params = BTreeMap::new();
        for part in query.split('&') {
            let (key, value) = part
                .split_once('=')
                .ok_or("unrecognized GitHub pagination query")?;
            if params.insert(key, value).is_some() {
                return Err("duplicate GitHub pagination query key".into());
            }
        }
        let target_page = params
            .remove("page")
            .and_then(|page| page.parse::<usize>().ok())
            .filter(|page| *page != 0)
            .ok_or("invalid GitHub pagination page")?;
        if path != expected_path || params != expected {
            return Err("GitHub pagination changed the requested repository scope".into());
        }
        let relation = relation
            .trim()
            .strip_prefix("rel=\"")
            .and_then(|relation| relation.strip_suffix('"'))
            .ok_or("unrecognized GitHub pagination relation")?;
        if !relations.insert(relation) {
            return Err("duplicate GitHub pagination relation".into());
        }
        match relation {
            "next" if target_page == page + 1 => next = true,
            // An extra empty confirmation page can link back to its last real
            // page. The caller validates last against actual page contents.
            "last" => last = Some(target_page),
            "first" if target_page == 1 => {}
            "prev" if page > 1 && target_page == page - 1 => {}
            _ => return Err("inconsistent GitHub repository pagination".into()),
        }
    }
    Ok((next, last))
}

fn repo(value: &Value, owner: &str) -> Result<Repo, String> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| component(name, false))
        .ok_or("invalid GitHub repository name")?;
    if value
        .get("owner")
        .and_then(|owner| owner.get("login"))
        .and_then(Value::as_str)
        .is_none_or(|login| !login.eq_ignore_ascii_case(owner))
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
        name: name.into(),
        default_branch,
    })
}

fn origin_matches(url: &str, owner: &str, name: &str) -> bool {
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com:22/"));
    path.is_some_and(|path| {
        let path = path.strip_suffix(".git").unwrap_or(path);
        path.eq_ignore_ascii_case(&format!("{owner}/{name}"))
    })
}

fn git(user: &User, path: Option<&Path>) -> Cmd {
    let mut command = Cmd::new(GIT)
        .as_user(user)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .budget(GIT_READ_BUDGET);
    if let Some(path) = path {
        command = command.arg("-C").arg(path);
    }
    command
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
        // Git reports a physical root (/private/var on macOS) even when the
        // admitted path uses a system alias (/var). An empty show-prefix proves
        // this -C directory is the root without mistaking that alias for nesting.
        || !Path::new(git_read(sys, user, path, &["rev-parse", "--show-toplevel"])?.trim()).is_absolute()
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
            // Peeled annotated-tag rows have a different oid and are checked by fsck.
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

// Read Git's effective URL without contacting the remote. Keep normal user
// configuration; refuse a stable rewrite outside this repository rather than
// mistaking the stored literal origin for the transport's selected endpoint.
// This is a point-in-time config read, not an atomic config/transport snapshot or
// proof about proxies, SSH commands, credentials, filters or hook callbacks.
fn admit_endpoint(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
    url: &str,
) -> Result<(), String> {
    let expanded = text(
        sys,
        &git(user, None).args(["ls-remote", "--get-url", "--", url]),
        "cannot inspect Git's effective GitHub repository URL",
    )?;
    let mut lines = expanded.lines();
    if lines
        .next()
        .is_none_or(|line| !origin_matches(line, owner, &repo.name))
        || lines.next().is_some()
    {
        return Err(
            "Git's effective URL does not name the configured GitHub repository; no clone started"
                .into(),
        );
    }
    Ok(())
}

fn clone_one(sys: &dyn System, user: &User, owner: &str, repo: &Repo) -> Result<(), String> {
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
    admit_endpoint(sys, user, owner, repo, &url)?;
    let source = text(
        sys,
        &git(user, None).args([
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
    // Remote discovery may have waited for the network; inspect the current
    // rewrite again before allocating or cloning. Neither check freezes config.
    admit_endpoint(sys, user, owner, repo, &url)?;
    let mut staging = Scratch::new_in(sys, home, "github-clone")
        .map_err(|_| "cannot allocate private repository staging in this home")?;
    let clone = staging.path().join("repo");
    let command = git(user, None)
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
    // The OS operation itself must reject an occupied final name; a prior stat
    // cannot prevent another process from creating it after this check.
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
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Live, Ran, Stat};
    use std::path::PathBuf;

    const GH: &str = "/opt/homebrew/bin/gh";
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

    fn fixture() -> (Fake, User, Github) {
        let mut sys = Fake::root();
        let who = user("alice", 501);
        sys.euid = who.uid;
        sys.stat_of(&who.home, directory(who.uid));
        sys.file(GH, b"synthetic gh");
        sys.file(GIT, b"synthetic git");
        let helper = Cmd::new(GIT).as_user(&who).args([
            "config",
            "--global",
            "--get-all",
            "credential.https://github.com.helper",
        ]);
        sys.on(
            &helper.display(),
            Ran::ok("\n!/opt/homebrew/bin/gh auth git-credential\n"),
        );
        let active = super::super::gh_cmd(Path::new(GH), &who).args([
            "api",
            "--hostname",
            "github.com",
            "user",
            "--jq",
            ".login",
        ]);
        sys.on(&active.display(), Ran::ok("alice\n"));
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

    fn api_key(sys: &dyn System, who: &User, endpoint: &str) -> String {
        gh_command(sys, who)
            .unwrap()
            .args([
                "api",
                "--hostname",
                "github.com",
                "--method",
                "GET",
                "--include",
                "-H",
                "Accept: application/vnd.github+json",
                endpoint,
            ])
            .display()
    }

    fn page(values: Vec<Value>, links: Option<String>) -> Ran {
        Ran::ok(&format!(
            "HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n{}\r\n{}",
            links.map_or_else(String::new, |links| format!("Link: {links}\r\n")),
            aterm_json::to_string(&values).unwrap()
        ))
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
        sys.on(
            &api_key(&sys, &who, &format!("{BASE}&page=1")),
            page(first, Some(links)),
        );
        let mut empty = value("zz-empty-private", 101, true);
        empty
            .as_object_mut()
            .unwrap()
            .insert("default_branch".into(), Value::Null);
        sys.on(
            &api_key(&sys, &who, &format!("{BASE}&page=2")),
            page(vec![empty], None),
        );
        let repos = enumerate(&sys, &who, &config).unwrap();
        assert_eq!(repos.len(), 31);
        assert_eq!(repos.last().unwrap().default_branch, None);
        assert_eq!(sys.calls().len(), 2);
        assert!(
            sys.calls()
                .iter()
                .all(|call| call.contains(" api ") && !call.contains("clone"))
        );

        // A full terminal page needs an empty-page confirmation. GitHub may
        // attach backward last/prev links to that out-of-range empty page.
        let (sys, who, config) = fixture();
        sys.on(
            &api_key(&sys, &who, &format!("{BASE}&page=1")),
            page(
                (0..30)
                    .map(|i| value(&format!("repo{i:03}"), i + 1, false))
                    .collect(),
                None,
            ),
        );
        sys.on(
            &api_key(&sys, &who, &format!("{BASE}&page=2")),
            page(
                vec![],
                Some(format!(
                    "<https://api.github.com{BASE}&page=1>; rel=\"prev\", <https://api.github.com{BASE}&page=1>; rel=\"last\""
                )),
            ),
        );
        assert_eq!(enumerate(&sys, &who, &config).unwrap().len(), 30);
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
                    sys.on(
                        &api_key(&sys, &who, &format!("{BASE}&page=2")),
                        page(vec![value("omitted", 101, true)], None),
                    );
                }
                _ => unreachable!(),
            }
            sys.on(
                &api_key(&sys, &who, &format!("{BASE}&page=1")),
                page(values, links),
            );
            assert!(enumerate(&sys, &who, &config).is_err());
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains("scratch") && !call.contains(" clone "))
            );
        }
        let (sys, who, config) = fixture();
        let key = api_key(&sys, &who, &format!("{BASE}&page=1"));
        sys.on(&key, page(vec![value("one", 1, true)], None));
        sys.refuse_complete_stdout(&key, "incomplete stdout");
        assert!(enumerate(&sys, &who, &config).is_err());
    }

    #[test]
    fn differing_personal_owner_is_refused_and_organization_scope_is_explicit() {
        let (sys, who, mut config) = fixture();
        config.owner = "other".into();
        let owner = api_key(&sys, &who, "/users/other");
        sys.on(
            &owner,
            Ran::ok("HTTP/2.0 200 OK\n\n{\"login\":\"other\",\"type\":\"User\"}"),
        );
        assert!(
            enumerate(&sys, &who, &config)
                .unwrap_err()
                .contains("organization")
        );
        let (sys, who, mut config) = fixture();
        config.owner = "team".into();
        sys.on(
            &api_key(&sys, &who, "/users/team"),
            Ran::ok("HTTP/2.0 200 OK\n\n{\"login\":\"team\",\"type\":\"Organization\"}"),
        );
        sys.on(
            &api_key(
                &sys,
                &who,
                "/orgs/team/repos?type=all&per_page=30&sort=full_name&direction=asc&page=1",
            ),
            page(vec![], None),
        );
        assert!(enumerate(&sys, &who, &config).unwrap().is_empty());
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
        sys.on(
            &api_key(&sys, &who, &format!("{BASE}&page=1")),
            page(
                vec![value("existing", 1, true), value("missing", 2, true)],
                None,
            ),
        );
        let rows = engine::run(
            &[item(&who, &config)],
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
            sys.on(
                &api_key(&sys, &who, &format!("{BASE}&page=1")),
                page(vec![value("one", 1, true)], None),
            );
            let row = engine::run(
                &[item(&who, &config)],
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
        let repo = Repo {
            name: "new".into(),
            default_branch: Some("main".into()),
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
        let command = git(who, None).args([
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
        let command = git(who, None)
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
        // Normal GitHub SSH rewrites keep the same owner/repository and remain
        // supported. The actual clone still receives the original literal URL.
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
        // A stored literal GitHub origin is not evidence about an insteadOf
        // redirect. Refuse before even remote discovery, without echoing its URL.
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
        // A redirect that changes while discovery waits is checked again before
        // clone allocation. This does not claim atomicity during either child.
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
            &git(&who, Some(&path)).args([
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
            name: "existing".into(),
            default_branch: Some("main".into()),
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
        // These commands deliberately have no usable responses. Identity-only
        // preservation must never try to deepen, inspect or repair owner history.
        sys.refuse_complete_stdout(
            &git(&who, Some(&path))
                .args(["rev-parse", "--is-shallow-repository"])
                .display(),
            "history intentionally unavailable",
        );
        let repo = Repo {
            name: "existing".into(),
            default_branch: Some("main".into()),
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
