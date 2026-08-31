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
//! ([`Aliases::Alab`]: its index entry has no `system` key and is not an `extra`) gets an
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
//! that hold none), [`reconcile_aliases`] mirrors the primary's env onto its alias, and
//! every sweep below keys on where a shim RESOLVES — the exec line — so an env-carrying
//! shim is pruned, tombstoned, rolled back and uninstalled exactly like a plain one.

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
    /// The policy for an index entry: ALab's own ⇔ no `system` key and not an `extra`.
    /// An UNLISTED program (`None`) is not ALab's: nothing vouches for it.
    #[must_use]
    pub fn for_program(program: Option<&crate::manifest::Program>) -> Self {
        match program {
            Some(p) if p.system.is_none() && !p.extra => Self::Alab,
            _ => Self::Off,
        }
    }

    /// The policy already in force for `program` on disk: `Alab` when any alias shim
    /// resolves into its store tree, else `Off`. For the verbs that hold no index — they
    /// must neither invent aliases for a vendor tool nor sweep the ones an install laid.
    #[must_use]
    pub fn laid_for(layout: &Layout, program: &str) -> Self {
        let prog_store = layout.prefix.join("store").join(program);
        let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
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
    for tool in tools {
        platform::install_shim_env(&build_dir.join("bin"), tool, &layout.shim(tool), env)?;
        if aliases == Aliases::Alab
            && let Some(alias) = tool.alias()
        {
            install_alias(layout, build_dir, tool, &alias, env)?;
        }
    }
    prune_stale_shims(layout, build_dir, tools, aliases);
    Ok(())
}

/// Lay `bin/<alias>` forwarding to the SAME executable `tool`'s own shim forwards to:
/// `<build_dir>/bin/<tool><EXE_SUFFIX>`. The shim file is the alias's
/// (`alab-ay`, `alab-ay.cmd`), the target is the primary's (`ay`, `ay.exe`) — which is
/// exactly the pair [`platform::install_shim`] keeps apart by taking the target's
/// [`ToolName`] and the shim path separately.
fn install_alias(
    layout: &Layout,
    build_dir: &Path,
    tool: &ToolName,
    alias: &ToolName,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    platform::install_shim_env(&build_dir.join("bin"), tool, &layout.shim(alias), env)
}

/// Bring the ALIASES of an already-installed program in line with `aliases` without
/// touching its primary shims: under [`Aliases::Alab`] lay the missing `alab-<tool>` for
/// every `tool` whose shim resolves into `build_dir`; under [`Aliases::Off`] sweep any
/// alias that resolves into this program's store. The primary shims are read, never
/// written, so an up-to-date program — which the install pipeline short-circuits before it
/// ever reaches [`install_tools`] — still gets its aliases the first pass after this
/// client lands, and a program whose index entry stops being ALab's own loses them.
/// Nothing here touches a dev-linked program's checkout shims (they resolve outside the
/// store) or a pending stub (it resolves nowhere).
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
            // same environment) is left alone, so the six-hourly tick rewrites nothing.
            if platform::resolve_shim(&shim).is_some_and(|t| t == wanted)
                && platform::shim_env_of(&shim) == env
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

/// Remove `bin/` shims this program owns that still point at a DIFFERENT build — the tools
/// a newer build dropped from its `exposes` — and every ALIAS of this program's that is no
/// longer wanted (its base is not in `installed`, or `aliases` is [`Aliases::Off`]),
/// whatever build it points at.
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
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
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
        let alias_wanted = aliases == Aliases::Alab
            && tool
                .alias_base()
                .is_some_and(|base| installed.contains(&base));
        if alias_wanted {
            continue;
        }
        let Some(target) = crate::platform::resolve_shim(&entry.path()) else {
            continue; // not a shim we can resolve (tombstone, real file, dangling)
        };
        // A plain shim is stale at a DIFFERENT build of this program; an alias that was
        // not just (re)wanted is stale at ANY build of it — the same build included, which
        // is the shape a policy flip (`Alab` → `Off`) or a dropped base leaves behind.
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
    if let Ok(entries) = std::fs::read_dir(layout.bin_dir()) {
        for e in entries.flatten() {
            if crate::platform::resolve_shim(&e.path()).is_some_and(|t| t.starts_with(build_dir)) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
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
    platform::install_tombstone_shim(&shim, &message)
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
    #[cfg(unix)]
    #[test]
    fn a_system_shaped_prefix_publishes_bin_and_channels_traversable() {
        let Some(layout) = system_prefix_fixture("mode-sys") else {
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

    /// The alias policy is read off the SIGNED index entry: ALab's own (no `system`, not an
    /// `extra`) aliases; a vendor tool, a system-satisfiable member and an unlisted name
    /// do not.
    #[test]
    fn the_alias_policy_follows_the_index_entry() {
        let program = |extra: bool, system: Option<&str>| crate::manifest::Program {
            repo: "x".into(),
            policy: String::new(),
            coherence_group: None,
            extra,
            system: system.map(str::to_string),
            unavailable_hint: None,
            requires: vec![],
        };
        assert_eq!(
            Aliases::for_program(Some(&program(false, None))),
            Aliases::Alab,
            "trust/ay/ty/clean: ALab's own"
        );
        assert_eq!(
            Aliases::for_program(Some(&program(true, None))),
            Aliases::Off,
            "codex/claude: a vendor extra"
        );
        assert_eq!(
            Aliases::for_program(Some(&program(false, Some("gh")))),
            Aliases::Off,
            "gh/emacs: a system copy may satisfy it"
        );
        assert_eq!(Aliases::for_program(None), Aliases::Off, "unlisted");
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
                assert!(body.contains(&format!("\\bin\\{name}.exe\" %*")), "{body}");
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

    /// The Windows `.cmd` alias, as content: the shim named for the ALIAS forwards to the
    /// PRIMARY's `.exe` — pure, so it is pinned from every build host.
    #[test]
    fn the_cmd_alias_forwards_to_the_primary_exe() {
        let target = Path::new(r"C:\Users\me\.aterm\pkg\store\ay\18\bin\ay.exe");
        assert_eq!(
            crate::platform::cmd_shim_content(target),
            "@\"C:\\Users\\me\\.aterm\\pkg\\store\\ay\\18\\bin\\ay.exe\" %*\r\n"
        );
        assert_eq!(
            crate::platform::parse_cmd_shim_target(&crate::platform::cmd_shim_content(target)),
            Some(target.to_path_buf())
        );
    }
}
