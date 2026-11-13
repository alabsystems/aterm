// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Atomic `store/<program>/current` activation and `bin/` shims.

use std::io;
use std::path::{Path, PathBuf};

use crate::Layout;
use crate::platform::{self};
use crate::store::{ToolName, split_exposed};

/// Whether a program's exposed tools also get their `alab-<tool>` aliases.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Aliases {
    /// One of ALab's own programs: `alab-<tool>` beside every `<tool>`.
    Alab,
    /// A vendor tool, a system-satisfiable member, or a harness, whose own name already says
    /// it is ALab's.
    Off,
}

impl Aliases {
    /// The policy for an index entry.
    #[must_use]
    pub fn for_program(name: &str, program: Option<&crate::manifest::Program>) -> Self {
        match program {
            Some(p)
                if p.system.is_none()
                    && !crate::stub::is_agent_program(name)
                    && !crate::harness::is_harness(name) =>
            {
                Self::Alab
            }
            _ => Self::Off,
        }
    }

    /// The policy already in force for `program` on disk.
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

/// Atomically point `link` at `target` ([`crate::platform::atomic_symlink`]).
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

/// Install `bin/` shims for the manifest's raw `exposes` list.
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

/// [`install_shims_env`] that hands what it says to `say` instead of stdout.
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
    // The sidecar is the same record by another route (a launcher reaching the
    // binary by its own path reads it there, never from a shim's text), so a lay
    // carrying an env records it too, as `install_shim_env` does. A plain lay
    // records nothing: the build's declaration stands over a shim laid plain
    // (`crate::ops::exec_env`), and only a landing undeclares.
    if !env.is_empty()
        && let Some(build) = crate::shim_env::store_build_of(&build_dir.join("bin"))
    {
        crate::shim_env::write_sidecar(&build, env)?;
    }
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
        let primary_read = platform::read_shim(&primary);
        let Some((target, primary_is)) = primary_read
            .target
            .and_then(|t| agent_primary(layout, name, &t).map(|is| (t, is)))
        else {
            // not installed here (a pending stub, a tombstone): the sweep answers it
            continue;
        };
        let dev_link = primary_is == AgentPrimary::DevLink;
        let env = primary_read.env;
        #[cfg(all(test, unix))]
        agents_checkpoint::at(layout, agents_checkpoint::Event::Read(name));
        let twin = layout.agent_shim(&tool);
        let twin_read = platform::read_shim(&twin);
        let exec = twin_exec(layout, name, &target, primary_is);
        let named =
            |body: Option<&str>| body.and_then(|b| crate::stub::sh_named_atpkg(b, "__atpkg"));
        let standing =
            named(twin_read.body.as_deref()).or_else(|| named(primary_read.body.as_deref()));
        let atpkg = crate::stub::Embedder::this_process().embed(standing.as_deref());
        let prelude = if dev_link {
            String::new()
        } else {
            let mut prelude = platform::twin_prelude(
                name,
                &layout.prefix,
                atpkg.as_deref(),
                crate::selfupdate::verbs_of(name),
                &crate::yolo::policy_of(name),
            );
            prelude.push_str(&crate::launch_tap::sh_prelude(name, atpkg.as_deref()));
            prelude
        };
        if (exec.managed.is_some() || (!dev_link && !prelude.is_empty()))
            && let Err(e) =
                primary_executable(&primary, &target, exec.managed.as_deref(), &env, &prelude)
                    .and_then(|file| {
                        if primary_read
                            .body
                            .as_deref()
                            .is_some_and(|have| have.as_bytes() == file.body)
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
        // The token block goes into the twin alone, before its last line, unless the twin is a
        // dev link (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.4).
        let token = !dev_link;
        let twin_file = || {
            if exec.path == target {
                primary_executable(&twin, &target, exec.managed.as_deref(), &env, &prelude)
            } else {
                platform::twin_executable_to_env(&twin, &exec.path, &env, &prelude)
            }
            .map(|file| with_token(file, name, atpkg.as_deref(), token))
        };
        if twin_read.target.as_deref() == Some(exec.path.as_path())
            && twin_read.env == env
            && twin_read
                .body
                .as_deref()
                .is_some_and(|have| twin_file().is_ok_and(|want| have.as_bytes() == want.body))
        {
            continue;
        }
        let laid = layout
            .ensure_dir(&layout.agents_dir())
            .and_then(|()| twin_file())
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

/// `file`, an agent program's twin, with the token block ([`crate::launch_tap::with_token_block`])
/// where `on`.
fn with_token(
    file: crate::lay::Executable,
    program: &str,
    atpkg: Option<&Path>,
    on: bool,
) -> crate::lay::Executable {
    if !on {
        return file;
    }
    match String::from_utf8(file.body) {
        Ok(body) => crate::lay::Executable::new(
            file.path,
            crate::launch_tap::with_token_block(program, body, atpkg),
        ),
        Err(error) => crate::lay::Executable::new(file.path, error.into_bytes()),
    }
}

fn primary_executable(
    shim: &Path,
    target: &Path,
    _managed: Option<&Path>,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> io::Result<crate::lay::Executable> {
    #[cfg(unix)]
    if let Some(launch) = _managed {
        return Ok(crate::lay::Executable::new(
            shim,
            platform::sh_shim_content_managed(target, env, launch, prelude),
        ));
    }
    platform::twin_executable_to_env(shim, target, env, prelude)
}

/// `store/<program>/exec/` ([`twin_exec`]).
#[must_use]
pub fn exec_dir(layout: &Layout, program: &str) -> PathBuf {
    layout.prefix.join("store").join(program).join("exec")
}

struct AgentExec {
    path: PathBuf,
    managed: Option<PathBuf>,
}

fn twin_exec(layout: &Layout, program: &str, target: &Path, primary: AgentPrimary) -> AgentExec {
    if cfg!(not(unix)) || crate::alab::agent(program).is_none() {
        return AgentExec {
            path: target.to_path_buf(),
            managed: None,
        };
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
    let laid = entry.as_ref().filter(|entry| {
        std::fs::read_link(entry).ok().as_deref() == Some(target)
            || layout
                .ensure_dir(&exec_dir(layout, program))
                .and_then(|()| platform::atomic_symlink(target, entry))
                .is_ok()
    });
    AgentExec {
        path: laid.cloned().unwrap_or_else(|| target.to_path_buf()),
        managed: entry,
    }
}

/// What `program`'s `agents/` twin runs, spelled as its `bin/` primary spells it.
#[must_use]
pub fn twin_target(layout: &Layout, program: &str) -> Option<PathBuf> {
    let tool = ToolName::new(program)?;
    let raw = platform::resolve_shim(&layout.agent_shim(&tool))?;
    Some(unstable_spelling(layout, program, raw))
}

/// [`twin_target`]'s mapping of one exec target ([`exec_dir`]).
fn unstable_spelling(layout: &Layout, program: &str, raw: PathBuf) -> PathBuf {
    if raw.parent() == Some(exec_dir(layout, program).as_path())
        && let Ok(named) = std::fs::read_link(&raw)
    {
        return named;
    }
    raw
}

/// What an agent program's `bin/` shim forwards to, as `agents/` may name it ([`agent_primary`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentPrimary {
    /// A build in an agent program's store tree.
    Store,
    /// A file in the program's recorded dev-link checkout (`aterm pkg link`).
    DevLink,
}

/// What `target`, the forward of `program`'s `bin/` shim, is when an `agents/` twin may name
/// it: a build in an agent program's store tree, or a file inside the program's recorded
/// dev-link checkout ([`crate::linkmode::linked_checkout`]).
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
#[path = "activate_agents_checkpoint_tests.rs"]
pub(crate) mod agents_checkpoint;

#[cfg(all(test, unix))]
fn shim_is_rendered(shim: &Path, target: &Path, env: &crate::shim_env::ShimEnv) -> bool {
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
    let mut installed = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if crate::lay::in_flight_here(&name) {
            continue;
        }
        let path = entry.path();
        let keep = name
            .to_str()
            .and_then(ToolName::from_shim_file)
            .is_some_and(|tool| {
                crate::stub::is_agent_program(tool.as_str())
                    && (platform::resolve_shim(&path)
                        .map(|t| unstable_spelling(layout, tool.as_str(), t))
                        .is_some_and(|t| {
                            agent_primary(layout, tool.as_str(), &t).is_some()
                                && platform::resolve_shim(&layout.shim(&tool))
                                    .is_some_and(|b| b == t)
                        })
                        || (crate::stub::is_pending_stub(&path)
                            && !installed
                                .get_or_insert_with(|| crate::ops::active_builds(layout))
                                .contains_key(tool.as_str())))
            });
        if !keep {
            let _ = std::fs::remove_file(path);
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
            let primary_read = platform::read_shim(&layout.shim(tool));
            let Some(wanted) = primary_read
                .target
                .filter(|t| t.starts_with(build_dir) && t.file_name().is_some())
            else {
                continue;
            };
            let Some(target_bin) = wanted.parent().map(Path::to_path_buf) else {
                continue;
            };
            let env = primary_read.env;
            let shim = layout.shim(&alias);
            let alias_read = platform::read_shim(&shim);
            if alias_read.target.as_deref() == Some(wanted.as_path())
                && alias_read.env == env
                && (cfg!(not(unix))
                    || alias_read.body.as_deref().is_some_and(|have| {
                        platform::shim_executable_to_env(&shim, &wanted, &env)
                            .is_ok_and(|want| have.as_bytes() == want.body)
                    }))
            {
                continue;
            }
            if !alias_read
                .target
                .is_some_and(|t| crate::ops::store_build_of(&layout.prefix, &t).is_some())
                && (alias_read.body.is_some() || std::fs::symlink_metadata(&shim).is_ok())
            {
                continue;
            }
            layout.ensure_dir(&layout.bin_dir())?;
            platform::install_shim_env(&target_bin, tool, &shim, &env)?;
        }
    }
    prune_stale_shims(layout, build_dir, tools, aliases);
    Ok(())
}

/// Primary shims whose exported environment differs ([`crate::shim_env::read_sidecar`]).
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
            let read = platform::read_shim(&shim);
            let target = read.target.filter(|t| t.starts_with(build_dir))?;
            (read.env != declared).then_some((shim, target))
        })
        .collect();
    (!shims.is_empty()).then_some(EnvDrift { declared, shims })
}

/// Re-lay every primary shim of `builds` ([`shim_env_drift`]).
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

/// Store build when [`ToolName::from_shim_file`] refuses `name` ([`crate::ops::store_build_of`]).
pub(crate) fn refused_name_shim(layout: &Layout, name: &str, shim: &Path) -> Option<(String, u64)> {
    if ToolName::from_shim_file(name).is_some() {
        return None;
    }
    crate::platform::resolve_shim(shim)
        .and_then(|target| crate::ops::store_build_of(&layout.prefix, &target))
}

/// The `bin/` shims [`refused_name_shim`] names.
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

/// The file names of the `bin/` shims [`refused_name_shim`] names ([`remove_refused_name_shims`]).
pub(crate) fn refused_name_shims(layout: &Layout) -> Vec<String> {
    refused_name_entries(layout, None)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Remove every `bin/` shim [`refused_name_shim`] names.
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

/// Remove `bin/` shims this program owns that point at a different build, and every alias of
/// this program's no longer wanted (`aliases` is [`Aliases::Off`], or its base is neither in
/// `installed` nor another tool of this build: one whose shim resolves into this build, or one
/// the build records it exposes ([`crate::store::recorded_exposes`]) whose shim a file that does
/// not resolve into this program's store holds while the alias itself still runs this build).
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
    let mut recorded = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if crate::lay::in_flight_here(&name) {
            continue;
        }
        let path = entry.path();
        let Some(name) = name.to_str() else { continue };
        let Some(tool) = crate::store::ToolName::from_shim_file(name) else {
            if refused_name_shim(layout, name, &path).is_some_and(|(p, _)| p == program) {
                let _ = std::fs::remove_file(path);
            }
            continue;
        };
        if installed.contains(&tool) {
            continue;
        }
        if aliases == Aliases::Alab
            && tool
                .alias_base()
                .is_some_and(|base| installed.contains(&base))
        {
            continue;
        }
        let Some(target) = crate::platform::resolve_shim(&path) else {
            // not a shim we can resolve (tombstone, real file)
            continue;
        };
        let Some((p, b)) = crate::ops::store_build_of(&layout.prefix, &target) else {
            continue;
        };
        if p != program {
            continue;
        }
        let alias_wanted = aliases == Aliases::Alab
            && tool.alias_base().is_some_and(|base| {
                let base_shim = layout.shim(&base);
                let base_owner = crate::platform::resolve_shim(&base_shim)
                    .and_then(|t| crate::ops::store_build_of(&layout.prefix, &t));
                base_owner
                    .as_ref()
                    .is_some_and(|(bp, bb)| bp == &program && *bb == build)
                    // The base's shim is a file outside this program's store (the user's, another
                    // program's) while the alias still runs this build: the alias is the one way
                    // left to run its tool. This program's own shim at another build is not one:
                    // this loop removes it, so keeping the alias for it would turn on the order
                    // `bin/` lists in.
                    || (b == build
                        && !base_owner.as_ref().is_some_and(|(bp, _)| bp == &program)
                        && recorded
                            .get_or_insert_with(|| {
                                crate::store::recorded_exposes(build_dir).unwrap_or_default()
                            })
                            .contains(&base)
                        && (base_owner.is_some() || std::fs::symlink_metadata(&base_shim).is_ok()))
            });
        if alias_wanted {
            continue;
        }
        if b != build || tool.is_alias() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Best-effort undo of [`activate_build`] and a partial [`install_tools`] pass.
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
#[path = "activate_tests.rs"]
mod tests;
