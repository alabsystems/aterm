// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The private inventory selects required gitlinks; the parent's tracked tree supplies
//! their identities and exact commits. Optional submodules are never visited.

use super::repos::{self, Repo};
use crate::checkout::{self, Remote};
use crate::model::User;
use crate::port::{Cmd, Scratch, System};
use aterm_json::Value;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(1800);

fn local(sys: &dyn System, user: &User, path: Option<&Path>) -> Cmd {
    checkout::git(sys, user, path)
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("--no-lazy-fetch")
        .args(["-c", "core.fsmonitor=false"])
}

fn require_local_reads(sys: &dyn System, user: &User, repo: &Repo) -> Result<(), String> {
    if repo.required_submodules.is_empty() {
        return Ok(());
    }
    if !supports_local_reads(sys, user)? {
        return Err("Git must support --no-lazy-fetch to inspect required dependencies safely; required dependencies stay untouched".into());
    }
    Ok(())
}

/// The selected executable must recognize the option; its environment equivalent
/// alone can be silently ignored by older Git. No repository or object is read.
pub(super) fn supports_local_reads(sys: &dyn System, user: &User) -> Result<bool, String> {
    let ran = sys.run_complete_stdout(&local(sys, user, None).arg("--version"))
        .map_err(|_| "cannot check Git's --no-lazy-fetch capability; required dependencies stay untouched")?;
    Ok(ran.success())
}

/// Admit canonical, bounded relative paths, with no overlaps even on a case-insensitive
/// filesystem. This is inventory policy, not a list inferred from .gitmodules.
pub(super) fn paths(value: Option<&Value>) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .filter(|values| values.len() <= 64)
        .ok_or("required_submodules must be an array of at most 64 relative gitlink paths")?;
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for value in values {
        let path = value
            .as_str()
            .filter(|path| valid_path(path))
            .ok_or("required_submodules contains a noncanonical relative gitlink path")?;
        let folded = path.to_ascii_lowercase();
        if seen.iter().any(|old: &String| {
            old == &folded
                || folded.starts_with(&format!("{old}/"))
                || old.starts_with(&format!("{folded}/"))
        }) {
            return Err("required_submodules contains duplicate or overlapping paths".into());
        }
        seen.insert(folded);
        paths.push(path.to_owned());
    }
    Ok(paths)
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path.split('/').count() <= 16
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.eq_ignore_ascii_case(".git")
                && !part.ends_with('.')
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

#[derive(Clone, PartialEq, Eq)]
struct Dependency {
    path: String,
    pin: String,
    owner: String,
    name: String,
    declared_url: String,
}

impl Dependency {
    fn repository(&self) -> Repo {
        Repo {
            owner: self.owner.clone(),
            name: self.name.clone(),
            default_branch: None,
            size_kib: 0,
            fork: false,
            archived: false,
            required_submodules: Vec::new(),
        }
    }

    fn url(&self) -> String {
        format!("https://github.com/{}/{}.git", self.owner, self.name)
    }
}

fn read(sys: &dyn System, command: Cmd, why: &str) -> Result<Vec<u8>, String> {
    let ran = sys
        .run_complete_stdout(&command)
        .map_err(|_| why.to_owned())?;
    if !ran.success() {
        return Err(why.into());
    }
    Ok(ran.stdout)
}

fn text(sys: &dyn System, command: Cmd, why: &str) -> Result<String, String> {
    String::from_utf8(read(sys, command, why)?).map_err(|_| why.into())
}

fn tree_entry(bytes: &[u8], path: &str, mode: &str) -> Result<String, String> {
    let entry = std::str::from_utf8(bytes).map_err(|_| "cannot read a required gitlink")?;
    let (header, listed) = entry
        .strip_suffix('\0')
        .and_then(|entry| entry.split_once('\t'))
        .ok_or("required dependency is not one tracked tree entry")?;
    let fields = header.split_whitespace().collect::<Vec<_>>();
    if listed != path
        || fields.len() != 3
        || fields[0] != mode
        || !repos::oid(fields[2])
        || fields[1] != if mode == "160000" { "commit" } else { "blob" }
    {
        return Err("required dependency is not a canonical tracked gitlink".into());
    }
    Ok(fields[2].to_owned())
}

fn dependency_identity(url: &str, parent: &Repo) -> Result<(String, String), String> {
    let remote = if url.starts_with("./") || url.starts_with("../") {
        if url.len() > 512
            || url.split('/').count() > 16
            || !url.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.')
            })
        {
            return Err("required dependency has an invalid relative GitHub URL".into());
        }
        // Git resolves submodule relatives with the parent repository as a directory:
        // ../sibling.git retains its owner; ../../owner/repo.git changes it.
        let mut pieces = vec![parent.owner.as_str(), parent.name.as_str()];
        for piece in url.split('/') {
            match piece {
                "" => return Err("required dependency URL has an empty path component".into()),
                "." => {}
                ".." => {
                    pieces
                        .pop()
                        .ok_or("required dependency URL escapes its GitHub host")?;
                }
                piece => pieces.push(piece),
            }
        }
        let [owner, name] = pieces.as_slice() else {
            return Err(
                "required dependency URL must resolve to one GitHub owner and repository".into(),
            );
        };
        Remote::of(&format!("https://github.com/{owner}/{name}"))
    } else {
        Remote::of(url)
    };
    let Remote::Github { owner, name } = remote else {
        return Err(
            "required dependency must name a credential-free GitHub HTTPS or SSH URL".into(),
        );
    };
    if !super::login(&owner)
        || name.is_empty()
        || name.len() > 100
        || name == "."
        || name == ".."
        || name.ends_with('.')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("required dependency has an invalid GitHub identity".into());
    }
    Ok((owner, name))
}

fn resolve(
    sys: &dyn System,
    user: &User,
    parent: &Path,
    repo: &Repo,
    path: &str,
) -> Result<Dependency, String> {
    directories(sys, user, parent, path)?;
    repos::verify_existing(sys, user, &repo.owner, repo, parent)?;
    let git = || local(sys, user, Some(parent));
    let revision = text(
        sys,
        git().args(["rev-parse", "--verify", "HEAD^{commit}"]),
        "cannot read the parent commit for required dependencies",
    )?;
    let revision = revision.trim();
    if !repos::oid(revision) {
        return Err("cannot read the parent commit for required dependencies".into());
    }
    // All tracked reads belong to one commit. The admitted identity compares selected
    // pins/URLs, so an unrelated parent commit does not discard a dependency download.
    let modules = format!("{revision}:.gitmodules");
    let pin = tree_entry(
        &read(
            sys,
            git().args(["ls-tree", "-z", revision, "--", path]),
            "cannot read the required dependency's tracked commit",
        )?,
        path,
        "160000",
    )?;
    tree_entry(
        &read(
            sys,
            git().args(["ls-tree", "-z", revision, "--", ".gitmodules"]),
            "cannot read the parent's tracked .gitmodules",
        )?,
        ".gitmodules",
        "100644",
    )?;
    // An edited declaration or staged gitlink is the person's intent, not permission to
    // overwrite it using the tree the automatic pass initially saw. The worktree's copy is
    // compared as it is on disk: no filter driver the parent names runs on it.
    let programs = checkout::Programs::named(sys, git())
        .map_err(|why| format!("the parent: {why}; required dependencies were left untouched"))?;
    read(
        sys,
        programs.off(git()).args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--quiet",
            revision,
            "--",
            ".gitmodules",
        ]),
        "the parent's .gitmodules has changes; required dependencies were left untouched",
    )?;
    let index = text(
        sys,
        git().args(["ls-files", "--stage", "-z", "--", path]),
        "cannot read the required dependency's index entry",
    )?;
    if index != format!("160000 {pin} 0\t{path}\0") {
        return Err("the required gitlink has staged changes; left untouched".into());
    }
    let declarations = text(
        sys,
        git().args([
            "config",
            "--blob",
            &modules,
            "--no-includes",
            "--null",
            "--get-regexp",
            "^submodule\\..*\\.path$",
        ]),
        "cannot read tracked submodule paths",
    )?;
    let mut matching = Vec::new();
    for entry in declarations.split_terminator('\0') {
        let (key, value) = entry
            .split_once('\n')
            .ok_or("malformed tracked submodule path")?;
        if value == path {
            let name = key
                .strip_prefix("submodule.")
                .and_then(|name| name.strip_suffix(".path"))
                .filter(|name| valid_path(name))
                .ok_or("invalid tracked submodule name")?;
            matching.push(name.to_owned());
        }
    }
    let [name] = matching.as_slice() else {
        return Err("required gitlink must have exactly one tracked submodule declaration".into());
    };
    let url = text(
        sys,
        git().args([
            "config",
            "--blob",
            &modules,
            "--no-includes",
            "--null",
            "--get-all",
            &format!("submodule.{name}.url"),
        ]),
        "cannot read the tracked submodule URL",
    )?;
    let url = url
        .strip_suffix('\0')
        .filter(|url| !url.contains('\0') && !url.contains('\n'))
        .ok_or("required dependency must have exactly one tracked URL")?;
    let (owner, name) = dependency_identity(url, repo)?;
    Ok(Dependency {
        path: path.into(),
        pin,
        owner,
        name,
        declared_url: url.into(),
    })
}

/// Work at its recorded pin is never checked out again. Missing checkout objects can be
/// fetched without moving it; a different commit moves only from a clean recorded pin.
#[derive(PartialEq, Eq)]
enum State {
    Ready,
    Missing,
    Lagging,
    ObjectsMissing,
}

fn directories(
    sys: &dyn System,
    user: &User,
    parent: &Path,
    relative: &str,
) -> Result<PathBuf, String> {
    let mut at = parent.to_owned();
    for part in std::iter::once(None).chain(relative.split('/').map(Some)) {
        if let Some(part) = part {
            at.push(part);
        }
        match sys.stat(&at) {
            Ok(stat) if stat.dir && !stat.symlink && stat.uid == user.uid => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(at),
            _ => {
                return Err(
                    "required dependency path is linked, foreign or occupied; left untouched"
                        .into(),
                );
            }
        }
    }
    Ok(at)
}

fn state(
    sys: &dyn System,
    user: &User,
    parent: &Path,
    dependency: &Dependency,
) -> Result<State, String> {
    directories(sys, user, parent, &dependency.path)?;
    let path = parent.join(&dependency.path);
    match sys.stat(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(State::Missing),
        Ok(_) if sys.list_dir(&path).is_ok_and(|names| names.is_empty()) => {
            return Ok(State::Missing);
        }
        _ => {}
    }
    let git = || local(sys, user, Some(&path));
    let metadata = sys
        .stat(&path.join(".git"))
        .map_err(|_| "required dependency has no owned Git metadata; left untouched")?;
    if metadata.symlink || metadata.uid != user.uid {
        return Err("required dependency Git metadata is linked or foreign; left untouched".into());
    }
    let top = text(
        sys,
        git().args(["rev-parse", "--show-toplevel"]),
        "required dependency is occupied by an unreadable checkout; left untouched",
    )?;
    if Path::new(top.trim())
        != sys
            .canonicalize(&path)
            .map_err(|_| "cannot resolve required dependency checkout; left untouched")?
    {
        return Err("required dependency is not its own top-level checkout; left untouched".into());
    }
    repos::verify_existing(
        sys,
        user,
        &dependency.owner,
        &dependency.repository(),
        &path,
    )?;
    let head = text(
        sys,
        git().args(["rev-parse", "--verify", "HEAD^{commit}"]),
        "cannot read required dependency HEAD",
    )?;
    if head.trim() == dependency.pin {
        return Ok(if checkout_objects(sys, user, &path, &dependency.pin)? {
            State::Ready
        } else {
            State::ObjectsMissing
        });
    }
    if !repos::oid(head.trim()) {
        return Err("cannot read required dependency commit".into());
    }
    // A checkout may lazily fetch missing blobs in a partial clone. Moving only a
    // complete, self-contained dependency keeps network work in the admitted fetch below.
    complete_storage(sys, user, &path)?;
    // Its files are compared as they are on disk: no filter driver it names (git-lfs, say)
    // runs on one, so a stat-dirty file a driver would clean reads as changed, and the
    // dependency is left as it is.
    let programs = checkout::Programs::named(sys, git())
        .map_err(|why| format!("required dependency: {why}; left untouched"))?;
    let status = read(
        sys,
        programs.off(git()).args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=all",
        ]),
        "cannot read required dependency changes",
    )?;
    if !status.is_empty() {
        return Err("required dependency has uncommitted or untracked work; left untouched".into());
    }
    let branch = sys
        .run_complete_stdout(&git().args(["symbolic-ref", "--quiet", "HEAD"]))
        .map_err(|_| "cannot inspect required dependency branch")?;
    if branch.status != Some(1) {
        return Err(
            "required dependency is on a branch or its branch is unreadable; left untouched".into(),
        );
    }
    let recorded = text(
        sys,
        local(sys, user, Some(parent)).args([
            "log",
            "-1",
            "--format=%H",
            &format!("--find-object={}", head.trim()),
            "HEAD",
            "--",
            &dependency.path,
        ]),
        "cannot inspect previously recorded dependency pins",
    )?;
    if recorded.trim().is_empty() {
        return Err("required dependency is at an unrecorded commit; left untouched".into());
    }
    Ok(State::Lagging)
}

/// Why a required dependency cannot yet be measured as ready.
#[derive(Debug)]
pub(super) enum Unready {
    Git(String),
    Preserved(String),
}

/// Whether every explicitly required gitlink is already populated at its recorded commit.
/// A tool wait is distinct from preserved work or invalid metadata; neither is an exemption.
pub(super) fn ready(
    sys: &dyn System,
    user: &User,
    parent: &Path,
    repo: &Repo,
) -> Result<bool, Unready> {
    require_local_reads(sys, user, repo).map_err(Unready::Git)?;
    let mut ready = true;
    for path in &repo.required_submodules {
        let named = |why: String| Unready::Preserved(format!("{path}: {why}"));
        let dependency = resolve(sys, user, parent, repo, path).map_err(named)?;
        ready &= state(sys, user, parent, &dependency).map_err(named)? == State::Ready;
    }
    Ok(ready)
}

fn network(
    sys: &dyn System,
    user: &User,
    path: Option<&Path>,
    dependency: &Dependency,
) -> Result<Cmd, String> {
    let ordinary = local(sys, user, path).args(checkout::NO_SIDE_RUNS).args([
        "-c",
        "submodule.recurse=false",
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.https.allow=always",
        "-c",
        "protocol.ssh.allow=always",
        "-c",
        "http.followRedirects=false",
    ]);
    repos::clone_command(
        sys,
        user,
        ordinary,
        &dependency.owner,
        &dependency.repository(),
        &dependency.url(),
    )
}

pub(super) fn complete_storage(sys: &dyn System, user: &User, path: &Path) -> Result<(), String> {
    repos::verify_integrity(sys, user, path)
        .map_err(|_| "required dependency has shallow or partial history; left untouched")?;
    let alternates = text(
        sys,
        local(sys, user, Some(path)).args(["rev-parse", "--git-path", "objects/info/alternates"]),
        "cannot inspect required dependency object storage",
    )?;
    let alternates = Path::new(alternates.trim());
    let alternates = if alternates.is_absolute() {
        alternates.to_owned()
    } else {
        path.join(alternates)
    };
    if !matches!(sys.stat(&alternates), Err(error) if error.kind() == io::ErrorKind::NotFound) {
        return Err("required dependency uses an alternate object store; left untouched".into());
    }
    Ok(())
}

fn checkout_objects(sys: &dyn System, user: &User, path: &Path, pin: &str) -> Result<bool, String> {
    // A commit can exist without its tree or blobs. Walk only this snapshot, with no
    // ancestry or optional gitlinks, and keep object reads unable to start a lazy fetch.
    sys.run_complete_stdout(&local(sys, user, Some(path)).args([
        "rev-list",
        "--objects",
        "--no-walk",
        "--quiet",
        "--missing=error",
        &format!("{pin}^{{commit}}"),
        "--",
    ]))
    .map(|ran| ran.success())
    .map_err(|_| "cannot inspect the required dependency's checkout objects".into())
}

fn fetch_objects(
    sys: &dyn System,
    user: &User,
    path: &Path,
    dependency: &Dependency,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<(), crate::checkout::AcquisitionFailure> {
    let command = network(sys, user, Some(path), dependency)?
        .budget(BUDGET)
        .args([
            "fetch",
            // Negotiating an already-present commit can leave missing associated objects
            // missing. A refetch obtains the closure as a fresh download would.
            "--refetch",
            "--no-recurse-submodules",
            "--no-tags",
            "--",
            &dependency.url(),
            &dependency.pin,
        ]);
    current()?;
    crate::checkout::acquisition_read(
        sys,
        &command,
        "cannot fetch the required dependency's recorded commit",
    )?;
    current()?;
    if !checkout_objects(sys, user, path, &dependency.pin)? {
        return Err("required dependency download lacks its checkout objects".into());
    }
    Ok(())
}

fn checkout_pin(
    sys: &dyn System,
    user: &User,
    path: &Path,
    dependency: &Dependency,
) -> Result<(), String> {
    if !checkout_objects(sys, user, path, &dependency.pin)? {
        return Err("required dependency lacks its checkout objects; left untouched".into());
    }
    read(
        sys,
        local(sys, user, Some(path))
            .args(checkout::NO_SIDE_RUNS)
            .args([
                "-c",
                "submodule.recurse=false",
                "checkout",
                "--detach",
                "--no-recurse-submodules",
                &dependency.pin,
            ]),
        "cannot check out the required dependency's exact gitlink commit",
    )?;
    let head = text(
        sys,
        local(sys, user, Some(path)).args(["rev-parse", "--verify", "HEAD^{commit}"]),
        "cannot verify required dependency commit",
    )?;
    if head.trim() != dependency.pin {
        return Err("required dependency did not reach its exact gitlink commit".into());
    }
    Ok(())
}

/// Acquire only selected dependencies. Missing ones publish from private staging; an
/// interrupted download therefore exposes no half checkout at the required path.
pub(super) fn repair(
    sys: &dyn System,
    user: &User,
    parent: &Path,
    repo: &Repo,
    authorized: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<(), crate::checkout::AcquisitionFailure> {
    if repo.required_submodules.is_empty() {
        return Ok(());
    }
    authorized()?;
    require_local_reads(sys, user, repo)?;
    for path in &repo.required_submodules {
        authorized()?;
        let dependency = resolve(sys, user, parent, repo, path)?;
        let current = || -> Result<(), String> {
            authorized()?;
            if resolve(sys, user, parent, repo, path)? != dependency {
                return Err(
                    "required dependency's gitlink or tracked URL changed during acquisition"
                        .into(),
                );
            }
            directories(sys, user, parent, path)?;
            Ok(())
        };
        current()?;
        // init registers this selected gitlink without downloading anything or replacing
        // an existing local URL. Acquisition below uses its separately admitted route.
        read(
            sys,
            local(sys, user, Some(parent))
                .args(checkout::NO_SIDE_RUNS)
                .args([
                    "-c",
                    "submodule.recurse=false",
                    "submodule",
                    "init",
                    "--",
                    path,
                ]),
            "cannot initialize the selected required submodule",
        )?;
        current()?;
        match state(sys, user, parent, &dependency)? {
            State::Ready => continue,
            State::ObjectsMissing => {
                current()?;
                let inner = parent.join(path);
                complete_storage(sys, user, &inner)?;
                fetch_objects(sys, user, &inner, &dependency, &current)?;
            }
            State::Lagging => {
                current()?;
                let inner = parent.join(path);
                // Inspect availability only in a complete object store: a partial clone's
                // read could otherwise fetch implicitly, outside the admitted route.
                complete_storage(sys, user, &inner)?;
                let available = checkout_objects(sys, user, &inner, &dependency.pin)?;
                current()?;
                if !available {
                    fetch_objects(sys, user, &inner, &dependency, &current)?;
                }
                current()?;
                match state(sys, user, parent, &dependency)? {
                    State::Ready => continue,
                    State::Lagging => {}
                    State::Missing | State::ObjectsMissing => {
                        return Err(
                            "required dependency disappeared before alignment; left untouched"
                                .into(),
                        );
                    }
                }
                let moving = || -> Result<(), String> {
                    current()?;
                    if state(sys, user, parent, &dependency)? != State::Lagging {
                        return Err(
                            "required dependency changed before checkout; left untouched".into(),
                        );
                    }
                    Ok(())
                };
                let _boundary = sys.update_boundary().hold(
                    aterm_types::update_boundary::GuardedItem::ClonePublishRename,
                    &moving,
                )?;
                checkout_pin(sys, user, &parent.join(path), &dependency)?;
            }
            State::Missing => {
                current()?;
                let mut staging = Scratch::new_in(sys, Path::new(&user.home), "github-clone")
                    .map_err(|_| "cannot allocate private required-dependency staging")?;
                let marking = staging.path().join(".owner.new");
                let _owner = sys
                    .exclusive(&marking, Duration::ZERO)
                    .map_err(|_| "cannot mark private required-dependency staging")?;
                sys.rename(&marking, &staging.path().join(".owner"))
                    .map_err(|_| "cannot publish private required-dependency staging ownership")?;
                let clone = staging.path().join("repo");
                current()?;
                crate::checkout::acquisition_read(
                    sys,
                    &network(sys, user, None, &dependency)?
                        .budget(BUDGET)
                        .args([
                            "clone",
                            "--no-checkout",
                            "--no-recurse-submodules",
                            "--no-single-branch",
                            "--tags",
                            "--origin",
                            "origin",
                            "--template=",
                            "--",
                            &dependency.url(),
                        ])
                        .arg(&clone),
                    "required dependency download failed; existing worktrees left untouched",
                )?;
                current()?;
                repos::verify_existing(
                    sys,
                    user,
                    &dependency.owner,
                    &dependency.repository(),
                    &clone,
                )?;
                complete_storage(sys, user, &clone)?;
                if !checkout_objects(sys, user, &clone, &dependency.pin)? {
                    fetch_objects(sys, user, &clone, &dependency, &current)?;
                }
                current()?;
                checkout_pin(sys, user, &clone, &dependency)?;
                current()?;
                let _boundary = sys.update_boundary().hold(
                    aterm_types::update_boundary::GuardedItem::ClonePublishRename,
                    &current,
                )?;
                if state(sys, user, parent, &dependency)? != State::Missing {
                    return Err(
                        "required dependency destination appeared during download; left untouched"
                            .into(),
                    );
                }
                // Only create admitted absent ancestors; never traverse a link or remove a
                // nonempty destination. No-replace publication wins no race against user work.
                let destination = parent.join(path);
                let mut at = parent.to_owned();
                let parts = Path::new(path).components().collect::<Vec<_>>();
                for part in &parts[..parts.len() - 1] {
                    at.push(part);
                    current()?;
                    if matches!(sys.stat(&at), Err(error) if error.kind() == io::ErrorKind::NotFound)
                    {
                        sys.create_dir_all(&at, 0o700)
                            .map_err(|_| "cannot create required dependency parent")?;
                        sys.chown(&at, user.uid, user.gid)
                            .map_err(|_| "cannot own required dependency parent")?;
                    }
                }
                current()?;
                sys.remove_empty_dir(&destination).map_err(
                    |_| "required dependency destination is no longer empty; left untouched",
                )?;
                sys.rename_noreplace(&clone, &destination).map_err(
                    |_| "cannot publish required dependency without replacing existing work",
                )?;
                staging.close().map_err(
                    |_| "required dependency published but its empty staging could not be removed",
                )?;
            }
        }
        current()?;
        if state(sys, user, parent, &dependency)? != State::Ready {
            return Err("required dependency remains outside its recorded gitlink commit".into());
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests;
