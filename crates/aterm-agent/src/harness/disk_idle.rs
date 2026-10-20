// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER 3: WHOLE IDLE BUILD DIRECTORIES OF LIVE CHECKOUTS (design
//! `docs/DESIGN-disk-awareness-2026-10-17.md` §4.4) — the one grant over a
//! live checkout's files. Every clause fails closed: a clause that cannot be
//! read keeps the directory. The clauses are the 2026-10-17 hand run's, which
//! freed 864.7 GiB from 104 whole directories, made fd-relative and
//! lock-held:
//!
//! 1. **What it is.** A build directory discovery found on the measured
//!    volume under a git checkout — the worktree root above the build
//!    directory itself, at or under the root discovery found it from (a
//!    `target` link into ANOTHER checkout is never judged by the quiet one it
//!    was reached from) — never an agent's `CARGO_TARGET_DIR` (its agent
//!    runs, and other checkouts may share it), never x.py's bootstrap trees
//!    (hours to rebuild), never one inside another found build directory or
//!    holding one, never one that is its own checkout.
//! 2. **Its marks.** A signed `CACHEDIR.TAG`, no source at its root, every
//!    profile known, and the whole-removal fences (no mount point inside it,
//!    nothing of another user's: `super::whole_fences`).
//! 3. **Idle.** Every profile unwritten for `disk.idle_build_days` (default
//!    seven), on what a compile writes.
//! 4. **The checkout is not being worked in**: (a) no session of this
//!    instance works inside it; (b) no running process of this uid has its
//!    working directory, executable or an open file there
//!    (`super::discover::unused_under`, asked again before the removal and
//!    between profiles); (c) its git `index` and `logs/HEAD` are unwritten for
//!    the window, read, no `git` run; (d) no working-tree file outside build
//!    directories and `.git/` was written within the window, by one bounded
//!    walk ([`tree_quiet`]) — an editor that saves in place moves no git file
//!    and holds no descriptor, but it moves an mtime. A walk cut short keeps
//!    it (in the hand run this clause refused 59 of 104).
//! 5. **Nothing installed points into it** ([`inbound_links`]): no link in
//!    `~/.local/bin`, `~/.cargo/bin`, `$ATPKG_BIN` or rustup's linked
//!    toolchains resolves inside it. Links are resolved one component at a
//!    time, each step only where the mount table places it on a local,
//!    non-automounted volume ([`resolve_local`]): a link into a stalled
//!    network share is never followed, so it cannot hang the watch.
//!
//! The removal is `super::target::reclaim_profiles`: each profile EMPTIED of
//! cargo's own outputs under all of its locks, its directory and lock files
//! kept, nothing at the build directory's root that is not a profile touched;
//! clause 4 is read again at the removal, and 4b and 4c once more under each
//! profile's locks, right before its delete.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_sysprobe::volumes::Table;

use super::discover::Origin;
use super::target::{CARGO_LOCKS, TargetDir, human_age};
use super::{MAX_WALK_ENTRIES, RUSTC_INFO, cachedir_tag_signed, mtime_of};

/// `disk.idle_build_days`: a build directory every profile of which is
/// unwritten this many days, its checkout quiet as long, is tier 3's. The
/// hand run used fourteen; atpkg's own cache takes any build directory at
/// thirty. Below one turns tier 3 off.
pub const DEFAULT_IDLE_BUILD_DAYS: i64 = 7;

/// The working-tree walk's wall-clock bound ([`tree_quiet`]).
pub const TREE_WALL: Duration = Duration::from_secs(10);

/// The most entries one listing of a link folder reads ([`inbound_links`]).
pub const MAX_LINKS: usize = 4096;

/// What makes one build directory a tier-3 candidate, as the survey read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleFacts {
    /// The checkout it serves (for a worktree, the worktree's root): its
    /// working tree, its git files and its processes are what was judged.
    pub checkout: PathBuf,
    /// The newest last use of its profiles (unix seconds).
    pub last_used: i64,
    /// How many profiles it holds.
    pub profiles: usize,
    /// The bytes its profiles hold (what emptying them releases, as far as the
    /// walk got) and whether that is a floor.
    pub bytes: (u64, bool),
}

/// What the assessment reads beyond one build directory ([`assess`]).
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// Unix seconds.
    pub now: i64,
    /// The idle window, in seconds (`disk.idle_build_days`).
    pub window_s: i64,
    /// The measured volume's device: a build directory on another is not one.
    pub device: Option<u64>,
    /// The working directory of every session of this instance (clause 4a).
    pub session_cwds: &'a [PathBuf],
    /// What the installed links resolve to ([`inbound_links`]), or why they
    /// could not be read (then no build directory is a candidate).
    pub links: &'a Result<Vec<PathBuf>, String>,
    /// Every build directory the survey holds: a candidate is inside none and
    /// holds none, and the working-tree walk passes over them.
    pub found: &'a [PathBuf],
    /// `$HOME`: never a checkout (a search from it, or a dotfiles repository
    /// there, would make every folder of it one working tree).
    pub home: Option<&'a Path>,
    /// The mount table: a git directory is read only where it places it on a
    /// local, non-automounted volume ([`git_quiet`]).
    pub mounts: Option<&'a Table>,
}

/// The checkout a build directory found as `origin` serves: the nearest git
/// worktree root above the (canonical) build directory itself (a search from
/// `$HOME` finds `~/aterm/target-x` under `~`, and its checkout is
/// `~/aterm`), else the root discovery found it under — which [`assess`]
/// then refuses unless it is that very worktree ([`fits_root`]). `None`: an
/// origin tier 3 never takes from — an agent's `CARGO_TARGET_DIR` (only a
/// running agent names one, and other checkouts may build there), x.py's
/// trees, a link's, a session's scratch build, a named one, a build cache
/// mirror (those are tier 1's when their owner is gone).
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

/// Clause 1's check of the checkout tier 3 judges: a git worktree root
/// (clause 4c has git files to read), not the build directory itself, and
/// the one discovery reached the build directory from — at or under its
/// root, or that root's own worktree. A `~/a-wt/target` linking into
/// `~/z-main/target.noindex` is canonical under `~/z-main`, which `~/a-wt`'s
/// quiet says nothing of.
///
/// # Errors
///
/// Why the checkout is not one tier 3 can judge.
pub fn fits_root(origin: &Origin, build_dir: &Path, checkout: &Path) -> Result<(), String> {
    if checkout == build_dir || !build_dir.starts_with(checkout) {
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
        if !checkout.starts_with(&root) && at.as_deref() != Some(checkout) {
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

/// Clauses 1-3, 4a, 4c, 4d and 5 for the build directory `t` (clause 4b, the
/// running processes, is one scan for every candidate: [`processes_clear`]),
/// with the whole-removal fences `fences`.
///
/// # Errors
///
/// The first clause it fails, as why tier 3 keeps it.
pub fn assess(
    t: &TargetDir,
    cx: &Context<'_>,
    fences: &dyn Fn(&TargetDir) -> Result<(), String>,
) -> Result<IdleFacts, String> {
    let checkout = checkout_of(&t.origin, &t.path)
        .ok_or("tier 3 takes only a git checkout's build directory")?;
    if cx
        .home
        .is_some_and(|h| std::fs::canonicalize(h).map_or_else(|_| h == checkout, |h| h == checkout))
        || checkout.parent().is_none()
    {
        return Err(format!(
            "its checkout would be {}, which is no checkout",
            checkout.display()
        ));
    }
    fits_root(&t.origin, &t.path, &checkout)?;
    if cx.device.is_none() || t.device != cx.device {
        return Err("it is not on the measured volume".to_owned());
    }
    if cx
        .found
        .iter()
        .any(|o| o != &t.path && (t.path.starts_with(o) || o.starts_with(&t.path)))
    {
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
    if let Some(cwd) = cx
        .session_cwds
        .iter()
        .find(|c| c.starts_with(&checkout) || c.starts_with(&t.path))
    {
        return Err(format!("a session works in {}", cwd.display()));
    }
    match cx.links {
        Err(why) => return Err(why.clone()),
        Ok(targets) => {
            if let Some(to) = targets.iter().find(|to| to.starts_with(&t.path)) {
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

/// Clause 4b for every candidate at once: one scan of the running processes
/// (`processes`) over each candidate's checkout and build directory, each
/// candidate answered with who uses it — or, for every candidate, why the
/// scan could not rule anyone out.
#[must_use]
pub fn processes_clear(
    candidates: &[(PathBuf, PathBuf)],
    processes: super::discover::Processes<'_>,
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
            if let Some(u) = scan
                .users
                .iter()
                .find(|u| u.path.starts_with(checkout) || u.path.starts_with(dir))
            {
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

/// Clause 4c: the checkout's git `index` and `logs/HEAD` (a worktree's in
/// the git directory its `.git` file names) unwritten for `window_s` at
/// `now`. Read, no `git` run. A checkout with no `.git` has nothing here to
/// say (clause 4d still walks it). A git directory a `.git` file names is
/// read only once [`resolve_local`] places it, every step, on a local,
/// non-automounted volume of `mounts` (a stalled share must not hang the
/// watch); one it cannot place keeps the directory.
///
/// # Errors
///
/// One was written within the window, or the `.git` could not be read, or
/// the git directory it names could not be placed on a local volume.
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

/// Clause 4d: nothing of `checkout`'s working tree outside build directories
/// and `.git` — no file, no folder — written within `window_s` at `now`. One
/// bounded walk: at most [`MAX_WALK_ENTRIES`] entries and `wall` of clock,
/// never through a link, never onto another device, never into `skip` (the
/// survey's build directories) or a folder carrying a build tool's marker.
///
/// # Errors
///
/// What was written within the window, or why the walk could not finish: a
/// walk cut short keeps the directory.
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
    let dev = device(&root);
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
                if name == ".git"
                    || device(&meta) != dev
                    || skip.iter().any(|s| s == &path)
                    || build_marked(&path)
                {
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

/// The device `meta` is on (`st_dev`); one value everywhere else, where the
/// walk then never leaves a device it cannot tell apart.
fn device(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        meta.dev()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
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

/// `path` with every link on it resolved, ONE COMPONENT AT A TIME, each
/// component looked at (`lstat`, `readlink`) only once `table` — read whole
/// — places the path so far on a local, non-automounted mount: a stalled
/// network share or an automount trigger is never touched, so this cannot
/// block where `std::fs::canonicalize` would. `None`: a step lands on such a
/// mount, the table was not read whole, a component is missing or could not
/// be read, or more than [`MAX_HOPS`] links.
#[must_use]
pub fn resolve_local(path: &Path, table: &Table) -> Option<PathBuf> {
    resolve_local_reading(path, table).ok().flatten()
}

/// A local resolution, a positively remote/automounted path (`Ok(None)`),
/// or an unknown reading. The inbound-link fence must keep unknown distinct.
fn resolve_local_reading(path: &Path, table: &Table) -> std::io::Result<Option<PathBuf>> {
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
    let mut at = PathBuf::from("/");
    let mut hops = 0usize;
    while let Some(part) = todo.pop_front() {
        if part == ".." {
            at.pop();
            continue;
        }
        let next = at.join(&part);
        let m = aterm_sysprobe::volumes::mount_of(&next, table).ok_or_else(|| {
            std::io::Error::other(format!("{} could not be placed on a mount", next.display()))
        })?;
        if !m.local || m.automounted {
            return Ok(None);
        }
        let meta = std::fs::symlink_metadata(&next).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("{} could not be resolved: {error}", next.display()),
            )
        })?;
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
            at = PathBuf::from("/");
        }
        for c in to.components().rev() {
            match c {
                Component::Normal(n) => todo.push_front(n.to_os_string()),
                Component::ParentDir => todo.push_front("..".into()),
                _ => {}
            }
        }
    }
    Ok(Some(at))
}

/// Clause 5's facts: where every link in `dirs` (`~/.local/bin`,
/// `~/.cargo/bin`, `$ATPKG_BIN`, `~/.rustup/toolchains`) points — as written
/// (a relative one against its folder) and, where [`resolve_local`] resolves
/// it on local volumes of `table`, resolved (a link that leads onto a share
/// or an automount is not followed there: what it reaches is not on the
/// measured volume). A folder that is not there says nothing; each listing
/// reads at most [`MAX_LINKS`] entries.
///
/// # Errors
///
/// A folder that is there could not be listed, held more than [`MAX_LINKS`],
/// an entry or local link could not be read/resolved, or no mount table was
/// read whole: then no build directory is a candidate. Each enumeration folder
/// is resolved before it is listed; a positively missing folder is skipped,
/// while an unreadable, remote or automounted folder keeps every candidate.
/// Relative targets use that resolved physical folder. A remote or automounted
/// component is not followed either: it cannot prove the final destination is
/// elsewhere (later `..` components or a link can lead back to a local path).
pub fn inbound_links(dirs: &[PathBuf], table: Option<&Table>) -> Result<Vec<PathBuf>, String> {
    let Some(table) = table.filter(|t| t.complete) else {
        return Err(
            "the mount table was not read whole, so no installed link is followed".to_owned(),
        );
    };
    let mut out = Vec::new();
    for dir in dirs {
        // Classify every component, including symlink destinations, before
        // listing the folder: read_dir would otherwise follow a remote link.
        let placed = match resolve_local_reading(dir, table) {
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
        // The installed directory may itself point into a build profile;
        // its ordinary files must keep that profile even without any symlink.
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
            match resolve_local_reading(&link, table).map_err(|error| error.to_string())? {
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

/// The folders clause 5 reads for `home` ([`inbound_links`]): `~/.local/bin`,
/// `~/.cargo/bin`, atpkg's `bin/` beside its `store` (`$ATPKG_BIN`) and
/// rustup's toolchains (a linked one is a link there).
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

// Unix-only: its fixtures are symlinks and mode bits.
#[cfg(all(test, unix))]
#[path = "disk_idle_tests.rs"]
mod tests;
