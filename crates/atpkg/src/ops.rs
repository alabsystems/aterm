// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local store operations behind `which`, `list`, and `uninstall`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::Layout;
use crate::store::ToolName;

/// List `bin/` — the one door every scan in this module and in [`crate::activate`] goes through.
pub(crate) fn read_bin_dir(layout: &Layout) -> io::Result<std::fs::ReadDir> {
    std::fs::read_dir(layout.bin_dir())
}

/// Resolve `tool` to the store path its `bin/<tool>` shim points at.
#[must_use]
pub fn which(layout: &Layout, tool: &str) -> Option<PathBuf> {
    crate::platform::resolve_shim(&layout.shim(&ToolName::new(tool)?))
}

/// Env for `tool` ([`crate::shim_env::read_sidecar`], [`crate::activate::reassert_shim_env`]).
#[cfg(all(test, unix))]
fn exec_env(layout: &Layout, tool: &str) -> crate::shim_env::ShimEnv {
    let Some(name) = ToolName::new(tool) else {
        return crate::shim_env::ShimEnv::NONE;
    };
    exec_env_from(layout, &crate::platform::read_shim(&layout.shim(&name)))
}

fn exec_env_from(layout: &Layout, read: &crate::platform::ShimRead) -> crate::shim_env::ShimEnv {
    let declared = read
        .target
        .as_deref()
        .and_then(|target| store_build_of(&layout.prefix, target))
        .map(|(program, build)| crate::shim_env::read_sidecar(&layout.build_dir(&program, build)))
        .unwrap_or_default();
    if declared.is_empty() {
        read.env.clone()
    } else {
        declared
    }
}

/// A tool as its shim would run it, for a door that execs PAST the shim.
#[derive(Debug, PartialEq, Eq)]
pub struct Managed {
    /// The file to exec.
    pub target: PathBuf,
    /// What the shim would export before its `exec`.
    pub env: crate::shim_env::ShimEnv,
}

/// [`Managed`] for `tool`, or `None` when no shim of it forwards to a build.
#[must_use]
pub fn managed(layout: &Layout, tool: &str) -> Option<Managed> {
    managed_with_source(layout, tool).map(|(_, managed)| managed)
}

/// Read a shim once, retaining its payload path beside the possibly routed executable.
pub(crate) fn managed_with_source(layout: &Layout, tool: &str) -> Option<(PathBuf, Managed)> {
    let shim = layout.shim(&ToolName::new(tool)?);
    let read = crate::platform::read_shim(&shim);
    let source = read.target.clone()?;
    managed_from_read(layout, &shim, &read).map(|managed| (source, managed))
}

pub(crate) fn managed_from_read(
    layout: &Layout,
    shim: &Path,
    read: &crate::platform::ShimRead,
) -> Option<Managed> {
    let target = read.target.as_ref()?;
    let env = exec_env_from(layout, read);
    Some(Managed {
        target: crate::compat::route_for_shim(shim, target).unwrap_or_else(|| target.clone()),
        env,
    })
}

/// One listing of `bin/`, resolved once ([`ToolName`]).
pub(crate) struct BinScan {
    entries: Vec<BinEntry>,
}

/// One `bin/` entry as [`BinScan`] read it.
struct BinEntry {
    /// The logical name ([`ToolName`], [`crate::store::shim_allowed`]).
    tool: Option<ToolName>,
    /// What the shim forwards to, `None` for a tombstone, a pending stub, or anything unresolvable.
    target: Option<PathBuf>,
}

impl BinScan {
    /// Photograph `bin/` now.
    pub(crate) fn capture(layout: &Layout) -> Self {
        let mut entries = Vec::new();
        let Ok(listing) = read_bin_dir(layout) else {
            return Self { entries };
        };
        for e in listing.flatten() {
            entries.push(BinEntry {
                tool: e
                    .file_name()
                    .to_str()
                    .and_then(crate::store::ToolName::from_shim_file),
                target: crate::platform::resolve_shim(&e.path()),
            });
        }
        Self { entries }
    }

    /// The resolved target of every entry that has one, in listing order.
    fn targets(&self) -> impl Iterator<Item = &Path> {
        self.entries.iter().filter_map(|e| e.target.as_deref())
    }

    /// Every entry that both names a tool and resolves, in listing order.
    fn tools(&self) -> impl Iterator<Item = (&ToolName, &Path)> {
        self.entries
            .iter()
            .filter_map(|e| Some((e.tool.as_ref()?, e.target.as_deref()?)))
    }
}

/// Parse `(program, build)` out of a shim target like `…/store/<program>/<build>/bin/<tool>`.
pub(crate) fn program_build_of_target(target: &Path) -> Option<(String, u64)> {
    let (program, build) = program_build_ref_of_target(target)?;
    Some((program.to_string(), build))
}

fn program_build_ref_of_target(target: &Path) -> Option<(&str, u64)> {
    let mut comps = target.iter();
    let _ = comps.find(|c| *c == "store")?;
    let program = comps.next()?.to_str()?;
    let build = build_in_path(comps.next()?)?;
    Some((program, build))
}

/// Parse `(program, build)` out of a path that must lie inside `prefix`'s store.
/// Dot-parent paths need native resolution: a symlinked directory before `..`
/// can make lexical normalization name a different store. Keep the executable's
/// invocation path unchanged; only this classification uses the resolved path.
#[must_use]
pub fn store_build_of(prefix: &Path, target: &Path) -> Option<(String, u64)> {
    let resolved;
    let (prefix, target) = if [prefix, target].into_iter().any(|path| {
        path.components()
            .any(|part| part == std::path::Component::ParentDir)
    }) {
        resolved = (prefix.canonicalize().ok()?, target.canonicalize().ok()?);
        (resolved.0.as_path(), resolved.1.as_path())
    } else {
        (prefix, target)
    };
    let rel = target.strip_prefix(prefix.join("store")).ok()?;
    let mut comps = rel.components();
    let (Some(std::path::Component::Normal(program)), Some(std::path::Component::Normal(build))) =
        (comps.next(), comps.next())
    else {
        return None;
    };
    if comps.any(|part| !matches!(part, std::path::Component::Normal(_))) {
        return None;
    }
    let program = program.to_str()?.to_string();
    let build = build_in_path(build)?;
    Some((program, build))
}

/// A command in an already selected build: its directories are real and at most 16 final
/// aliases resolve to a regular file inside that build's resolved root. A link's text is
/// normalized and fenced before its target is looked up. This admits Artifact.links without
/// traversing a foreign, escaping or nonregular target or a symlinked target ancestor.
/// Return the logical bin path so argv0 and the build's environment keep their usual spelling.
/// This is a point-in-time filesystem observation, not fresh package authentication.
pub(crate) fn tool_in_build(build_dir: &Path, tool: &ToolName) -> Option<PathBuf> {
    let bin = build_dir.join("bin");
    if !std::fs::symlink_metadata(&bin)
        .is_ok_and(|meta| meta.is_dir() && !crate::platform::is_reparse(&meta))
    {
        return None;
    }
    let root = std::fs::canonicalize(build_dir).ok()?;
    let logical = bin.join(tool.exe_file());
    let mut target = root.join("bin").join(tool.exe_file());
    for followed in 0..=16 {
        let next_target = {
            let mut components = target.strip_prefix(&root).ok()?.components().peekable();
            let mut at = root.clone();
            let mut next_target = None;
            while let Some(component) = components.next() {
                let std::path::Component::Normal(name) = component else {
                    return None;
                };
                at.push(name);
                let meta = std::fs::symlink_metadata(&at).ok()?;
                if components.peek().is_some() {
                    if !meta.is_dir() || crate::platform::is_reparse(&meta) {
                        return None;
                    }
                } else if meta.is_file() && !crate::platform::is_reparse(&meta) {
                    return Some(logical);
                } else if meta.file_type().is_symlink() && followed < 16 {
                    let raw = std::fs::read_link(&at).ok()?;
                    let next = crate::seam::absolute_target(&raw, &at);
                    if !next.starts_with(&root) {
                        return None;
                    }
                    next_target = Some(next);
                    break;
                } else {
                    return None;
                }
            }
            next_target
        };
        target = next_target?;
    }
    None
}

/// The build a path into the store names by its directory: `<build>`, or `<build>.noindex`,
/// the folder a build laid aside lives in ([`aterm_toolchain::layout`]), which a canonical
/// path (a running image, a compiler's sysroot) names.
fn build_in_path(name: &std::ffi::OsStr) -> Option<u64> {
    crate::store::parse_build_name(build_dir_name(name)?)
}

/// The build name a store build's directory `name` spells, `<build>` or the laid-aside
/// `<build>.noindex` read as `<build>` ([`build_in_path`], [`crate::shim_env::of_store_binary`]).
pub(crate) fn build_dir_name(name: &std::ffi::OsStr) -> Option<&str> {
    let name = name.to_str()?;
    Some(
        name.strip_suffix(aterm_toolchain::build_cache::NOINDEX)
            .unwrap_or(name),
    )
}

#[must_use]
/// The command names `program`'s installed build puts on `PATH`.
pub fn installed_exposes(layout: &Layout, program: &str) -> Option<Vec<String>> {
    let shims = read_bin_dir(layout).ok()?;
    let mut out = Vec::new();
    for shim in shims.flatten() {
        let name = shim.file_name().to_string_lossy().into_owned();
        let tool = ToolName::from_shim_file(&name);
        if tool.as_ref().is_some_and(ToolName::is_alias) {
            continue;
        }
        let Some(target) = crate::platform::resolve_shim(&shim.path()) else {
            continue;
        };
        let Some((owner, _build)) = program_build_ref_of_target(&target) else {
            continue;
        };
        if owner != program {
            continue;
        }
        out.push(tool.map_or(name, |tool| tool.as_str().to_string()));
    }
    if out.is_empty() { None } else { Some(out) }
}

/// The active build of each program.
pub fn active_builds(layout: &Layout) -> BTreeMap<String, u64> {
    active_builds_in(&BinScan::capture(layout))
}

/// The active build of `program`, when one of its `bin/` shims resolves to a present build.
#[must_use]
pub fn active_build_of(layout: &Layout, program: &str) -> Option<u64> {
    let shims = read_bin_dir(layout).ok()?;
    let mut found = None;
    for shim in shims.flatten() {
        let Some(target) = crate::platform::resolve_shim(&shim.path()) else {
            continue;
        };
        let Some((owner, build)) = program_build_ref_of_target(&target) else {
            continue;
        };
        if owner != program || found == Some(build) {
            continue;
        }
        if !crate::store::presence(&target).is_absent() {
            found = Some(build);
        }
    }
    found
}

/// [`active_builds`] over a listing already taken ([`active_tools_in`]).
#[must_use]
pub(crate) fn active_builds_in(scan: &BinScan) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for target in scan.targets() {
        let Some((program, build)) = program_build_ref_of_target(target) else {
            continue;
        };
        if out.get(program) == Some(&build) {
            continue;
        }
        // A shim that names a build is not a build that is there, so the target is stat'd
        // and `list`, `which` and `doctor` agree. Only proved absence drops it: a store this
        // user may execute but not stat (EACCES, EPERM, EIO) is still there
        // ([`crate::store::Presence`]).
        if crate::store::presence(target).is_absent() {
            continue;
        }
        out.insert(program.to_string(), build);
    }
    out
}

/// The tool names whose `bin/` shims point into `store/<program>/<build>/`.
#[must_use]
pub fn active_tools(layout: &Layout, program: &str, build: u64) -> Vec<ToolName> {
    active_names_in(&BinScan::capture(layout), program, build, false)
}

/// [`active_tools`] over a `bin/` listing already taken.
#[must_use]
pub(crate) fn active_tools_in(scan: &BinScan, program: &str, build: u64) -> Vec<ToolName> {
    active_names_in(scan, program, build, false)
}

/// The `alab-<tool>` alias names pointing into `store/<program>/<build>/` ([`active_tools`]).
#[must_use]
pub fn active_aliases(layout: &Layout, program: &str, build: u64) -> Vec<ToolName> {
    active_names_in(&BinScan::capture(layout), program, build, true)
}

/// The shared scan behind [`active_tools`] (`aliases == false`) and [`active_aliases`] (`true`).
fn active_names_in(scan: &BinScan, program: &str, build: u64, aliases: bool) -> Vec<ToolName> {
    let mut out = Vec::new();
    for (tool, target) in scan.tools() {
        if tool.is_alias() == aliases
            && program_build_ref_of_target(target) == Some((program, build))
        {
            out.push(tool.clone());
        }
    }
    out.sort();
    out
}

/// List installed `(program, build)` pairs by walking `store/<program>/<build>/`.
#[must_use]
pub fn list_installed(layout: &Layout) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let store = layout.prefix.join("store");
    let Ok(programs) = std::fs::read_dir(&store) else {
        return out;
    };
    for prog in programs.flatten() {
        if !prog.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let pname_os = prog.file_name();
        let Some(pname) = pname_os.to_str().map(str::to_string) else {
            continue;
        };
        for n in installed_builds_in(&prog.path()) {
            out.push((pname.clone(), n));
        }
    }
    out.sort();
    out
}

/// List installed builds for `program` by walking `store/<program>/<build>/`.
#[must_use]
pub fn list_installed_for(layout: &Layout, program: &str) -> Vec<u64> {
    if uninstall_name_shape(program).is_err() {
        return Vec::new();
    }
    let dir = layout.prefix.join("store").join(program);
    if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_dir()) {
        return Vec::new();
    }
    let mut out = installed_builds_in(&dir);
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether `(program, build)` is an installed complete build in `layout`.
#[must_use]
pub fn is_build_installed(layout: &Layout, program: &str, build: u64) -> bool {
    if uninstall_name_shape(program).is_err() {
        return false;
    }
    let prog_dir = layout.prefix.join("store").join(program);
    std::fs::symlink_metadata(&prog_dir).is_ok_and(|m| m.file_type().is_dir())
        && crate::store::build_is_complete(&prog_dir.join(build.to_string()))
}

fn installed_builds_in(dir: &Path) -> Vec<u64> {
    let mut out = Vec::new();
    if let Ok(builds) = std::fs::read_dir(dir) {
        for b in builds.flatten() {
            let bname_os = b.file_name();
            if let Some(n) = bname_os.to_str().and_then(crate::store::parse_build_name) {
                // Only complete builds count: a tree a crash left partial has no
                // marker, and counting it would report up to date and never repair it.
                if crate::store::build_is_complete(&b.path()) {
                    out.push(n);
                }
            }
        }
    }
    out
}

/// The shape rule [`uninstall`] applies before touching anything.
pub(crate) fn uninstall_name_shape(program: &str) -> io::Result<()> {
    if program.is_empty()
        || program == "."
        || program == ".."
        || program.contains('/')
        || program.contains('\\')
        || program.contains('\0')
        || !matches!(
            Path::new(program)
                .components()
                .collect::<Vec<_>>()
                .as_slice(),
            [std::path::Component::Normal(_)]
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program name must be a single safe path component (no separators, `.`, `..`, or NUL)",
        ));
    }
    Ok(())
}

/// Uninstall `program` (`atpkg uninstall <program>`) ([`crate::compat`]).
pub fn uninstall(layout: &Layout, program: &str) -> io::Result<()> {
    uninstall_name_shape(program)?;

    let prog_store = layout.prefix.join("store").join(program);
    let _first_exec = crate::store::first_exec_program_removal(&prog_store)?;
    let sdk = crate::compat::prepare_sdk_removal(layout, program, true)?;
    if let Some(held) = crate::store::held_build_in(&prog_store) {
        return Err(io::Error::other(format!(
            "{} stays in place: a resolve check's child in it never exited on its own, and \
             macOS may still be assessing that first exec; uninstall again in a few minutes",
            held.display()
        )));
    }

    for dir in [layout.bin_dir(), layout.agents_dir()] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if let Some(target) = crate::platform::resolve_shim(&path)
                && target.starts_with(&prog_store)
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    crate::activate::sweep_agents_dir(layout);

    if prog_store.starts_with(&layout.prefix) && prog_store.exists() {
        crate::store::unmark_program_builds(&prog_store)?;
        if let Ok(entries) = std::fs::read_dir(&prog_store) {
            for entry in entries.flatten() {
                crate::store::unseal_bin(&entry.path());
            }
        }
        std::fs::remove_dir_all(&prog_store)?;
    }

    crate::compat::remove_program_guarded(layout, program, &sdk)?;
    Ok(())
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
