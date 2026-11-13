// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dynamic loader resolution check for `sysroot-bundle` members.

use std::path::Path;

/// Why a resolve check refused a build ([`resolve_check`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unresolved {
    /// The check's sentence.
    pub why: String,
    /// Whether it is a verdict on the build's bytes on this Mac, which running them again
    /// repeats: the program died by a signal (its loader could not resolve it), or the kernel
    /// refused it for what the build shipped ([`refused_by_its_bytes`]). Otherwise it is a fault
    /// of this run or this machine: a hold that could not be written, a spawn that ran short of
    /// resources or that a mount or a policy refused, a wait that failed.
    pub verdict: bool,
}

impl Unresolved {
    fn fault(why: String) -> Self {
        Self {
            why,
            verdict: false,
        }
    }
}

/// Whether the kernel's refusal to start `bin` is a verdict on what the build shipped
/// ([`Unresolved`]): an image it will not execute (`ENOEXEC`, run as the image itself, never as
/// a shell script), or a file shipped with no execute bit (`EACCES` while `bin` reads so). A
/// refusal of a file that may be executed (`EACCES` from a `noexec` mount, `EPERM` from an
/// execution policy) is the machine's, which an administrator can lift: no verdict.
fn refused_by_its_bytes(e: &std::io::Error, bin: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        match e.raw_os_error() {
            Some(libc::ENOEXEC) => true,
            Some(libc::EACCES) => {
                std::fs::metadata(bin).is_ok_and(|m| m.permissions().mode() & 0o111 == 0)
            }
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (e, bin);
        false
    }
}

/// Runs `bin --version`, the image itself (a file the kernel will not execute is not handed to
/// the system shell, whose own exit would pass for the program's), and requires a non-signal
/// exit code.
///
/// # Errors
/// When the build cannot be held, or the binary cannot be spawned or is killed by a signal,
/// each said with whether it is a verdict on the build ([`Unresolved::verdict`]). The build is
/// held from before the spawn (`store::hold_first_exec`) and released only once the child
/// exited on its own or never started.
pub fn resolve_check(bin: &Path) -> Result<(), Unresolved> {
    use aterm_uds::spawnfd::{NativeImage as _, Stdio};
    let hold = crate::store::hold_first_exec(bin).map_err(|e| {
        Unresolved::fault(format!(
            "resolve check: cannot hold {} for its first exec: {e}",
            bin.display()
        ))
    })?;
    let mut child = match aterm_uds::spawnfd::command(bin)
        .native_image(true)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            hold.ran();
            return Err(Unresolved {
                why: format!("resolve check: cannot spawn {}: {e}", bin.display()),
                verdict: refused_by_its_bytes(&e, bin),
            });
        }
    };
    let (why, verdict) = match child.wait() {
        // Any exit code proves the loader resolved the bundle; a signal death is the
        // dyld/ld.so failure.
        Ok(status) if status.code().is_some() => {
            hold.ran();
            return Ok(());
        }
        Ok(_) => (
            String::from("was killed by a signal (unresolved dynamic library?)"),
            true,
        ),
        Err(e) => (format!("could not be waited for ({e})"), false),
    };
    // Nothing showed it ran, so macOS may still be assessing its first exec: `hold` stands.
    Err(Unresolved {
        why: format!(
            "resolve check: {} {why} — refusing a broken toolchain install; its build stays on \
             disk while macOS may still be assessing that first exec",
            bin.display()
        ),
        verdict,
    })
}

#[cfg(all(test, unix))]
#[path = "sysroot_tests.rs"]
mod tests;
