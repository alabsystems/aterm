// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Activation (§10): the atomic POSIX symlink swap that makes a staged store build the
//! live one, plus the `bin/` shim installation.
//!
//! Activation is **not** the app updater's in-session handoff (that is aterm-gui/
//! aterm-update's `.app` path). A CLI program's active build is selected by a symlink: `channels/<name>/current`
//! points at the chosen `store/<program>/<build>/`, and one `bin/<tool>` symlink per
//! exposed binary points into it. Each flip is an **atomic replace** — write a sibling
//! temp symlink, then `rename(2)` it over the target — so a reader never sees a missing or
//! half-written link, and a concurrent run (under `apply.lock`) cannot observe a torn
//! state. Every shim name is gated through [`crate::store::shim_allowed`]: a tool named
//! `sudo`/`ssh`/`git`/… is refused a shim and reported, never silently installed.
//!
//! # The `alab-<tool>` alias (owner decision 2026-08-27)
//!
//! ALab's bare tool names collide with other software — Homebrew's p11-kit installs a
//! certificate tool at `/opt/homebrew/bin/trust`, Homebrew core owns the formula names
//! `ty` and `clean` — and the managed `bin/` is deliberately APPENDED to `PATH` (a managed
//! tool never overrides what the user already had), so typing `trust` may run someone
//! else's copy. The PATH order stays. Instead every program that is ALab's OWN
//! ([`Aliases::Alab`]: its index entry has no `system` key) gets an
//! `alab-<tool>` shim beside every `<tool>` shim, forwarding to the SAME store executable.
//! Aliases are pruned, tombstoned, rolled back and uninstalled exactly like their primary
//! — they resolve into the same build, and every sweep in this crate keys on where a shim
//! resolves, not on its name. A vendor tool (`codex`, `claude`) or a system-satisfiable
//! member (`gh`, `emacs`) gets no alias: the alias exists to say "ALab's copy", and those
//! are not ALab's. A PENDING STUB is laid for the plain name only (`crate::stub`): the
//! alias exists to be unambiguous once installed, and a second promising name on `PATH`
//! before then would be noise.
//!
//! # The shim environment (design S7)
//!
//! A manifest may declare `shim_env = ["DISABLE_AUTOUPDATER=1"]` ([`crate::shim_env`]):
//! every shim of that program — primary and alias alike — then EXPORTS those variables
//! before it execs the store binary, so a managed vendor tool runs with its own updater
//! off. [`install_tools_env`] lays the shims with the env its caller holds (the signed
//! manifest on the install path; the build's `<build>.shim-env` sidecar on the verbs
//! that hold none), [`reassert_shim_env`] re-lays a primary whose exports have drifted
//! from that sidecar (the pass's half — an up-to-date program is never laid again),
//! [`reconcile_aliases`] mirrors the primary's env onto its alias, and every sweep below
//! keys on where a shim RESOLVES — the exec line — so an env-carrying shim is pruned,
//! tombstoned, rolled back and uninstalled exactly like a plain one.

use std::io;
use std::path::Path;

use crate::Layout;
use crate::platform::{self};
use crate::store::{ToolName, split_exposed};

/// Whether a program's exposed tools also get their `alab-<tool>` aliases (module doc).
/// Decided from the SIGNED index entry by [`Aliases::for_program`] on the install path,
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
    /// The policy for an index entry: ALab's own ⇔ no `system` key and not an AGENT
    /// PROGRAM (`claude`/`codex` are a vendor's, whatever their row says —
    /// [`crate::stub::AGENT_PROGRAMS`] — so they never grow an `alab-claude`). An UNLISTED
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

/// Atomically point `link` at `target`. The OS-specific indirection primitive:
/// [`crate::platform::atomic_symlink`] — a temp-symlink + `rename(2)` on POSIX (atomic,
/// no missing/half-written window), a directory **junction** on Windows. Re-exported here
/// (and via [`crate`]) so every managed directory symlink shares one entry point.
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    platform::atomic_symlink(target, link)
}

/// Make `build_dir` the active build: atomically flip BOTH pointers that select it —
/// `store/<program>/current → build_dir` and `channels/<channel>/current → build_dir`. The
/// channel directory is created hardened (`0700`, owned-by-uid) first. Idempotent —
/// re-activating the same build is a no-op-ish re-point.
///
/// **Two links, because one of them cannot answer the question GC asks.**
/// `channels/<channel>/current` is one symlink per channel and every program shares a channel
/// name (`[packages].channel`, default `stable`; a coherence group flips all its members
/// through the same one), so it holds only the LAST activation — `atpkg install ny` erases
/// `ay`'s pointer. That is fine for its actual job (`uninstall`'s dangling-link sweep), and
/// unusable as the per-program liveness witness [`crate::gc::live_builds`] needs: with the
/// channel link as the sole authority, every program but the most recently activated one has
/// no witness, GC abstains on it forever, and the store grows without bound. So the
/// per-program link is written too, and it is the one GC reads.
///
/// The per-program link goes FIRST. If it fails, nothing has flipped and the caller's "the
/// atomic activate didn't flip — nothing to undo" (`flow::flip_member`) still holds; if the
/// channel link then fails, the caller aborts and discards the staged build, leaving the
/// program link dangling — which resolves to no witness, so GC abstains rather than acting on
/// a half-activation.
pub fn activate_channel(layout: &Layout, channel: &str, build_dir: &Path) -> io::Result<()> {
    // The program name comes from the build dir's own place in the store, not from a
    // parameter, so the two links can never name different programs. A `build_dir` that is
    // not a store build dir (a synthetic fixture) simply gets no per-program link — and
    // therefore no GC witness, which is the fail-closed direction.
    if let Some((program, _)) = crate::ops::store_build_of(&layout.prefix, build_dir) {
        atomic_symlink(build_dir, &layout.program_current(&program))?;
    }
    let current = layout.channel_current(channel);
    let parent = current
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "channel path has no parent"))?;
    // THROUGH THE LAYOUT, so a root-owned SYSTEM prefix gets 0755 rather than 0700.
    // `Layout::ensure_dir` exists for exactly this and documents the failure it
    // prevents as observed: an unconditional 0700 installs a toolchain only root can
    // run, and it fails at the only moment that matters — the first non-root
    // invocation, with a bare "Permission denied". These three call sites were
    // reverted to the unconditional helper as collateral in a large rebase
    // (2026-08-20 round-8 audit).
    layout.ensure_dir(parent)?;
    atomic_symlink(build_dir, &current)
}

/// Install `bin/` shims for the manifest's raw `exposes` list, each pointing at the tool's
/// executable inside `<build_dir>/bin/`.
///
/// This is the crate's boundary between a `Vec<String>` off the wire and the validated
/// [`ToolName`]: a name that fails [`crate::store::shim_allowed`] (collides with a sensitive
/// command, or is malformed) never becomes a `ToolName` at all, so it is **skipped** and
/// returned for the caller to surface in `status.toml`. Returns the refused RAW names — the
/// report has to name what the manifest asked for, which is why the split happens here and
/// not inside the type. Empty when everything was installed.
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

/// [`install_shims`] whose shims export `env` before they exec (design S7) — what a
/// verb that re-lays an INSTALLED build passes its sidecar to
/// ([`crate::shim_env::read_sidecar`]), so a re-lay never strips what the signed manifest
/// declared (a `repair` that laid `claude` plain turned Claude Code's own updater back
/// on, 2026-09-23).
pub fn install_shims_env(
    layout: &Layout,
    build_dir: &Path,
    exposes: &[String],
    aliases: Aliases,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<Vec<String>> {
    let (tools, refused) = split_exposed(exposes);
    install_tools_env(layout, build_dir, &tools, aliases, env)?;
    Ok(refused)
}

/// The validated half of [`install_shims`]: install one shim per already-admitted
/// [`ToolName`] (plus its `alab-` alias under [`Aliases::Alab`]), then prune the shims a
/// previous build left behind. The `bin/` dir is created hardened first.
///
/// Split out (and `pub(crate)`) for the callers that already hold `ToolName`s — the
/// transaction flip in [`crate::flow`], whose staged member carries the admitted set — so the
/// raw list is not re-split, and so their refusal semantics cannot silently drift from this
/// one loop's.
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

/// [`install_tools`] whose shims — primary AND alias — export `env` before they exec
/// (design S7, module doc). The install path passes the signed manifest's
/// [`crate::manifest::PkgManifest::shim_env`]; the verbs that hold no manifest pass the
/// build's sidecar ([`crate::shim_env::read_sidecar`]). An empty `env` is exactly
/// [`install_tools`].
pub(crate) fn install_tools_env(
    layout: &Layout,
    build_dir: &Path,
    tools: &[ToolName],
    aliases: Aliases,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    let bin = layout.bin_dir();
    layout.ensure_dir(&bin)?;
    // THE EXEC ROOT BEFORE THE FIRST SHIM THAT NAMES THE BUILD (`crate::compat`). A trust
    // build that ships `bin/rustc` as a separate copy of `trustc` (bundles 8571, 8589,
    // 8590, 8595) has a tippy that refuses to run from the store; the renders below route
    // through `<prefix>/compat/trust/<n>` exactly when a whole root stands there, so it
    // has to stand before they are rendered — which also puts it ahead of the `current`
    // flip on every lane that ends here: install, the transaction flip, repair, seed,
    // flow's restore and linkmode's unlink (a rollback re-lays through
    // `flow::rollback_member`, which ensures the prior build's root itself). `Shallow`: a
    // few dozen `lstat`s when the root is already whole, and ZERO writes (a tippy running
    // from it pins its own link counts and ctimes). Every other build answers `Plain` without
    // creating anything, and a build without the copy keeps today's shims byte for byte. A
    // root that cannot be put in order costs this lay nothing: the shims render through
    // whatever whole root already stands, or plain — running the store path as they did
    // before this existed — and the pass's own reconcile retries; behind a link or a file
    // at `compat` or `compat/trust` no retry can, and `compat::not_laid_line` names that
    // path and the manual fix instead of promising `repair`. A root laid here is SAID:
    // it is the one moment the machine changes shape, and `repair` over a missing or
    // tampered root otherwise printed only "done".
    if let Some(n) = crate::compat::trust_build_of(layout, build_dir) {
        match crate::compat::ensure_root(layout, build_dir, crate::seam::Depth::Shallow) {
            Ok(crate::compat::Ensured::Built) => {
                println!("{}", crate::compat::laid_line(layout, n))
            }
            Ok(crate::compat::Ensured::Plain | crate::compat::Ensured::Present) => {}
            Err(e) => eprintln!("{}", crate::compat::not_laid_line(layout, n, &e)),
        }
    }
    // Rendered first, then laid: every primary and — under [`Aliases::Alab`] — its
    // `alab-<tool>` alias forwarding to the SAME executable. The alias is the primary's
    // target under the alias's file name — the pair [`platform::shim_executable_env`] keeps
    // apart by taking the target's [`ToolName`] and the shim path separately.
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
    prune_stale_shims(layout, build_dir, tools, aliases);
    // The front-of-PATH twin of an agent program's shim (owner decision 2026-09-10),
    // laid from the `bin/` shim just written so the two can never disagree.
    reconcile_agents(layout);
    Ok(())
}

/// Bring `<prefix>/agents/` ([`Layout::agents_dir`]) in line with `bin/`: for every AGENT
/// PROGRAM ([`crate::stub::AGENT_PROGRAMS`]) whose `bin/` shim resolves into the store,
/// lay — or refresh — the twin `agents/<tool>` with the SAME target and the SAME exported
/// environment (read off the `bin/` shim as laid, never re-derived, for the reason
/// [`reconcile_aliases`] gives) plus the self-update block ([`crate::selfupdate`]),
/// then sweep everything else out of `agents/` ([`sweep_agents_dir`]). Idempotent: a
/// twin that already resolves where the primary does with the same env and the bytes the
/// twin renderer lays ([`twin_is_rendered`]) is left alone, so the six-hourly tick
/// rewrites nothing.
///
/// Called at the end of every shim-laying pass — [`install_tools_env`] (hence every
/// install and `repair`), flow's undo, linkmode's link/unlink, and ONCE at the end of
/// `cli::reconcile_aliases`, after its per-program loop — so an agent program installed
/// by an older client gains its twin the first pass after this one lands, and an
/// uninstalled or tombstoned one loses it in the same motion.
///
/// ONCE PER PASS, NEVER ONCE PER PROGRAM. Nothing here is a function of any one program:
/// it walks [`crate::stub::AGENT_PROGRAMS`] and sweeps `agents/` whatever the caller was
/// reconciling. It used to hang off the end of [`reconcile_aliases`], which the pass runs
/// for EVERY active program, so a twelve-program machine paid twelve whole agents
/// reconciles per six-hourly tick (audit 2026-09-15).
///
/// BEST-EFFORT, like [`sweep_agents_dir`]: a twin that cannot be laid — `agents/`
/// uncreatable, the link refused — is reported on stderr and the pass goes on, because
/// this runs inside EVERY program's install and an `ay` or `trust` install must not fail
/// over a directory only `claude` and `codex` use (review 2026-09-10). The `bin/` shim
/// is already laid by then; the twin is retried on the next pass.
///
/// The keep-predicate is [`sweep_agents_dir`]'s: the primary must resolve into an AGENT
/// PROGRAM's store tree — a `bin/claude` into some other program's tree earns no twin,
/// which is what the sweep would have removed again anyway.
pub fn reconcile_agents(layout: &Layout) {
    for name in crate::stub::AGENT_PROGRAMS {
        let Some(tool) = ToolName::new(name) else {
            continue;
        };
        let primary = layout.shim(&tool);
        let Some(target) = platform::resolve_shim(&primary).filter(|t| {
            crate::ops::store_build_of(&layout.prefix, t)
                .is_some_and(|(program, _)| crate::stub::is_agent_program(&program))
        }) else {
            continue; // not installed here (a pending stub, a tombstone, a dev link): the sweep answers
        };
        let env = platform::shim_env_of(&primary);
        let twin = layout.agent_shim(&tool);
        // THE TWIN'S PRELUDE, rendered here, the one place the twin is laid, from the
        // co-located `atpkg` this process runs as ([`platform::twin_prelude`]): the
        // self-update block (2026-09-19, [`crate::selfupdate`]) — `case "$1" in
        // update|upgrade|install)` on the program's rostered verbs, so a `claude update`
        // typed on the managed name is answered by `atpkg __selfupdate` and never by the
        // vendor's own updater, which installs a copy this name never runs. No landing
        // prelude since Phase 2 (2026-09-22, [`crate::landing`]): a twin that still
        // carries one compares unequal below and is re-laid once.
        let prelude = platform::twin_prelude(
            name,
            &layout.prefix,
            &crate::stub::embedded_atpkg_path(),
            crate::selfupdate::verbs_of(name),
        );
        // Left alone only when it resolves where the primary does, exports the same
        // environment, and its BYTES are what the renderer lays now ([`twin_is_rendered`]):
        // a twin that forwards and exports right but renders differently — laid before
        // an exec root stood for its build, by a client whose shim text differed, or by
        // one that laid the landing prelude — would otherwise be kept forever by a
        // predicate that reads only its target.
        if platform::resolve_shim(&twin).is_some_and(|t| t == target)
            && platform::shim_env_of(&twin) == env
            && twin_is_rendered(&twin, &target, &env, &prelude)
        {
            continue;
        }
        let laid = layout
            .ensure_dir(&layout.agents_dir())
            .and_then(|()| platform::install_twin_to_env(&twin, &target, &env, &prelude));
        if let Err(e) = laid {
            eprintln!(
                "atpkg: {name}: the agents/ twin {} was not laid ({e}) — bin/{name} is in \
                 place; retried next pass",
                twin.display()
            );
        }
    }
    sweep_agents_dir(layout);
}

/// [`shim_is_rendered`] for an `agents/` twin: the bytes the TWIN renderer lays for
/// `target`, `env` and its `prelude` ([`platform::twin_executable_to_env`]).
fn twin_is_rendered(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> bool {
    // On every platform: a Windows twin laid from 2026-09-17 to 2026-09-22 carries the
    // landing prelude, and today's `.cmd` twin is the plain framed shim, so it is re-laid
    // once. Safe while it runs: every `.cmd` starts with the frame
    // (`platform::CMD_FRAME_HEAD`) and ends its batch on the line that runs the program,
    // so `cmd.exe`'s byte-offset resume never re-reads the new file. Unverified on Windows.
    platform::twin_executable_to_env(shim, target, env, prelude).is_ok_and(|want| {
        crate::metadata_io::read_bounded_regular(shim, platform::MAX_SHIM_BYTES)
            .is_ok_and(|have| have == want.body)
    })
}

/// Whether the shim at `shim` holds, byte for byte, what the one shim renderer
/// (`platform::shim_executable_to_env`) lays for `target` and `env` NOW — the half of the
/// alias and agents reconciles' "already right" that target and environment cannot see.
///
/// The render is not a function of target and environment alone: on Unix it carries the
/// guard line of an exec root when [`crate::compat::route_for_shim`] finds one standing
/// for the build. So a shim laid before that root stood (by an older client, or by a lay
/// whose root could not be built then) forwards and exports exactly right and is still
/// wrong, and a predicate reading only those two kept it on the store path — where the
/// build's own tippy refuses to start — for as long as the program stayed up to date. The
/// render is `stat`s and string building, no write; the read is one bounded, non-link
/// regular file, so a symlink an older atpkg left, or anything unreadable, reads as not
/// rendered and is re-laid the ordinary way (over the foreign-file guard where one
/// applies). A second pass over what the first laid compares equal and writes nothing.
///
/// Windows answers `true`: no exec root exists there, every render is the plain one, and
/// the reconciles keep today's target-and-environment predicate unchanged.
fn shim_is_rendered(shim: &Path, target: &Path, env: &crate::shim_env::ShimEnv) -> bool {
    if cfg!(not(unix)) {
        return true;
    }
    platform::shim_executable_to_env(shim, target, env).is_ok_and(|want| {
        crate::metadata_io::read_bounded_regular(shim, platform::MAX_SHIM_BYTES)
            .is_ok_and(|have| have == want.body)
    })
}

/// Keep `agents/` holding ONLY live agent shims: an entry stays iff its name is an agent
/// program's, it resolves into that program's store tree, and the `bin/` shim of the same
/// name resolves to the SAME target. Everything else — a name that is not an agent
/// program's, a twin of a build the primary has moved off (rolled back, updated), a twin
/// whose primary is now a tombstone or gone (uninstalled), a hand-dropped file — is
/// removed: this directory is FIRST on every `PATH`, so nothing may sit in it that
/// `bin/` does not vouch for. Best-effort and silent, like every sweep here.
pub fn sweep_agents_dir(layout: &Layout) {
    let Ok(entries) = std::fs::read_dir(layout.agents_dir()) else {
        return;
    };
    let installed = crate::ops::active_builds(layout);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let keep = name
            .to_str()
            .and_then(ToolName::from_shim_file)
            .is_some_and(|tool| {
                crate::stub::is_agent_program(tool.as_str())
                    && (platform::resolve_shim(&entry.path()).is_some_and(|t| {
                        crate::ops::store_build_of(&layout.prefix, &t)
                            .is_some_and(|(p, _)| crate::stub::is_agent_program(&p))
                            && platform::resolve_shim(&layout.shim(&tool)).is_some_and(|b| b == t)
                    })
                        // An agent program's PENDING stub stands here too (2026-09-15),
                        // until its build arrives and the twin replaces it.
                        || (crate::stub::is_pending_stub(&entry.path())
                            && !installed.contains_key(tool.as_str())))
            });
        if !keep {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Bring the ALIASES of an already-installed program in line with `aliases` without
/// touching its primary shims: under [`Aliases::Alab`] lay the missing `alab-<tool>` — or
/// re-lay one whose bytes are not what the renderer lays now ([`shim_is_rendered`]) — for
/// every `tool` whose shim resolves into `build_dir`; under [`Aliases::Off`] sweep any
/// alias that resolves into this program's store. The primary shims are read, never
/// written, so an up-to-date program — which the install pipeline short-circuits before it
/// ever reaches [`install_tools`] — still gets its aliases the first pass after this
/// client lands, and a program whose index entry stops being ALab's own loses them.
/// Nothing here touches a dev-linked program's checkout shims (they resolve outside the
/// store) or a pending stub (it resolves nowhere).
///
/// ALIASES ONLY: `agents/` is NOT reconciled here. This runs once per active program and
/// [`reconcile_agents`] answers for none of them in particular, so the pass calls it once
/// after its loop (`cli::reconcile_aliases`) — see that function's doc for what the
/// per-program repeat cost.
pub(crate) fn reconcile_aliases(
    layout: &Layout,
    build_dir: &Path,
    tools: &[ToolName],
    aliases: Aliases,
) -> io::Result<()> {
    if aliases == Aliases::Alab {
        for tool in tools {
            let Some(alias) = tool.alias() else { continue };
            // The alias mirrors the PRIMARY's forward target as it is spelled on disk —
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
            // Already right (an alias resolving exactly where the primary does, with the
            // same environment, whose bytes are what the renderer lays now) is left alone,
            // so the six-hourly tick rewrites nothing. The byte half is what reaches an
            // alias an older client laid PLAIN for a trust build that now has an exec
            // root (`crate::compat`): the pass short-circuits the up-to-date program before
            // any install renders it, and target + environment alone matched — so
            // `alab-tippy` stayed on the store path its own tippy refuses, forever.
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

/// The PRIMARY shims of one build whose exported environment is not the one that build
/// DECLARES ([`crate::shim_env::read_sidecar`], design S7) — what [`shim_env_drift`]
/// found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvDrift {
    /// What the build's sidecar declares — never empty.
    pub(crate) declared: crate::shim_env::ShimEnv,
    /// Each drifted primary: its `bin/` path and the target it forwards to, as spelled
    /// on disk.
    pub(crate) shims: Vec<(std::path::PathBuf, std::path::PathBuf)>,
}

/// THE ONE DRIFT PREDICATE: among `tools`, the primaries whose shim resolves into
/// `build_dir` and exports anything other than exactly what the build's sidecar declares.
/// The pass heals what this finds ([`reassert_shim_env`]) and the doctor names it (check
/// 10f), so the two can never disagree about which shim is wrong.
///
/// `None` when nothing drifted — and always when the build declares NO environment (no
/// sidecar: staged before S7, or a policy that declares none). That is the add-only half
/// of the rule: an absent sidecar is no evidence the env a shim exports should go, so a
/// shim's own exports stand. A build that DOES declare one is authoritative: its primaries
/// export exactly that, no less and no other.
///
/// Only a shim that resolves into `build_dir` is looked at — a tombstone, a pending stub
/// and a dev link resolve elsewhere, or nowhere. Reads only: one bounded sidecar read,
/// then one bounded read per tool; nothing lists `bin/`.
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

/// Re-lay every PRIMARY shim of `builds` (each a build dir and its active tools) that
/// [`shim_env_drift`] finds exporting other than its build declares, with exactly the
/// declared environment — the pass's half of the rule `install` and `repair` keep.
///
/// WHY THE PASS NEEDS IT. An up-to-date program is never laid again: the index lane's
/// `UpToDate` and the vendor lane's `KeepReason::UpToDate` both return before any shim is
/// rendered. So a `claude` shim laid PLAIN over a build whose sidecar declares
/// `DISABLE_AUTOUPDATER=1` — every `repair` before 2026-09-23 laid it so — stayed plain
/// until the program's next build landed (for a held `claude`, forever), and
/// [`reconcile_agents`] mirrored it onto the front-of-PATH twin: Claude Code's own updater
/// back on under the managed name, pass after pass, with nothing said (audit
/// 2026-09-23). The exec roots got [`crate::compat::reconcile`] for the same reason.
///
/// The target is the primary's own, as spelled on disk — never a re-derived path, for the
/// reason [`reconcile_aliases`] gives — and the render goes through the one shim renderer,
/// so a routed trust shim stays routed. Idempotent: a primary that already exports its
/// build's declaration
/// is never rewritten, so the six-hourly tick writes nothing once it is healed.
/// Best-effort like every reconcile here: a lay that fails is said on stderr and retried
/// next pass; one healed is said on stdout, once.
///
/// Called by the pass BEFORE its per-program alias reconcile and its agents reconcile,
/// which both copy the primary's exports — so the alias and the twin carry the healed
/// environment in the same pass.
pub(crate) fn reassert_shim_env(layout: &Layout, builds: &[(&Path, &[ToolName])]) {
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
                    "atpkg: {} does not export {}, which its build declares, and could not be \
                     re-rendered ({e}) — retried next pass",
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
    match files.iter().try_for_each(crate::lay::write_in_process) {
        Ok(()) => {
            for (path, env) in paths.iter().zip(&said) {
                println!(
                    "atpkg: {} re-laid — it exports {env} again, as its build declares",
                    path.display()
                );
            }
        }
        Err(e) => eprintln!(
            "atpkg: {} shim(s) that do not export what their build declares were not re-laid \
             ({e}) — retried next pass",
            paths.len()
        ),
    }
}

/// Remove `bin/` shims this program owns that still point at a DIFFERENT build — the tools
/// a newer build dropped from its `exposes` — and every ALIAS of this program's that is no
/// longer wanted (`aliases` is [`Aliases::Off`], or its base names neither `installed` nor
/// any other tool of THIS build), whatever build it points at.
///
/// Why this must exist: `install_shims` only writes the names the NEW build exposes, so a
/// dropped tool's shim survives pointing into the OLD build. `ops::active_builds` then folds
/// every shim into one entry per program (last write wins), so the stale name can report the
/// OLD build as active — and `gc::run`, which cli.rs invokes immediately after every install
/// and update, reclaims the "superseded" LIVE build. It is also why a yanked build's dropped
/// tool could never be tombstoned: `install_tombstone_shims` only revokes tools pointing at
/// the current build.
///
/// The predicate is deliberately narrow, because this DELETES files on the user's PATH. A
/// shim is removed only when it resolves into `<this prefix>/store/<this program>/<other
/// build>/` — the exact shape this function itself creates — or, for an alias, into
/// `<this prefix>/store/<this program>/` at all. Anything else is left alone: another
/// program's shims, a dev-link, a tombstone, a hand-made file, or any target outside the
/// store. The containment test is [`crate::ops::store_build_of`], which is ANCHORED to the
/// prefix; the unanchored `program_build_of_target` used here before answered `("ay", 18)`
/// for a dev-link into `~/src/store/ay/18/bin/ay`, i.e. it would have deleted a link into a
/// tree this manager does not own.
fn prune_stale_shims(layout: &Layout, build_dir: &Path, installed: &[ToolName], aliases: Aliases) {
    let Some((program, build)) = crate::ops::store_build_of(&layout.prefix, build_dir) else {
        return; // not a store build dir of ours — nothing to prune
    };
    let Ok(entries) = crate::ops::read_bin_dir(layout) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // A `bin/` entry is a FILE name; `installed` holds LOGICAL names. Comparing the two
        // directly — which is what this guard did — is the identity on Unix and always false
        // on Windows, where the entry reads `ay.cmd`: the guard never fired, so every shim
        // this call had just written was a deletion candidate. `from_shim_file` strips the
        // suffix so the comparison is logical-to-logical on both platforms. Its `None` (an
        // entry no `ToolName` could name, so nothing we could have written) also skips —
        // fail-closed is the right direction for the one predicate here that deletes.
        let Some(tool) = crate::store::ToolName::from_shim_file(name) else {
            continue;
        };
        // Never remove a name the new build still exposes — that shim was just written —
        // nor the alias just written beside it.
        if installed.contains(&tool) {
            continue;
        }
        // An alias is wanted when its base was just laid — or when the base is a tool of
        // THIS SAME build that this pass simply did not name. A partial set is the normal
        // shape on the unlink restore (`cli::restore_installed_shims` lays only the names
        // the dev link's marker owned, which `default_link_bins` makes the single
        // `target/release/<program>`), and the rule below already leaves every other tool's
        // plain shim standing because it names this build. The alias of a shim that stands
        // must stand too: without this, restoring a one-name link over a multi-tool program
        // deleted `alab-aylint` — the unambiguous name, which is the whole point of the
        // alias where Homebrew shadows the bare one — off a tool the restore never touched,
        // until some later `reconcile_aliases` pass laid it again. The restore's contract
        // is that it neither sweeps a store alias nor invents one.
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
        // A plain shim is stale at a DIFFERENT build of this program; an alias whose base
        // is wanted nowhere — not in this lay, not as a live tool of this build — is stale
        // at ANY build of it, the same build included, which is the shape a policy flip
        // (`Alab` → `Off`) or a dropped base leaves behind.
        if crate::ops::store_build_of(&layout.prefix, &target)
            .is_some_and(|(p, b)| p == program && (b != build || tool.is_alias()))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Best-effort undo of [`activate_channel`] plus a partial [`install_tools`] pass, for a
/// build that is about to be DISCARDED. An abort path that deletes a build AFTER activation
/// succeeded (the sysroot resolve-check discard in `flow::install_program`) must call this
/// first, or the deleted tree
/// stays live everywhere that matters: both `current` links dangle into it (and a broken
/// per-program link makes GC abstain on the program until the next activation), and any
/// shims already written this pass point at nothing.
///
/// Scoped strictly to THIS build: each `current` link is removed only if it names
/// `build_dir` (one that points elsewhere — a prior build, a concurrent flip — is left
/// alone), and only `bin/` entries resolving INTO `build_dir` are dropped. Removal goes
/// through [`platform::remove_link`] for the links (a Windows junction refuses
/// `remove_file`) and `remove_file` for the shims (a Windows shim is a `.cmd` regular
/// file).
pub(crate) fn undo_activation(layout: &Layout, channel: &str, build_dir: &Path) {
    if let Some((program, _)) = crate::ops::store_build_of(&layout.prefix, build_dir) {
        let own = layout.program_current(&program);
        if std::fs::read_link(&own).is_ok_and(|t| t == build_dir) {
            platform::remove_link(&own);
        }
    }
    let chan = layout.channel_current(channel);
    if std::fs::read_link(&chan).is_ok_and(|t| t == build_dir) {
        platform::remove_link(&chan);
    }
    if let Ok(entries) = crate::ops::read_bin_dir(layout) {
        for e in entries.flatten() {
            if crate::platform::resolve_shim(&e.path()).is_some_and(|t| t.starts_with(build_dir)) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    // A twin whose primary just went is a twin with nothing to vouch for it.
    sweep_agents_dir(layout);
}

/// Install a **failing tombstone shim** at `bin/<tool>` — a tiny script that prints a
/// yanked/revoked notice to stderr and exits nonzero — so a revoked build's OLD working shim
/// is actively DISABLED, not left runnable (§7). Written atomically (temp + `rename(2)`), so a
/// reader never sees a half-written script; the `rename` replaces the prior *symlink* shim
/// in place.
///
/// A tombstone is still a shim, so it must never shadow a sensitive name either — which is now
/// a property of the argument type rather than a repeated `shim_allowed` call: a tool named
/// `sudo`/`git`/… has no [`ToolName`], never had a live shim to disable, and cannot be named
/// here at all. (This is why the function no longer returns "refused".)
///
/// A later successful `atpkg update` re-runs [`install_shims`], whose `atomic_symlink` replaces
/// this regular-file tombstone with a fresh symlink, so the disable clears itself on recovery.
pub fn install_tombstone_shim(layout: &Layout, tool: &ToolName) -> io::Result<()> {
    let bin = layout.bin_dir();
    layout.ensure_dir(&bin)?;
    let shim = layout.shim(tool);

    // The failing-shim message. The tool-bearing text is the only variable part; the
    // platform backend embeds it injection-safely (Unix: a single-quoted `printf` arg;
    // Windows: a `cmd`-escaped `echo`). Built with `push_str` (no `format!`, Trust gate).
    let mut message = String::from("atpkg: ");
    message.push_str(tool.as_str());
    message.push_str(" was yanked/revoked — run `aterm pkg update`");
    // Atomic install through the platform backend (Unix: an executable `sh` script
    // temp+rename; Windows: a `.cmd` batch wrapper), replacing whatever shim was there.
    platform::install_tombstone_shim(&shim, &message)?;
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

    /// A prefix whose WHOLE chain is root-owned, or `None` when this run cannot build one.
    /// Only root can create a directory under a root-owned parent, and even root cannot
    /// under a SIP-protected `/usr`, so the caller SKIPS rather than fails — the
    /// `$HOME`-shape half of the property is what runs unprivileged.
    ///
    /// The candidates are ordinary root-owned system dirs; the first that both reads as the
    /// system shape (a brew-owned `/usr/local` does not) and accepts a `mkdir` wins.
    #[cfg(unix)]
    /// A skip here must be provably legitimate: callers skip by returning, which libtest
    /// reports as `ok`, so a fixture that quietly stops working deletes their coverage
    /// without failing anything. Only root can create in a root-owned parent, so an
    /// ordinary user's `None` is a fact about the machine; from root it is a regression,
    /// which [`system_fixture_is_available_when_this_process_could_build_one`] asserts.
    fn system_prefix_fixture(label: &str) -> Option<Layout> {
        for parent in ["/opt", "/usr/local", "/var/lib", "/usr/lib"] {
            let prefix =
                Path::new(parent).join(format!("atpkg-act-{label}-{}", std::process::id()));
            let layout = Layout { prefix };
            if !layout.is_system_prefix() {
                continue; // the parent chain is not root-owned — wrong shape, keep looking
            }
            let _ = std::fs::remove_dir_all(&layout.prefix);
            if std::fs::create_dir(&layout.prefix).is_err() {
                continue; // not root, or the parent refuses writes even to root
            }
            // `create_dir` applies the umask, so re-state the mode: the prefix itself must
            // stay non-group/other-writable or it is no longer the system shape.
            let shaped =
                std::fs::set_permissions(&layout.prefix, std::fs::Permissions::from_mode(0o755))
                    .is_ok()
                    && layout.is_system_prefix();
            if !shaped {
                let _ = std::fs::remove_dir_all(&layout.prefix);
                continue;
            }
            return Some(layout);
        }
        None
    }

    fn temp_prefix(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-act-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    fn tool(name: &str) -> ToolName {
        ToolName::new(name).unwrap()
    }

    /// `bin/<name>` for a name the test knows is admissible.
    fn shim_of(layout: &Layout, name: &str) -> PathBuf {
        layout.shim(&tool(name))
    }

    /// The `bin/` path a shim for `name` WOULD occupy — spelled out by hand because the
    /// callers of this are the refusal tests, where `ToolName::new` returns `None` and so
    /// `Layout::shim` cannot name the file at all. That is the property under test.
    fn refused_shim_path(layout: &Layout, name: &str) -> PathBuf {
        layout
            .bin_dir()
            .join(format!("{name}{}", crate::platform::SHIM_SUFFIX))
    }

    fn make_build(layout: &Layout, program: &str, build: u64, bins: &[&str]) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        for b in bins {
            // The concrete executable name a shim forwards to (`<b>.exe` on Windows). Spelled
            // out rather than routed through `ToolName::exe_file` on purpose: a build tree may
            // legitimately ship a binary whose name is refused a SHIM (the `sudo` fixture
            // below), and this fixture is laying down the build, not naming a shim.
            let name = format!("{b}{}", crate::platform::EXE_SUFFIX);
            std::fs::write(dir.join("bin").join(name), b"#!/bin/true\n").unwrap();
        }
        dir
    }

    #[test]
    fn a_dropped_tool_leaves_no_stale_shim_pointing_at_the_prior_build() {
        // The upstream drops `aylint` between 18 and 19. Before the prune, `bin/aylint`
        // kept resolving into build 18, `ops::active_builds` last-write-wins picked 18 as
        // ACTIVE (aylint sorts after ay), and the very next `gc::run` deleted the LIVE
        // build 19 — self-destruct inside a single install verb.
        let layout = temp_prefix("stale-shim");
        let b18 = make_build(&layout, "ay", 18, &["ay", "aylint"]);
        install_shims(&layout, &b18, &["ay".into(), "aylint".into()], Aliases::Off).unwrap();
        activate_channel(&layout, "stable", &b18).unwrap();

        let b19 = make_build(&layout, "ay", 19, &["ay"]);
        install_shims(&layout, &b19, &["ay".into()], Aliases::Off).unwrap();
        activate_channel(&layout, "stable", &b19).unwrap();

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

    #[cfg(unix)]
    #[test]
    fn the_prune_never_touches_another_programs_shims_or_foreign_files() {
        // The prune deletes files on the user's PATH, so its blast radius is the property
        // that matters most. Only THIS program's shims at a DIFFERENT build may go.
        let layout = temp_prefix("prune-scope");
        let ay18 = make_build(&layout, "ay", 18, &["ay", "aylint"]);
        install_shims(
            &layout,
            &ay18,
            &["ay".into(), "aylint".into()],
            Aliases::Off,
        )
        .unwrap();
        let ny7 = make_build(&layout, "ny", 7, &["ny"]);
        install_shims(&layout, &ny7, &["ny".into()], Aliases::Off).unwrap();

        // A tool the user installed themselves, pointing outside the store entirely.
        let outside = layout.prefix.join("hand-made");
        std::fs::write(&outside, b"#!/bin/true\n").unwrap();
        std::os::unix::fs::symlink(&outside, shim_of(&layout, "mytool")).unwrap();
        // A plain regular file in bin/ (a tombstone shim has this shape).
        std::fs::write(shim_of(&layout, "tombstoned"), b"#!/bin/sh\nexit 1\n").unwrap();
        // A dev-link into a CHECKOUT outside the prefix that happens to carry a
        // `store/ay/<n>/` tail. The unanchored parser reads this as ay@18 — so the prune
        // would delete a link into a tree the manager does not own; the anchored one sees it
        // is not under `<prefix>/store` at all.
        let devco = layout
            .prefix
            .parent()
            .unwrap()
            .join(format!("atpkg-devco-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&devco);
        let checkout = devco.join("store/ay/18/bin");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("aydev"), b"#!/bin/true\n").unwrap();
        std::os::unix::fs::symlink(checkout.join("aydev"), shim_of(&layout, "aydev")).unwrap();

        let ay19 = make_build(&layout, "ay", 19, &["ay"]);
        install_shims(&layout, &ay19, &["ay".into()], Aliases::Off).unwrap();

        assert!(
            !shim_of(&layout, "aylint").exists(),
            "ay's dropped tool is pruned"
        );
        assert!(
            shim_of(&layout, "ay").exists(),
            "the re-shimmed tool survives"
        );
        assert!(
            shim_of(&layout, "ny").exists(),
            "another program is untouched"
        );
        assert!(
            shim_of(&layout, "mytool").exists(),
            "a shim outside the store is untouched"
        );
        assert!(
            shim_of(&layout, "tombstoned").exists(),
            "a non-symlink in bin/ is untouched"
        );
        assert!(
            shim_of(&layout, "aydev").exists(),
            "a dev-link whose target merely LOOKS like store/ay/18 is untouched"
        );
        let _ = std::fs::remove_dir_all(&devco);
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// [`prune_stale_shims`] is the one predicate in this module that removes files from the
    /// user's `PATH`, so this enumerates the shapes a real `bin/` holds and pins exactly
    /// which of them a prune of `ay@19` removes and which it leaves. A change that puts the
    /// prune on the shared `bin/` listing ([`crate::ops::BinScan`]) cannot widen the blast
    /// radius without failing here.
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
        lay("aydoc", &b19, "aydoc"); // a tool of THIS build the pass did not name
        lay("aylint", &b18, "aylint"); // dropped by 19 — stale at the OLD build
        lay("alab-ay", &b19, "ay"); // alias whose base was just laid
        lay("alab-aydoc", &b19, "aydoc"); // alias whose base is a live tool of this build
        lay("alab-aylint", &b18, "aylint"); // alias of a dropped tool
        lay("ny", &n7, "ny"); // another program's shim
        lay("alab-ny", &n7, "ny"); // another program's alias
        // A dev link into a checkout that happens to carry a `store/ay/18/bin/ay` tail: an
        // unanchored parse answers `("ay", 18)` for it, so a prune using that would delete a
        // link into a tree this manager does not own. `store_build_of` is anchored.
        let checkout = layout.prefix.join("checkout/store/ay/18/bin");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("ay"), b"#!/bin/true\n").unwrap();
        std::os::unix::fs::symlink(checkout.join("ay"), shim_of(&layout, "devlink")).unwrap();
        // A hand-made file, a name no `ToolName` admits, and a tombstone (forwards nowhere).
        std::fs::write(layout.bin_dir().join("hand"), b"mine\n").unwrap();
        std::fs::write(refused_shim_path(&layout, "sudo"), b"mine\n").unwrap();
        install_tombstone_shim(&layout, &tool("tombstoned")).unwrap();

        prune_stale_shims(&layout, &b19, &[tool("ay")], Aliases::Alab);

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
    fn activate_channel_points_current_at_build_and_re_flips() {
        let layout = temp_prefix("chan");
        let b18 = make_build(&layout, "ay", 18, &["ay"]);
        activate_channel(&layout, "stable", &b18).unwrap();
        let cur = layout.channel_current("stable");
        assert_eq!(std::fs::read_link(&cur).unwrap(), b18);
        // It resolves to a real directory.
        assert!(std::fs::metadata(&cur).unwrap().is_dir());

        // Re-flip to a newer build — atomic re-point, no leftover temp.
        let b19 = make_build(&layout, "ay", 19, &["ay"]);
        activate_channel(&layout, "stable", &b19).unwrap();
        assert_eq!(std::fs::read_link(&cur).unwrap(), b19);
        // No stray temp symlinks left in the channel dir.
        let leftovers: Vec<_> = std::fs::read_dir(cur.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "no temp symlink should remain");
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The regression for the reason the per-program link exists: two programs share one
    /// channel name, so the channel link only ever remembers the last activation. Each
    /// program must still be able to say which of ITS builds is live — otherwise
    /// `gc::live_builds` proves nothing about `ay` and its superseded builds are never
    /// reclaimed.
    #[test]
    fn two_programs_on_one_channel_each_keep_their_own_current() {
        let layout = temp_prefix("two-progs");
        let ay19 = make_build(&layout, "ay", 19, &["ay"]);
        activate_channel(&layout, "stable", &ay19).unwrap();
        let ny7 = make_build(&layout, "ny", 7, &["ny"]);
        activate_channel(&layout, "stable", &ny7).unwrap();

        // The shared channel link holds only the LAST activation — this is not a bug in
        // activation, it is what one-link-per-channel means.
        assert_eq!(
            std::fs::read_link(layout.channel_current("stable")).unwrap(),
            ny7
        );
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
        activate_channel(&layout, "stable", &ay20).unwrap();
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
        // sudo is refused (sensitive), reported with the RAW name the manifest asked for,
        // and NOT shimmed — even though the build tree does ship a `bin/sudo`.
        assert_eq!(refused, vec!["sudo".to_string()]);
        let sudo = refused_shim_path(&layout, "sudo");
        assert!(!sudo.exists() && std::fs::symlink_metadata(&sudo).is_err());
        // ay + ny shims exist and resolve into the build's bin/. resolve_shim reads the
        // forward target cross-platform (symlink target on Unix, the `.cmd` target — the
        // exe-suffixed concrete binary — on Windows).
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

    /// ATERM-SPEC'S DISCOVERY READS EVERY SHIM ATPKG LAYS. `aterm_spec::verify`'s store
    /// tier finds `ty`/`ay`/`trust-ir` for a process whose PATH lacks `<prefix>/bin` (a
    /// `targo test`, CI, launchd). It mirrors this crate's shim format rather than depending
    /// on it, and it had drifted: it accepted only a SYMLINK, and atpkg has laid exec stubs
    /// since `targo` began refusing a symlinked `current_exe`, so the tier never answered
    /// (measured on m3, 2026-09-23: 0 of 62 `bin/` entries are symlinks). Every shape is laid
    /// here through atpkg's own writer and read back through aterm-spec's reader, so the next
    /// change to either side is a red test:
    /// * the plain stub, a stub exporting an environment, and a stub routed through an exec
    ///   root all resolve to the STORE file (the first `exec '` line);
    /// * a tombstone, a pending-program stub, and a live stub whose build was reclaimed all
    ///   read as ABSENT — the probe runs before PATH, so any of them answering would shadow
    ///   a working tool.
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
        // A LIVE forwarding shim (a symlink on Unix, a forwarding `.cmd` on Windows).
        assert!(
            crate::platform::resolve_shim(&shim).is_some(),
            "live shim forwards into the build"
        );

        // Tombstone it: the forwarding shim is REPLACED by a failing regular-file script.
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

        // A sensitive name is refused a tombstone (never shadows a core command). That is no
        // longer a runtime `Ok(false)` — `install_tombstone_shim(&layout, /* sudo */)` does
        // not COMPILE, because `ToolName::new("sudo")` is `None` and there is no other way to
        // name a `bin/` file. Assert the file the old fallible path could have written is
        // still absent, which is the observable half of that guarantee.
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

    #[test]
    fn atomic_symlink_replaces_existing_link() {
        let layout = temp_prefix("replace");
        let link = layout.prefix.join("current");
        // Real directory targets: the Windows junction backend resolves the target to an
        // absolute directory path (a bare `/tmp/a` literal would read back drive-qualified).
        let a = layout.prefix.join("target-a");
        let b = layout.prefix.join("target-b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        atomic_symlink(&a, &link).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), a);
        // Replacing an existing link succeeds and updates the target.
        atomic_symlink(&b, &link).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), b);
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The mode `bin/` and `channels/<ch>/` come out at is a property of the PREFIX SHAPE,
    /// and activation is where it was easiest to get wrong: these three entry points
    /// chmod'd their directory to `0700` unconditionally, so every install and update
    /// re-hardened the ONE directory on the user's PATH (undoing a correct `atpkg link`
    /// on the way).
    ///
    /// A `$HOME` prefix — this fixture, and every install that is not a system prefix —
    /// must be UNCHANGED by the routing: still exactly `0700`. The probe comparison is the
    /// shape-agnostic half: `bin/` carries the mode THIS layout gives its own directories,
    /// whatever shape the layout turns out to be.
    #[cfg(unix)]
    #[test]
    fn a_home_shaped_prefix_keeps_bin_and_channels_private() {
        let layout = temp_prefix("mode-home");
        assert!(
            !layout.is_system_prefix(),
            "a user-owned temp prefix is never the system shape"
        );
        let b18 = make_build(&layout, "ay", 18, &["ay"]);
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Off).unwrap();
        assert_eq!(mode_of(&layout.bin_dir()), 0o700, "bin/ stays private");

        activate_channel(&layout, "stable", &b18).unwrap();
        let chan = layout.channel_current("stable");
        assert_eq!(
            mode_of(chan.parent().unwrap()),
            0o700,
            "channels/<ch>/ stays private"
        );

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
    /// root-owned SYSTEM prefix, activation must publish `bin/` and `channels/<ch>/` at
    /// `0755`. Nothing upstream objects to `0700` — it satisfies `dir_safe_for_private_write`
    /// and Trust's launcher predicate alike — so the break surfaces only as a bare
    /// `Permission denied` at the first non-root invocation of an installed tool.
    ///
    /// Skips when this run cannot build an all-root-owned chain (see
    /// [`system_prefix_fixture`]); the `$HOME` shape is covered above.
    /// The fixture's `None` is only credible from a process that could not have built the
    /// shape. Root could have, so from root a `None` is the fixture quietly regressing while
    /// every test that guards on it goes on reporting `ok`.
    #[cfg(unix)]
    #[test]
    fn system_fixture_is_available_when_this_process_could_build_one() {
        // SAFETY: `geteuid` reads this process's own effective uid and cannot fail.
        let root = unsafe { libc::geteuid() } == 0;
        if !root {
            return;
        }
        let layout = system_prefix_fixture("mode-probe").expect(
            "running as root, so a root-owned system-shaped prefix is buildable — \
             a `None` here means the fixture has stopped working and every test \
             that guards on it is silently asserting nothing",
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    #[cfg(unix)]
    #[test]
    fn a_system_shaped_prefix_publishes_bin_and_channels_traversable() {
        let Some(layout) = system_prefix_fixture("mode-sys") else {
            // Not root: the shape is genuinely unbuildable here, and
            // `system_fixture_is_available_when_this_process_could_build_one` keeps that
            // excuse honest. Say so — this is the only assertion that tells the two prefix
            // shapes apart, so a silent `ok` would misreport it as covered.
            eprintln!(
                "SKIP: a_system_shaped_prefix_publishes_bin_and_channels_traversable \
                 needs a root-owned system prefix (run as root to gate it)"
            );
            return;
        };
        let b18 = make_build(&layout, "ay", 18, &["ay"]);
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Off).unwrap();
        assert_eq!(
            mode_of(&layout.bin_dir()),
            0o755,
            "a system prefix's bin/ must be traversable by every user, not root-only"
        );

        activate_channel(&layout, "stable", &b18).unwrap();
        let chan = layout.channel_current("stable");
        assert_eq!(
            mode_of(chan.parent().unwrap()),
            0o755,
            "channels/<ch>/ belongs to the same prefix and must not disagree"
        );

        install_tombstone_shim(&layout, &tool("ay")).unwrap();
        assert_eq!(
            mode_of(&layout.bin_dir()),
            0o755,
            "the revoke path must not re-harden the shared bin/ either"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// `undo_activation` unwinds exactly the DOOMED build's footprint and nothing wider.
    /// The channel link here has already moved on to another program's build, so it must
    /// SURVIVE the undo — the link-removal guard is "names this build", not "names this
    /// channel" — while the doomed build's witness link and shim both go. Without the
    /// undo, the sysroot resolve-check discard (`flow::install_program`) left both `current` links and
    /// the written shims dangling into a deleted tree.
    #[test]
    fn undo_activation_unwinds_only_the_doomed_build() {
        let layout = temp_prefix("undo");
        let doomed = make_build(&layout, "ay", 19, &["ay"]);
        activate_channel(&layout, "stable", &doomed).unwrap();
        install_tools(&layout, &doomed, &[tool("ay")], Aliases::Off).unwrap();
        // A second program activates on the SAME channel afterwards: `channels/stable`
        // now names ny's build; ay keeps its own witness link and shim.
        let other = make_build(&layout, "ny", 7, &["ny"]);
        activate_channel(&layout, "stable", &other).unwrap();
        install_tools(&layout, &other, &[tool("ny")], Aliases::Off).unwrap();

        undo_activation(&layout, "stable", &doomed);

        // The doomed build's whole footprint is gone...
        assert!(
            std::fs::symlink_metadata(layout.program_current("ay")).is_err(),
            "ay's witness link is removed"
        );
        assert!(
            crate::platform::resolve_shim(&layout.shim(&tool("ay"))).is_none(),
            "ay's shim is removed"
        );
        // ...and nothing else is: the channel link names ANOTHER build and survives,
        // as does the bystander program entirely.
        assert_eq!(
            std::fs::read_link(layout.channel_current("stable")).expect("channel link survives"),
            other
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

    /// THE SHIM ENVIRONMENT (design S7) rides on every shim `install_tools_env` lays —
    /// primary AND alias — and the env-carrying wrapper is still a shim to every sweep:
    /// it RESOLVES into the build, a newer build that drops the tool PRUNES it, a policy
    /// flip sweeps its alias, `undo_activation` unwinds it, `ops::uninstall` takes it,
    /// and a re-lay without an env leaves nothing of the exports behind.
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
        // A newer build drops `claudex`: its env-carrying shim AND alias are pruned.
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
        activate_channel(&layout, "stable", &b19).unwrap();
        undo_activation(&layout, "stable", &b19);
        assert!(crate::platform::resolve_shim(&shim_of(&layout, "claude")).is_none());
        // `ops::uninstall` sweeps the wrapper the same way.
        install_tools_env(&layout, &b19, &[tool("claude")], Aliases::Off, &env).unwrap();
        activate_channel(&layout, "stable", &b19).unwrap();
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

    /// A PARTIAL lay must not strip the aliases of the tools it did not name. The restore
    /// behind `atpkg unlink` (`cli::restore_installed_shims`) lays exactly the names the dev
    /// link's marker owned — and `cli::default_link_bins` makes that the single
    /// `target/release/<program>`, so a multi-tool program's link owns ONE name — at the
    /// installed build the program's other tools are already live at. The prune counted
    /// every other `alab-<x>` as unwanted at the current build and deleted it, while leaving
    /// the plain shim it aliases standing: the unambiguous name (it exists because Homebrew
    /// shadows the bare one) went away from a tool the restore never touched. The sweeps
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
        // The restore's shape: the SAME build, only the subset the link owned.
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Alab).unwrap();
        for t in ["ay", "aylint", "alab-ay", "alab-aylint"] {
            assert!(
                crate::platform::resolve_shim(&shim_of(&layout, t))
                    .is_some_and(|p| p.starts_with(&b18)),
                "{t}: survives a lay that did not name it"
            );
        }
        // A policy flip still takes EVERY alias of the program, subset or not.
        install_tools(&layout, &b18, &[tool("ay")], Aliases::Off).unwrap();
        for t in ["alab-ay", "alab-aylint"] {
            assert!(
                std::fs::symlink_metadata(shim_of(&layout, t)).is_err(),
                "{t}: swept by Aliases::Off"
            );
        }
        // And a NEWER build that drops `aylint` still prunes its shim AND its alias: the
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

    /// `reconcile_aliases` mirrors the PRIMARY's environment onto the alias it lays (an
    /// install a pre-alias client made), rewrites an alias whose env drifted from its
    /// primary's, and leaves an alias that already agrees alone.
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

    /// "ALREADY RIGHT" MEANS THE BYTES TOO. An alias and an agents twin that forward where
    /// their primary does and export what it exports, but whose bytes are not what the
    /// renderer lays — here a line an older writer appended; in production the exec-root
    /// guard a plain shim lacks (`crate::compat`, whose own test drives that shape) — are
    /// re-laid by the reconciles, and a second reconcile touches neither file. The two
    /// reconciles a pass runs, in the order it runs them: [`reconcile_aliases`] per active
    /// program, then [`reconcile_agents`] ONCE at the end.
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
        assert!(shim_is_rendered(&layout.shim(&claude), &target, &env));
        let link = layout.prefix.join("old-symlink-shim");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!shim_is_rendered(&link, &target, &env));
        reconcile_aliases(&layout, &c1, std::slice::from_ref(&claude), Aliases::Alab).unwrap();
        reconcile_agents(&layout);
        for (p, want) in [&alias, &twin].iter().zip(&rendered) {
            assert_eq!(&std::fs::read(p).unwrap(), want, "{} re-laid", p.display());
        }
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

    /// THE AGENTS TWIN (owner decision 2026-09-10): installing an agent program lays
    /// `agents/<tool>` beside `bin/<tool>` — same target, same exported env — and nothing
    /// else ever lands in `agents/`: an ALab tool gets no twin, a hand-dropped file and a
    /// foreign name are swept, the twin follows its primary across an update, a
    /// tombstone, a rollback-undo and an uninstall, and the pass's own agents reconcile
    /// re-lays a twin an older client never laid.
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
        // An ALab tool gets no twin; the agents dir holds ONLY agent programs.
        let ay = make_build(&layout, "ay", 18, &["ay"]);
        install_shims(&layout, &ay, &["ay".into()], Aliases::Alab).unwrap();
        assert!(!layout.agent_shim(&tool("ay")).exists());
        // Foreign entries are swept: a hand-dropped file, and a symlink under an agent
        // name that points OUTSIDE the store (nothing in bin/ vouches for it).
        std::fs::write(layout.agents_dir().join("mine"), b"#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink("/usr/bin/true", layout.agents_dir().join("codex")).unwrap();
        reconcile_agents(&layout);
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
        undo_activation(&layout, "stable", &c2);
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

    /// THE TWIN CARRIES THE SELF-UPDATE BLOCK AND NO LANDING PRELUDE (Phase 2,
    /// 2026-09-22, `crate::landing`; from 2026-09-16 this test pinned the landing prelude
    /// in the twin): the `case "$1"` on the rostered verbs and the hand-over to `atpkg
    /// __selfupdate` with the prefix operand, ahead of the exports; no `[ -f <marker> ]`,
    /// no `__landing`; `bin/` carries neither. Every reader keyed on the target still
    /// resolves the twin to the STORE target; the twin is laid once and a second reconcile
    /// writes nothing; and the twins older clients laid — the primary's bytes (before any
    /// block), the landing-only twin (2026-09-16 to 2026-09-18) and the block-then-landing
    /// twin (2026-09-19 to 2026-09-22) — are each re-laid to today's once. A marker left
    /// standing changes nothing: the twin runs the store build.
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
        let marker = layout.landing_marker(&claude);
        assert!(
            !twin_body.contains("__landing") && !twin_body.contains("if [ -f "),
            "no landing prelude: {twin_body}"
        );
        assert!(
            !twin_body.contains(&marker.display().to_string()),
            "the twin never names a marker: {twin_body}"
        );
        // THE SELF-UPDATE BLOCK: the `case "$1"` on the rostered verbs and the hand-over
        // to `atpkg __selfupdate` with the program, the prefix and the arguments verbatim.
        assert!(
            twin_body.contains("case \"$1\" in\n  update|upgrade|install)\n"),
            "{twin_body}"
        );
        let hand_over = format!(
            "exec \"$__atpkg\" __selfupdate 'claude' '{}' -- \"$@\"; fi",
            layout.prefix.display()
        );
        assert!(twin_body.contains(&hand_over), "{twin_body}");
        assert!(
            !twin_body.contains("command -v atpkg") && !twin_body.contains("exec atpkg "),
            "no PATH fallback (review, 2026-09-16): {twin_body}"
        );
        assert!(
            !primary_body.contains("__selfupdate") && !primary_body.contains("case \"$1\""),
            "bin/ carries no block: {primary_body}"
        );
        // The block sits AHEAD of the exports, and the real exec is the last line.
        let block_at = twin_body.find("case \"$1\"").unwrap();
        let export_at = twin_body.find("export DISABLE_AUTOUPDATER").unwrap();
        assert!(block_at < export_at, "{twin_body}");
        assert!(twin_body.trim_end().ends_with("\"$@\""), "{twin_body}");
        // Every literal `exec '` line names the store target — none names atpkg.
        let exec_lines: Vec<&str> = twin_body
            .lines()
            .filter(|l| l.trim().starts_with("exec '"))
            .collect();
        assert_eq!(exec_lines.len(), 1, "{twin_body}");
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
        // The twins older clients laid, each re-laid to today's on the next reconcile:
        // the primary's bytes (before any block); the LANDING-ONLY twin (2026-09-16 to
        // 2026-09-18); the BLOCK-THEN-LANDING twin (2026-09-19 to 2026-09-22).
        let atpkg = crate::stub::embedded_atpkg_path();
        let landing = platform::sh_landing_prelude("claude", &layout.prefix, &marker, &atpkg);
        let mut block_then_landing = platform::sh_selfupdate_prelude(
            "claude",
            &layout.prefix,
            &atpkg,
            crate::selfupdate::verbs_of("claude"),
        );
        block_then_landing.push_str(&landing);
        for (what, older) in [
            ("before any block", primary_body.clone()),
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
        // The twin runs, in a real /bin/sh, straight to the store target — with a marker
        // an older client's pass left standing too: today's twin never reads it.
        std::fs::write(c1.join("bin/claude"), b"#!/bin/sh\necho \"store: $*\"\n").unwrap();
        std::fs::set_permissions(
            c1.join("bin/claude"),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        std::fs::create_dir_all(layout.landing_dir()).unwrap();
        std::fs::write(
            &marker,
            format!(
                "atpkg-landing-v1 build=2026091702 pid={} from=2026091601 version=- \
                 from_version=-\n",
                std::process::id()
            ),
        )
        .unwrap();
        let out = std::process::Command::new(&twin)
            .arg("--probe")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "store: --probe\n");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "", "nothing printed");
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// THE PRELUDE NEVER STRANDS THE TOOL (review, 2026-09-16), run through a real
    /// `/bin/sh`: the self-update block (2026-09-19, `crate::selfupdate`) hands `update`
    /// and `install latest --force` as the FIRST argument to the embedded atpkg as
    /// `__selfupdate claude <prefix> -- <args verbatim>`; `--probe`, `-p update` and
    /// `--debug install` run the store build untouched (the first token only — a prompt
    /// and a debug filter are not verbs); with the embedded atpkg gone, `update` runs the
    /// store build with nothing printed and exit 0 — an OLDER `atpkg` on `PATH` that would
    /// answer `unknown verb` exit 2 is never consulted. AND A STANDING LANDING MARKER
    /// CHANGES NOTHING (Phase 2, 2026-09-22; until then a non-verb under a marker went to
    /// `__landing`): today's twin runs the store build; only a twin an older client laid
    /// with the landing prelude still hands a non-verb to `__landing`, which now `exec`s
    /// `bin/<program>` at once (`cli::cmd_landing`).
    #[cfg(unix)]
    #[test]
    fn the_twin_prelude_runs_the_store_build_when_the_embedded_atpkg_is_gone() {
        let layout = temp_prefix("agents-twin-strand");
        let claude = tool("claude");
        let marker = layout.landing_marker(&claude);
        let exe = |path: &Path, body: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        // The store build: records that it ran, with its arguments.
        let target = layout.build_dir("claude", 2026091601).join("bin/claude");
        exe(&target, "#!/bin/sh\necho \"store: $*\"\nexit 0\n");
        // An OLDER atpkg on PATH: no `__selfupdate` verb.
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
                    atpkg,
                    crate::selfupdate::verbs_of("claude"),
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
            // THE SELF-UPDATE VERBS as the first argument hand over to the embedded
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
            // The embedded atpkg is gone: `update` runs the STORE build, nothing printed,
            // exit 0; the older atpkg on PATH is never consulted (the exec is the
            // guarantee).
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
        // A twin an OLDER client laid (the block, then the landing prelude) under that
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

    /// The twin is BEST-EFFORT and agent-store-only (review 2026-09-10): with `agents/`
    /// unmakeable (a file squats on its name) an `ay` install — and a `claude` install —
    /// still succeeds, `bin/` is laid, and nothing in `agents/` is claimed; and a
    /// `bin/claude` that resolves into a NON-agent store tree earns no twin, which is the
    /// predicate the sweep applies, so the two never disagree pass over pass.
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
        // A `bin/codex` into a NON-agent program's store tree: no twin laid — and the
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

    /// THE AGENTS RECONCILE IS THE PASS'S, NOT EVERY PROGRAM'S (audit 2026-09-15).
    /// [`reconcile_aliases`] runs once per ACTIVE PROGRAM and [`reconcile_agents`] answers
    /// for none of them in particular, so hanging the second off the end of the first made
    /// a P-program machine do the whole agents reconcile P times per pass. The alias
    /// reconcile now leaves `agents/` alone; the pass calls [`reconcile_agents`] once after
    /// its loop.
    #[cfg(unix)]
    #[test]
    fn the_alias_reconcile_leaves_the_agents_twin_to_the_once_per_pass_reconcile() {
        let layout = temp_prefix("agents-once-per-pass");
        let env = crate::shim_env::ShimEnv::NONE;
        let claude = tool("claude");
        let c1 = make_build(&layout, "claude", 2026091001, &["claude"]);
        install_tools_env(
            &layout,
            &c1,
            std::slice::from_ref(&claude),
            Aliases::Off,
            &env,
        )
        .unwrap();
        let ay_build = make_build(&layout, "ay", 18, &["ay"]);
        install_tools_env(&layout, &ay_build, &[tool("ay")], Aliases::Alab, &env).unwrap();
        let twin = layout.agent_shim(&claude);
        let alab_ay = tool("ay").alias().expect("ay carries an alab- alias");
        assert!(twin.exists(), "precondition: the install laid the twin");
        assert!(
            layout.shim(&alab_ay).exists(),
            "precondition: the alias too"
        );

        // An UNRELATED program's alias reconcile — the P-1 repeats of every pass — does
        // its own work and nothing under agents/.
        std::fs::remove_file(&twin).unwrap();
        std::fs::remove_file(layout.shim(&alab_ay)).unwrap();
        reconcile_aliases(&layout, &ay_build, &[tool("ay")], Aliases::Alab).unwrap();
        assert!(
            layout.shim(&alab_ay).exists(),
            "the alias reconcile still lays the alias it exists for"
        );
        assert!(
            !twin.exists(),
            "ay's alias reconcile must not re-do the agents reconcile"
        );
        // Nor does the agent program's OWN alias reconcile: agents/ is the pass's job.
        reconcile_aliases(&layout, &c1, std::slice::from_ref(&claude), Aliases::Off).unwrap();
        assert!(!twin.exists(), "nor claude's own");

        // The pass's single call is what lays it.
        reconcile_agents(&layout);
        assert_eq!(
            platform::resolve_shim(&twin),
            platform::resolve_shim(&layout.shim(&claude)),
            "the once-per-pass reconcile lays the twin the alias pass left alone"
        );
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// The other half of that fix, where the behaviour above cannot reach: the pass
    /// wrapper `cli::reconcile_aliases` calls [`reconcile_agents`] EXACTLY ONCE, at its
    /// own body's indent — outside the per-program loop, where an inner call would be the
    /// repeat this fix removes — and [`install_tools_env`] still reconciles, since that is
    /// what lays a freshly installed agent program's twin.
    #[test]
    fn the_agents_reconcile_is_wired_once_per_pass_and_never_per_program() {
        // A top-level fn body: from its signature to the first `}` in column 0.
        let body = |src: &str, sig: &str| -> String {
            let start = src.find(sig).unwrap_or_else(|| panic!("no such fn: {sig}"));
            let end = src[start..]
                .find("\n}\n")
                .map_or(src.len(), |i| start + i + 3);
            src[start..end].to_string()
        };
        let pass = body(
            include_str!("cli.rs"),
            "\nfn reconcile_aliases(layout: &crate::store::Layout, index: &crate::manifest::Index) {",
        );
        assert_eq!(
            pass.matches("crate::activate::reconcile_agents(layout);")
                .count(),
            1,
            "the pass reconciles agents/ exactly once"
        );
        assert!(
            pass.contains("\n    crate::activate::reconcile_agents(layout);\n"),
            "…at the wrapper's own indent, after the per-program loop and not inside it"
        );
        let me = include_str!("activate.rs");
        assert!(
            !body(me, "\npub(crate) fn reconcile_aliases(").contains("reconcile_agents("),
            "the per-program alias reconcile must not carry the pass's agents reconcile"
        );
        assert!(
            body(me, "\npub(crate) fn install_tools_env(").contains("reconcile_agents(layout);"),
            "the install lane still lays a fresh agent program's twin"
        );
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
    /// the SAME executable — `active_builds` still sees one build, `active_tools` lists the
    /// primaries and `active_aliases` the aliases — and a program installed under
    /// `Aliases::Off` gets none.
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
            // PRIMARY's executable name — on Windows `alab-ay.cmd` → `…\bin\ay.exe`.
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
        activate_channel(&layout, "stable", &b18).unwrap();
        let ny7 = make_build(&layout, "ny", 7, &["ny"]);
        install_shims(&layout, &ny7, &["ny".into()], Aliases::Alab).unwrap();

        // Build 19 drops aylint: its shim AND its alias go; ay's alias moves to 19.
        let b19 = make_build(&layout, "ay", 19, &["ay"]);
        install_shims(&layout, &b19, &["ay".into()], Aliases::Alab).unwrap();
        activate_channel(&layout, "stable", &b19).unwrap();
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
        // re-shim of the SAME build sweeps ay's alias, keeps ay, and leaves ny's alias.
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
        undo_activation(&layout, "stable", &b19);
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

    /// An already-installed program — one the install pipeline short-circuits as up to
    /// date, so `install_tools` never runs for it again — gets its aliases from the pass's
    /// reconcile: laid when missing, left alone when right, swept under `Off`, and never
    /// written over a file this manager did not lay.
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

    /// The Windows `.cmd` alias, as content: the resume-proof frame every `.cmd` starts
    /// with (2026-09-18), then the shim named for the ALIAS forwarding to the PRIMARY's
    /// `.exe`, ending its batch on that line — pure, so it is pinned from every build
    /// host.
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
