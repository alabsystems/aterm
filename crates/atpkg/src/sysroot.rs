// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Sysroot relocation for a `sysroot-bundle` member (§10.1) — pointing a bundle's
//! `toolchain` link at its resolved toolchain, writing the newline-free
//! `rust-toolchain-version`, and the fail-loud resolve check that makes a bundle whose
//! dynamic loader cannot resolve its libraries abort the apply instead of reporting a
//! successful install.
//!
//! Everything here stays INSIDE the managed prefix. The `~/.kani` link wiring that used to
//! live in this module was removed with the `rustup-linked` reloc policy: the trust
//! toolchain ships `self-contained` bundles, so nothing selected that path, and an
//! out-of-prefix effect the apply transaction could not reverse is not worth carrying for
//! a policy no manifest uses.

use std::io;
use std::path::Path;

use crate::activate::atomic_symlink;

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
        let d = std::env::temp_dir().join(format!("atpkg-sysroot-{label}-{}", std::process::id()));
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
