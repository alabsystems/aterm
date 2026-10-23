// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIVE PROBE.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use aterm_types::domain::ENV_PARENT_SESSION_ID;

use super::claude_settings::OwnerAsk;
use super::git_config::{GitView, WorkerEnv};
use super::probe::{LiveProbe, OnDisk, PolicyProbe, WorkerWrites};
use super::rm_breaker::abs_components;
use super::shell_startup::ShellStartup;

impl PolicyProbe for LiveProbe {
    fn hidden_twin_in(
        &self,
        comps: &[String],
        home: Option<&Path>,
    ) -> Result<Option<String>, String> {
        hidden_twin_within(comps, HIDDEN_TWIN_ENTRIES, home, &folder_names)
    }

    fn real_components(&self, comps: &[String]) -> Option<Vec<String>> {
        real_components(comps)
    }

    fn rm_physical(&self, comps: &[String]) -> Result<OnDisk, String> {
        physical(comps)
    }

    /// The process leading `group`, read through the kernel
    /// ([`atpkg::caller_shell::process_args`]) and taken for the worker only
    /// when the `ATERM_PARENT_SESSION_ID` every aterm session hands its shell
    /// names `sid`: a process whose variable names another session, or none,
    /// is not the worker.
    fn worker_proc(&self, group: u32, sid: &str) -> Result<WorkerEnv, String> {
        let args = atpkg::caller_shell::process_args(group).ok_or_else(|| {
            format!("the environment of {sid}'s foreground process {group} could not be read")
        })?;
        bound_to(WorkerEnv::new(args).with_pid(group), group, sid)
    }

    /// [`WorkerEnv::raw_git_view`], the worker's writable places judged with every
    /// path resolved (`canonicalize`), then PROJECTED ([`GitView::project`]):
    /// the policy judges the very view a tape can hold.
    fn git_view(
        &self,
        worker: &WorkerEnv,
        dir: &Path,
        writes: &WorkerWrites<'_>,
    ) -> Result<GitView, String> {
        let resolved = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        worker
            .raw_git_view(dir, &|d| writes.holds(d, &resolved))
            .map(|view| view.project())
    }

    fn settings_env(&self, worker: &WorkerEnv, launch: &Path) -> Result<(), String> {
        worker.git_settings_env(launch)
    }

    fn shell_startup(&self, worker: &WorkerEnv, launch: &Path) -> Result<ShellStartup, String> {
        super::shell_startup::read(worker, launch)
    }

    fn owner_asks(&self, worker: &WorkerEnv, launch: &Path) -> Result<Vec<OwnerAsk>, String> {
        super::claude_settings::owner_asks(worker, launch)
    }

    fn shell_cwds(&self, worker: &WorkerEnv, cwd: &Path) -> Vec<PathBuf> {
        worker
            .claude_dir(cwd)
            .map(|claude| crate::harness::footer::shell_cwds(&claude, cwd))
            .unwrap_or_default()
    }
}

/// `worker`, the process leading `group`, taken for session `sid`'s worker only when its `ATERM_PARENT_SESSION_ID` names `sid`.
pub(crate) fn bound_to(worker: WorkerEnv, group: u32, sid: &str) -> Result<WorkerEnv, String> {
    match worker.var(ENV_PARENT_SESSION_ID) {
        Some(tab) if tab == sid => Ok(worker),
        Some(_) => Err(format!(
            "{sid}'s foreground process {group} belongs to another session (its \
             {ENV_PARENT_SESSION_ID} names another)"
        )),
        None => Err(format!(
            "{sid}'s foreground process {group} names no session \
             ({ENV_PARENT_SESSION_ID} unset, or its environment hidden)"
        )),
    }
}

/// Maximum directory entries [`hidden_twin_within`] lists before deferring to the user.
pub(crate) const HIDDEN_TWIN_ENTRIES: usize = 20_000;

/// One folder's entry names, as [`hidden_twin_within`] reads them.
pub(crate) type FolderNames = dyn Iterator<Item = OsString>;

/// The real [`std::fs::read_dir`] reader for [`hidden_twin_within`].
pub(crate) fn folder_names(dir: &Path) -> std::io::Result<Box<FolderNames>> {
    let entries = std::fs::read_dir(dir)?;
    Ok(Box::new(entries.flatten().map(|e| e.file_name())))
}

/// Whether `dir` lies at or under a root macOS guards with a consent dialog that
/// a held Full Disk Access grant does not cover or whose Allow is forgotten when
/// the responsible process exits (`~/Library/Containers`,
/// `~/Library/Group Containers`, `~/Library/CloudStorage`,
/// `~/Library/Mobile Documents`), as spelled or after resolving symlinks.
fn never_listed(dir: &Path, home: Option<&Path>, real_home: Option<&Path>) -> bool {
    let Some(home) = home else {
        return false;
    };
    let under = |h: &Path, p: &Path| {
        atpkg::protected::under_app_data(h, p) || atpkg::protected::under_file_provider_domain(h, p)
    };
    if under(home, dir) || real_home.is_some_and(|rh| under(rh, dir)) {
        return true;
    }
    let Ok(real_dir) = std::fs::canonicalize(dir) else {
        return false;
    };
    under(home, &real_dir) || real_home.is_some_and(|rh| under(rh, &real_dir))
}

/// An entry of a folder on `comps`' way whose name differs from the one the box draws only by characters the box may not have drawn.
pub(crate) fn hidden_twin_within(
    comps: &[String],
    max_entries: usize,
    home: Option<&Path>,
    names: &dyn Fn(&Path) -> std::io::Result<Box<FolderNames>>,
) -> Result<Option<String>, String> {
    let real_home = home.and_then(|h| std::fs::canonicalize(h).ok());
    let mut dir = PathBuf::from("/");
    for comp in comps {
        if never_listed(&dir, home, real_home.as_deref()) {
            return Err(format!(
                "a Read whose folder is under another app's data or a file-provider domain and is never listed: {dir:?}"
            ));
        }
        match names(&dir) {
            Ok(entries) => {
                for (n, file_name) in entries.enumerate() {
                    if n >= max_entries {
                        return Err(format!(
                            "a Read in a folder too big to prove (over {max_entries} entries): {dir:?}"
                        ));
                    }
                    let name = file_name.to_string_lossy().into_owned();
                    let shown: String = name.chars().filter(|c| (' '..='~').contains(c)).collect();
                    if name != shown && shown.eq_ignore_ascii_case(comp) {
                        return Ok(Some(dir.join(&name).to_string_lossy().into_owned()));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "a Read whose folder cannot be listed ({e}): {dir:?}"
                ));
            }
        }
        dir.push(comp);
        if never_listed(&dir, home, real_home.as_deref()) {
            return Err(format!(
                "a Read whose folder is under another app's data or a file-provider domain and is never listed: {dir:?}"
            ));
        }
        match std::fs::metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// `comps` with every symlink resolved: the deepest existing ancestor
/// canonicalized, plus the not-yet-existing components as written.
fn real_components(comps: &[String]) -> Option<Vec<String>> {
    let mut k = comps.len();
    loop {
        let p = format!("/{}", comps[..k].join("/"));
        match std::fs::canonicalize(&p) {
            Ok(real) => {
                let mut out = abs_components(&real.to_string_lossy())?;
                out.extend(comps[k..].iter().cloned());
                return Some(out);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && k > 0 => {
                if std::fs::symlink_metadata(&p).is_ok() {
                    return None;
                }
                k -= 1;
            }
            Err(_) => return None,
        }
    }
}

/// How many symbolic links [`physical`] follows for one path before it gives
/// up (the kernel's own limit is of this order: `MAXSYMLINKS` is 32 on macOS).
const MAX_LINK_HOPS: usize = 32;

/// Where the absolute path `comps` leads on the disk, walked as the kernel
/// walks it: each component that is a symbolic link replaced by its target
/// (an absolute target from `/`, a relative one from the link's directory, a
/// `..` in it the physical parent) until one does not exist, and from there
/// the rest as written, since nothing below a missing directory can be a
/// link yet.
fn physical(comps: &[String]) -> Result<OnDisk, String> {
    let mut todo: VecDeque<OsString> = comps.iter().map(OsString::from).collect();
    let mut real = PathBuf::from("/");
    let mut hops = 0usize;
    let mut foreign_link = false;
    while let Some(c) = todo.pop_front() {
        if c.is_empty() || c == "." {
            continue;
        }
        if c == ".." {
            real.pop();
            continue;
        }
        let path = real.join(&c);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                hops += 1;
                if hops > MAX_LINK_HOPS {
                    return Err(format!(
                        "{} leads through more than {MAX_LINK_HOPS} symbolic links",
                        path.display()
                    ));
                }
                foreign_link |= !owned_by_root(&m);
                let to = std::fs::read_link(&path).map_err(|e| {
                    format!("the symbolic link {} cannot be read ({e})", path.display())
                })?;
                if to.has_root() {
                    real = PathBuf::from("/");
                }
                for part in to.components().rev() {
                    match part {
                        Component::Normal(n) => todo.push_front(n.to_os_string()),
                        Component::ParentDir => todo.push_front(OsString::from("..")),
                        Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
                    }
                }
            }
            Ok(_) => real.push(&c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                real.push(&c);
                while let Some(rest) = todo.pop_front() {
                    if rest == ".." {
                        return Err(format!(
                            "{} does not exist, and a symbolic link's `..` climbs out of it",
                            path.display()
                        ));
                    }
                    if !rest.is_empty() && rest != "." {
                        real.push(&rest);
                    }
                }
            }
            Err(e) => {
                return Err(format!(
                    "{} cannot be examined on the disk ({e})",
                    path.display()
                ));
            }
        }
    }
    let comps = real
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    Ok(OnDisk {
        comps,
        foreign_link,
    })
}

/// Whether root owns this link: then no one else made it or can re-point it
/// (macOS's `/tmp -> private/tmp`, `/var -> private/var`).
#[cfg(unix)]
fn owned_by_root(m: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    m.uid() == 0
}

#[cfg(not(unix))]
fn owned_by_root(_: &std::fs::Metadata) -> bool {
    false
}
