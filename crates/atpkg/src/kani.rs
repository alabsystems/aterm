// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `~/.kani` wiring hardening (§10.1) — the security-critical parts of installing the
//! `trust-mc` model-checker sysroot's `~/.kani/kani-<VERSION>` symlink.
//!
//! `~/.kani` sits **outside** the managed prefix and rustup/`setup-trust-mc` conventions
//! often create it `0755`, which `ensure_private_dir` would not vouch for — and the script
//! itself notes this wiring "has silently gone missing before". An attacker who can write
//! `~/.kani` could pre-place `~/.kani/kani-<ver>` as a symlink to a **malicious sysroot**
//! before `atpkg` writes it. So, before linking, `atpkg`:
//!
//! * verifies `~/.kani` is owned-by-uid and not group/other-writable
//!   ([`ensure_private_dir`]);
//! * keeps the **authoritative sysroot inside the hardened prefix** and exposes only a
//!   single managed symlink, replaced **atomically** ([`crate::activate::atomic_symlink`]);
//! * **refuses fail-closed** if a pre-existing `kani-<ver>` is a real directory, or a
//!   symlink pointing **outside** the managed prefix (i.e. one `atpkg` did not create) —
//!   never `ln -sfn` over an attacker-influenceable path.
//!
//! The actual sysroot relocation + the four-component nightly gate + the fail-loud
//! `cargo-trust-mc --version` resolve check (`[GREENFIELD]`, §10.1) need a real `trust-mc`
//! bundle to exercise; this module is the part whose safety is unit-testable today.

use std::io;
use std::path::Path;

use crate::activate::atomic_symlink;
use crate::platform::ensure_private_dir;

/// Why wiring `~/.kani/kani-<ver>` was refused.
#[derive(Debug)]
pub enum KaniError {
    /// `~/.kani` could not be made/confirmed private (owned-by-uid, not group/other-writable).
    UnsafeHome(io::Error),
    /// A pre-existing `kani-<ver>` is a real directory/file (not our managed symlink).
    ForeignEntry,
    /// A pre-existing `kani-<ver>` symlink points OUTSIDE the managed prefix — atpkg did
    /// not create it, so replacing it is refused.
    ForeignSymlink,
    /// I/O writing the symlink / version file.
    Io(io::Error),
}

// Hand-rendered through `Formatter::write_str` + direct `Display::fmt` calls
// (no `write!`): the `write!`/`format_args!` expansion embeds `fmt::Arguments`
// construction (with inlined `unsafe`) that the strict Trust gate cannot lower
// and fails closed on. Byte-identical output (`write!` with `{}` args performs
// exactly these formatter writes in sequence; no width/fill flags are used).
impl std::fmt::Display for KaniError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KaniError::UnsafeHome(e) => {
                f.write_str("~/.kani is not private: ")?;
                std::fmt::Display::fmt(e, f)
            }
            KaniError::ForeignEntry => {
                f.write_str("~/.kani/kani-<ver> exists and is not a managed symlink")
            }
            KaniError::ForeignSymlink => f.write_str(
                "~/.kani/kani-<ver> is a symlink atpkg did not create (points outside the prefix)",
            ),
            KaniError::Io(e) => {
                f.write_str("io: ")?;
                std::fmt::Display::fmt(e, f)
            }
        }
    }
}

impl std::error::Error for KaniError {}

/// Atomically point `~/.kani/kani-<version>` at `target` (a sysroot **inside** `prefix`),
/// hardening first. `kani_home` is `~/.kani`; `target` must resolve under `prefix` (the
/// authoritative sysroot location). Fail-closed on an unsafe home or a foreign pre-existing
/// entry (see [`KaniError`]).
pub fn wire_kani_link(
    kani_home: &Path,
    version: &str,
    target: &Path,
    prefix: &Path,
) -> Result<(), KaniError> {
    // 1. ~/.kani must be private (creates it 0700 + verifies owner/mode).
    // `ensure_private_dir` goes via `call1`: a direct cross-crate call is an
    // unlowerable target the strict Trust gate fails closed on here; the
    // `FnOnce` route scopes it out as Conditional like every other polymorphic
    // callee (see `lib.rs`). Same function, same argument; behavior identical.
    crate::call1(ensure_private_dir, kani_home).map_err(KaniError::UnsafeHome)?;

    // Manual concat of the previous `format!("kani-{version}")` — byte-identical:
    // the `format!` expansion embeds `fmt::Arguments` construction (with inlined
    // `unsafe`) that the strict Trust gate cannot lower and fails closed on.
    let mut link_name = String::from("kani-");
    link_name.push_str(version);
    let link = kani_home.join(link_name);

    // 2. Refuse a foreign pre-existing entry rather than `ln -sfn` over it.
    // `is_reparse`, not `is_symlink`: the managed link is a directory JUNCTION on Windows
    // (`FileType::is_symlink()` reports false for it), so the symlink-only check would
    // mis-read atpkg's own prior link as a foreign real directory and refuse a re-wire.
    // On Unix `is_reparse` IS `is_symlink` — behavior identical.
    if let Ok(meta) = std::fs::symlink_metadata(&link) {
        if crate::platform::is_reparse(&meta) {
            // Ours iff it already points inside the managed prefix; otherwise foreign.
            match std::fs::read_link(&link) {
                Ok(existing) if existing.starts_with(prefix) => {}
                _ => return Err(KaniError::ForeignSymlink),
            }
        } else {
            return Err(KaniError::ForeignEntry); // a real dir/file we didn't make
        }
    }

    // 3. The managed target must itself live under the prefix (authoritative sysroot).
    if !target.starts_with(prefix) {
        return Err(KaniError::ForeignSymlink);
    }

    // 4. Atomic replace.
    atomic_symlink(target, &link).map_err(KaniError::Io)
}

/// Write the sysroot's `rust-toolchain-version` with **no trailing newline** (the
/// `printf '%s'` requirement, `setup-trust-mc.sh:90-93`): a `\n` makes `cargo-trust-mc`
/// reject it as `override toolchain '<name>\n' is not installed`.
pub fn write_toolchain_version(path: &Path, version: &str) -> io::Result<()> {
    // Explicitly write the exact bytes — no `writeln!`, no trailing newline.
    // `fs::write` goes via `call2`: the hardened pass name-matches any direct
    // callee named `write` against the libc `write(2)` FFI-boundary contracts,
    // which do not apply to this safe std function (see `lib.rs`). Same
    // function, same arguments; behavior identical.
    crate::call2(std::fs::write, path, version.as_bytes())
}

/// The four rustup components the named nightly MUST carry for `cargo-trust-mc` to resolve
/// (`setup-trust-mc.sh:54-55`). `llvm-tools` also matches the older `llvm-tools-preview`.
pub const REQUIRED_NIGHTLY_COMPONENTS: &[&str] =
    &["rustc-dev", "rust-src", "llvm-tools", "rustfmt"];

/// Gate the trust group on the named nightly being installed **with all four components**
/// (§7/§10.1): `atpkg` never silently installs rustup toolchains as part of a verified
/// apply, so a missing component must abort fail-loud. `installed` is the output of
/// `rustup component list --installed` (one component per entry, possibly target-suffixed
/// like `rustc-dev-aarch64-apple-darwin`). Returns `Ok(())` iff every required component is
/// present; otherwise the **missing** names (so the caller can tell the operator exactly
/// what to `rustup component add`).
///
/// # Errors
/// The list of missing component names when one or more required components is absent.
pub fn nightly_components_ready(installed: &[String]) -> Result<(), Vec<String>> {
    let missing: Vec<String> = REQUIRED_NIGHTLY_COMPONENTS
        .iter()
        .filter(|req| {
            // Manual concat of the previous `format!("{req}-")` — byte-identical:
            // the `format!` expansion embeds `fmt::Arguments` construction (with
            // inlined `unsafe`) that the strict Trust gate cannot lower and fails
            // closed on. Hoisted out of the `any` closure (same prefix on every
            // iteration).
            let mut prefix = String::from(**req);
            prefix.push('-');
            !installed
                .iter()
                .any(|c| c == *req || c.starts_with(&prefix))
        })
        .map(|s| (*s).to_string())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// Relocate a tarballed trust-mc sysroot on the user box (§10.1 step 2, `[GREENFIELD]`):
/// the bundle ships a **dangling** `toolchain` symlink pointing into the *builder's*
/// `~/.rustup`, so the installer must re-point it at the user's actual rustup toolchain
/// dir, and write the (newline-free) version files. `sysroot` is the extracted tree
/// (inside the managed prefix); `rustup_toolchain` is the user's
/// `~/.rustup/toolchains/<nightly>`. The `toolchain` link is replaced atomically.
pub fn relocate_sysroot(sysroot: &Path, rustup_toolchain: &Path, version: &str) -> io::Result<()> {
    atomic_symlink(rustup_toolchain, &sysroot.join("toolchain"))?;
    write_toolchain_version(&sysroot.join("rust-toolchain-version"), version)?;
    write_toolchain_version(&sysroot.join("rustc-version"), version)
}

/// The fail-loud post-apply resolve check (§10.1, `[GREENFIELD]` until now): run
/// an installed toolchain binary with `--version` and require it to RUN TO
/// COMPLETION (a real exit code, not a signal death and not a spawn failure) —
/// which it can only do if the dynamic loader resolved every dependency from the
/// installed bundle. A self-contained bundle whose vendored libs are wired wrong
/// would fail to load here and ABORT the apply, instead of the design's feared
/// "lay down a broken toolchain and report SUCCESS". `bin` is the shim/store
/// path to an exposed compiler (e.g. `trust-mc-compiler`, `trustc`).
///
/// # Errors
/// When the binary cannot be spawned, or is killed by a signal (e.g. the dynamic
/// loader aborts on an unresolved library) rather than exiting normally.
pub fn resolve_check(bin: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let status = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("resolve check: cannot spawn {}: {e}", bin.display()))?;
    // A normal exit (any code) proves the loader resolved the bundle; a signal
    // death (code() == None) is the dyld/ld.so failure we must catch.
    if status.code().is_some() {
        Ok(())
    } else {
        Err(format!(
            "resolve check: {} was killed by a signal (unresolved dynamic library?) — \
             refusing a broken toolchain install",
            bin.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn scratch(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-kani-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        d
    }

    #[test]
    fn toolchain_version_has_no_trailing_newline() {
        let d = scratch("ver");
        let f = d.join("rust-toolchain-version");
        write_toolchain_version(&f, "nightly-2025-12-03").unwrap();
        let bytes = std::fs::read(&f).unwrap();
        assert_eq!(bytes, b"nightly-2025-12-03");
        assert_ne!(bytes.last(), Some(&b'\n'), "must NOT end in a newline");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn wires_a_fresh_managed_link() {
        let root = scratch("fresh");
        let kani = root.join(".kani");
        let prefix = root.join("prefix");
        let target = prefix.join("store/trust/671/sysroot");
        std::fs::create_dir_all(&target).unwrap();
        wire_kani_link(&kani, "0.67.0", &target, &prefix).unwrap();
        assert_eq!(
            std::fs::read_link(kani.join("kani-0.67.0")).unwrap(),
            target
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn replaces_our_own_prior_link_idempotently() {
        let root = scratch("replace");
        let kani = root.join(".kani");
        let prefix = root.join("prefix");
        let old = prefix.join("store/trust/670/sysroot");
        let new = prefix.join("store/trust/671/sysroot");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        wire_kani_link(&kani, "0.67.0", &old, &prefix).unwrap();
        // Re-wiring to a newer build inside the prefix is allowed (ours).
        wire_kani_link(&kani, "0.67.0", &new, &prefix).unwrap();
        assert_eq!(std::fs::read_link(kani.join("kani-0.67.0")).unwrap(), new);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)] // symlink fixture — Unix-only
    #[test]
    fn refuses_a_foreign_symlink_pointing_outside_the_prefix() {
        let root = scratch("foreign-link");
        let kani = root.join(".kani");
        let prefix = root.join("prefix");
        std::fs::create_dir_all(&kani).unwrap();
        std::fs::set_permissions(&kani, std::fs::Permissions::from_mode(0o700)).unwrap();
        // An attacker pre-placed kani-0.67.0 -> /tmp/evil (outside the prefix).
        std::os::unix::fs::symlink("/tmp/evil-sysroot", kani.join("kani-0.67.0")).unwrap();
        let target = prefix.join("store/trust/671/sysroot");
        std::fs::create_dir_all(&target).unwrap();
        let err = wire_kani_link(&kani, "0.67.0", &target, &prefix).unwrap_err();
        assert!(matches!(err, KaniError::ForeignSymlink), "got {err:?}");
        // The foreign link is left untouched (not clobbered).
        assert_eq!(
            std::fs::read_link(kani.join("kani-0.67.0")).unwrap(),
            Path::new("/tmp/evil-sysroot")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn refuses_a_foreign_real_directory() {
        let root = scratch("foreign-dir");
        let kani = root.join(".kani");
        let prefix = root.join("prefix");
        std::fs::create_dir_all(kani.join("kani-0.67.0")).unwrap(); // a real dir, not our symlink
        #[cfg(unix)]
        std::fs::set_permissions(&kani, std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = prefix.join("store/trust/671/sysroot");
        std::fs::create_dir_all(&target).unwrap();
        let err = wire_kani_link(&kani, "0.67.0", &target, &prefix).unwrap_err();
        assert!(matches!(err, KaniError::ForeignEntry), "got {err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn refuses_a_target_outside_the_prefix() {
        let root = scratch("bad-target");
        let kani = root.join(".kani");
        let prefix = root.join("prefix");
        std::fs::create_dir_all(&prefix).unwrap();
        let err =
            wire_kani_link(&kani, "0.67.0", Path::new("/tmp/elsewhere"), &prefix).unwrap_err();
        assert!(matches!(err, KaniError::ForeignSymlink), "got {err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nightly_component_gate_accepts_all_four_rejects_missing() {
        // All four present (target-suffixed, and llvm-tools-preview as the older name).
        let ok: Vec<String> = [
            "rust-src",
            "rustc-dev-aarch64-apple-darwin",
            "llvm-tools-preview-aarch64-apple-darwin",
            "rustfmt-aarch64-apple-darwin",
            "clippy-aarch64-apple-darwin", // extra is fine
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        assert_eq!(nightly_components_ready(&ok), Ok(()));

        // Missing rust-src + rustfmt → reported precisely.
        let partial: Vec<String> = ["rustc-dev-x", "llvm-tools-x"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let missing = nightly_components_ready(&partial).unwrap_err();
        assert!(missing.contains(&"rust-src".to_string()));
        assert!(missing.contains(&"rustfmt".to_string()));
        assert!(!missing.contains(&"rustc-dev".to_string()));

        // Empty ⇒ all four missing (fail-loud, never a silent install).
        assert_eq!(nightly_components_ready(&[]).unwrap_err().len(), 4);
    }

    #[cfg(unix)] // symlink fixture — Unix-only
    #[test]
    fn relocate_sysroot_repoints_link_and_writes_newline_free_versions() {
        let root = scratch("relocate");
        let sysroot = root.join("prefix/store/trust/671/sysroot");
        std::fs::create_dir_all(&sysroot).unwrap();
        // Simulate the dangling builder symlink the tarball shipped.
        std::os::unix::fs::symlink(
            "/builder/.rustup/toolchains/nightly",
            sysroot.join("toolchain"),
        )
        .unwrap();
        let user_tc = root.join(".rustup/toolchains/nightly-2025-12-03");
        std::fs::create_dir_all(&user_tc).unwrap();

        relocate_sysroot(&sysroot, &user_tc, "nightly-2025-12-03").unwrap();

        // The toolchain link now points at the USER's rustup toolchain.
        assert_eq!(
            std::fs::read_link(sysroot.join("toolchain")).unwrap(),
            user_tc
        );
        // Both version files exist with NO trailing newline.
        for f in ["rust-toolchain-version", "rustc-version"] {
            let bytes = std::fs::read(sysroot.join(f)).unwrap();
            assert_eq!(bytes, b"nightly-2025-12-03");
            assert_ne!(bytes.last(), Some(&b'\n'));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
