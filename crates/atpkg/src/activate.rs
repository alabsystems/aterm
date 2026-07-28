// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Activation (§10): the atomic POSIX symlink swap that makes a staged store build the
//! live one, plus the `bin/` shim installation.
//!
//! Activation is **not** the updater's `renamex_np`/re-exec (that is the macOS `.app`
//! path). A CLI program's active build is selected by a symlink: `channels/<name>/current`
//! points at the chosen `store/<program>/<build>/`, and one `bin/<tool>` symlink per
//! exposed binary points into it. Each flip is an **atomic replace** — write a sibling
//! temp symlink, then `rename(2)` it over the target — so a reader never sees a missing or
//! half-written link, and a concurrent run (under `apply.lock`) cannot observe a torn
//! state. Every shim name is gated through [`crate::store::shim_allowed`]: a tool named
//! `sudo`/`ssh`/`git`/… is refused a shim and reported, never silently installed.

use std::io;
use std::path::Path;

use crate::Layout;
use crate::platform::{self, ensure_private_dir};
use crate::store::{ToolName, split_exposed};

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
    ensure_private_dir(parent)?;
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
) -> io::Result<Vec<String>> {
    let (tools, refused) = split_exposed(exposes);
    install_tools(layout, build_dir, &tools)?;
    Ok(refused)
}

/// The validated half of [`install_shims`]: install one shim per already-admitted
/// [`ToolName`], then prune the shims a previous build left behind. The `bin/` dir is created
/// hardened first.
///
/// Split out (and `pub(crate)`) for the callers that already hold `ToolName`s — the
/// transaction flip in [`crate::flow`], whose staged member carries the admitted set — so the
/// raw list is not re-split, and so their refusal semantics cannot silently drift from this
/// one loop's.
pub(crate) fn install_tools(
    layout: &Layout,
    build_dir: &Path,
    tools: &[ToolName],
) -> io::Result<()> {
    let bin = layout.bin_dir();
    ensure_private_dir(&bin)?;
    for tool in tools {
        platform::install_shim(&build_dir.join("bin"), tool, &layout.shim(tool))?;
    }
    prune_stale_shims(layout, build_dir, tools);
    Ok(())
}

/// Remove `bin/` shims this program owns that still point at a DIFFERENT build — the tools
/// a newer build dropped from its `exposes`.
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
/// build>/` — the exact shape this function itself creates. Anything else is left alone:
/// another program's shims, a dev-link, a tombstone, a hand-made file, or any target outside
/// the store. The containment test is [`crate::ops::store_build_of`], which is ANCHORED to
/// the prefix; the unanchored `program_build_of_target` used here before answered `("ay", 18)`
/// for a dev-link into `~/src/store/ay/18/bin/ay`, i.e. it would have deleted a link into a
/// tree this manager does not own.
fn prune_stale_shims(layout: &Layout, build_dir: &Path, installed: &[ToolName]) {
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
        // Never remove a name the new build still exposes — that shim was just written.
        if installed.contains(&tool) {
            continue;
        }
        let Some(target) = crate::platform::resolve_shim(&entry.path()) else {
            continue; // not a shim we can resolve (tombstone, real file, dangling)
        };
        if crate::ops::store_build_of(&layout.prefix, &target)
            .is_some_and(|(p, b)| p == program && b != build)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Best-effort undo of [`activate_channel`] plus a partial [`install_tools`] pass, for a
/// build that is about to be DISCARDED. An abort path that deletes a build AFTER activation
/// succeeded (sourcebuild's shim-failure arm) must call this first, or the deleted tree
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
    ensure_private_dir(&bin)?;
    let shim = layout.shim(tool);

    // The failing-shim message. The tool-bearing text is the only variable part; the
    // platform backend embeds it injection-safely (Unix: a single-quoted `printf` arg;
    // Windows: a `cmd`-escaped `echo`). Built with `push_str` (no `format!`, Trust gate).
    let mut message = String::from("atpkg: ");
    message.push_str(tool.as_str());
    message.push_str(" was yanked/revoked — run `atpkg update`");
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
        install_shims(&layout, &b18, &["ay".into(), "aylint".into()]).unwrap();
        activate_channel(&layout, "stable", &b18).unwrap();

        let b19 = make_build(&layout, "ay", 19, &["ay"]);
        install_shims(&layout, &b19, &["ay".into()]).unwrap();
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
        install_shims(&layout, &ay18, &["ay".into(), "aylint".into()]).unwrap();
        let ny7 = make_build(&layout, "ny", 7, &["ny"]);
        install_shims(&layout, &ny7, &["ny".into()]).unwrap();

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
        install_shims(&layout, &ay19, &["ay".into()]).unwrap();

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
        install_shims(&layout, &b18, &["ay".into(), "aylint".into()]).unwrap();
        install_shims(&layout, &b18, &["ay".into(), "aylint".into()]).unwrap();
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
        let refused = install_shims(&layout, &build, &exposes).unwrap();
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
        install_shims(&layout, &build, &exposes).unwrap();
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

    /// `undo_activation` unwinds exactly the DOOMED build's footprint and nothing wider.
    /// The channel link here has already moved on to another program's build, so it must
    /// SURVIVE the undo — the link-removal guard is "names this build", not "names this
    /// channel" — while the doomed build's witness link and shim both go. Without the
    /// undo, sourcebuild's discard-after-activation abort left both `current` links and
    /// the written shims dangling into a deleted tree.
    #[test]
    fn undo_activation_unwinds_only_the_doomed_build() {
        let layout = temp_prefix("undo");
        let doomed = make_build(&layout, "ay", 19, &["ay"]);
        activate_channel(&layout, "stable", &doomed).unwrap();
        install_tools(&layout, &doomed, &[tool("ay")]).unwrap();
        // A second program activates on the SAME channel afterwards: `channels/stable`
        // now names ny's build; ay keeps its own witness link and shim.
        let other = make_build(&layout, "ny", 7, &["ny"]);
        activate_channel(&layout, "stable", &other).unwrap();
        install_tools(&layout, &other, &[tool("ny")]).unwrap();

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
}
