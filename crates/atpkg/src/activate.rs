// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Activation (§10): the atomic `store/<program>/current` flip that makes a staged store
//! build live, and the `bin/` shims that forward into it.
//!
//! Every write is an atomic replace (sibling temp, then `rename(2)`), so a reader never
//! sees a missing or half-written link or shim. Every shim name passes
//! [`crate::store::shim_allowed`]: a tool named `sudo`/`ssh`/`git`/… gets no shim and is
//! reported.
//!
//! # The `alab-<tool>` alias
//!
//! ALab's bare tool names collide with other software (Homebrew's p11-kit `trust`, the
//! `ty` and `clean` formulae), and the managed `bin/` is appended to `PATH`, so `trust` may
//! run someone else's copy. Every program that is ALab's own ([`Aliases::Alab`]) gets an
//! `alab-<tool>` shim beside each `<tool>` shim, forwarding to the same executable. Every
//! sweep keys on where a shim resolves, not its name, so aliases are pruned, tombstoned,
//! rolled back and uninstalled with their primary. Vendor tools and system-satisfiable
//! members get no alias, and a pending stub (`crate::stub`) is laid for the plain name only.
//!
//! # The shim environment (design S7)
//!
//! A manifest's `shim_env` ([`crate::shim_env`]) makes every shim of the program export
//! those variables before it execs (e.g. a vendor tool's own updater off).
//! [`install_tools_env`] lays shims with the env its caller holds (the signed manifest, or
//! the build's `.shim-env` sidecar), [`reassert_shim_env`] re-lays a primary that drifted
//! from the sidecar, and [`reconcile_aliases`] mirrors the primary's env onto its alias.

use std::io;
use std::path::{Path, PathBuf};

use crate::Layout;
use crate::platform::{self};
use crate::store::{ToolName, split_exposed};

/// Whether a program's exposed tools also get their `alab-<tool>` aliases (module doc).
/// Decided from the signed index entry by [`Aliases::for_program`] on the install path,
/// and from what is already on disk by [`Aliases::laid_for`] on the verbs that run
/// without an index (`unlink`'s restore, `rollback`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Aliases {
    /// One of ALab's own programs: `alab-<tool>` beside every `<tool>`.
    Alab,
    /// A vendor tool or a system-satisfiable member: the plain names only, and any
    /// alias a previous policy laid for this program is swept.
    Off,
}

impl Aliases {
    /// The policy for an index entry: ALab's own ⇔ no `system` key and not an agent
    /// program (`claude`/`codex` are a vendor's, whatever their row says —
    /// [`crate::stub::AGENT_PROGRAMS`] — so they never grow an `alab-claude`). An unlisted
    /// program (`None`) is not ALab's: nothing vouches for it.
    #[must_use]
    pub fn for_program(name: &str, program: Option<&crate::manifest::Program>) -> Self {
        match program {
            Some(p) if p.system.is_none() && !crate::stub::is_agent_program(name) => Self::Alab,
            _ => Self::Off,
        }
    }

    /// The policy already in force for `program` on disk: `Alab` when any alias shim
    /// resolves into its store tree, else `Off`. For the verbs that hold no index — they
    /// must neither invent aliases for a vendor tool nor sweep the ones an install laid.
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

/// Atomically point `link` at `target` ([`crate::platform::atomic_symlink`]: temp symlink +
/// `rename(2)` on POSIX, a directory junction on Windows): the one entry point for every
/// managed directory link.
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    platform::atomic_symlink(target, link)
}

/// Make `build_dir` the active build: atomically flip `store/<program>/current` to it, the
/// per-program link [`crate::gc::live_builds`] reads as the liveness witness. Idempotent.
/// The program comes from the build dir's place in the store; a dir that is not a store
/// build is refused (`InvalidInput`).
pub fn activate_build(layout: &Layout, build_dir: &Path) -> io::Result<()> {
    let Some((program, _)) = crate::ops::store_build_of(&layout.prefix, build_dir) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a store build directory",
        ));
    };
    atomic_symlink(build_dir, &layout.program_current(&program))
}

/// Install `bin/` shims for the manifest's raw `exposes` list, each forwarding to the tool's
/// executable in `<build_dir>/bin/`. A name that fails [`crate::store::shim_allowed`] never
/// becomes a [`ToolName`] and is skipped; returns the refused raw names, for the caller to
/// say (a landing records them, [`crate::status::record_refused_shims`]).
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

/// [`install_shims`] whose shims export `env` before they exec (design S7): a verb re-laying
/// an installed build passes its sidecar ([`crate::shim_env::read_sidecar`]), so a re-lay
/// never strips what the signed manifest declared.
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

/// [`install_shims_env`] that hands what it says (an exec root it laid) to `say` instead
/// of stdout: a surface whose stdout is one document (`aterm pkg repair --json`, `aterm
/// doctor --json`) gathers it into the row of the item that laid it.
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

/// The validated half of [`install_shims`]: lay one shim per admitted [`ToolName`] (plus
/// its alias under [`Aliases::Alab`]), then prune what a previous build left. For callers
/// that already hold `ToolName`s (the transaction flip in [`crate::flow`]), so the raw list
/// is not re-split.
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

/// [`install_tools`] whose shims, primary and alias, export `env` before they exec (design
/// S7): the signed manifest's [`crate::manifest::PkgManifest::shim_env`] on the install
/// path, the build's sidecar ([`crate::shim_env::read_sidecar`]) elsewhere.
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
    // The exec root (`crate::compat`) must stand before any shim naming the build renders,
    // since the renders route through `<prefix>/compat/trust/<n>` exactly when a whole
    // root stands; that also puts it ahead of the `current` flip on every lane ending here
    // (a rollback ensures its own in `flow::rollback_member`). `Shallow` writes nothing over
    // a whole root. A root that cannot be laid costs this lay nothing: the shims render
    // plain and the pass's reconcile retries. A root laid here is said: it is the one moment
    // the machine changes shape.
    if let Some(n) = crate::compat::trust_build_of(layout, build_dir) {
        match crate::compat::ensure_root(layout, build_dir, crate::seam::Depth::Shallow) {
            Ok(crate::compat::Ensured::Built) => say(crate::compat::laid_line(layout, n)),
            Ok(crate::compat::Ensured::Plain | crate::compat::Ensured::Present) => {}
            Err(e) => eprintln!("{}", crate::compat::not_laid_line(layout, n, &e)),
        }
    }
    // Render everything, then lay: each primary and, under [`Aliases::Alab`], its
    // `alab-<tool>` alias forwarding to the same executable.
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
    // The front-of-PATH twin, laid from the `bin/` shim just written so the two agree.
    reconcile_agents(layout);
    Ok(())
}

/// Bring `<prefix>/agents/` ([`Layout::agents_dir`]) in line with `bin/`: for every agent
/// program ([`crate::stub::AGENT_PROGRAMS`]) whose `bin/` shim resolves into an agent
/// program's store tree or the program's recorded dev-link checkout ([`agent_primary`]), lay
/// or refresh the twin `agents/<tool>` with the same target and exported environment (read
/// off the `bin/` shim) plus the self-update block ([`crate::selfupdate`]), which the primary
/// gets too; then [`sweep_agents_dir`]. A dev link's twin gets no block and its `bin/` shim
/// stays as the link laid it. What already matches is left alone.
///
/// Runs once at the end of every shim-laying pass ([`install_tools_env`], flow's undo,
/// linkmode's link/unlink, and after `cli::reconcile_aliases`'s per-program loop), never
/// per program, and one at a time with every sweep ([`AGENTS_TURN`]). Best-effort: a twin
/// that cannot be laid is reported on stderr and retried next pass, so an `ay` install never
/// fails over a directory only agent programs use.
pub fn reconcile_agents(layout: &Layout) {
    let _turn = AGENTS_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The read of each primary and the publish of what it selected hold the publication
    // turn too: otherwise a concurrent rollback, tombstone or first landing could replace
    // the primary after the read and be overwritten with its stale (possibly dangling)
    // target.
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
            continue; // not installed here (a pending stub, a tombstone): the sweep answers
        };
        let dev_link = primary_is == AgentPrimary::DevLink;
        let env = platform::shim_env_of(&primary);
        #[cfg(all(test, unix))]
        agents_checkpoint::at(layout, agents_checkpoint::Event::Read(name));
        let twin = layout.agent_shim(&tool);
        // WHAT THE TWIN EXECS ([`twin_exec`]): the primary's target, except for ALab bytes,
        // whose twin names a stable path that a new build or a re-link re-points by a rename
        // in that path's own directory — `exec` makes the program's argv0 the path it
        // names, and codex-alab's live upgrade watches argv0's directory.
        let exec = twin_exec(layout, name, &target, primary_is);
        // THE TWIN'S PRELUDE, rendered here, the one place the twin is laid
        // ([`platform::twin_prelude`]): the self-update block ([`crate::selfupdate`]) —
        // `case "$1" in update|upgrade|install)` on the program's rostered verbs, so a
        // `claude update` typed on the managed name is answered by `atpkg __selfupdate` and
        // never by the vendor's own updater, which installs a copy this name never runs. A twin
        // that still carries the retired landing prelude ([`crate::landing`]) compares unequal
        // below and is re-laid once.
        //
        // WHICH atpkg it hands over to is [`crate::stub::Embedder`]'s call over the one the
        // standing twin names — or, with no twin standing, the one `bin/`
        // names: the co-located `atpkg` of the process running this pass only when it is
        // there and lasts at least as long as that one — a pass run from a dev build or a
        // scratch copy re-points no twin at a path that is not there, nor at a `target/`
        // build while the installed app's atpkg stands.
        let named = |shim: &Path| {
            crate::metadata_io::read_bounded_regular_utf8(shim, 64 * 1024)
                .ok()
                .and_then(|body| crate::stub::sh_named_atpkg(&body, "__atpkg"))
        };
        let standing = named(&twin).or_else(|| named(&primary));
        let atpkg = crate::stub::Embedder::this_process().embed(standing.as_deref());
        // A DEV LINK'S TWIN carries no prelude (self-update or `--yolo`): the
        // checkout is the developer's, and atpkg updates no build of it nor
        // rewrites its argv.
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
        // `bin/` answers the same verbs: outside an aterm session the rc hook takes
        // `agents/` off PATH and appends `bin/` (`crate::hooks`), so `claude` there is
        // `bin/claude`, and the vendor's updater run on the managed copy sets `installMethod`
        // to `native` in `~/.claude.json` and installs a copy under `~/.local`. Laid only when
        // its bytes differ; the forward `exec`s `bin/` again and the re-entry guard
        // ([`crate::selfupdate::REENTRY_VAR`]) lets it through to the store build. Windows
        // renders no block. A dev link's `bin/` shim is the link itself (`linkmode::link`),
        // never rewritten here.
        if !dev_link
            && !prelude.is_empty()
            && !twin_is_rendered(&primary, &target, &env, &prelude)
            && let Err(e) = platform::twin_executable_to_env(&primary, &target, &env, &prelude)
                .and_then(|file| crate::lay::write_in_publication_turn(&file, &publication))
        {
            eprintln!(
                "atpkg: {name}: bin/{name} lacks the twin's prelude ({e}); runs the store \
                 build; retried next pass"
            );
        }
        // Left alone only when it resolves where the primary does, exports the same
        // environment, and its BYTES are what the renderer lays now ([`twin_is_rendered`]):
        // a twin that forwards and exports right but renders differently — laid before
        // an exec root stood for its build, by a client whose shim text differed, or by
        // one that laid the landing prelude — would otherwise be kept forever by a
        // predicate that reads only its target.
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

/// `store/<program>/exec/`: the directory holding the stable path an ALab build's twin
/// execs ([`twin_exec`]) — `exec/<entrypoint file>`, a symlink to the file the primary runs,
/// re-pointed by a rename INSIDE this directory whenever an ALab store build or an ALab dev
/// link becomes the primary. Not a build name ([`crate::store::parse_build_name`]), so no
/// store walker reads it as a build or as debris; `uninstall` takes it with the program's
/// tree.
#[must_use]
pub fn exec_dir(layout: &Layout, program: &str) -> PathBuf {
    layout.prefix.join("store").join(program).join("exec")
}

/// THE PATH AN AGENT TWIN EXECS for its primary's `target`. For every build that is not ALab's
/// it is `target` itself, byte for byte as before. For ALab bytes ([`crate::alab`]) — the
/// active store build (`store/<program>/current` names it) or a dev link of an ALab package
/// — it is `store/<program>/exec/<file>` ([`exec_dir`]), a symlink to `target` laid by an
/// atomic rename in that directory.
///
/// WHY. A `sh` `exec '<path>'` makes the program's argv0 that path, and codex-alab's live
/// upgrade (codex-alab `alab-tui/src/upgrade.rs`, `alab-autopilot/src/extension.rs`) watches
/// argv0's PARENT directory, non-recursively, and re-execs when argv0 then resolves to another
/// file. So the rename that moves a running codex-alab onto a new build — or a re-link (AD-4)
/// — must happen in argv0's own directory: `exec/` is that directory. The store build's own
/// path names a file no update replaces, and `current/bin/codex` has `current/bin` for its
/// parent, which resolves into the OLD build, while `current` flips in `store/<program>/`.
/// The twin's last line stays `exec '<absolute path>' "$@"`, the one shape codex-alab's and
/// aterm's twin readers parse, and its self-update block stays ahead of it.
///
/// A primary that is not ALab's (a person's vendor choice) never re-points the entry: a
/// running codex-alab stays on its own bytes and never crosses onto the vendor's.
///
/// Falls back to `target` whenever the stable path cannot be vouched for: `current` not
/// naming the build the primary runs, a dev link whose target is outside its checkout, or an
/// entry that could not be laid. Unix only (Windows twins are `.cmd` files).
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

/// What `program`'s `agents/` twin runs, spelled as its `bin/` primary spells it: the twin's
/// exec target ([`platform::resolve_shim`]) with a [`twin_exec`] stable prefix resolved back
/// to the build or checkout it names now. `None` with no twin, or a pending or tombstoned one.
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
/// front of `PATH`. `None` otherwise: no twin is laid or kept for a target nothing vouches
/// for. The check is lexical: the link marker holds the absolute checkout, the shim its
/// absolute target.
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
/// [`crate::lay::PUBLICATION_TURN`]. A first run's set completion and the vendor lane's
/// thread each lay shims and end in a reconcile; interleaved, one walk would sweep a twin
/// the other had just judged, or both lay one name at once. Locked at each site, not through
/// a helper, so the lock-order census sees every holder.
static AGENTS_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fixture may park a real agent-shim read or write for its own prefix only. Callbacks
/// run outside the registry lock, and removing the guard unregisters the fixture. Unix test
/// builds only, as those fixtures (cli's `tests`) are.
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
    // On every platform: an older Windows twin carries the landing prelude and today's `.cmd`
    // twin is the plain framed shim, so it is re-laid once. Safe while it runs: every `.cmd`
    // starts with the frame (`platform::CMD_FRAME_HEAD`) and ends its batch on the line that
    // runs the program, so `cmd.exe`'s byte-offset resume never re-reads the new file.
    // Unverified on Windows.
    platform::twin_executable_to_env(shim, target, env, prelude).is_ok_and(|want| {
        crate::metadata_io::read_bounded_regular(shim, platform::MAX_SHIM_BYTES)
            .is_ok_and(|have| have == want.body)
    })
}

/// Whether `shim` holds, byte for byte, what the one shim renderer
/// (`platform::shim_executable_to_env`) lays for `target` and `env` now: the half of the
/// reconciles' "already right" that target and environment cannot see.
///
/// On Unix the render carries an exec root's guard line when
/// [`crate::compat::route_for_shim`] finds one for the build, so a shim laid before that
/// root stood forwards and exports right yet stays on the store path, where the build's
/// tippy refuses to start. The read is one bounded, non-link regular file: a symlink or
/// anything unreadable reads as not rendered and is re-laid. Windows answers `true` (no
/// exec roots there).
fn shim_is_rendered(shim: &Path, target: &Path, env: &crate::shim_env::ShimEnv) -> bool {
    if cfg!(not(unix)) {
        return true;
    }
    platform::shim_executable_to_env(shim, target, env).is_ok_and(|want| {
        crate::metadata_io::read_bounded_regular(shim, platform::MAX_SHIM_BYTES)
            .is_ok_and(|have| have == want.body)
    })
}

/// Keep only live agent shims in `agents/`: an entry stays iff its name is an agent
/// program's, it resolves into an agent program's store tree or the program's recorded
/// dev-link checkout ([`agent_primary`]), and `bin/`'s shim of the same name resolves to the
/// same target (or it is the pending stub of a program not yet installed). `agents/` is
/// first on every `PATH`, so nothing may sit there that `bin/` does not vouch for.
/// Best-effort and silent; takes [`AGENTS_TURN`], then the publication turn.
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
                        // An agent program's PENDING stub stands here too,
                        // until its build arrives and the twin replaces it.
                        || (crate::stub::is_pending_stub(&entry.path())
                            && !installed.contains_key(tool.as_str())))
            });
        if !keep {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Bring an installed program's aliases in line with `aliases` without writing its
/// primaries: under [`Aliases::Alab`] lay a missing `alab-<tool>`, or re-lay one whose bytes
/// are not the current render ([`shim_is_rendered`]), for every `tool` whose shim resolves
/// into `build_dir`; under [`Aliases::Off`] sweep aliases resolving into this program's
/// store. An up-to-date program never reaches [`install_tools`], so this is how it gains or
/// loses aliases. Dev-link shims and pending stubs are untouched.
///
/// `agents/` is not reconciled here: this runs per program, and the pass calls
/// [`reconcile_agents`] once after its loop (`cli::reconcile_aliases`).
pub(crate) fn reconcile_aliases(
    layout: &Layout,
    build_dir: &Path,
    tools: &[ToolName],
    aliases: Aliases,
) -> io::Result<()> {
    if aliases == Aliases::Alab {
        for tool in tools {
            let Some(alias) = tool.alias() else { continue };
            // The alias mirrors the primary's forward target as it is spelled on disk —
            // never a re-derived path that could differ in spelling (a canonicalized
            // build dir) and make the tick rewrite a correct alias every pass. A primary
            // that does not resolve into this build dir is not this build's to alias.
            let Some(wanted) = platform::resolve_shim(&layout.shim(tool))
                .filter(|t| t.starts_with(build_dir) && t.file_name().is_some())
            else {
                continue;
            };
            let Some(target_bin) = wanted.parent().map(Path::to_path_buf) else {
                continue;
            };
            // The alias exports what the primary exports (design S7) — read off the
            // primary as laid, never re-derived, for the same reason as the target.
            let env = platform::shim_env_of(&layout.shim(tool));
            let shim = layout.shim(&alias);
            // Already right — same target, same env, and the bytes the renderer lays now —
            // is left alone, so the tick rewrites nothing. The byte check catches an alias
            // laid plain for a trust build that now has an exec root (`crate::compat`).
            if platform::resolve_shim(&shim).is_some_and(|t| t == wanted)
                && platform::shim_env_of(&shim) == env
                && shim_is_rendered(&shim, &wanted, &env)
            {
                continue;
            }
            // Only over a name that is free or that this manager laid: a foreign file
            // (a hand-made `alab-x`, a dev link) keeps its place.
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

/// The primary shims of one build whose exported environment is not the one that build
/// declares ([`crate::shim_env::read_sidecar`], design S7) — what [`shim_env_drift`]
/// found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvDrift {
    /// What the build's sidecar declares — never empty.
    pub(crate) declared: crate::shim_env::ShimEnv,
    /// Each drifted primary: its `bin/` path and the target it forwards to, as spelled
    /// on disk.
    pub(crate) shims: Vec<(std::path::PathBuf, std::path::PathBuf)>,
}

/// The one drift predicate: among `tools`, the primaries whose shim resolves into
/// `build_dir` and exports anything other than exactly what the build's sidecar declares.
/// The pass heals what it finds ([`reassert_shim_env`]) and the doctor's check 10f names it.
///
/// `None` when nothing drifted, and always when the build declares no environment: an
/// absent sidecar is no evidence a shim's exports should go (add-only). A declaring build
/// is authoritative. Reads only: one sidecar, then one bounded read per tool.
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

/// Re-lay every primary shim of `builds` (build dir, active tools) that [`shim_env_drift`]
/// finds exporting other than its build declares, with exactly the declared environment.
/// An up-to-date program is never laid again, so without this a `claude` shim laid plain
/// over a build declaring `DISABLE_AUTOUPDATER=1` would stay plain, and
/// [`reconcile_agents`] would copy it onto the twin.
///
/// The target is the primary's own as spelled on disk and the render the one shim
/// renderer's, so a routed trust shim stays routed. Best-effort: a failed lay is said on
/// stderr and retried next pass. The pass runs this before the alias and agents
/// reconciles, which copy the primary's exports.
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

/// The store build a `bin/` entry named `name` (a file name, `pwsh.cmd` on Windows) at
/// `shim` forwards into, when it is a shim under a name no shim may carry now
/// ([`ToolName::from_shim_file`] refuses it: a shell, another command a shim must never
/// intercept, a name that is not plain ASCII) and its target lies in this prefix's store
/// ([`crate::ops::store_build_of`]). Only an older atpkg laid such a shim, before its name
/// joined the deny-list (design S13); nothing this build writes is one, and an entry that
/// forwards anywhere else (a link a person made, a tombstone) is never one.
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

/// The file names of the `bin/` shims [`refused_name_shim`] names, by name: what
/// [`remove_refused_name_shims`] would remove.
pub(crate) fn refused_name_shims(layout: &Layout) -> Vec<String> {
    refused_name_entries(layout, None)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Remove every `bin/` shim [`refused_name_shim`] names (`program`: only those into that
/// program's store), and return the file names removed. The sweep `aterm pkg repair`, every
/// pass's base relay and a revocation run, so a shim an older atpkg laid under a name now
/// refused never outlives the upgrade, and never keeps a revoked build running.
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
/// this build). A dropped tool's shim would otherwise keep the old build looking active to
/// `ops::active_builds`, so gc could reclaim the live one and no tombstone could revoke it.
///
/// It deletes files on the user's PATH, so a shim goes only when it resolves into
/// `<prefix>/store/<program>/<other build>/` (an alias: anywhere in
/// `<prefix>/store/<program>/`), by the prefix-anchored [`crate::ops::store_build_of`].
fn prune_stale_shims(layout: &Layout, build_dir: &Path, installed: &[ToolName], aliases: Aliases) {
    let _publication = crate::lay::PUBLICATION_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((program, build)) = crate::ops::store_build_of(&layout.prefix, build_dir) else {
        return; // not a store build dir of ours — nothing to prune
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
        // `bin/` entries are file names (`ay.cmd` on Windows); `installed` holds logical
        // names, so compare through `from_shim_file`. Its `None` skips, fail closed in the
        // one predicate that deletes, except for a shim into this program's store under a
        // name no shim may carry now ([`refused_name_shim`]): an older atpkg laid it, at
        // whatever build, and no lay re-points it, so it goes.
        let Some(tool) = crate::store::ToolName::from_shim_file(name) else {
            if refused_name_shim(layout, name, &entry.path()).is_some_and(|(p, _)| p == program) {
                let _ = std::fs::remove_file(entry.path());
            }
            continue;
        };
        // Never remove a name the new build still exposes — that shim was just written —
        // nor the alias just written beside it.
        if installed.contains(&tool) {
            continue;
        }
        // An alias stays when its base was just laid or is a tool of this same build the
        // pass did not name — the partial set `cli::restore_installed_shims` lays — or the
        // restore would delete the alias of a tool it never touched.
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
            continue; // not a shim we can resolve (tombstone, real file, dangling)
        };
        // A plain shim is stale at a different build of this program; an alias whose base
        // is wanted nowhere — not in this lay, not as a live tool of this build — is stale
        // at any build of it, the same build included, which is the shape a policy flip
        // (`Alab` → `Off`) or a dropped base leaves behind.
        if crate::ops::store_build_of(&layout.prefix, &target)
            .is_some_and(|(p, b)| p == program && (b != build || tool.is_alias()))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Best-effort undo of [`activate_build`] and a partial [`install_tools`] pass, for a build
/// about to be discarded after activation (`flow::abort_activated_install`): otherwise its
/// `current` link dangles (and GC abstains on the program) and its shims point at nothing.
///
/// Scoped to this build: `current` is removed only if it names `build_dir`, and only `bin/`
/// entries resolving into `build_dir` go. The link goes through [`platform::remove_link`]
/// (a Windows junction refuses `remove_file`).
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
    // A twin whose primary just went is a twin with nothing to vouch for it.
    sweep_agents_dir(layout);
}

/// Install a failing tombstone shim at `bin/<tool>`: a tiny script that prints a
/// yanked/revoked notice to stderr and exits nonzero, so a revoked build's shim is disabled,
/// not left runnable (§7). Written atomically over the prior shim. A sensitive name has no
/// [`ToolName`], so it can never be tombstoned. The next successful install re-lays the
/// shim over it.
pub fn install_tombstone_shim(layout: &Layout, tool: &ToolName) -> io::Result<()> {
    let bin = layout.bin_dir();
    layout.ensure_dir(&bin)?;
    let shim = layout.shim(tool);

    // The platform backend embeds the tool-bearing text injection-safely (Unix: a
    // single-quoted `printf` arg; Windows: a `cmd`-escaped `echo`).
    let message = format!(
        "atpkg: {} was yanked/revoked — run `aterm pkg update`",
        tool.as_str()
    );
    // Unix: an executable `sh` script; Windows: a `.cmd` batch. Both temp + rename.
    platform::install_tombstone_shim(&shim, &message)?;
    #[cfg(all(test, unix))]
    agents_checkpoint::at(layout, agents_checkpoint::Event::Written(tool.as_str()));
    // A revoked agent program must not stay runnable through its front-of-PATH twin:
    // the tombstone resolves to no target, so the sweep drops the twin.
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
    /// [`Layout::is_system_prefix`] answers yes for ([`crate::store::system_shape`]). Only
    /// root can create under a real root-owned chain, so a fixture that waited for one ran
    /// nowhere; the shape is what activation reads, and this one is answered without root.
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
            // `<b>.exe` on Windows. Spelled out, not `ToolName::exe_file`: a build may ship a
            // binary that is refused a shim (`sudo` below).
            let name = format!("{b}{}", crate::platform::EXE_SUFFIX);
            std::fs::write(dir.join("bin").join(name), b"#!/bin/true\n").unwrap();
        }
        dir
    }

    #[test]
    fn a_dropped_tool_leaves_no_stale_shim_pointing_at_the_prior_build() {
        // 19 drops `aylint`: a stale `bin/aylint` into 18 would make 18 read as active and
        // the next `gc::run` delete the live 19.
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
        // `bin/<shim>` forwarding to `<build>/bin/<target>`, laid the way the installer lays
        // one (temp + rename), so every entry below is a real shim and not a stand-in.
        let lay = |shim: &str, build: &PathBuf, target: &str| {
            platform::install_shim_env(
                &build.join("bin"),
                &tool(target),
                &layout.shim(&tool(shim)),
                &crate::shim_env::ShimEnv::NONE,
            )
            .unwrap();
        };
        lay("ay", &b19, "ay"); // the tool this pass just laid
        lay("aydoc", &b19, "aydoc"); // a tool of this build the pass did not name
        lay("aylint", &b18, "aylint"); // dropped by 19, stale at the old build
        lay("alab-ay", &b19, "ay"); // alias whose base was just laid
        lay("alab-aydoc", &b19, "aydoc"); // alias whose base is a live tool of this build
        lay("alab-aylint", &b18, "aylint"); // alias of a dropped tool
        lay("ny", &n7, "ny"); // another program's shim
        lay("alab-ny", &n7, "ny"); // another program's alias
        // A dev link into a checkout with a `store/ay/18/bin/ay` tail: `store_build_of` is
        // anchored to the prefix, so it is not ours.
        let checkout = layout.prefix.join("checkout/store/ay/18/bin");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("ay"), b"#!/bin/true\n").unwrap();
        std::os::unix::fs::symlink(checkout.join("ay"), shim_of(&layout, "devlink")).unwrap();
        // A hand-made file, a name no `ToolName` admits, and a tombstone (forwards nowhere).
        std::fs::write(layout.bin_dir().join("hand"), b"mine\n").unwrap();
        std::fs::write(refused_shim_path(&layout, "sudo"), b"mine\n").unwrap();
        install_tombstone_shim(&layout, &tool("tombstoned")).unwrap();
        // A lay this process has in flight, staged and waiting for the turn to rename it: its
        // temp resolves into build 18 like the stale `aylint`, and stays for its writer. The
        // same temp of a process that is gone (pid 1 is init's, never a test's) goes.
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
        // Idempotence: activating the build that is already live must not disturb bin/.
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
        // It resolves to a real directory.
        assert!(std::fs::metadata(&cur).unwrap().is_dir());

        // Re-flip to a newer build — atomic re-point, no leftover temp.
        let b19 = make_build(&layout, "ay", 19, &["ay"]);
        activate_build(&layout, &b19).unwrap();
        assert_eq!(std::fs::read_link(&cur).unwrap(), b19);
        // No stray temp symlinks left beside the link.
        let leftovers: Vec<_> = std::fs::read_dir(cur.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "no temp symlink should remain");
        // Activation writes one link: the retired per-channel family is never recreated.
        assert!(!layout.prefix.join("channels").exists());
        // A directory that is not a store build is refused, not activated witness-less.
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

        // Both per-program links survive.
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            ay19
        );
        assert_eq!(
            std::fs::read_link(layout.program_current("ny")).unwrap(),
            ny7
        );

        // And it re-points, rather than accumulating.
        let ay20 = make_build(&layout, "ay", 20, &["ay"]);
        activate_build(&layout, &ay20).unwrap();
        assert_eq!(
            std::fs::read_link(layout.program_current("ay")).unwrap(),
            ay20
        );
        // `current` is not a build: `list_installed` must not report it as one.
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
        // sudo is refused (sensitive), reported with the raw name the manifest asked for,
        // and NOT shimmed — even though the build tree does ship a `bin/sudo`.
        assert_eq!(refused, vec!["sudo".to_string()]);
        let sudo = refused_shim_path(&layout, "sudo");
        assert!(!sudo.exists() && std::fs::symlink_metadata(&sudo).is_err());
        // ay and ny resolve into the build's bin/ (the exec stub on Unix, the `.cmd` target
        // on Windows).
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

        // The pending stub is the lowest-precedence occupant: it never writes over a
        // tombstone, so the name is cleared first.
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
        // A live shim exists (the "old working shim") pointing into a build.
        let build = make_build(&layout, "ay", 18, &["ay"]);
        let exposes = vec!["ay".to_string()];
        install_shims(&layout, &build, &exposes, Aliases::Off).unwrap();
        let shim = shim_of(&layout, "ay");
        // A live forwarding shim (an exec stub on Unix, a forwarding `.cmd` on Windows).
        assert!(
            crate::platform::resolve_shim(&shim).is_some(),
            "live shim forwards into the build"
        );

        // Tombstone it: the forwarding shim is replaced by a failing regular-file script.
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
        // exec-bit fixture — Unix-only
        #[cfg(unix)]
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "tombstone is executable"
        );

        // Running it exits nonzero and names the tool on stderr (actively disabled).
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

        // A sensitive name has no `ToolName`, so it cannot be tombstoned at all.
        assert!(ToolName::new("sudo").is_none());
        assert!(std::fs::symlink_metadata(refused_shim_path(&layout, "sudo")).is_err());

        // No stray temp left behind.
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
    /// `0755`. Nothing upstream objects to `0700` — it satisfies `dir_safe_for_private_write`
    /// and Trust's launcher predicate alike — so the break surfaces only as a bare
    /// `Permission denied` at the first non-root invocation of an installed tool.
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
        // A second program activates afterwards; ay keeps its own witness link and shim.
        let other = make_build(&layout, "ny", 7, &["ny"]);
        activate_build(&layout, &other).unwrap();
        install_tools(&layout, &other, &[tool("ny")], Aliases::Off).unwrap();

        undo_activation(&layout, &doomed);

        // The doomed build's whole footprint is gone...
        assert!(
            std::fs::symlink_metadata(layout.program_current("ay")).is_err(),
            "ay's witness link is removed"
        );
        assert!(
            crate::platform::resolve_shim(&layout.shim(&tool("ay"))).is_none(),
            "ay's shim is removed"
        );
        // ...and nothing else is: the bystander program survives entirely.
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
        // A newer build drops `claudex`: its env-carrying shim and alias are pruned.
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
        // A policy flip to Off sweeps the env-carrying alias like a plain one.
        install_tools_env(&layout, &b19, &[tool("claude")], Aliases::Off, &env).unwrap();
        assert!(std::fs::symlink_metadata(shim_of(&layout, "alab-claude")).is_err());
        // Re-laid with no env: the plain shim, nothing left of the exports.
        install_tools(&layout, &b19, &[tool("claude")], Aliases::Off).unwrap();
        assert_eq!(
            crate::platform::shim_env_of(&shim_of(&layout, "claude")),
            crate::shim_env::ShimEnv::NONE
        );
        // And back with it: the same name, now exporting again — temp+rename over the
        // plain one.
        install_tools_env(&layout, &b19, &[tool("claude")], Aliases::Off, &env).unwrap();
        assert_eq!(
            crate::platform::shim_env_of(&shim_of(&layout, "claude")),
            env
        );
        // `undo_activation` unwinds an env-carrying shim by where it resolves.
        activate_build(&layout, &b19).unwrap();
        undo_activation(&layout, &b19);
        assert!(crate::platform::resolve_shim(&shim_of(&layout, "claude")).is_none());
        // `ops::uninstall` sweeps the wrapper the same way.
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
    /// by `cli::default_link_bins` — at the build the other tools are live at. The sweeps
    /// that must still fire are asserted after it.
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
        // The restore's shape: the same build, only the subset the link owned.
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Alab).unwrap();
        for t in ["ay", "aylint", "alab-ay", "alab-aylint"] {
            assert!(
                crate::platform::resolve_shim(&shim_of(&layout, t))
                    .is_some_and(|p| p.starts_with(&b18)),
                "{t}: survives a lay that did not name it"
            );
        }
        // A policy flip still takes every alias of the program, subset or not.
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Off).unwrap();
        for t in ["alab-ay", "alab-aylint"] {
            assert!(
                std::fs::symlink_metadata(shim_of(&layout, t)).is_err(),
                "{t}: swept by Aliases::Off"
            );
        }
        // And a newer build that drops `aylint` still prunes its shim and its alias: the
        // base no longer names this build either, whichever order `bin/` is read in.
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
        // The primary carries an env; no alias yet (a pre-alias install).
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
        // Agreeing: untouched (the tick rewrites nothing).
        let before = std::fs::metadata(&alias).unwrap().modified().unwrap();
        reconcile_aliases(&layout, &b18, &[tool("ay")], Aliases::Alab).unwrap();
        assert_eq!(
            std::fs::metadata(&alias).unwrap().modified().unwrap(),
            before
        );
        // The primary loses its env (a re-pin without the key): the alias follows.
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
    /// lacks, `crate::compat`) are re-laid, and a second reconcile writes neither. Pass
    /// order: [`reconcile_aliases`], then [`reconcile_agents`].
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
        // A symlink an older atpkg left reads as not rendered, the rendered file as rendered.
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
        // An ALab tool gets no twin; the agents dir holds only agent programs.
        let ay = make_build(&layout, "ay", 18, &["ay"]);
        install_shims(&layout, &ay, &["ay".into()], Aliases::Alab).unwrap();
        assert!(!layout.agent_shim(&tool("ay")).exists());
        // Foreign entries are swept: a hand-dropped file, and a symlink under an agent
        // name that points outside the store (nothing in bin/ vouches for it).
        std::fs::write(layout.agents_dir().join("mine"), b"#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink("/usr/bin/true", layout.agents_dir().join("codex")).unwrap();
        // A twin lay this process has in flight stays for its writer to rename; the temp a
        // process that is gone left (pid 1 is init's) is not an agent name, and goes.
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
        // The twin follows an update to a newer build.
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
        // A pre-agents client's install: primary present, twin missing — the pass's own
        // agents reconcile lays it without touching the primary.
        std::fs::remove_file(&twin).unwrap();
        reconcile_agents(&layout);
        assert_eq!(
            platform::resolve_shim(&twin),
            platform::resolve_shim(&layout.shim(&claude))
        );
        // A tombstoned primary takes the twin with it (a yanked build must not stay
        // runnable through the front-of-PATH copy) …
        install_tombstone_shim(&layout, &claude).unwrap();
        assert!(!twin.exists(), "no twin over a tombstone");
        // … a re-install brings it back …
        install_tools_env(
            &layout,
            &c2,
            std::slice::from_ref(&claude),
            Aliases::Off,
            &env,
        )
        .unwrap();
        assert!(twin.exists());
        // … undoing that activation drops it again …
        undo_activation(&layout, &c2);
        assert!(!twin.exists(), "undo sweeps the twin of the doomed build");
        // … and an uninstall leaves nothing under agents/ at all.
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
    /// `bin/claude` IS the managed name). Every reader keyed on the target still resolves
    /// the twin to the STORE target; the twin is laid once and a second reconcile writes
    /// nothing; and the twins older clients laid — the primary's bytes (before any block),
    /// the landing-only twin and the block-then-landing twin — are each re-laid to today's
    /// once. What the block does when run is
    /// `the_twin_prelude_runs_the_store_build_when_the_embedded_atpkg_is_gone`'s.
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
        // Idempotent: a second reconcile writes nothing.
        let before = std::fs::metadata(&twin).unwrap().modified().unwrap();
        reconcile_agents(&layout);
        assert_eq!(
            std::fs::metadata(&twin).unwrap().modified().unwrap(),
            before
        );
        assert_eq!(std::fs::read_to_string(&twin).unwrap(), twin_body);
        // Twins older clients laid, each re-laid to today's once: the primary's bytes (no
        // block), landing-only, and block-then-landing.
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
    /// older `atpkg` on `PATH`. Only a twin an older client laid with the landing prelude
    /// still hands a non-verb to `__landing`.
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
        // The store build: records that it ran, with its arguments.
        let target = layout.build_dir("claude", 2026091601).join("bin/claude");
        exe(&target, "#!/bin/sh\necho \"store: $*\"\nexit 0\n");
        // An older atpkg on PATH: no `__selfupdate` verb.
        let path_dir = layout.prefix.join("older-path");
        exe(
            &path_dir.join("atpkg"),
            "#!/bin/sh\necho \"atpkg: unknown verb $1\" >&2\nexit 2\n",
        );
        // The embedded co-located atpkg: records the hand-over.
        let embedded = layout.prefix.join("bundle/atpkg");
        exe(&embedded, "#!/bin/sh\necho \"co-located: $*\"\nexit 0\n");
        // The twin as `reconcile_agents` lays it: the self-update block on claude's
        // rostered verbs, and nothing else ahead of the exports.
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
                    // No yolo block: this test strand-proofs the self-update
                    // hand-over alone (the yolo block's executed proof is in
                    // platform's twin tests).
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
            // Non-verbs: the store build, whatever atpkg is where.
            assert_eq!(
                run(&render(&embedded), &["--probe", "--", "x"]),
                store("--probe -- x"),
                "{label}"
            );
            // The self-update verbs as the first argument hand over to the embedded
            // atpkg, the arguments verbatim.
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
            // Not the first token: not intercepted — a prompt, a debug filter, a flag.
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
            // The embedded atpkg is gone: `update` runs the store build, silently; the
            // older atpkg on PATH is never consulted.
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
        // A marker an older client's pass left standing: today's twin never reads it.
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"atpkg-landing-v1 build=2026091702 pid=1\n").unwrap();
        check("a standing marker");
        // A twin an older client laid (the block, then the landing prelude) under that
        // marker still hands a non-verb to `__landing` — the verb that stays for it.
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
        // A regular file where `agents/` must be a directory: ensure_dir fails.
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
        // Clear the squatter: the next pass lays the twin.
        std::fs::remove_file(layout.agents_dir()).unwrap();
        reconcile_agents(&layout);
        let twin = layout.agent_shim(&claude);
        assert_eq!(
            platform::resolve_shim(&twin),
            platform::resolve_shim(&layout.shim(&claude))
        );
        // A `bin/codex` into a non-agent program's store tree: no twin laid — and the
        // sweep, which applies the same predicate, has nothing to remove.
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

    /// The alias policy is read off the SIGNED index entry: ALab's own (no `system`)
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
            // The alias shim carries the platform's shim suffix and its target the
            // primary's executable name — on Windows `alab-ay.cmd` → `…\bin\ay.exe`.
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

        // A vendor tool installed with the policy off: the plain name only.
        let codex = make_build(&layout, "codex", 7, &["codex"]);
        install_shims(&layout, &codex, &["codex".into()], Aliases::Off).unwrap();
        assert!(crate::platform::resolve_shim(&layout.shim(&tool("codex"))).is_some());
        assert!(
            std::fs::symlink_metadata(layout.shim(&tool("alab-codex"))).is_err(),
            "no alias for a vendor tool"
        );
        assert_eq!(Aliases::laid_for(&layout, "codex"), Aliases::Off);
        // The alias of a sensitive name is never laid, because it has no ToolName.
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

        // Build 19 drops aylint: its shim and its alias go; ay's alias moves to 19.
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

        // The policy flips to Off for ay (say its index entry grew a `system` key): a
        // re-shim of the same build sweeps ay's alias, keeps ay, and leaves ny's alias.
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

        // Back to Alab; then undo_activation unwinds the alias with the build.
        install_shims(&layout, &b19, &["ay".into()], Aliases::Alab).unwrap();
        assert!(crate::platform::resolve_shim(&shim_of(&layout, "alab-ay")).is_some());
        undo_activation(&layout, &b19);
        assert!(std::fs::symlink_metadata(shim_of(&layout, "ay")).is_err());
        assert!(
            std::fs::symlink_metadata(shim_of(&layout, "alab-ay")).is_err(),
            "undo takes the alias"
        );

        // And uninstall: every shim resolving into the program's store goes, alias included.
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
        // Installed by a client that predates aliases: primaries only.
        let b18 = make_build(&layout, "ay", 18, &["ay", "aylint"]);
        install_shims(&layout, &b18, &["ay".into(), "aylint".into()], Aliases::Off).unwrap();
        assert!(std::fs::symlink_metadata(shim_of(&layout, "alab-ay")).is_err());
        // A hand-made file already holds one alias name: it is not ours to replace.
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
        // Idempotent: a second reconcile rewrites nothing (the primary is untouched too).
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
        // Off sweeps the alias this manager laid, and only that.
        reconcile_aliases(&layout, &b18, &tools, Aliases::Off).unwrap();
        assert!(std::fs::symlink_metadata(shim_of(&layout, "alab-ay")).is_err());
        assert!(shim_of(&layout, "alab-aylint").exists());
        assert!(crate::platform::resolve_shim(&shim_of(&layout, "ay")).is_some());
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The Windows `.cmd` alias: the resume-proof frame, then a forward to the primary's
    /// `.exe` that ends the batch on that line. Pure, so every build host pins it.
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
