// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 3 idle build-directory reclamation for live checkouts, gated by [`tree_quiet`],
//! [`Context::cwd_unknown`], [`inbound_links`] ([`resolve_local`]), and live [`Sessions`]
//! re-checks.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_sysprobe::volumes::Table;

use super::discover::Origin;
use super::target::{CARGO_LOCKS, TargetDir, human_age};
use super::{MAX_WALK_ENTRIES, RUSTC_INFO, cachedir_tag_signed, mtime_of};

/// `disk.idle_build_days`.
pub const DEFAULT_IDLE_BUILD_DAYS: i64 = 7;

/// The working-tree walk's wall-clock bound ([`tree_quiet`]).
pub const TREE_WALL: Duration = Duration::from_secs(10);

/// The most entries one listing of a link folder reads ([`inbound_links`]).
pub const MAX_LINKS: usize = 4096;

/// What makes one build directory a tier-3 candidate, as the survey read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleFacts {
    /// The checkout it serves (for a worktree, the worktree's root).
    pub checkout: PathBuf,
    /// The newest last use of its profiles (unix seconds).
    pub last_used: i64,
    /// How many profiles it holds.
    pub profiles: usize,
    /// The bytes its profiles hold (what emptying them releases, as far as the walk got).
    pub bytes: (u64, bool),
}

/// What the assessment reads beyond one build directory ([`assess`]).
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// Unix seconds.
    pub now: i64,
    /// The idle window, in seconds (`disk.idle_build_days`).
    pub window_s: i64,
    /// The measured volume's device.
    pub device: Option<u64>,
    /// The working directory of every session of this instance (clause 4a).
    pub session_cwds: &'a [SessionCwd],
    /// Why the working directory of some session of this instance is not known.
    pub cwd_unknown: Option<&'a str>,
    /// What the installed links resolve to ([`inbound_links`]), or why they could not be read.
    pub links: &'a Result<Vec<PathBuf>, String>,
    /// Every build directory the survey holds.
    pub found: &'a [PathBuf],
    /// `$HOME`: never a checkout.
    pub home: Option<&'a Path>,
    /// The mount table: a git directory is read only on a local volume.
    pub mounts: Option<&'a Table>,
}

/// This instance's sessions as clause 4a reads them at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sessions {
    /// Each session's working directory, as it reported it.
    pub cwds: Vec<PathBuf>,
    /// Why the working directory of some session is not known.
    pub unknown: Option<String>,
}

impl Sessions {
    /// Clause 4a for the build directory `dir` of `checkout` over these sessions.
    ///
    /// # Errors
    /// Why a session may work inside `dir` or `checkout`.
    pub fn clear(&self, checkout: &Path, dir: &Path, table: Option<&Table>) -> Result<(), String> {
        if let Some(why) = &self.unknown {
            return Err(why.clone());
        }
        let cwds: Vec<SessionCwd> = self.cwds.iter().map(|c| SessionCwd::on(c, table)).collect();
        sessions_clear(&cwds, None, checkout, dir, table)
    }
}

/// Clause 4a for the build directory `dir` of `checkout`.
///
/// # Errors
/// Why a session may work inside `dir` or `checkout`.
pub fn sessions_clear(
    cwds: &[SessionCwd],
    unknown: Option<&str>,
    checkout: &Path,
    dir: &Path,
    table: Option<&Table>,
) -> Result<(), String> {
    if let Some(why) = unknown {
        return Err(why.to_owned());
    }
    if let Some(cwd) = cwds
        .iter()
        .find(|c| c.may_work_in(checkout, table) || c.may_work_in(dir, table))
    {
        return Err(format!("a session works in {}", cwd.path().display()));
    }
    Ok(())
}

/// One session's working directory as clause 4a reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCwd {
    /// Resolved — by [`SessionCwd::on`] link by link on local volumes.
    Placed(PathBuf),
    /// As written, not resolved.
    Unresolved(PathBuf),
}

impl SessionCwd {
    /// `cwd` resolved where it can be (`std::fs::canonicalize`, an open of the folder).
    #[must_use]
    pub fn of(cwd: &Path) -> SessionCwd {
        let _quiet = aterm_sysprobe::quiet_errors();
        std::fs::canonicalize(cwd).map_or_else(
            |_| SessionCwd::Unresolved(cwd.to_path_buf()),
            SessionCwd::Placed,
        )
    }

    /// `cwd` as clause 4a reads it, in the survey and at the removal alike.
    #[must_use]
    pub fn on(cwd: &Path, table: Option<&Table>) -> SessionCwd {
        let _quiet = aterm_sysprobe::quiet_errors();
        table.and_then(|t| resolve_local(cwd, t)).map_or_else(
            || SessionCwd::Unresolved(cwd.to_path_buf()),
            SessionCwd::Placed,
        )
    }

    /// The path, for a line.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            SessionCwd::Placed(p) | SessionCwd::Unresolved(p) => p,
        }
    }

    /// May a session working here work inside `dir` (clause 4a)? Unknown is yes.
    #[must_use]
    pub fn may_work_in(&self, dir: &Path, table: Option<&Table>) -> bool {
        match self {
            SessionCwd::Placed(cwd) => under(cwd, dir, table) != Some(false),
            SessionCwd::Unresolved(cwd) if cfg!(windows) => {
                if under(cwd, dir, table) == Some(true) {
                    return true;
                }
                let volume = |p: &Path| {
                    table
                        .and_then(|t| aterm_sysprobe::volumes::mount_of(p, t))
                        .and_then(aterm_sysprobe::volumes::Mount::key)
                };
                match (volume(cwd), volume(dir)) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                }
            }
            SessionCwd::Unresolved(cwd) => cwd.starts_with(dir),
        }
    }
}

/// Is `path` at or under `dir`.
#[must_use]
pub fn under(path: &Path, dir: &Path, table: Option<&Table>) -> Option<bool> {
    match table {
        Some(t) if cfg!(windows) => t.is_under(path, dir),
        _ => aterm_sysprobe::paths::is_under(path, dir),
    }
}

/// Are `a` and `b` one path, by [`under`] both ways? `None`: either way
/// cannot be placed.
fn same_path(a: &Path, b: &Path, table: Option<&Table>) -> Option<bool> {
    Some(under(a, b, table)? && under(b, a, table)?)
}

/// The checkout a build directory found as `origin` serves.
#[must_use]
pub fn checkout_of(origin: &Origin, build_dir: &Path) -> Option<PathBuf> {
    match origin {
        Origin::Checkout { root } => Some(
            build_dir
                .parent()
                .and_then(super::discover::worktree_root_of)
                .unwrap_or_else(|| root.clone()),
        ),
        Origin::AgentTargetDir
        | Origin::Named
        | Origin::BuildCache { .. }
        | Origin::Bootstrap { .. }
        | Origin::ClaudeScratch { .. }
        | Origin::Linked { .. } => None,
    }
}

/// Clause 1's check of the checkout tier 3 judges.
///
/// # Errors
/// Why the checkout is not one tier 3 can judge.
pub fn fits_root(origin: &Origin, build_dir: &Path, checkout: &Path) -> Result<(), String> {
    if same_path(checkout, build_dir, None) != Some(false)
        || under(build_dir, checkout, None) != Some(true)
    {
        return Err(format!(
            "its checkout would be {}, which does not hold it as a build directory",
            checkout.display()
        ));
    }
    if super::discover::worktree_root_of(checkout).as_deref() != Some(checkout) {
        return Err(format!(
            "no git checkout holds it ({} is none), so whether it is worked in cannot be read",
            checkout.display()
        ));
    }
    if let Origin::Checkout { root } = origin {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        let at = super::discover::worktree_root_of(&root);
        if under(checkout, &root, None) != Some(true) && at.as_deref() != Some(checkout) {
            return Err(format!(
                "it lies in the checkout {}, not the one it was found from ({}): reached through \
                 a link, it is not judged by that one's quiet",
                checkout.display(),
                root.display()
            ));
        }
    }
    Ok(())
}

/// Clauses 1-3, 4a, 4c, 4d and 5 for the build directory `t`.
///
/// # Errors
/// The first clause it fails, as why tier 3 keeps it.
pub fn assess(
    t: &TargetDir,
    cx: &Context<'_>,
    fences: &dyn Fn(&TargetDir) -> Result<(), String>,
) -> Result<IdleFacts, String> {
    let checkout = checkout_of(&t.origin, &t.path)
        .ok_or("tier 3 takes only a git checkout's build directory")?;
    let is_home = |h: &Path| {
        let h = std::fs::canonicalize(h).unwrap_or_else(|_| h.to_path_buf());
        same_path(&h, &checkout, cx.mounts) != Some(false)
    };
    if cx.home.is_some_and(is_home) || checkout.parent().is_none() {
        return Err(format!(
            "its checkout would be {}, which is no checkout",
            checkout.display()
        ));
    }
    fits_root(&t.origin, &t.path, &checkout)?;
    if cx.device.is_none() || t.device != cx.device {
        return Err("it is not on the measured volume".to_owned());
    }
    if cx.found.iter().any(|o| {
        same_path(o, &t.path, cx.mounts) != Some(true)
            && (under(&t.path, o, cx.mounts) != Some(false)
                || under(o, &t.path, cx.mounts) != Some(false))
    }) {
        return Err("a build directory is inside it, or it is inside one".to_owned());
    }
    let marker = t.fences()?;
    if marker != "CACHEDIR.TAG" {
        return Err(format!(
            "only a signed CACHEDIR.TAG lets a whole build directory go, and it carries {marker}"
        ));
    }
    fences(t)?;
    if t.profiles.is_empty() {
        return Err("it holds no cargo profile".to_owned());
    }
    let mut newest = i64::MIN;
    for p in &t.profiles {
        let Some(m) = p.last_used else {
            return Err(format!(
                "{}: its caches' clock could not be read",
                p.path.display()
            ));
        };
        newest = newest.max(m);
    }
    let age = cx.now.saturating_sub(newest);
    if age < cx.window_s {
        return Err(format!(
            "a profile was written into {} ago, inside the {} window",
            human_age(age),
            human_age(cx.window_s)
        ));
    }
    sessions_clear(
        cx.session_cwds,
        cx.cwd_unknown,
        &checkout,
        &t.path,
        cx.mounts,
    )?;
    match cx.links {
        Err(why) => return Err(why.clone()),
        Ok(targets) => {
            if let Some(to) = targets
                .iter()
                .find(|to| under(to, &t.path, cx.mounts) != Some(false))
            {
                return Err(format!(
                    "an installed link resolves into it ({})",
                    to.display()
                ));
            }
        }
    }
    git_quiet(&checkout, cx.now, cx.window_s, cx.mounts)?;
    tree_quiet(&checkout, cx.found, cx.now, cx.window_s, TREE_WALL)?;
    let mut bytes = (0u64, false);
    for p in &t.profiles {
        let (b, partial) = super::target::total_bytes(&p.path);
        bytes = (bytes.0.saturating_add(b), bytes.1 || partial);
    }
    Ok(IdleFacts {
        checkout,
        last_used: newest,
        profiles: t.profiles.len(),
        bytes,
    })
}

/// Clause 4b for every candidate.
#[must_use]
pub fn processes_clear(
    candidates: &[(PathBuf, PathBuf)],
    processes: super::discover::Processes<'_>,
    table: Option<&Table>,
) -> Vec<Result<(), String>> {
    let dirs: Vec<PathBuf> = candidates
        .iter()
        .flat_map(|(checkout, dir)| [checkout.clone(), dir.clone()])
        .collect();
    if dirs.is_empty() {
        return Vec::new();
    }
    let scan = match processes(&dirs, &[std::process::id()]) {
        Ok(scan) => scan,
        Err(e) => {
            let why = format!("the running processes could not be listed: {e}");
            return candidates.iter().map(|_| Err(why.clone())).collect();
        }
    };
    let unread = scan
        .unreadable
        .iter()
        .find(|(_, exe)| !super::discover::platform_service(exe.as_deref()));
    candidates
        .iter()
        .map(|(checkout, dir)| {
            if let Some(u) = scan.users.iter().find(|u| {
                under(&u.path, checkout, table) != Some(false)
                    || under(&u.path, dir, table) != Some(false)
            }) {
                return Err(format!(
                    "process {} uses {} (its {})",
                    u.pid,
                    u.path.display(),
                    match u.how {
                        "cwd" => "working directory",
                        "exe" => "executable",
                        _ => "open file",
                    }
                ));
            }
            if let Some((pid, exe)) = unread {
                return Err(format!(
                    "process {pid}{} could not be read, so it is not ruled out",
                    exe.as_ref()
                        .map(|e| format!(" ({})", e.display()))
                        .unwrap_or_default()
                ));
            }
            Ok(())
        })
        .collect()
}

/// Clause 4c: the checkout's git `index` and `logs/HEAD`.
///
/// # Errors
/// One was written within the window, or the `.git` could not be read.
pub fn git_quiet(
    checkout: &Path,
    now: i64,
    window_s: i64,
    mounts: Option<&Table>,
) -> Result<(), String> {
    let dot = checkout.join(".git");
    let gitdir = match std::fs::symlink_metadata(&dot) {
        Ok(m) if m.is_dir() => dot,
        Ok(m) if m.is_file() => {
            let text = std::fs::read(&dot)
                .ok()
                .filter(|b| b.len() <= 4096)
                .and_then(|b| String::from_utf8(b).ok())
                .ok_or_else(|| format!("{} could not be read", dot.display()))?;
            let named = text
                .lines()
                .find_map(|l| l.strip_prefix("gitdir:"))
                .map(|p| PathBuf::from(p.trim()))
                .ok_or_else(|| format!("{} names no git directory", dot.display()))?;
            let named = if named.is_absolute() {
                named
            } else {
                checkout.join(named)
            };
            mounts
                .and_then(|t| resolve_local(&named, t))
                .ok_or_else(|| {
                    format!(
                        "its git directory {} could not be placed on a local volume, so it is \
                         not read",
                        named.display()
                    )
                })?
        }
        Ok(_) => {
            return Err(format!(
                "{} is a link, which is not followed",
                dot.display()
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{} could not be read: {e}", dot.display())),
    };
    for f in ["index", "logs/HEAD"] {
        let path = gitdir.join(f);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let Some(m) = mtime_of(&path) else {
                    return Err(format!("{} could not be dated", path.display()));
                };
                let age = now.saturating_sub(m);
                if age < window_s {
                    return Err(format!(
                        "its git {f} was written {} ago, inside the {} window",
                        human_age(age),
                        human_age(window_s)
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
        }
    }
    Ok(())
}

/// Clause 4d: nothing of `checkout`'s working tree outside build directories and `.git`.
///
/// # Errors
/// What was written within the window, or why the walk could not finish.
pub fn tree_quiet(
    checkout: &Path,
    skip: &[PathBuf],
    now: i64,
    window_s: i64,
    wall: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    let root = std::fs::symlink_metadata(checkout)
        .map_err(|e| format!("{} could not be read: {e}", checkout.display()))?;
    let dev = device(checkout, &root)
        .ok_or_else(|| format!("{}'s volume could not be named", checkout.display()))?;
    let mut seen = 0usize;
    let mut stack = vec![checkout.to_path_buf()];
    let fresh = |path: &Path, meta: &std::fs::Metadata| -> Result<(), String> {
        let m = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .ok_or_else(|| format!("{} could not be dated", path.display()))?;
        let age = now.saturating_sub(m);
        if age < window_s {
            return Err(format!(
                "{} in its working tree was written {} ago, inside the {} window",
                path.display(),
                human_age(age),
                human_age(window_s)
            ));
        }
        Ok(())
    };
    while let Some(dir) = stack.pop() {
        if started.elapsed() >= wall {
            return Err("the walk of its working tree ran out of time".to_owned());
        }
        let rd = std::fs::read_dir(&dir)
            .map_err(|e| format!("{} could not be read: {e}", dir.display()))?;
        for entry in rd {
            let entry = entry.map_err(|e| format!("{} could not be read: {e}", dir.display()))?;
            seen += 1;
            super::beat::tick(1);
            if seen > MAX_WALK_ENTRIES {
                return Err(format!(
                    "its working tree holds more than {MAX_WALK_ENTRIES} entries"
                ));
            }
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("{} could not be read: {e}", path.display()))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                let name = entry.file_name();
                if name == ".git" {
                    continue;
                }
                match device(&path, &meta) {
                    Some(d) if d == dev => {}
                    Some(_) => continue,
                    None => {
                        return Err(format!("{}'s volume could not be named", path.display()));
                    }
                }
                if skip.iter().any(|s| s == &path) || build_marked(&path) {
                    continue;
                }
                fresh(&path, &meta)?;
                stack.push(path);
            } else if meta.is_file() {
                if CARGO_LOCKS.iter().any(|l| entry.file_name() == *l) {
                    continue;
                }
                fresh(&path, &meta)?;
            }
        }
    }
    Ok(())
}

/// The device the entry at `path` (read as `meta`, a link not followed) is
/// on: `st_dev`; on Windows the volume serial of the one identity export
/// ([`aterm_sysprobe::volumes::file_id`], read through a handle, the width
/// every device fence compares).
fn device(path: &Path, meta: &std::fs::Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = path;
        Some(meta.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        aterm_sysprobe::volumes::file_id(path, false).map(|id| id.dev)
    }
}

/// Does the folder at `dir` carry a build tool's marker (it is a build
/// directory, whose writes are a build's, not an edit)?
fn build_marked(dir: &Path) -> bool {
    cachedir_tag_signed(&dir.join("CACHEDIR.TAG"))
        || std::fs::symlink_metadata(dir.join(RUSTC_INFO)).is_ok_and(|m| m.is_file())
}

/// The most links [`resolve_local`] follows for one path.
const MAX_HOPS: usize = 40;

/// `path` with every link on it resolved, one component at a time.
#[must_use]
pub fn resolve_local(path: &Path, table: &Table) -> Option<PathBuf> {
    resolve_local_reading(path, table, false).ok().flatten()
}

/// A local resolution, a positively remote/automounted path (`Ok(None)`),
/// or an unknown reading. `dangling`: a component that is not there ends the walk with the
/// place it names, the rest of the path beneath it as written (nothing past it exists to
/// follow); otherwise it is an unknown reading.
fn resolve_local_reading(
    path: &Path,
    table: &Table,
    dangling: bool,
) -> std::io::Result<Option<PathBuf>> {
    use std::collections::VecDeque;
    use std::path::Component;
    if !table.complete || !path.is_absolute() {
        return Err(std::io::Error::other(
            "the path or complete mount table could not be read",
        ));
    }
    let mut todo: VecDeque<std::ffi::OsString> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_os_string()),
            Component::ParentDir => Some("..".into()),
            _ => None,
        })
        .collect();
    let root_of = |p: &Path| -> PathBuf {
        p.components()
            .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
            .collect()
    };
    let mut at = root_of(path);
    let mut hops = 0usize;
    while let Some(part) = todo.pop_front() {
        if part == ".." {
            at.pop();
            continue;
        }
        let next = table.long_spelling(&at.join(&part)).into_owned();
        if !local_at(&next, table)? {
            return Ok(None);
        }
        let meta = match std::fs::symlink_metadata(&next) {
            Ok(meta) => meta,
            Err(error) if dangling && error.kind() == std::io::ErrorKind::NotFound => {
                let mut named = next;
                for part in todo {
                    if part == ".." {
                        named.pop();
                    } else {
                        named.push(part);
                    }
                }
                return Ok(Some(named));
            }
            Err(error) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!("{} could not be resolved: {error}", next.display()),
                ));
            }
        };
        if !meta.file_type().is_symlink() {
            at = next;
            continue;
        }
        hops += 1;
        if hops > MAX_HOPS {
            return Err(std::io::Error::other(format!(
                "{} has too many link hops",
                path.display()
            )));
        }
        let to = std::fs::read_link(&next).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("{} could not be read: {error}", next.display()),
            )
        })?;
        if to.is_absolute() {
            at = root_of(&to);
        } else if to.has_root() {
            // Windows' `\x`: the root of the drive the link is on.
            at = root_of(&at);
        } else if matches!(to.components().next(), Some(Component::Prefix(_))) {
            // Windows' `C:x`: relative to a drive's current directory,
            // which is the reading process's, not the link's.
            return Err(std::io::Error::other(format!(
                "{} names a drive-relative path",
                next.display()
            )));
        }
        for c in to.components().rev() {
            match c {
                Component::Normal(n) => todo.push_front(n.to_os_string()),
                Component::ParentDir => todo.push_front("..".into()),
                _ => {}
            }
        }
    }
    Ok(local_at(&at, table)?.then_some(at))
}

/// Does `table` place `path` on a local, non-automounted mount?
fn local_at(path: &Path, table: &Table) -> std::io::Result<bool> {
    let m = aterm_sysprobe::volumes::mount_of(path, table).ok_or_else(|| {
        std::io::Error::other(format!("{} could not be placed on a mount", path.display()))
    })?;
    Ok(m.local && !m.automounted)
}

/// Clause 5's facts: where every link in `dirs` resolves, and where one that dangles points.
///
/// # Errors
/// A folder could not be listed, exceeded [`MAX_LINKS`], or a link failed to resolve.
pub fn inbound_links(dirs: &[PathBuf], table: Option<&Table>) -> Result<Vec<PathBuf>, String> {
    let Some(table) = table.filter(|t| t.complete) else {
        return Err(
            "the mount table was not read whole, so no installed link is followed".to_owned(),
        );
    };
    let mut out = Vec::new();
    for dir in dirs {
        let placed = match resolve_local_reading(dir, table, false) {
            Ok(Some(placed)) => placed,
            Ok(None) => {
                return Err(format!(
                    "{} could not be resolved without following a remote or automounted directory",
                    dir.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        out.push(placed.clone());
        let rd = match std::fs::read_dir(&placed) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{} could not be listed: {e}", dir.display())),
        };
        for (n, entry) in rd.enumerate() {
            if n >= MAX_LINKS {
                return Err(format!(
                    "{} holds more than {MAX_LINKS} entries, too many to read",
                    dir.display()
                ));
            }
            let entry = entry.map_err(|e| format!("{} could not be listed: {e}", dir.display()))?;
            let kind = entry.file_type().map_err(|error| {
                format!(
                    "{} could not be classified: {error}",
                    entry.path().display()
                )
            })?;
            if !kind.is_symlink() {
                #[cfg(windows)]
                if let Some(to) = cmd_shim_target(&entry.path(), kind)? {
                    let shim = entry.path();
                    if !to.is_absolute() {
                        return Err(format!(
                            "{} runs {}, whose place is wherever it is run from",
                            shim.display(),
                            to.display()
                        ));
                    }
                    if resolve_local_reading(&to, table, false)
                        .map_err(|error| error.to_string())?
                        .is_none()
                    {
                        return Err(format!(
                            "{} could not be resolved without following a remote or \
                             automounted path",
                            shim.display()
                        ));
                    }
                    out.push(shim_link(&shim, &to)?);
                }
                continue;
            }
            let link = entry.path();
            let Ok(to) = std::fs::read_link(&link) else {
                return Err(format!("{} could not be read", link.display()));
            };
            out.push(if to.is_absolute() {
                to
            } else {
                placed.join(to)
            });
            match resolve_local_reading(&link, table, true).map_err(|error| error.to_string())? {
                Some(real) => out.push(real),
                None => {
                    return Err(format!(
                        "{} could not be resolved without following a remote or automounted path",
                        link.display()
                    ));
                }
            }
        }
    }
    Ok(out)
}

/// The program the `.cmd` shim at `path` (listed as `kind`) runs.
///
/// # Errors
/// A `.cmd` that is there but could not be read.
#[cfg(windows)]
fn cmd_shim_target(path: &Path, kind: std::fs::FileType) -> Result<Option<PathBuf>, String> {
    let named_cmd = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd"));
    if !kind.is_file() || !named_cmd {
        return Ok(None);
    }
    atpkg::platform::read_shim_target(path)
        .map_err(|error| format!("{} could not be read: {error}", path.display()))
}

/// The link clause 5 records for the shim at `shim` running the absolute program `to`.
///
/// # Errors
/// The program could not be resolved.
#[cfg(windows)]
fn shim_link(shim: &Path, to: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(to).map_err(|error| {
        format!(
            "{} runs {}, which could not be resolved: {error}",
            shim.display(),
            to.display()
        )
    })
}

/// The folders clause 5 reads for `home` ([`inbound_links`]).
#[must_use]
pub fn link_dirs(home: &Path, store: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        home.join(".local").join("bin"),
        home.join(".cargo").join("bin"),
        home.join(".rustup").join("toolchains"),
    ];
    if let Some(prefix) = store.parent() {
        dirs.push(prefix.join("bin"));
    }
    dirs
}

#[cfg(all(test, unix))]
#[path = "disk_idle_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "disk_idle_windows_tests.rs"]
mod windows_tests;
