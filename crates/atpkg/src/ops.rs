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

/// File to exec for `tool` without its shim ([`which`], [`crate::compat::route_for_shim`]).
fn exec_path(layout: &Layout, tool: &str) -> Option<PathBuf> {
    let shim = layout.shim(&ToolName::new(tool)?);
    let target = crate::platform::resolve_shim(&shim)?;
    Some(crate::compat::route_for_shim(&shim, &target).unwrap_or(target))
}

/// Env for `tool` ([`crate::shim_env::read_sidecar`], [`crate::activate::reassert_shim_env`]).
fn exec_env(layout: &Layout, tool: &str) -> crate::shim_env::ShimEnv {
    let Some(name) = ToolName::new(tool) else {
        return crate::shim_env::ShimEnv::NONE;
    };
    let shim = layout.shim(&name);
    let declared = crate::platform::resolve_shim(&shim)
        .and_then(|target| store_build_of(&layout.prefix, &target))
        .map(|(program, build)| crate::shim_env::read_sidecar(&layout.build_dir(&program, build)))
        .unwrap_or_default();
    if declared.is_empty() {
        crate::platform::shim_env_of(&shim)
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
    Some(Managed {
        target: exec_path(layout, tool)?,
        env: exec_env(layout, tool),
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
    let comps: Vec<&std::ffi::OsStr> = target.iter().collect();
    let i = comps.iter().position(|c| *c == "store")?;
    let program = comps.get(i + 1)?.to_str()?.to_string();
    let build = comps.get(i + 2)?.to_str()?.parse::<u64>().ok()?;
    Some((program, build))
}

/// Parse `(program, build)` out of a path that must lie inside `prefix`'s store.
#[must_use]
pub fn store_build_of(prefix: &Path, target: &Path) -> Option<(String, u64)> {
    let rel = target.strip_prefix(prefix.join("store")).ok()?;
    let mut comps = rel.components();
    let (Some(std::path::Component::Normal(program)), Some(std::path::Component::Normal(build))) =
        (comps.next(), comps.next())
    else {
        return None;
    };
    let program = program.to_str()?.to_string();
    let build = build.to_str()?.parse::<u64>().ok()?;
    Some((program, build))
}

#[must_use]
/// The command names `program`'s installed build puts on `PATH`.
pub fn installed_exposes(layout: &Layout, program: &str) -> Option<Vec<String>> {
    let shims = read_bin_dir(layout).ok()?;
    let mut out = Vec::new();
    for shim in shims.flatten() {
        let Some(target) = crate::platform::resolve_shim(&shim.path()) else {
            continue;
        };
        let Some((owner, _build)) = program_build_of_target(&target) else {
            continue;
        };
        if owner != program {
            continue;
        }
        let name = shim.file_name().to_string_lossy().into_owned();
        let tool = ToolName::from_shim_file(&name);
        if tool.as_ref().is_some_and(ToolName::is_alias) {
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

/// [`active_builds`] over a listing already taken ([`active_tools_in`]).
#[must_use]
pub(crate) fn active_builds_in(scan: &BinScan) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for target in scan.targets() {
        // A shim that names a build is not a build that is there, so the target is stat'd
        // and `list`, `which` and `doctor` agree
        // (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md). Only proved absence drops
        // it: a store this user may execute but not stat (EACCES, EPERM, EIO) is still
        // there ([`crate::store::Presence`]).
        if crate::store::presence(target).is_absent() {
            continue;
        }
        if let Some((program, build)) = program_build_of_target(target) {
            out.insert(program, build);
        }
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
        if let Some((p, b)) = program_build_of_target(target)
            && p == program
            && b == build
            && tool.is_alias() == aliases
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
        if let Ok(builds) = std::fs::read_dir(prog.path()) {
            for b in builds.flatten() {
                let bname_os = b.file_name();
                if let Some(n) = bname_os.to_str().and_then(crate::store::parse_build_name) {
                    // Only complete builds count: a tree a crash left partial has no
                    // marker, and counting it would report up to date and never repair it.
                    if crate::store::build_is_complete(&b.path()) {
                        out.push((pname.clone(), n));
                    }
                }
            }
        }
    }
    out.sort();
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
        crate::store::unmark_program_builds(&prog_store);
        if let Ok(entries) = std::fs::read_dir(&prog_store) {
            for entry in entries.flatten() {
                crate::store::unseal_bin(&entry.path());
            }
        }
        std::fs::remove_dir_all(&prog_store)?;
    }

    crate::compat::remove_program(layout, program)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activate::{activate_build, install_shims};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn tool(name: &str) -> ToolName {
        ToolName::new(name).unwrap()
    }

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-ops-{label}-"))
    }

    fn install(layout: &Layout, program: &str, build: u64) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("bin").join(tool(program).exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        install_shims(
            layout,
            &dir,
            &[program.to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        activate_build(layout, &dir).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        dir
    }

    /// An interrupted uninstall (^C, SIGTERM, an EACCES in the walk) leaves no `<n>.ready`
    /// marker beside a half-emptied tree.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_uninstall_leaves_no_build_still_vouched_for() {
        let l = layout("uninstall-interrupted");
        let keep = install(&l, "trust", 8590);
        let doomed = install(&l, "trust", 8595);
        assert!(crate::store::build_is_complete(&keep));
        assert!(crate::store::build_is_complete(&doomed));
        std::fs::set_permissions(keep.join("bin"), std::fs::Permissions::from_mode(0o500)).unwrap();
        let outcome = uninstall(&l, "trust");
        let aborted = keep.join("bin").join(tool("trust").exe_file()).is_file();
        std::fs::set_permissions(keep.join("bin"), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            aborted && outcome.is_err(),
            "the fixture must make the removal abort partway, or it proves nothing: \
             aborted={aborted} outcome={outcome:?}"
        );
        for (n, dir) in [(8590u64, &keep), (8595, &doomed)] {
            assert!(
                !crate::store::build_is_complete(dir),
                "build {n} must not still be vouched for after an aborted uninstall"
            );
        }
        assert!(
            !list_installed(&l).iter().any(|(p, _)| p == "trust"),
            "and nothing reports trust as installed — so rollback cannot select the wreck"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn partial_build_without_marker_is_not_listed() {
        let l = layout("partial");
        let dir = l.build_dir("ay", 18);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/ay"), b"partial").unwrap();
        assert!(!crate::store::build_is_complete(&dir));
        assert!(
            list_installed(&l).is_empty(),
            "partial (marker-less) build is not installed"
        );
        crate::store::mark_build_ready(&dir).unwrap();
        assert_eq!(list_installed(&l), vec![("ay".into(), 18)]);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn store_build_of_is_anchored_to_the_prefix() {
        let prefix = Path::new("/p");
        assert_eq!(
            store_build_of(prefix, Path::new("/p/store/ay/18/bin/ay")),
            Some(("ay".to_string(), 18))
        );
        assert_eq!(
            store_build_of(prefix, Path::new("/p/store/ay/18")),
            Some(("ay".to_string(), 18))
        );
        assert_eq!(
            store_build_of(prefix, Path::new("/other/store/ay/18")),
            None
        );
        assert_eq!(
            program_build_of_target(Path::new("/other/store/ay/18")),
            Some(("ay".to_string(), 18)),
            "the unanchored parser is why the anchored one has to exist"
        );
        assert_eq!(store_build_of(prefix, Path::new("/p/store/ay")), None);
        assert_eq!(store_build_of(prefix, Path::new("/p/store/ay/head")), None);
        assert_eq!(store_build_of(prefix, Path::new("/p/store/../ay/18")), None);
    }

    #[test]
    fn a_shim_pointing_at_a_deleted_build_is_not_live() {
        let l = layout("dangling");
        install(&l, "ay", 17);
        assert_eq!(
            active_builds(&l).get("ay").copied(),
            Some(17),
            "precondition: a real install is live"
        );
        std::fs::remove_dir_all(l.build_dir("ay", 17)).unwrap();
        assert!(
            crate::platform::resolve_shim(&l.shim(&tool("ay"))).is_some(),
            "the shim itself must still resolve — otherwise this test proves nothing"
        );
        assert!(
            !active_builds(&l).contains_key("ay"),
            "a shim whose target no longer exists must not be reported live"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A target that is there but cannot be stat'd (EACCES, EPERM, EIO) is still live: only
    /// the permission to look is removed, so the answer must not change.
    #[cfg(unix)]
    #[test]
    fn a_shim_whose_target_cannot_be_stat_ed_is_still_live() {
        let l = layout("unstattable");
        install(&l, "ay", 17);
        assert_eq!(
            active_builds(&l).get("ay").copied(),
            Some(17),
            "precondition: a real install is live"
        );
        let target = crate::platform::resolve_shim(&l.bin_dir().join("ay")).unwrap();
        let holder = target.parent().unwrap().to_path_buf();
        let saved = std::fs::metadata(&holder).unwrap().permissions();
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o000)).unwrap();
        let stat = std::fs::metadata(&target);
        if stat.is_ok() {
            std::fs::set_permissions(&holder, saved).unwrap();
            let _ = std::fs::remove_dir_all(&l.prefix);
            aterm_uds::host::could_not_run(
                "a stat through a mode-0 folder succeeded: root, or a file system that ignores modes",
            );
        }
        assert_eq!(
            stat.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "the fixture must fail the stat for the reason under test"
        );
        let live = active_builds(&l).get("ay").copied();
        std::fs::set_permissions(&holder, saved).unwrap();
        let _ = std::fs::remove_dir_all(&l.prefix);
        assert_eq!(
            live,
            Some(17),
            "a target we merely could not LOOK at is not a target that is gone"
        );
    }

    #[test]
    fn active_builds_reflects_the_shim_not_a_staged_inactive_build() {
        let l = layout("active");
        install(&l, "ay", 17);
        let staged = l.build_dir("ay", 18);
        std::fs::create_dir_all(staged.join("bin")).unwrap();
        std::fs::write(staged.join("bin/ay"), b"#!/bin/true\n").unwrap();
        crate::store::mark_build_ready(&staged).unwrap();

        assert!(list_installed(&l).contains(&("ay".to_string(), 18)));
        assert_eq!(active_builds(&l).get("ay").copied(), Some(17));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn active_tools_lists_only_this_builds_shims() {
        let l = layout("active-tools");
        install(&l, "ay", 18);
        install(&l, "ny", 9);
        assert_eq!(active_tools(&l, "ay", 18), vec![tool("ay")]);
        assert_eq!(active_tools(&l, "ny", 9), vec![tool("ny")]);
        assert!(active_tools(&l, "ay", 17).is_empty());
        assert!(active_tools(&l, "nope", 1).is_empty());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn list_installed_reads_only_the_canonical_build_name() {
        let l = layout("canonical-build-names");
        install(&l, "ay", 18);
        for lookalike in ["+19", "019"] {
            let dir = l.prefix.join("store").join("ay").join(lookalike);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::write(
                dir.join("bin").join(tool("ay").exe_file()),
                b"#!/bin/true\n",
            )
            .unwrap();
            crate::store::mark_build_ready(&dir).unwrap();
            assert!(
                crate::store::build_is_complete(&dir),
                "{lookalike} must look every bit as complete as a real build, or the \
                 fixture proves nothing"
            );
        }
        assert_eq!(
            list_installed(&l),
            vec![("ay".into(), 18)],
            "a build-number lookalike is not an installed build"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn uninstall_removes_shim_store_and_channel_link() {
        let l = layout("uninstall");
        install(&l, "ay", 18);
        install(&l, "ny", 9);
        assert!(which(&l, "ay").is_some());

        uninstall(&l, "ay").unwrap();
        assert!(which(&l, "ny").is_some(), "another program is untouched");
        assert!(l.build_dir("ny", 9).exists());

        assert!(which(&l, "ay").is_none(), "shim removed");
        assert!(!l.build_dir("ay", 18).exists(), "store tree reclaimed");
        assert!(
            !l.prefix.join("store/ay").exists(),
            "program store dir removed"
        );
        assert!(std::fs::symlink_metadata(l.program_current("ay")).is_err());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn uninstall_rejects_path_traversal_names() {
        let l = layout("uninstall-traversal");

        let name = format!("atpkg-victim-do-not-delete-{}", std::process::id());
        let victim = l.prefix.parent().unwrap().join(&name);
        let _ = std::fs::remove_dir_all(&victim);
        std::fs::create_dir_all(victim.join("keep")).unwrap();

        let traversal = format!("../../{name}");
        let err = uninstall(&l, &traversal).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            victim.join("keep").exists(),
            "out-of-prefix directory must NOT be deleted by a traversal program name"
        );

        let err = uninstall(&l, "..").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            l.prefix.exists(),
            "managed prefix must survive `uninstall ..`"
        );

        for bad in ["", ".", "a/b", "a\\b", "x\0y"] {
            assert_eq!(
                uninstall(&l, bad).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }

        for drive in ["C:", "C:x", "C:.."] {
            let escapes = !l.prefix.join("store").join(drive).starts_with(&l.prefix);
            assert_eq!(
                escapes,
                cfg!(windows),
                "{drive:?}: the join leaves the prefix"
            );
            assert_eq!(uninstall_name_shape(drive).is_err(), escapes, "{drive:?}");
        }

        let _ = std::fs::remove_dir_all(&victim);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// [`exec_env`] is the build's declaration when it has one, whatever the shim as laid
    /// says; else the shim's own exports (add-only); nothing for a plain shim over a build
    /// declaring none, a name with no shim, or a name no shim could have.
    #[cfg(unix)]
    #[test]
    fn exec_env_is_what_the_build_declares_else_what_the_shim_exports() {
        let l = layout("exec-env");
        let dir = install(&l, "ay", 17);
        let none = crate::shim_env::ShimEnv::NONE;
        let laid = crate::shim_env::ShimEnv::admit(&["AY_QUIET=1".to_string()]).unwrap();
        let declared =
            crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        assert_eq!(
            exec_env(&l, "ay"),
            none,
            "a plain shim over a build declaring nothing"
        );

        crate::activate::install_tools_env(
            &l,
            &dir,
            &[tool("ay")],
            crate::activate::Aliases::Off,
            &laid,
        )
        .unwrap();
        assert_eq!(
            exec_env(&l, "ay"),
            laid,
            "no declaration: the shim's own exports stand"
        );

        crate::shim_env::write_sidecar(&dir, &declared).unwrap();
        assert_eq!(
            exec_env(&l, "ay"),
            declared,
            "a declaration wins over the shim as laid"
        );
        crate::activate::install_tools(&l, &dir, &[tool("ay")], crate::activate::Aliases::Off)
            .unwrap();
        assert_eq!(
            crate::platform::shim_env_of(&l.shim(&tool("ay"))),
            none,
            "fixture: the drifted shape — the shim laid plain"
        );
        assert_eq!(exec_env(&l, "ay"), declared, "…and over a shim laid plain");

        assert_eq!(exec_env(&l, "absent"), none);
        assert_eq!(exec_env(&l, "../ay"), none);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
