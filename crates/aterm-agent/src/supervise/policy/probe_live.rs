// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIVE PROBE: the approval policy's looks at this machine, as the policy
//! always made them ([`super::probe`]'s module docs say why they are behind a
//! probe at all).
//!
//! The bodies here are the ones that were inline in the policy — the Read
//! rule's hidden-twin walk and its resolver (`policy/approval.rs`), the rm
//! rule's walk of a path's links (`policy/rm_breaker.rs`), the worker's
//! process read through the kernel and bound to its session
//! (`approval_loop.rs`) — moved here unchanged. The looks whose readers the
//! window's host shares (Claude Code's settings, the shell's startup files,
//! git's configuration as the worker loads it, the Bash tool's directories)
//! are called from here and stay where their other callers find them.
//!
//! This file is where the approval policy touches the filesystem, a process
//! or the kernel: the `supervise-fx` source guard (`aterm-census`, phase 3)
//! names a direct look anywhere else in the supervisor, so a new one goes
//! through a probe — and onto a harness tape — or is a finding. It names, as
//! well, a call from anywhere above the door of a reader this arm calls
//! (`shell_startup::read`, `claude_settings::owner_asks`,
//! `WorkerEnv::raw_git_view`, …: the guard finds them as the `fn`s of their
//! files that reach a look's home) and the [`LiveProbe`] itself.

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
    fn hidden_twin(&self, comps: &[String]) -> Result<Option<String>, String> {
        hidden_twin_within(comps, HIDDEN_TWIN_ENTRIES)
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

/// `worker`, the process leading `group`, taken for session `sid`'s worker
/// only when its `ATERM_PARENT_SESSION_ID` names `sid`.
///
/// A refusal says THAT the variable names another session, never which: the
/// reason is the probe's answer, a harness tape holds it, and what crosses a
/// probe of the worker is its digest and pid ([`super::probe`]), never a
/// variable's value.
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

/// How many entries of one folder [`hidden_twin_within`] lists before it holds the press
/// for a person (a build folder of hundreds of thousands costs a decision a
/// third of a second, and the loop looks again until the box leaves).
const HIDDEN_TWIN_ENTRIES: usize = 20_000;

/// An entry of a folder on `comps`' way whose name differs from the one the box
/// draws only by characters the box may not have drawn. Claude Code's Read
/// opens the REAL name, and this screen draws some characters as nothing at all
/// (a zero-width space or joiner, a soft hyphen, a byte-order mark, a tag
/// character: elided, not substituted, so the plain-ASCII rule cannot see
/// them): `x<ZWSP>y`, a link to a secret, is drawn `xy`, a name that does not
/// exist, or a decoy that does. Such an entry beside the shown name means the
/// shown name is not proven to be the one read: the press waits for a person.
/// Only ASCII reaches here, so what an entry shows is its name less every
/// character outside it, compared as the default volume compares names (APFS
/// ignores case: `Q<ZWSP>R` is what a Read of `qr` opens). The twin's full
/// path (it may be a folder on the way), or `Err` for a folder that cannot be
/// listed, and for one too big to list in a decision
/// ([`HIDDEN_TWIN_ENTRIES`]: about a microsecond an entry, measured). At most
/// `max_entries` of one folder are listed (a test's small bound).
pub(crate) fn hidden_twin_within(
    comps: &[String],
    max_entries: usize,
) -> Result<Option<String>, String> {
    let mut dir = PathBuf::from("/");
    for comp in comps {
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                for (n, entry) in entries.flatten().enumerate() {
                    if n >= max_entries {
                        return Err(format!(
                            "a Read in a folder too big to prove (over {max_entries} entries): {dir:?}"
                        ));
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
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
        // Nothing below a name that is no folder, or is not there.
        match std::fs::metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// `comps` with every symlink resolved: the deepest existing ancestor
/// canonicalized, plus the not-yet-existing components as written. `None` for
/// a dangling link or a refused look.
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
/// link yet. `Err` when a link's target climbs out of a missing directory
/// with `..`, when the links do not end ([`MAX_LINK_HOPS`]), or when the
/// disk refuses the look (a permission, a file where a directory should be).
///
/// Not [`real_components`] (the Read rule's resolver):
/// that one asks `canonicalize`, which does not say whose links led where,
/// and a root reached through a link the worker could re-point is no root
/// here ([`OnDisk::foreign_link`], `ScratchRoot::on_disk`); it follows the
/// last component too, where rm removes a link itself and follows only the
/// directories above it; and it gives up on a dangling link, where this walks
/// on as written to where a directory made there later would put the rm.
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
