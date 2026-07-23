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
use crate::store::shim_allowed;

/// Atomically point `link` at `target`. The OS-specific indirection primitive:
/// [`crate::platform::atomic_symlink`] — a temp-symlink + `rename(2)` on POSIX (atomic,
/// no missing/half-written window), a directory **junction** on Windows. Re-exported here
/// (and via [`crate`]) so `channels/<ch>/current` and the Kani dir links share one entry.
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    platform::atomic_symlink(target, link)
}

/// Make `build_dir` the active build for `channel`: atomically flip
/// `channels/<channel>/current → build_dir`. The channel directory is created hardened
/// (`0700`, owned-by-uid) first. Idempotent — re-activating the same build is a no-op-ish
/// re-point.
pub fn activate_channel(layout: &Layout, channel: &str, build_dir: &Path) -> io::Result<()> {
    let current = layout.channel_current(channel);
    let parent = current
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "channel path has no parent"))?;
    ensure_private_dir(parent)?;
    atomic_symlink(build_dir, &current)
}

/// Install `bin/<tool>` shims for the manifest's `exposes` list, each pointing at
/// `<build_dir>/bin/<tool>`. Names that fail [`shim_allowed`] (collide with a sensitive
/// command, or are malformed) are **skipped** and returned, so the caller can surface
/// them in `status.toml`. The `bin/` dir is created hardened first. Returns the list of
/// refused names (empty when all were installed).
pub fn install_shims(
    layout: &Layout,
    build_dir: &Path,
    exposes: &[String],
) -> io::Result<Vec<String>> {
    let bin = layout.bin_dir();
    ensure_private_dir(&bin)?;
    let mut refused = Vec::new();
    for tool in exposes {
        if !shim_allowed(tool) {
            refused.push(tool.clone());
            continue;
        }
        platform::install_shim(&build_dir.join("bin"), tool, &layout.shim(tool))?;
    }
    Ok(refused)
}

/// Install a **failing tombstone shim** at `bin/<tool>` — a tiny script that prints a
/// yanked/revoked notice to stderr and exits nonzero — so a revoked build's OLD working shim
/// is actively DISABLED, not left runnable (§7). Written atomically (temp + `rename(2)`), so a
/// reader never sees a half-written script; the `rename` replaces the prior *symlink* shim
/// in place. Returns `Ok(true)` when the tombstone was installed, `Ok(false)` when `tool`
/// fails [`shim_allowed`] (a tombstone must ALSO never shadow a sensitive name — it is written
/// through the identical deny-list gate as a live shim).
///
/// A later successful `atpkg update` re-runs [`install_shims`], whose `atomic_symlink` replaces
/// this regular-file tombstone with a fresh symlink, so the disable clears itself on recovery.
pub fn install_tombstone_shim(layout: &Layout, tool: &str) -> io::Result<bool> {
    // A tombstone is still a shim: it MUST pass the sensitive-name deny-list. A tool named
    // `sudo`/`git`/… never had a live shim to disable, so refusing here is correct + safe.
    if !shim_allowed(tool) {
        return Ok(false);
    }
    let bin = layout.bin_dir();
    ensure_private_dir(&bin)?;
    let shim = layout.shim(tool);

    // The failing-shim message. The tool-bearing text is the only variable part; the
    // platform backend embeds it injection-safely (Unix: a single-quoted `printf` arg;
    // Windows: a `cmd`-escaped `echo`). Built with `push_str` (no `format!`, Trust gate).
    let mut message = String::from("atpkg: ");
    message.push_str(tool);
    message.push_str(" was yanked/revoked — run `atpkg update`");
    // Atomic install through the platform backend (Unix: an executable `sh` script
    // temp+rename; Windows: a `.cmd` batch wrapper), replacing whatever shim was there.
    platform::install_tombstone_shim(&shim, &message)?;
    Ok(true)
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

    fn make_build(layout: &Layout, program: &str, build: u64, bins: &[&str]) -> PathBuf {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        for b in bins {
            // The concrete executable name a shim forwards to (`<b>.exe` on Windows).
            let name = format!("{b}{}", crate::platform::EXE_SUFFIX);
            std::fs::write(dir.join("bin").join(name), b"#!/bin/true\n").unwrap();
        }
        dir
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

    #[test]
    fn install_shims_creates_allowed_and_refuses_sensitive() {
        let layout = temp_prefix("shims");
        let build = make_build(&layout, "ay", 18, &["ay", "sudo", "ny"]);
        let exposes = vec!["ay".to_string(), "sudo".to_string(), "ny".to_string()];
        let refused = install_shims(&layout, &build, &exposes).unwrap();
        // sudo is refused (sensitive), reported, and NOT shimmed.
        assert_eq!(refused, vec!["sudo".to_string()]);
        assert!(
            !layout.shim("sudo").exists()
                && std::fs::symlink_metadata(layout.shim("sudo")).is_err()
        );
        // ay + ny shims exist and resolve into the build's bin/. resolve_shim reads the
        // forward target cross-platform (symlink target on Unix, the `.cmd` target — the
        // exe-suffixed concrete binary — on Windows).
        for tool in ["ay", "ny"] {
            let shim = layout.shim(tool);
            let target = crate::platform::resolve_shim(&shim).unwrap();
            assert_eq!(
                target,
                build
                    .join("bin")
                    .join(format!("{tool}{}", crate::platform::EXE_SUFFIX))
            );
            assert!(
                std::fs::metadata(&target).unwrap().is_file(),
                "{tool} shim resolves to the binary"
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
        let shim = layout.shim("ay");
        // A LIVE forwarding shim (a symlink on Unix, a forwarding `.cmd` on Windows).
        assert!(
            crate::platform::resolve_shim(&shim).is_some(),
            "live shim forwards into the build"
        );

        // Tombstone it: the forwarding shim is REPLACED by a failing regular-file script.
        assert!(install_tombstone_shim(&layout, "ay").unwrap());
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

        // A sensitive name is refused a tombstone (never shadows a core command) and writes
        // nothing to bin/.
        assert!(!install_tombstone_shim(&layout, "sudo").unwrap());
        assert!(std::fs::symlink_metadata(layout.shim("sudo")).is_err());

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
}
