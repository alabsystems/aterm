// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Activation (§10): the atomic `store/<program>/current` flip that makes a staged store build live, and the `bin/` shims that forward into it.

use std::io;
use std::path::{Path, PathBuf};

use crate::Layout;
use crate::platform::{self};
use crate::store::{ToolName, split_exposed};

/// Whether a program's exposed tools also get their `alab-<tool>` aliases (module doc).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Aliases {
    /// One of ALab's own programs: `alab-<tool>` beside every `<tool>`.
    Alab,
    /// A vendor tool or a system-satisfiable member.
    Off,
}

impl Aliases {
    /// The policy for an index entry.
    #[must_use]
    pub fn for_program(name: &str, program: Option<&crate::manifest::Program>) -> Self {
        match program {
            Some(p) if p.system.is_none() && !crate::stub::is_agent_program(name) => Self::Alab,
            _ => Self::Off,
        }
    }

    /// The policy already in force for `program` on disk: `Alab` when any alias shim resolves into its store tree, else `Off`.
    #[must_use]
    pub fn laid_for(layout: &Layout, program: &str) -> Self {
        let prog_store = layout.prefix.join("store").join(program);
        let Ok(entries) = crate::ops::read_bin_dir(layout) else {
            return Self::Off;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(tool) = ToolName::from_shim_file(name) else {
                continue;
            };
            if tool.is_alias()
                && platform::resolve_shim(&e.path()).is_some_and(|t| t.starts_with(&prog_store))
            {
                return Self::Alab;
            }
        }
        Self::Off
    }
}

/// Atomically point `link` at `target` ([`crate::platform::atomic_symlink`]: temp symlink + `rename(2)` on POSIX, a directory junction on Windows).
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    platform::atomic_symlink(target, link)
}

/// Make `build_dir` the active build.
pub fn activate_build(layout: &Layout, build_dir: &Path) -> io::Result<()> {
    let Some((program, _)) = crate::ops::store_build_of(&layout.prefix, build_dir) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a store build directory",
        ));
    };
    atomic_symlink(build_dir, &layout.program_current(&program))
}

/// Install `bin/` shims for the manifest's raw `exposes` list, each forwarding to the tool's executable in `<build_dir>/bin/`.
pub fn install_shims(
    layout: &Layout,
    build_dir: &Path,
    exposes: &[String],
    aliases: Aliases,
) -> io::Result<Vec<String>> {
    let (tools, refused) = split_exposed(exposes);
    install_tools(layout, build_dir, &tools, aliases)?;
    Ok(refused)
}

/// [`install_shims`] whose shims export `env` before they exec (design S7).
pub fn install_shims_env(
    layout: &Layout,
    build_dir: &Path,
    exposes: &[String],
    aliases: Aliases,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<Vec<String>> {
    install_shims_env_saying(layout, build_dir, exposes, aliases, env, &mut |line| {
        println!("{line}");
    })
}

/// [`install_shims_env`] that hands what it says (an exec root it laid) to `say` instead of stdout.
pub(crate) fn install_shims_env_saying(
    layout: &Layout,
    build_dir: &Path,
    exposes: &[String],
    aliases: Aliases,
    env: &crate::shim_env::ShimEnv,
    say: &mut dyn FnMut(String),
) -> io::Result<Vec<String>> {
    let (tools, refused) = split_exposed(exposes);
    install_tools_env_saying(layout, build_dir, &tools, aliases, env, say)?;
    Ok(refused)
}

/// The validated half of [`install_shims`].
pub(crate) fn install_tools(
    layout: &Layout,
    build_dir: &Path,
    tools: &[ToolName],
    aliases: Aliases,
) -> io::Result<()> {
    install_tools_env(
        layout,
        build_dir,
        tools,
        aliases,
        &crate::shim_env::ShimEnv::NONE,
    )
}

/// [`install_tools`] whose shims, primary and alias, export `env` before they exec (design S7).
pub(crate) fn install_tools_env(
    layout: &Layout,
    build_dir: &Path,
    tools: &[ToolName],
    aliases: Aliases,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    install_tools_env_saying(layout, build_dir, tools, aliases, env, &mut |line| {
        println!("{line}");
    })
}

/// [`install_tools_env`], saying through `say` ([`install_shims_env_saying`]).
fn install_tools_env_saying(
    layout: &Layout,
    build_dir: &Path,
    tools: &[ToolName],
    aliases: Aliases,
    env: &crate::shim_env::ShimEnv,
    say: &mut dyn FnMut(String),
) -> io::Result<()> {
    let bin = layout.bin_dir();
    layout.ensure_dir(&bin)?;
    if let Some(n) = crate::compat::trust_build_of(layout, build_dir) {
        match crate::compat::ensure_root(layout, build_dir, crate::seam::Depth::Shallow) {
            Ok(crate::compat::Ensured::Built) => say(crate::compat::laid_line(layout, n)),
            Ok(crate::compat::Ensured::Plain | crate::compat::Ensured::Present) => {}
            Err(e) => eprintln!("{}", crate::compat::not_laid_line(layout, n, &e)),
        }
    }
    let mut files = Vec::new();
    for tool in tools {
        files.push(platform::shim_executable_env(
            &build_dir.join("bin"),
            tool,
            &layout.shim(tool),
            env,
        )?);
        if aliases == Aliases::Alab
            && let Some(alias) = tool.alias()
        {
            files.push(platform::shim_executable_env(
                &build_dir.join("bin"),
                tool,
                &layout.shim(&alias),
                env,
            )?);
        }
    }
    files.iter().try_for_each(crate::lay::write_in_process)?;
    #[cfg(all(test, unix))]
    for tool in tools {
        agents_checkpoint::at(layout, agents_checkpoint::Event::Written(tool.as_str()));
    }
    prune_stale_shims(layout, build_dir, tools, aliases);
    reconcile_agents(layout);
    Ok(())
}

/// Bring `<prefix>/agents/` ([`Layout::agents_dir`]) in line with `bin/`.
pub fn reconcile_agents(layout: &Layout) {
    let _turn = AGENTS_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let publication = crate::lay::PUBLICATION_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for name in crate::stub::AGENT_PROGRAMS {
        let Some(tool) = ToolName::new(name) else {
            continue;
        };
        let primary = layout.shim(&tool);
        let Some((target, primary_is)) = platform::resolve_shim(&primary)
            .and_then(|t| agent_primary(layout, name, &t).map(|is| (t, is)))
        else {
            // not installed here (a pending stub, a tombstone): the sweep answers
            continue;
        };
        let dev_link = primary_is == AgentPrimary::DevLink;
        let env = platform::shim_env_of(&primary);
        #[cfg(all(test, unix))]
        agents_checkpoint::at(layout, agents_checkpoint::Event::Read(name));
        let twin = layout.agent_shim(&tool);
        let exec = twin_exec(layout, name, &target, primary_is);
        let named = |shim: &Path| {
            crate::metadata_io::read_bounded_regular_utf8(shim, 64 * 1024)
                .ok()
                .and_then(|body| crate::stub::sh_named_atpkg(&body, "__atpkg"))
        };
        let standing = named(&twin).or_else(|| named(&primary));
        let atpkg = crate::stub::Embedder::this_process().embed(standing.as_deref());
        let prelude = if dev_link {
            String::new()
        } else {
            platform::twin_prelude(
                name,
                &layout.prefix,
                atpkg.as_deref(),
                crate::selfupdate::verbs_of(name),
                &crate::yolo::policy_of(name),
            )
        };
        if (exec != target || (!dev_link && !prelude.is_empty()))
            && let Err(e) =
                primary_executable(&primary, &target, &exec, &env, &prelude).and_then(|file| {
                    if crate::metadata_io::read_bounded_regular(&primary, platform::MAX_SHIM_BYTES)
                        .is_ok_and(|have| have == file.body)
                    {
                        Ok(())
                    } else {
                        crate::lay::write_in_publication_turn(&file, &publication)
                    }
                })
        {
            eprintln!(
                "atpkg: {name}: bin/{name} could not retain its managed launch ({e}); \
                 retried next pass"
            );
        }
        if platform::resolve_shim(&twin).is_some_and(|t| t == exec)
            && platform::shim_env_of(&twin) == env
            && twin_is_rendered(&twin, &exec, &env, &prelude)
        {
            continue;
        }
        let laid = layout
            .ensure_dir(&layout.agents_dir())
            .and_then(|()| platform::twin_executable_to_env(&twin, &exec, &env, &prelude))
            .and_then(|file| crate::lay::write_in_publication_turn(&file, &publication));
        if let Err(e) = laid {
            eprintln!(
                "atpkg: {name}: agents/ twin {} not laid ({e}); bin/{name} is in place; \
                 retried next pass",
                twin.display()
            );
        }
    }
    sweep_agents(layout);
}

fn primary_executable(
    shim: &Path,
    target: &Path,
    _exec: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> io::Result<crate::lay::Executable> {
    #[cfg(unix)]
    if _exec != target {
        return Ok(crate::lay::Executable::new(
            shim,
            platform::sh_shim_content_managed(target, env, _exec, prelude),
        ));
    }
    platform::twin_executable_to_env(shim, target, env, prelude)
}

/// `store/<program>/exec/`: the directory holding the stable path an ALab build's primary and twin exec ([`twin_exec`]).
#[must_use]
pub fn exec_dir(layout: &Layout, program: &str) -> PathBuf {
    layout.prefix.join("store").join(program).join("exec")
}

/// The stable launch path for an ALab primary and its agent twin. For a non-ALab build,
/// it is `target` itself, byte for byte as before.
fn twin_exec(layout: &Layout, program: &str, target: &Path, primary: AgentPrimary) -> PathBuf {
    if cfg!(not(unix)) || crate::alab::agent(program).is_none() {
        return target.to_path_buf();
    }
    let alab = match primary {
        AgentPrimary::Store => {
            crate::ops::store_build_of(&layout.prefix, target).is_some_and(|(owner, build)| {
                owner == program
                    && crate::alab::build_is_alab(layout, program, build)
                    && crate::alab::current_build(layout, program) == Some(build)
            })
        }
        AgentPrimary::DevLink => {
            crate::linkmode::linked_checkout(layout, program).is_some_and(|checkout| {
                crate::alab::is_alab_package(&checkout) && target.starts_with(&checkout)
            })
        }
    };
    let entry = target
        .file_name()
        .filter(|_| alab)
        .map(|name| exec_dir(layout, program).join(name));
    let laid = entry.filter(|entry| {
        std::fs::read_link(entry).ok().as_deref() == Some(target)
            || layout
                .ensure_dir(&exec_dir(layout, program))
                .and_then(|()| platform::atomic_symlink(target, entry))
                .is_ok()
    });
    laid.unwrap_or_else(|| target.to_path_buf())
}

/// What `program`'s `agents/` twin runs, spelled as its `bin/` primary spells it.
#[must_use]
pub fn twin_target(layout: &Layout, program: &str) -> Option<PathBuf> {
    let tool = ToolName::new(program)?;
    let raw = platform::resolve_shim(&layout.agent_shim(&tool))?;
    Some(unstable_spelling(layout, program, raw))
}

/// [`twin_target`]'s mapping of one exec target: an [`exec_dir`] entry reads back as the
/// file it names.
fn unstable_spelling(layout: &Layout, program: &str, raw: PathBuf) -> PathBuf {
    if raw.parent() == Some(exec_dir(layout, program).as_path())
        && let Ok(named) = std::fs::read_link(&raw)
    {
        return named;
    }
    raw
}

/// What an agent program's `bin/` shim forwards to, as `agents/` may name it
/// ([`agent_primary`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentPrimary {
    /// A build in an agent program's store tree.
    Store,
    /// A file in the program's recorded dev-link checkout (`aterm pkg link`).
    DevLink,
}

/// What `target`, the forward of `program`'s `bin/` shim, is when an `agents/` twin may name
/// it: a build in an agent program's store tree, or a file inside the program's recorded
/// dev-link checkout ([`crate::linkmode::linked_checkout`]), so a dev link also wins at the
/// front of `PATH`.
fn agent_primary(layout: &Layout, program: &str, target: &Path) -> Option<AgentPrimary> {
    if crate::ops::store_build_of(&layout.prefix, target)
        .is_some_and(|(owner, _)| crate::stub::is_agent_program(&owner))
    {
        return Some(AgentPrimary::Store);
    }
    (crate::stub::is_agent_program(program)
        && crate::linkmode::linked_checkout(layout, program)
            .is_some_and(|checkout| target.starts_with(checkout)))
    .then_some(AgentPrimary::DevLink)
}

/// The turn every reconcile and sweep of `agents/` takes, before
/// [`crate::lay::PUBLICATION_TURN`].
static AGENTS_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fixture may park a real agent-shim read or write for its own prefix only.
#[cfg(all(test, unix))]
pub(crate) mod agents_checkpoint {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use crate::store::Layout;

    pub(crate) enum Event<'a> {
        Read(&'a str),
        Written(&'a str),
        Waiting(&'a str),
    }

    type Hook = Arc<dyn Fn(Event<'_>) + Send + Sync>;
    static HOOKS: Mutex<BTreeMap<PathBuf, Hook>> = Mutex::new(BTreeMap::new());

    pub(crate) fn at(layout: &Layout, event: Event<'_>) {
        let hook = HOOKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&layout.prefix)
            .cloned();
        if let Some(hook) = hook {
            hook(event);
        }
    }

    pub(crate) fn watch(layout: &Layout, hook: Hook) -> Watch {
        assert!(
            HOOKS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(layout.prefix.clone(), hook)
                .is_none()
        );
        Watch(layout.prefix.clone())
    }

    pub(crate) fn waiting(path: &std::path::Path) {
        let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
            return;
        };
        let hooks: Vec<Hook> = HOOKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(prefix, _)| path.parent() == Some(prefix.join("bin").as_path()))
            .map(|(_, hook)| hook.clone())
            .collect();
        for hook in hooks {
            hook(Event::Waiting(name));
        }
    }

    pub(crate) struct Watch(PathBuf);

    impl Drop for Watch {
        fn drop(&mut self) {
            HOOKS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.0);
        }
    }
}

/// [`shim_is_rendered`] for an `agents/` twin: the bytes the twin renderer lays for
/// `target`, `env` and its `prelude` ([`platform::twin_executable_to_env`]).
fn twin_is_rendered(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> bool {
    platform::twin_executable_to_env(shim, target, env, prelude).is_ok_and(|want| {
        crate::metadata_io::read_bounded_regular(shim, platform::MAX_SHIM_BYTES)
            .is_ok_and(|have| have == want.body)
    })
}

/// Whether `shim` holds, byte for byte, what the one shim renderer
/// (`platform::shim_executable_to_env`) lays for `target` and `env` now: the half of the
/// reconciles' "already right" that target and environment cannot see.
fn shim_is_rendered(shim: &Path, target: &Path, env: &crate::shim_env::ShimEnv) -> bool {
    if cfg!(not(unix)) {
        return true;
    }
    platform::shim_executable_to_env(shim, target, env).is_ok_and(|want| {
        crate::metadata_io::read_bounded_regular(shim, platform::MAX_SHIM_BYTES)
            .is_ok_and(|have| have == want.body)
    })
}

/// Keep only live agent shims in `agents/`.
pub fn sweep_agents_dir(layout: &Layout) {
    let _turn = AGENTS_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _publication = crate::lay::PUBLICATION_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    sweep_agents(layout);
}

/// [`sweep_agents_dir`] inside a turn already taken.
fn sweep_agents(layout: &Layout) {
    let Ok(entries) = std::fs::read_dir(layout.agents_dir()) else {
        return;
    };
    let installed = crate::ops::active_builds(layout);
    for entry in entries.flatten() {
        let name = entry.file_name();
        if crate::lay::in_flight_here(&name) {
            continue;
        }
        let keep = name
            .to_str()
            .and_then(ToolName::from_shim_file)
            .is_some_and(|tool| {
                crate::stub::is_agent_program(tool.as_str())
                    && (platform::resolve_shim(&entry.path())
                        .map(|t| unstable_spelling(layout, tool.as_str(), t))
                        .is_some_and(|t| {
                            agent_primary(layout, tool.as_str(), &t).is_some()
                                && platform::resolve_shim(&layout.shim(&tool))
                                    .is_some_and(|b| b == t)
                        })
                        || (crate::stub::is_pending_stub(&entry.path())
                            && !installed.contains_key(tool.as_str())))
            });
        if !keep {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Bring an installed program's aliases in line with `aliases` without writing its primaries.
pub(crate) fn reconcile_aliases(
    layout: &Layout,
    build_dir: &Path,
    tools: &[ToolName],
    aliases: Aliases,
) -> io::Result<()> {
    if aliases == Aliases::Alab {
        for tool in tools {
            let Some(alias) = tool.alias() else { continue };
            let Some(wanted) = platform::resolve_shim(&layout.shim(tool))
                .filter(|t| t.starts_with(build_dir) && t.file_name().is_some())
            else {
                continue;
            };
            let Some(target_bin) = wanted.parent().map(Path::to_path_buf) else {
                continue;
            };
            let env = platform::shim_env_of(&layout.shim(tool));
            let shim = layout.shim(&alias);
            if platform::resolve_shim(&shim).is_some_and(|t| t == wanted)
                && platform::shim_env_of(&shim) == env
                && shim_is_rendered(&shim, &wanted, &env)
            {
                continue;
            }
            match std::fs::symlink_metadata(&shim) {
                Err(_) => {}
                Ok(_)
                    if platform::resolve_shim(&shim).is_some_and(|t| {
                        crate::ops::store_build_of(&layout.prefix, &t).is_some()
                    }) => {}
                Ok(_) => continue,
            }
            layout.ensure_dir(&layout.bin_dir())?;
            platform::install_shim_env(&target_bin, tool, &shim, &env)?;
        }
    }
    prune_stale_shims(layout, build_dir, tools, aliases);
    Ok(())
}

/// The primary shims of one build whose exported environment is not the one that build declares ([`crate::shim_env::read_sidecar`], design S7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvDrift {
    /// What the build's sidecar declares — never empty.
    pub(crate) declared: crate::shim_env::ShimEnv,
    /// Each drifted primary: its `bin/` path and the target it forwards to, as spelled on disk.
    pub(crate) shims: Vec<(std::path::PathBuf, std::path::PathBuf)>,
}

/// The one drift predicate.
pub(crate) fn shim_env_drift(
    layout: &Layout,
    build_dir: &Path,
    tools: &[ToolName],
) -> Option<EnvDrift> {
    let declared = crate::shim_env::read_sidecar(build_dir);
    if declared.is_empty() {
        return None;
    }
    let shims: Vec<_> = tools
        .iter()
        .map(|tool| layout.shim(tool))
        .filter_map(|shim| {
            let target = platform::resolve_shim(&shim).filter(|t| t.starts_with(build_dir))?;
            (platform::shim_env_of(&shim) != declared).then_some((shim, target))
        })
        .collect();
    (!shims.is_empty()).then_some(EnvDrift { declared, shims })
}

/// Re-lay every primary shim of `builds` (build dir, active tools) that [`shim_env_drift`] finds exporting other than its build declares, with exactly the declared environment.
pub(crate) fn reassert_shim_env(layout: &Layout, builds: &[(&Path, &[ToolName])]) {
    let publication = crate::lay::PUBLICATION_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = Vec::new();
    let mut said = Vec::new();
    for (build_dir, tools) in builds {
        let Some(drift) = shim_env_drift(layout, build_dir, tools) else {
            continue;
        };
        for (shim, target) in &drift.shims {
            match platform::shim_executable_to_env(shim, target, &drift.declared) {
                Ok(file) => {
                    files.push(file);
                    said.push(drift.declared.spelled());
                }
                Err(e) => eprintln!(
                    "atpkg: {} lacks its build's {} export; re-render failed ({e}); \
                     retried next pass",
                    shim.display(),
                    drift.declared.spelled()
                ),
            }
        }
    }
    if files.is_empty() {
        return;
    }
    let paths: Vec<std::path::PathBuf> = files.iter().map(|f| f.path.clone()).collect();
    match files
        .iter()
        .try_for_each(|file| crate::lay::write_in_publication_turn(file, &publication))
    {
        Ok(()) => {
            for (path, env) in paths.iter().zip(&said) {
                println!(
                    "atpkg: {} re-laid — it exports {env} again, as its build declares",
                    path.display()
                );
            }
        }
        Err(e) => eprintln!(
            "atpkg: {} shim(s) lacking their build's exports not re-laid ({e}); retried next \
             pass",
            paths.len()
        ),
    }
}

/// Store build that a `bin/` entry `name` at `shim` forwards into when [`ToolName::from_shim_file`] refuses `name` ([`crate::ops::store_build_of`]).
pub(crate) fn refused_name_shim(layout: &Layout, name: &str, shim: &Path) -> Option<(String, u64)> {
    if ToolName::from_shim_file(name).is_some() {
        return None;
    }
    crate::platform::resolve_shim(shim)
        .and_then(|target| crate::ops::store_build_of(&layout.prefix, &target))
}

/// The `bin/` shims [`refused_name_shim`] names (`program`: only those into that program's
/// store), as (file name, path), by name.
fn refused_name_entries(layout: &Layout, program: Option<&str>) -> Vec<(String, PathBuf)> {
    let Ok(entries) = crate::ops::read_bin_dir(layout) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| !crate::lay::in_flight_here(&entry.file_name()))
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            let (owner, _) = refused_name_shim(layout, &name, &entry.path())?;
            program
                .is_none_or(|want| owner == want)
                .then(|| (name, entry.path()))
        })
        .collect();
    found.sort();
    found
}

/// The file names of the `bin/` shims [`refused_name_shim`] names, by name: what [`remove_refused_name_shims`] would remove.
pub(crate) fn refused_name_shims(layout: &Layout) -> Vec<String> {
    refused_name_entries(layout, None)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Remove every `bin/` shim [`refused_name_shim`] names (`program`: only those into that program's store), and return the file names removed.
pub(crate) fn remove_refused_name_shims(layout: &Layout, program: Option<&str>) -> Vec<String> {
    let _publication = crate::lay::PUBLICATION_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    refused_name_entries(layout, program)
        .into_iter()
        .filter(|(_, path)| std::fs::remove_file(path).is_ok())
        .map(|(name, _)| name)
        .collect()
}

/// Remove `bin/` shims this program owns that point at a different build (tools a newer
/// build dropped from `exposes`), and every alias of this program's no longer wanted
/// (`aliases` is [`Aliases::Off`], or its base is neither in `installed` nor another tool of
/// this build).
fn prune_stale_shims(layout: &Layout, build_dir: &Path, installed: &[ToolName], aliases: Aliases) {
    let _publication = crate::lay::PUBLICATION_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((program, build)) = crate::ops::store_build_of(&layout.prefix, build_dir) else {
        // not a store build dir of ours — nothing to prune
        return;
    };
    let Ok(entries) = crate::ops::read_bin_dir(layout) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if crate::lay::in_flight_here(&name) {
            continue;
        }
        let Some(name) = name.to_str() else { continue };
        let Some(tool) = crate::store::ToolName::from_shim_file(name) else {
            if refused_name_shim(layout, name, &entry.path()).is_some_and(|(p, _)| p == program) {
                let _ = std::fs::remove_file(entry.path());
            }
            continue;
        };
        if installed.contains(&tool) {
            continue;
        }
        let alias_wanted = aliases == Aliases::Alab
            && tool.alias_base().is_some_and(|base| {
                installed.contains(&base)
                    || crate::platform::resolve_shim(&layout.shim(&base)).is_some_and(|t| {
                        crate::ops::store_build_of(&layout.prefix, &t)
                            .is_some_and(|(p, b)| p == program && b == build)
                    })
            });
        if alias_wanted {
            continue;
        }
        let Some(target) = crate::platform::resolve_shim(&entry.path()) else {
            // not a shim we can resolve (tombstone, real file, dangling)
            continue;
        };
        if crate::ops::store_build_of(&layout.prefix, &target)
            .is_some_and(|(p, b)| p == program && (b != build || tool.is_alias()))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Best-effort undo of [`activate_build`] and a partial [`install_tools`] pass, for a build about to be discarded after activation (`flow::abort_activated_install`).
pub(crate) fn undo_activation(layout: &Layout, build_dir: &Path) {
    if let Some((program, _)) = crate::ops::store_build_of(&layout.prefix, build_dir) {
        let own = layout.program_current(&program);
        if std::fs::read_link(&own).is_ok_and(|t| t == build_dir) {
            platform::remove_link(&own);
        }
    }
    {
        let _publication = crate::lay::PUBLICATION_TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(entries) = crate::ops::read_bin_dir(layout) {
            for e in entries.flatten() {
                if crate::platform::resolve_shim(&e.path())
                    .is_some_and(|t| t.starts_with(build_dir))
                {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    sweep_agents_dir(layout);
}

/// Install a failing tombstone shim at `bin/<tool>`.
pub fn install_tombstone_shim(layout: &Layout, tool: &ToolName) -> io::Result<()> {
    let bin = layout.bin_dir();
    layout.ensure_dir(&bin)?;
    let shim = layout.shim(tool);

    let message = format!(
        "atpkg: {} was yanked/revoked — run `aterm pkg update`",
        tool.as_str()
    );
    platform::install_tombstone_shim(&shim, &message)?;
    #[cfg(all(test, unix))]
    agents_checkpoint::at(layout, agents_checkpoint::Event::Written(tool.as_str()));
    sweep_agents_dir(layout);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// The permission bits of an existing directory, for the prefix-shape assertions.
    #[cfg(unix)]
    fn mode_of(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// A prefix of the SYSTEM shape: a scratch prefix this thread's
    /// [`Layout::is_system_prefix`] answers yes for ([`crate::store::system_shape`]).
    #[cfg(unix)]
    fn system_prefix_fixture(label: &str) -> crate::testdir::ScratchLayout {
        let layout = temp_prefix(label);
        crate::store::system_shape::force(&layout.prefix);
        layout
    }

    fn temp_prefix(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-act-{label}-"))
    }

    fn tool(name: &str) -> ToolName {
        ToolName::new(name).unwrap()
    }

    /// `bin/<name>` for a name the test knows is admissible.
    fn shim_of(layout: &Layout, name: &str) -> PathBuf {
        layout.shim(&tool(name))
    }

    /// The `bin/` path a shim for `name` would occupy, for the refusal tests, where
    /// `ToolName::new` is `None` and `Layout::shim` cannot name it.
    fn refused_shim_path(layout: &Layout, name: &str) -> PathBuf {
        layout
            .bin_dir()
            .join(format!("{name}{}", crate::platform::SHIM_SUFFIX))
    }

    fn make_build(layout: &Layout, program: &str, build: u64, bins: &[&str]) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        for b in bins {
            let name = format!("{b}{}", crate::platform::EXE_SUFFIX);
            std::fs::write(dir.join("bin").join(name), b"#!/bin/true\n").unwrap();
        }
        dir
    }

    #[test]
    fn a_dropped_tool_leaves_no_stale_shim_pointing_at_the_prior_build() {
        let layout = temp_prefix("stale-shim");
        let b18 = make_build(&layout, "ay", 18, &["ay", "aylint"]);
        install_shims(&layout, &b18, &["ay".into(), "aylint".into()], Aliases::Off).unwrap();
        activate_build(&layout, &b18).unwrap();

        let b19 = make_build(&layout, "ay", 19, &["ay"]);
        install_shims(&layout, &b19, &["ay".into()], Aliases::Off).unwrap();
        activate_build(&layout, &b19).unwrap();

        assert!(
            !shim_of(&layout, "aylint").exists(),
            "a tool the new build dropped must not keep a shim into the prior build"
        );
        assert_eq!(
            crate::ops::active_builds(&layout).get("ay"),
            Some(&19),
            "the active build must be the one just activated"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// [`prune_stale_shims`] is the one predicate here that deletes from the user's `PATH`:
    /// this pins exactly which shapes of a real `bin/` a prune of `ay@19` removes.
    #[cfg(unix)]
    #[test]
    fn the_prune_deletes_exactly_the_stale_and_leaves_everything_else() {
        let layout = temp_prefix("prune-pin");
        let b18 = make_build(&layout, "ay", 18, &["ay", "aylint"]);
        let b19 = make_build(&layout, "ay", 19, &["ay", "aydoc"]);
        let n7 = make_build(&layout, "ny", 7, &["ny"]);
        layout.ensure_dir(&layout.bin_dir()).unwrap();
        let lay = |shim: &str, build: &PathBuf, target: &str| {
            platform::install_shim_env(
                &build.join("bin"),
                &tool(target),
                &layout.shim(&tool(shim)),
                &crate::shim_env::ShimEnv::NONE,
            )
            .unwrap();
        };
        lay("ay", &b19, "ay");
        lay("aydoc", &b19, "aydoc");
        lay("aylint", &b18, "aylint");
        lay("alab-ay", &b19, "ay");
        lay("alab-aydoc", &b19, "aydoc");
        lay("alab-aylint", &b18, "aylint");
        lay("ny", &n7, "ny");
        lay("alab-ny", &n7, "ny");
        let checkout = layout.prefix.join("checkout/store/ay/18/bin");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("ay"), b"#!/bin/true\n").unwrap();
        std::os::unix::fs::symlink(checkout.join("ay"), shim_of(&layout, "devlink")).unwrap();
        std::fs::write(layout.bin_dir().join("hand"), b"mine\n").unwrap();
        std::fs::write(refused_shim_path(&layout, "sudo"), b"mine\n").unwrap();
        install_tombstone_shim(&layout, &tool("tombstoned")).unwrap();
        let in_flight = crate::lay::staged_at(&shim_of(&layout, "aylint"), "aylint", u64::MAX);
        let orphaned = layout.bin_dir().join(".aylint.lay-1-7");
        for temp in [&in_flight, &orphaned] {
            platform::install_shim_env(
                &b18.join("bin"),
                &tool("aylint"),
                temp,
                &crate::shim_env::ShimEnv::NONE,
            )
            .unwrap();
        }

        prune_stale_shims(&layout, &b19, &[tool("ay")], Aliases::Alab);

        assert!(
            in_flight.exists(),
            "an in-flight lay is its writer's to publish"
        );
        std::fs::remove_file(&in_flight).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(layout.bin_dir())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "alab-ay",
                "alab-aydoc",
                "alab-ny",
                "ay",
                "aydoc",
                "devlink",
                "hand",
                "ny",
                "sudo",
                "tombstoned",
            ],
            "only `aylint` (stale at build 18) and its alias go; a dev link, another \
             program's shims, a hand-made file, a refused name and a tombstone all stay"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    #[test]
    fn re_shimming_the_same_build_prunes_nothing() {
        let layout = temp_prefix("prune-idem");
        let b18 = make_build(&layout, "ay", 18, &["ay", "aylint"]);
        install_shims(&layout, &b18, &["ay".into(), "aylint".into()], Aliases::Off).unwrap();
        install_shims(&layout, &b18, &["ay".into(), "aylint".into()], Aliases::Off).unwrap();
        assert!(shim_of(&layout, "ay").exists());
        assert!(
            shim_of(&layout, "aylint").exists(),
            "same build ⇒ nothing is stale"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    #[test]
    fn activate_build_points_current_at_build_and_re_flips() {
        let layout = temp_prefix("chan");
        let b18 = make_build(&layout, "ay", 18, &["ay"]);
        activate_build(&layout, &b18).unwrap();
        let cur = layout.program_current("ay");
        assert_eq!(std::fs::read_link(&cur).unwrap(), b18);
        assert!(std::fs::metadata(&cur).unwrap().is_dir());

        let b19 = make_build(&layout, "ay", 19, &["ay"]);
        activate_build(&layout, &b19).unwrap();
        assert_eq!(std::fs::read_link(&cur).unwrap(), b19);
        let leftovers: Vec<_> = std::fs::read_dir(cur.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "no temp symlink should remain");
        assert!(!layout.prefix.join("channels").exists());
        let elsewhere = layout.prefix.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert_eq!(
            activate_build(&layout, &elsewhere).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(std::fs::read_link(&cur).unwrap(), b19);
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// Two programs activated in turn each keep their own `current`, the per-program
    /// witness `gc::live_builds` reads.
    #[test]
    fn two_programs_on_one_channel_each_keep_their_own_current() {
        let layout = temp_prefix("two-progs");
        let ay19 = make_build(&layout, "ay", 19, &["ay"]);
        activate_build(&layout, &ay19).unwrap();
        let ny7 = make_build(&layout, "ny", 7, &["ny"]);
        activate_build(&layout, &ny7).unwrap();

        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            ay19
        );
        assert_eq!(
            std::fs::read_link(layout.program_current("ny")).unwrap(),
            ny7
        );

        let ay20 = make_build(&layout, "ay", 20, &["ay"]);
        activate_build(&layout, &ay20).unwrap();
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            ay20
        );
        let dir = layout.build_dir("ay", 19);
        crate::store::mark_build_ready(&dir).unwrap();
        assert_eq!(
            crate::ops::list_installed(&layout),
            vec![("ay".to_string(), 19)]
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    #[test]
    fn install_shims_creates_allowed_and_refuses_sensitive() {
        let layout = temp_prefix("shims");
        let build = make_build(&layout, "ay", 18, &["ay", "sudo", "ny"]);
        let exposes = vec!["ay".to_string(), "sudo".to_string(), "ny".to_string()];
        let refused = install_shims(&layout, &build, &exposes, Aliases::Off).unwrap();
        assert_eq!(refused, vec!["sudo".to_string()]);
        let sudo = refused_shim_path(&layout, "sudo");
        assert!(!sudo.exists() && std::fs::symlink_metadata(&sudo).is_err());
        for name in ["ay", "ny"] {
            let t = tool(name);
            let shim = layout.shim(&t);
            let target = crate::platform::resolve_shim(&shim).unwrap();
            assert_eq!(target, build.join("bin").join(t.exe_file()));
            assert!(
                std::fs::metadata(&target).unwrap().is_file(),
                "{name} shim resolves to the binary"
            );
        }
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// aterm-spec's store discovery (`aterm_spec::verify::resolve_store_shim`) mirrors this
    /// crate's shim format rather than depending on it, so every shape is laid through
    /// atpkg's writer and read back through aterm-spec's reader:
    /// * the plain stub, one with exports, and one routed through an exec root resolve to
    ///   the store file (the first `exec '` line);
    /// * a tombstone, a pending stub and a stub whose build was reclaimed read as absent:
    ///   the probe runs before PATH, so any of them answering would shadow a working tool.
    #[cfg(unix)]
    #[test]
    fn aterm_spec_discovery_reads_every_shim_atpkg_lays() {
        let layout = temp_prefix("spec-probe");
        let build = make_build(&layout, "ty", 3007, &["ty"]);
        let target = build.join("bin").join("ty");
        let want = std::fs::canonicalize(&target).unwrap();
        let shim = shim_of(&layout, "ty");
        let read = || aterm_spec::verify::resolve_store_shim(&shim);

        install_shims(&layout, &build, &["ty".to_string()], Aliases::Off).unwrap();
        assert_eq!(read(), Some(want.clone()), "the plain stub");

        let env = crate::shim_env::ShimEnv::admit(&["TY_MODE=1".to_string()]).unwrap();
        crate::platform::install_shim_env(&build.join("bin"), &tool("ty"), &shim, &env).unwrap();
        assert!(
            std::fs::read_to_string(&shim)
                .unwrap()
                .contains("export TY_MODE="),
            "fixture: the stub exports"
        );
        assert_eq!(read(), Some(want.clone()), "a stub with exports");

        let route = layout
            .prefix
            .join("compat")
            .join("ty")
            .join("bin")
            .join("ty");
        let routed = crate::platform::sh_shim_content_routed(
            &target,
            &crate::shim_env::ShimEnv::NONE,
            Some(&route),
        );
        crate::lay::write_in_process(&crate::lay::Executable::new(&shim, routed.as_bytes()))
            .unwrap();
        assert_eq!(read(), Some(want.clone()), "a routed stub");

        install_tombstone_shim(&layout, &tool("ty")).unwrap();
        assert_eq!(read(), None, "a tombstone is absent");

        std::fs::remove_file(&shim).unwrap();
        crate::stub::write_pending_stub(&layout, &tool("ty")).unwrap();
        assert!(
            crate::stub::pending_stub_exists(&layout, "ty"),
            "fixture: a pending stub"
        );
        assert_eq!(read(), None, "a pending-program stub is absent");

        install_shims(&layout, &build, &["ty".to_string()], Aliases::Off).unwrap();
        std::fs::remove_dir_all(&build).unwrap();
        assert_eq!(read(), None, "a stub whose build is gone is absent");
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    #[test]
    fn tombstone_shim_disables_a_revoked_tool_and_refuses_sensitive_names() {
        let layout = temp_prefix("tomb");
        let build = make_build(&layout, "ay", 18, &["ay"]);
        let exposes = vec!["ay".to_string()];
        install_shims(&layout, &build, &exposes, Aliases::Off).unwrap();
        let shim = shim_of(&layout, "ay");
        assert!(
            crate::platform::resolve_shim(&shim).is_some(),
            "live shim forwards into the build"
        );

        install_tombstone_shim(&layout, &tool("ay")).unwrap();
        let meta = std::fs::symlink_metadata(&shim).unwrap();
        assert!(
            meta.file_type().is_file(),
            "tombstone is a regular file, not the old symlink"
        );
        assert!(
            crate::platform::resolve_shim(&shim).is_none(),
            "tombstone no longer forwards anywhere"
        );
        #[cfg(unix)]
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "tombstone is executable"
        );

        let out = std::process::Command::new(&shim).output().unwrap();
        assert!(!out.status.success(), "tombstone shim exits nonzero");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("ay") && err.contains("yanked/revoked"),
            "stderr: {err}"
        );
        assert!(
            out.stdout.is_empty(),
            "the notice goes to stderr, not stdout"
        );

        assert!(ToolName::new("sudo").is_none());
        assert!(std::fs::symlink_metadata(refused_shim_path(&layout, "sudo")).is_err());

        let leftovers: Vec<_> = std::fs::read_dir(layout.bin_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tomb-"))
            .collect();
        assert!(leftovers.is_empty(), "no temp tombstone should remain");
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// `bin/`'s mode follows the prefix shape: a `$HOME` prefix keeps it `0700`, and every
    /// activation entry point leaves it at the mode this layout gives its own directories.
    #[cfg(unix)]
    #[test]
    fn a_home_shaped_prefix_keeps_bin_private() {
        let layout = temp_prefix("mode-home");
        assert!(
            !layout.is_system_prefix(),
            "a user-owned temp prefix is never the system shape"
        );
        let b18 = make_build(&layout, "ay", 18, &["ay"]);
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Off).unwrap();
        assert_eq!(mode_of(&layout.bin_dir()), 0o700, "bin/ stays private");

        activate_build(&layout, &b18).unwrap();

        install_tombstone_shim(&layout, &tool("ay")).unwrap();
        let probe = layout.prefix.join("mode-probe");
        layout.ensure_dir(&probe).unwrap();
        assert_eq!(
            mode_of(&layout.bin_dir()),
            mode_of(&probe),
            "bin/ carries this layout's own dir mode, not a hardcoded 0700"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The regression, and the only assertion that can tell the two shapes apart: under a
    /// root-owned SYSTEM prefix, activation must publish `bin/` at
    /// `0755`.
    #[cfg(unix)]
    #[test]
    fn a_system_shaped_prefix_publishes_bin_traversable() {
        let layout = system_prefix_fixture("mode-sys");
        assert!(layout.is_system_prefix(), "the fixture is the system shape");
        let b18 = make_build(&layout, "ay", 18, &["ay"]);
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Off).unwrap();
        assert_eq!(
            mode_of(&layout.bin_dir()),
            0o755,
            "a system prefix's bin/ must be traversable by every user, not root-only"
        );

        activate_build(&layout, &b18).unwrap();

        install_tombstone_shim(&layout, &tool("ay")).unwrap();
        assert_eq!(
            mode_of(&layout.bin_dir()),
            0o755,
            "the revoke path must not re-harden the shared bin/ either"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// `undo_activation` removes exactly the doomed build's `current` link and shims; a
    /// bystander program activated afterwards keeps both.
    #[test]
    fn undo_activation_unwinds_only_the_doomed_build() {
        let layout = temp_prefix("undo");
        let doomed = make_build(&layout, "ay", 19, &["ay"]);
        activate_build(&layout, &doomed).unwrap();
        install_tools(&layout, &doomed, &[tool("ay")], Aliases::Off).unwrap();
        let other = make_build(&layout, "ny", 7, &["ny"]);
        activate_build(&layout, &other).unwrap();
        install_tools(&layout, &other, &[tool("ny")], Aliases::Off).unwrap();

        undo_activation(&layout, &doomed);

        assert!(
            std::fs::symlink_metadata(layout.program_current("ay")).is_err(),
            "ay's witness link is removed"
        );
        assert!(
            crate::platform::resolve_shim(&layout.shim(&tool("ay"))).is_none(),
            "ay's shim is removed"
        );
        assert_eq!(
            std::fs::read_link(layout.program_current("ny")).expect("ny's witness survives"),
            other
        );
        assert!(
            crate::platform::resolve_shim(&layout.shim(&tool("ny")))
                .is_some_and(|t| t.starts_with(&other)),
            "ny's shim survives"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The shim environment (design S7) rides on every shim `install_tools_env` lays,
    /// primary and alias, and an env-carrying shim resolves, prunes, sweeps, undoes and
    /// uninstalls like a plain one; a re-lay without env leaves no exports.
    #[test]
    fn env_shims_resolve_prune_undo_and_uninstall_like_plain_ones() {
        let layout = temp_prefix("env-shims");
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let b18 = make_build(&layout, "claude", 18, &["claude", "claudex"]);
        install_tools_env(
            &layout,
            &b18,
            &[tool("claude"), tool("claudex")],
            Aliases::Alab,
            &env,
        )
        .unwrap();
        for t in ["claude", "claudex", "alab-claude", "alab-claudex"] {
            let shim = shim_of(&layout, t);
            let base = t.strip_prefix("alab-").unwrap_or(t);
            assert_eq!(
                crate::platform::resolve_shim(&shim).unwrap(),
                b18.join("bin").join(tool(base).exe_file()),
                "{t} resolves through the wrapper"
            );
            assert_eq!(
                crate::platform::shim_env_of(&shim),
                env,
                "{t} exports the env (primary and alias alike)"
            );
        }
        assert_eq!(
            crate::ops::active_builds(&layout).get("claude"),
            Some(&18),
            "active_builds reads the wrapper like any shim"
        );
        assert_eq!(
            crate::ops::active_tools(&layout, "claude", 18),
            vec![tool("claude"), tool("claudex")]
        );
        let b19 = make_build(&layout, "claude", 19, &["claude"]);
        install_tools_env(&layout, &b19, &[tool("claude")], Aliases::Alab, &env).unwrap();
        for t in ["claudex", "alab-claudex"] {
            assert!(
                std::fs::symlink_metadata(shim_of(&layout, t)).is_err(),
                "{t}: pruned with its build"
            );
        }
        assert!(
            crate::platform::resolve_shim(&shim_of(&layout, "claude"))
                .is_some_and(|p| p.starts_with(&b19))
        );
        install_tools_env(&layout, &b19, &[tool("claude")], Aliases::Off, &env).unwrap();
        assert!(std::fs::symlink_metadata(shim_of(&layout, "alab-claude")).is_err());
        install_tools(&layout, &b19, &[tool("claude")], Aliases::Off).unwrap();
        assert_eq!(
            crate::platform::shim_env_of(&shim_of(&layout, "claude")),
            crate::shim_env::ShimEnv::NONE
        );
        install_tools_env(&layout, &b19, &[tool("claude")], Aliases::Off, &env).unwrap();
        assert_eq!(
            crate::platform::shim_env_of(&shim_of(&layout, "claude")),
            env
        );
        activate_build(&layout, &b19).unwrap();
        undo_activation(&layout, &b19);
        assert!(crate::platform::resolve_shim(&shim_of(&layout, "claude")).is_none());
        install_tools_env(&layout, &b19, &[tool("claude")], Aliases::Off, &env).unwrap();
        activate_build(&layout, &b19).unwrap();
        crate::store::mark_build_ready(&b19).unwrap();
        assert!(crate::which(&layout, "claude").is_some());
        crate::ops::uninstall(&layout, "claude").unwrap();
        assert!(
            crate::which(&layout, "claude").is_none(),
            "the wrapper is swept"
        );
        assert!(std::fs::symlink_metadata(shim_of(&layout, "claude")).is_err());
        assert!(!layout.prefix.join("store").join("claude").exists());
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// A partial lay keeps the aliases of the tools it did not name: `atpkg unlink`'s
    /// restore (`cli::restore_installed_shims`) lays only the names the dev link owned — one,
    /// by `cli::default_link_bins` — at the build the other tools are live at.
    #[test]
    fn a_partial_lay_keeps_the_aliases_of_this_builds_other_tools() {
        let layout = temp_prefix("partial-lay-aliases");
        let b18 = make_build(&layout, "ay", 18, &["ay", "aylint"]);
        install_tools(&layout, &b18, &[tool("ay"), tool("aylint")], Aliases::Alab).unwrap();
        for t in ["ay", "aylint", "alab-ay", "alab-aylint"] {
            assert!(
                crate::platform::resolve_shim(&shim_of(&layout, t))
                    .is_some_and(|p| p.starts_with(&b18)),
                "{t}: laid at this build"
            );
        }
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Alab).unwrap();
        for t in ["ay", "aylint", "alab-ay", "alab-aylint"] {
            assert!(
                crate::platform::resolve_shim(&shim_of(&layout, t))
                    .is_some_and(|p| p.starts_with(&b18)),
                "{t}: survives a lay that did not name it"
            );
        }
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Off).unwrap();
        for t in ["alab-ay", "alab-aylint"] {
            assert!(
                std::fs::symlink_metadata(shim_of(&layout, t)).is_err(),
                "{t}: swept by Aliases::Off"
            );
        }
        install_tools(&layout, &b18, &[tool("ay"), tool("aylint")], Aliases::Alab).unwrap();
        let b19 = make_build(&layout, "ay", 19, &["ay"]);
        install_tools(&layout, &b19, &[tool("ay")], Aliases::Alab).unwrap();
        for t in ["aylint", "alab-aylint"] {
            assert!(
                std::fs::symlink_metadata(shim_of(&layout, t)).is_err(),
                "{t}: pruned with the build that dropped it"
            );
        }
        assert!(
            crate::platform::resolve_shim(&shim_of(&layout, "alab-ay"))
                .is_some_and(|p| p.starts_with(&b19)),
            "the live alias moved to the new build"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// `reconcile_aliases` mirrors the primary's environment onto the alias it lays,
    /// rewrites an alias whose env drifted from its primary's, and leaves an agreeing one.
    #[test]
    fn reconcile_aliases_mirrors_the_primary_env() {
        let layout = temp_prefix("env-alias-reconcile");
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let b18 = make_build(&layout, "ay", 18, &["ay"]);
        install_tools_env(&layout, &b18, &[tool("ay")], Aliases::Off, &env).unwrap();
        assert!(std::fs::symlink_metadata(shim_of(&layout, "alab-ay")).is_err());
        reconcile_aliases(&layout, &b18, &[tool("ay")], Aliases::Alab).unwrap();
        let alias = shim_of(&layout, "alab-ay");
        assert_eq!(
            crate::platform::resolve_shim(&alias),
            crate::platform::resolve_shim(&shim_of(&layout, "ay"))
        );
        assert_eq!(
            crate::platform::shim_env_of(&alias),
            env,
            "the alias exports what the primary does"
        );
        let before = std::fs::metadata(&alias).unwrap().modified().unwrap();
        reconcile_aliases(&layout, &b18, &[tool("ay")], Aliases::Alab).unwrap();
        assert_eq!(
            std::fs::metadata(&alias).unwrap().modified().unwrap(),
            before
        );
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Off).unwrap();
        reconcile_aliases(&layout, &b18, &[tool("ay")], Aliases::Alab).unwrap();
        assert_eq!(
            crate::platform::shim_env_of(&alias),
            crate::shim_env::ShimEnv::NONE
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// "Already right" means the bytes too: an alias and a twin that forward and export
    /// right but differ from the render (in production, the exec-root guard a plain shim
    /// lacks, `crate::compat`) are re-laid, and a second reconcile writes neither.
    #[cfg(unix)]
    #[test]
    fn a_reconcile_relays_an_alias_or_twin_whose_bytes_differ_and_then_writes_nothing() {
        let layout = temp_prefix("rendered-bytes");
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let claude = tool("claude");
        let c1 = make_build(&layout, "claude", 2026091001, &["claude"]);
        install_tools_env(
            &layout,
            &c1,
            std::slice::from_ref(&claude),
            Aliases::Alab,
            &env,
        )
        .unwrap();
        let alias = layout.shim(&tool("alab-claude"));
        let twin = layout.agent_shim(&claude);
        let rendered: Vec<Vec<u8>> = [&alias, &twin]
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        for p in [&alias, &twin] {
            let mut body = std::fs::read(p).unwrap();
            body.extend_from_slice(b"# laid by another writer\n");
            std::fs::write(p, body).unwrap();
            assert_eq!(
                platform::resolve_shim(p),
                platform::resolve_shim(&layout.shim(&claude))
            );
            assert_eq!(platform::shim_env_of(p), env, "target and env still agree");
            let target = platform::resolve_shim(p).unwrap();
            assert!(!shim_is_rendered(p, &target, &env), "{}", p.display());
        }
        let target = platform::resolve_shim(&layout.shim(&claude)).unwrap();
        let link = layout.prefix.join("old-symlink-shim");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!shim_is_rendered(&link, &target, &env));
        reconcile_aliases(&layout, &c1, std::slice::from_ref(&claude), Aliases::Alab).unwrap();
        reconcile_agents(&layout);
        for (p, want) in [&alias, &twin].iter().zip(&rendered) {
            assert_eq!(&std::fs::read(p).unwrap(), want, "{} re-laid", p.display());
        }
        assert!(shim_is_rendered(&alias, &target, &env));
        let stamp = |p: &Path| std::fs::symlink_metadata(p).unwrap().modified().unwrap();
        let before = (stamp(&alias), stamp(&twin));
        std::thread::sleep(std::time::Duration::from_millis(20));
        reconcile_aliases(&layout, &c1, std::slice::from_ref(&claude), Aliases::Alab).unwrap();
        reconcile_agents(&layout);
        assert_eq!(stamp(&alias), before.0, "a second pass writes no alias");
        assert_eq!(
            stamp(&twin),
            before.1,
            "a second pass writes no twin, tagged or not"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// Installing an agent program lays `agents/<tool>` beside `bin/<tool>` (same target,
    /// same env); nothing else lands in `agents/`, and the twin follows its primary across an
    /// update, a tombstone, an undo and an uninstall.
    #[cfg(unix)]
    #[test]
    fn an_agent_program_gets_a_front_of_path_twin_that_follows_its_primary() {
        let layout = temp_prefix("agents-twin");
        let claude = tool("claude");
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let c1 = make_build(&layout, "claude", 2026091001, &["claude"]);
        install_tools_env(
            &layout,
            &c1,
            std::slice::from_ref(&claude),
            Aliases::Off,
            &env,
        )
        .unwrap();
        let twin = layout.agent_shim(&claude);
        assert_eq!(
            platform::resolve_shim(&twin),
            platform::resolve_shim(&layout.shim(&claude)),
            "the twin forwards exactly where the primary does"
        );
        assert_eq!(
            platform::shim_env_of(&twin),
            env,
            "and exports exactly what the primary exports"
        );
        let ay = make_build(&layout, "ay", 18, &["ay"]);
        install_shims(&layout, &ay, &["ay".into()], Aliases::Alab).unwrap();
        assert!(!layout.agent_shim(&tool("ay")).exists());
        std::fs::write(layout.agents_dir().join("mine"), b"#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink("/usr/bin/true", layout.agents_dir().join("codex")).unwrap();
        let in_flight = crate::lay::staged_at(&twin, "claude", u64::MAX);
        let orphaned = layout.agents_dir().join(".claude.lay-1-7");
        for temp in [&in_flight, &orphaned] {
            std::fs::write(temp, b"#!/bin/sh\n").unwrap();
        }
        reconcile_agents(&layout);
        assert!(
            in_flight.exists(),
            "an in-flight lay is its writer's to publish"
        );
        assert!(!orphaned.exists(), "another process's leftover temp");
        std::fs::remove_file(&in_flight).unwrap();
        assert!(
            !layout.agents_dir().join("mine").exists(),
            "not an agent name"
        );
        assert!(
            !layout.agents_dir().join("codex").exists(),
            "not into the store"
        );
        assert!(twin.exists(), "the live twin survives the sweep");
        let c2 = make_build(&layout, "claude", 2026091101, &["claude"]);
        install_tools_env(
            &layout,
            &c2,
            std::slice::from_ref(&claude),
            Aliases::Off,
            &env,
        )
        .unwrap();
        assert!(
            platform::resolve_shim(&twin).is_some_and(|t| t.starts_with(&c2)),
            "twin moved with the primary"
        );
        std::fs::remove_file(&twin).unwrap();
        reconcile_agents(&layout);
        assert_eq!(
            platform::resolve_shim(&twin),
            platform::resolve_shim(&layout.shim(&claude))
        );
        install_tombstone_shim(&layout, &claude).unwrap();
        assert!(!twin.exists(), "no twin over a tombstone");
        install_tools_env(
            &layout,
            &c2,
            std::slice::from_ref(&claude),
            Aliases::Off,
            &env,
        )
        .unwrap();
        assert!(twin.exists());
        undo_activation(&layout, &c2);
        assert!(!twin.exists(), "undo sweeps the twin of the doomed build");
        install_tools_env(
            &layout,
            &c2,
            std::slice::from_ref(&claude),
            Aliases::Off,
            &env,
        )
        .unwrap();
        assert!(twin.exists());
        crate::ops::uninstall(&layout, "claude").unwrap();
        assert!(!twin.exists(), "uninstall removes the twin");
        assert!(
            std::fs::read_dir(layout.agents_dir())
                .map(|d| d.count() == 0)
                .unwrap_or(true),
            "agents/ is empty after the only agent program is gone"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// THE TWIN CARRIES NO LANDING PRELUDE, and `bin/` carries the same bytes (outside aterm
    /// `bin/claude` IS the managed name).
    #[cfg(unix)]
    #[test]
    fn the_agents_twin_carries_the_self_update_block_and_no_landing_prelude() {
        let layout = temp_prefix("agents-twin-prelude");
        let claude = tool("claude");
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
        let c1 = make_build(&layout, "claude", 2026091601, &["claude"]);
        install_tools_env(
            &layout,
            &c1,
            std::slice::from_ref(&claude),
            Aliases::Off,
            &env,
        )
        .unwrap();
        let twin = layout.agent_shim(&claude);
        let primary = layout.shim(&claude);
        let twin_body = std::fs::read_to_string(&twin).unwrap();
        let primary_body = std::fs::read_to_string(&primary).unwrap();
        let marker = layout.landing_dir().join(claude.as_str());
        assert!(
            !twin_body.contains("__landing") && !twin_body.contains("if [ -f "),
            "no landing prelude: {twin_body}"
        );
        assert!(
            !twin_body.contains(&marker.display().to_string()),
            "the twin never names a marker: {twin_body}"
        );
        assert_eq!(
            primary_body, twin_body,
            "bin/ carries the twin's block: outside aterm it is the managed name"
        );
        assert_eq!(
            platform::resolve_shim(&twin),
            platform::resolve_shim(&primary),
            "the twin resolves where the primary does"
        );
        assert_eq!(
            crate::platform::parse_sh_shim_target(&twin_body),
            Some(c1.join("bin/claude")),
            "the block never matches the target parser"
        );
        assert_eq!(
            platform::shim_env_of(&twin),
            env,
            "the exports are read as before"
        );
        assert!(!crate::stub::is_pending_stub(&twin), "not a pending stub");
        let before = std::fs::metadata(&twin).unwrap().modified().unwrap();
        reconcile_agents(&layout);
        assert_eq!(
            std::fs::metadata(&twin).unwrap().modified().unwrap(),
            before
        );
        assert_eq!(std::fs::read_to_string(&twin).unwrap(), twin_body);
        let atpkg = crate::stub::co_located_atpkg_path();
        let landing = platform::sh_landing_prelude("claude", &layout.prefix, &marker, &atpkg);
        let mut block_then_landing = platform::sh_selfupdate_prelude(
            "claude",
            &layout.prefix,
            &atpkg,
            crate::selfupdate::verbs_of("claude"),
        );
        block_then_landing.push_str(&landing);
        for (what, older) in [
            (
                "before any block",
                platform::sh_shim_content_twin(&c1.join("bin/claude"), &env, None, ""),
            ),
            (
                "landing-only",
                platform::sh_shim_content_twin(&c1.join("bin/claude"), &env, None, &landing),
            ),
            (
                "block then landing",
                platform::sh_shim_content_twin(
                    &c1.join("bin/claude"),
                    &env,
                    None,
                    &block_then_landing,
                ),
            ),
        ] {
            assert_ne!(older, twin_body, "{what}");
            std::fs::write(&twin, &older).unwrap();
            reconcile_agents(&layout);
            assert_eq!(
                std::fs::read_to_string(&twin).unwrap(),
                twin_body,
                "{what}: re-laid to today's twin"
            );
            let before = std::fs::metadata(&twin).unwrap().modified().unwrap();
            reconcile_agents(&layout);
            assert_eq!(
                std::fs::metadata(&twin).unwrap().modified().unwrap(),
                before,
                "{what}: the second reconcile writes nothing"
            );
        }
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The prelude never strands the tool, under a real `/bin/sh`: `update` and `install
    /// latest --force` as the first argument go to the embedded atpkg as `__selfupdate claude
    /// <prefix> -- <args>`; `--probe`, `-p update` and `--debug install` run the store build;
    /// with the embedded atpkg gone `update` runs the store build, silent, exit 0, never an
    /// older `atpkg` on `PATH`.
    #[cfg(unix)]
    #[test]
    fn the_twin_prelude_runs_the_store_build_when_the_embedded_atpkg_is_gone() {
        let layout = temp_prefix("agents-twin-strand");
        let claude = tool("claude");
        let marker = layout.landing_dir().join(claude.as_str());
        let exe = |path: &Path, body: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let target = layout.build_dir("claude", 2026091601).join("bin/claude");
        exe(&target, "#!/bin/sh\necho \"store: $*\"\nexit 0\n");
        let path_dir = layout.prefix.join("older-path");
        exe(
            &path_dir.join("atpkg"),
            "#!/bin/sh\necho \"atpkg: unknown verb $1\" >&2\nexit 2\n",
        );
        let embedded = layout.prefix.join("bundle/atpkg");
        exe(&embedded, "#!/bin/sh\necho \"co-located: $*\"\nexit 0\n");
        let render = |atpkg: &Path| {
            platform::sh_shim_content_twin(
                &target,
                &crate::shim_env::ShimEnv::NONE,
                None,
                &platform::twin_prelude(
                    "claude",
                    &layout.prefix,
                    Some(atpkg),
                    crate::selfupdate::verbs_of("claude"),
                    &crate::yolo::Policy::NONE,
                ),
            )
        };
        let run = |body: &str, args: &[&str]| {
            let twin = layout.prefix.join("twin-under-test");
            exe(&twin, body);
            let out = std::process::Command::new(&twin)
                .args(args)
                .env("PATH", &path_dir)
                .output()
                .unwrap();
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        };
        let store = |args: &str| (Some(0), format!("store: {args}\n"), String::new());
        let co_located = |verb: &str, args: &str| {
            (
                Some(0),
                format!(
                    "co-located: {verb} claude {} -- {args}\n",
                    layout.prefix.display()
                ),
                String::new(),
            )
        };
        let check = |label: &str| {
            assert_eq!(
                run(&render(&embedded), &["--probe", "--", "x"]),
                store("--probe -- x"),
                "{label}"
            );
            assert_eq!(
                run(&render(&embedded), &["update"]),
                co_located("__selfupdate", "update"),
                "{label}"
            );
            assert_eq!(
                run(&render(&embedded), &["install", "latest", "--force"]),
                co_located("__selfupdate", "install latest --force"),
                "{label}"
            );
            assert_eq!(
                run(&render(&embedded), &["-p", "update"]),
                store("-p update"),
                "{label}"
            );
            assert_eq!(
                run(&render(&embedded), &["--debug", "install"]),
                store("--debug install"),
                "{label}"
            );
            assert_eq!(run(&render(&embedded), &[]), store(""), "{label}");
            assert_eq!(
                run(
                    &render(Path::new("/gone/after/relocation/atpkg")),
                    &["update"]
                ),
                store("update"),
                "{label}: an older atpkg on PATH must not be exec'd into `unknown verb`"
            );
        };
        check("no marker");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"atpkg-landing-v1 build=2026091702 pid=1\n").unwrap();
        check("a standing marker");
        let mut older = platform::sh_selfupdate_prelude(
            "claude",
            &layout.prefix,
            &embedded,
            crate::selfupdate::verbs_of("claude"),
        );
        older.push_str(&platform::sh_landing_prelude(
            "claude",
            &layout.prefix,
            &marker,
            &embedded,
        ));
        let older_twin =
            platform::sh_shim_content_twin(&target, &crate::shim_env::ShimEnv::NONE, None, &older);
        assert_eq!(
            run(&older_twin, &["--probe"]),
            co_located("__landing", "--probe")
        );
        assert_eq!(
            run(&older_twin, &["update"]),
            co_located("__selfupdate", "update")
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The twin is best-effort and agent-store-only: with `agents/` unmakeable, an `ay` and
    /// a `claude` install still succeed and lay `bin/`; a `bin/codex` resolving into a
    /// non-agent store tree earns no twin, the same predicate the sweep applies.
    #[cfg(unix)]
    #[test]
    fn the_agents_twin_is_best_effort_and_agent_store_only() {
        let layout = temp_prefix("agents-best-effort");
        let env = crate::shim_env::ShimEnv::NONE;
        std::fs::create_dir_all(&layout.prefix).unwrap();
        std::fs::write(layout.agents_dir(), b"squatter").unwrap();
        let ay = make_build(&layout, "ay", 18, &["ay"]);
        install_tools_env(&layout, &ay, &[tool("ay")], Aliases::Alab, &env)
            .expect("a non-agent install never fails over the agents dir");
        assert!(shim_of(&layout, "ay").exists());
        let claude = tool("claude");
        let c1 = make_build(&layout, "claude", 2026091001, &["claude"]);
        install_tools_env(
            &layout,
            &c1,
            std::slice::from_ref(&claude),
            Aliases::Off,
            &env,
        )
        .expect("even the agent program's own install: bin/ is laid, the twin is retried");
        assert!(platform::resolve_shim(&layout.shim(&claude)).is_some());
        assert!(
            std::fs::symlink_metadata(layout.agents_dir())
                .unwrap()
                .is_file(),
            "the squatter is not replaced"
        );
        std::fs::remove_file(layout.agents_dir()).unwrap();
        reconcile_agents(&layout);
        let twin = layout.agent_shim(&claude);
        assert_eq!(
            platform::resolve_shim(&twin),
            platform::resolve_shim(&layout.shim(&claude))
        );
        let codex = tool("codex");
        let foreign = make_build(&layout, "ay", 19, &["codex"]);
        platform::install_shim_env(&foreign.join("bin"), &codex, &layout.shim(&codex), &env)
            .unwrap();
        reconcile_agents(&layout);
        assert!(
            !layout.agent_shim(&codex).exists(),
            "a primary outside an agent program's store tree earns no twin"
        );
        assert!(twin.exists(), "the real twin is untouched");
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The alias policy is read off the signed index entry: ALab's own (no `system`)
    /// aliases; an agent program, a system-satisfiable member and an unlisted name do not.
    #[test]
    fn the_alias_policy_follows_the_index_entry() {
        let program = |system: Option<&str>| crate::manifest::Program {
            repo: "x".into(),
            policy: String::new(),
            coherence_group: None,
            system: system.map(str::to_string),
            unavailable_hint: None,
            requires: vec![],
        };
        assert_eq!(
            Aliases::for_program("ay", Some(&program(None))),
            Aliases::Alab,
            "trust/ay/ty/clean: ALab's own"
        );
        assert_eq!(
            Aliases::for_program("claude", Some(&program(None))),
            Aliases::Off,
            "codex/claude: an agent program is a vendor's"
        );
        assert_eq!(
            Aliases::for_program("gh", Some(&program(Some("gh")))),
            Aliases::Off,
            "gh/emacs: a system copy may satisfy it"
        );
        assert_eq!(Aliases::for_program("x", None), Aliases::Off, "unlisted");
    }

    /// `alab-<tool>` is laid beside every `<tool>` shim of an ALab program, forwarding to
    /// the same executable (`active_builds` still sees one build, `active_tools` lists the
    /// primaries and `active_aliases` the aliases); a program under `Aliases::Off` gets none.
    #[test]
    fn an_alab_program_gets_an_alias_beside_every_shim_forwarding_to_the_same_target() {
        let layout = temp_prefix("alias-lay");
        let b18 = make_build(&layout, "ay", 18, &["ay", "aylint"]);
        let refused = install_shims(
            &layout,
            &b18,
            &["ay".into(), "aylint".into()],
            Aliases::Alab,
        )
        .unwrap();
        assert!(refused.is_empty());
        for name in ["ay", "aylint"] {
            let t = tool(name);
            let alias = t.alias().unwrap();
            let primary = crate::platform::resolve_shim(&layout.shim(&t)).unwrap();
            let via_alias = crate::platform::resolve_shim(&layout.shim(&alias))
                .unwrap_or_else(|| panic!("{} is laid", alias.as_str()));
            assert_eq!(
                via_alias, primary,
                "the alias forwards where the primary does"
            );
            assert_eq!(via_alias, b18.join("bin").join(t.exe_file()));
            assert_eq!(
                layout.shim(&alias).file_name().unwrap().to_str().unwrap(),
                format!("alab-{name}{}", crate::platform::SHIM_SUFFIX)
            );
            #[cfg(windows)]
            {
                let body = std::fs::read_to_string(layout.shim(&alias)).unwrap();
                assert!(
                    body.contains(&format!("\\bin\\{name}.exe\" %* & @exit /b")),
                    "{body}"
                );
            }
        }
        assert_eq!(crate::ops::active_builds(&layout).get("ay"), Some(&18));
        assert_eq!(
            crate::ops::active_tools(&layout, "ay", 18),
            vec![tool("ay"), tool("aylint")],
            "the primaries, and only the primaries"
        );
        assert_eq!(
            crate::ops::active_aliases(&layout, "ay", 18),
            vec![tool("alab-ay"), tool("alab-aylint")]
        );
        assert_eq!(
            crate::ops::installed_exposes(&layout, "ay").map(|mut v| {
                v.sort();
                v
            }),
            Some(vec!["ay".to_string(), "aylint".to_string()]),
            "the exposed set a dev link must cover never names an alias"
        );
        assert_eq!(Aliases::laid_for(&layout, "ay"), Aliases::Alab);
        assert_eq!(Aliases::laid_for(&layout, "codex"), Aliases::Off);

        let codex = make_build(&layout, "codex", 7, &["codex"]);
        install_shims(&layout, &codex, &["codex".into()], Aliases::Off).unwrap();
        assert!(crate::platform::resolve_shim(&layout.shim(&tool("codex"))).is_some());
        assert!(
            std::fs::symlink_metadata(layout.shim(&tool("alab-codex"))).is_err(),
            "no alias for a vendor tool"
        );
        assert_eq!(Aliases::laid_for(&layout, "codex"), Aliases::Off);
        assert!(std::fs::symlink_metadata(refused_shim_path(&layout, "alab-sudo")).is_err());
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// Aliases follow their primaries through every sweep: a dropped tool's alias is pruned
    /// with it, a re-shim moves the alias to the new build, a policy flip to `Off` sweeps
    /// the aliases of that program and nothing else, `undo_activation` and `uninstall`
    /// take them with the build.
    #[test]
    fn aliases_are_pruned_swept_undone_and_uninstalled_with_their_primary() {
        let layout = temp_prefix("alias-sweep");
        let b18 = make_build(&layout, "ay", 18, &["ay", "aylint"]);
        install_shims(
            &layout,
            &b18,
            &["ay".into(), "aylint".into()],
            Aliases::Alab,
        )
        .unwrap();
        activate_build(&layout, &b18).unwrap();
        let ny7 = make_build(&layout, "ny", 7, &["ny"]);
        install_shims(&layout, &ny7, &["ny".into()], Aliases::Alab).unwrap();

        let b19 = make_build(&layout, "ay", 19, &["ay"]);
        install_shims(&layout, &b19, &["ay".into()], Aliases::Alab).unwrap();
        activate_build(&layout, &b19).unwrap();
        assert!(std::fs::symlink_metadata(shim_of(&layout, "aylint")).is_err());
        assert!(
            std::fs::symlink_metadata(shim_of(&layout, "alab-aylint")).is_err(),
            "a dropped tool's alias is pruned with it"
        );
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&layout, "alab-ay")).unwrap(),
            b19.join("bin").join(tool("ay").exe_file())
        );
        assert_eq!(crate::ops::active_builds(&layout).get("ay"), Some(&19));

        install_shims(&layout, &b19, &["ay".into()], Aliases::Off).unwrap();
        assert!(crate::platform::resolve_shim(&shim_of(&layout, "ay")).is_some());
        assert!(
            std::fs::symlink_metadata(shim_of(&layout, "alab-ay")).is_err(),
            "the alias is swept when the policy says Off"
        );
        assert!(
            crate::platform::resolve_shim(&shim_of(&layout, "alab-ny")).is_some(),
            "another program's alias is untouched"
        );
        assert_eq!(Aliases::laid_for(&layout, "ay"), Aliases::Off);

        install_shims(&layout, &b19, &["ay".into()], Aliases::Alab).unwrap();
        assert!(crate::platform::resolve_shim(&shim_of(&layout, "alab-ay")).is_some());
        undo_activation(&layout, &b19);
        assert!(std::fs::symlink_metadata(shim_of(&layout, "ay")).is_err());
        assert!(
            std::fs::symlink_metadata(shim_of(&layout, "alab-ay")).is_err(),
            "undo takes the alias"
        );

        assert!(crate::platform::resolve_shim(&shim_of(&layout, "alab-ny")).is_some());
        crate::ops::uninstall(&layout, "ny").unwrap();
        assert!(std::fs::symlink_metadata(shim_of(&layout, "ny")).is_err());
        assert!(
            std::fs::symlink_metadata(shim_of(&layout, "alab-ny")).is_err(),
            "uninstall takes the alias"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// An up-to-date program (the pipeline short-circuits, so `install_tools` never runs)
    /// gets its aliases from the pass's reconcile: laid when missing, left alone when right,
    /// swept under `Off`, and never written over a file this manager did not lay.
    #[test]
    fn reconcile_lays_the_aliases_of_an_already_installed_program() {
        let layout = temp_prefix("alias-reconcile");
        let b18 = make_build(&layout, "ay", 18, &["ay", "aylint"]);
        install_shims(&layout, &b18, &["ay".into(), "aylint".into()], Aliases::Off).unwrap();
        assert!(std::fs::symlink_metadata(shim_of(&layout, "alab-ay")).is_err());
        std::fs::write(shim_of(&layout, "alab-aylint"), b"#!/bin/sh\nexit 3\n").unwrap();

        let tools = crate::ops::active_tools(&layout, "ay", 18);
        reconcile_aliases(&layout, &b18, &tools, Aliases::Alab).unwrap();
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&layout, "alab-ay")).unwrap(),
            b18.join("bin").join(tool("ay").exe_file()),
            "the missing alias is laid"
        );
        assert_eq!(
            std::fs::read(shim_of(&layout, "alab-aylint")).unwrap(),
            b"#!/bin/sh\nexit 3\n",
            "a foreign file under an alias name is left alone"
        );
        let before = std::fs::symlink_metadata(shim_of(&layout, "alab-ay"))
            .unwrap()
            .modified()
            .unwrap();
        reconcile_aliases(&layout, &b18, &tools, Aliases::Alab).unwrap();
        assert_eq!(
            std::fs::symlink_metadata(shim_of(&layout, "alab-ay"))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
        assert!(crate::platform::resolve_shim(&shim_of(&layout, "ay")).is_some());
        reconcile_aliases(&layout, &b18, &tools, Aliases::Off).unwrap();
        assert!(std::fs::symlink_metadata(shim_of(&layout, "alab-ay")).is_err());
        assert!(shim_of(&layout, "alab-aylint").exists());
        assert!(crate::platform::resolve_shim(&shim_of(&layout, "ay")).is_some());
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The Windows `.cmd` alias: the resume-proof frame, then a forward to the primary's
    /// `.exe` that ends the batch on that line.
    #[test]
    fn the_cmd_alias_forwards_to_the_primary_exe() {
        let target = Path::new(r"C:\Users\me\.aterm\pkg\store\ay\18\bin\ay.exe");
        let mut want = crate::platform::cmd_frame();
        want.push_str(
            "@\"C:\\Users\\me\\.aterm\\pkg\\store\\ay\\18\\bin\\ay.exe\" %* & @exit /b\r\n",
        );
        assert_eq!(crate::platform::cmd_shim_content(target), want);
        assert_eq!(
            crate::platform::parse_cmd_shim_target(&crate::platform::cmd_shim_content(target)),
            Some(target.to_path_buf())
        );
    }
}
