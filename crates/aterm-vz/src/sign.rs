// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The helper's content-addressed ad hoc signing (`docs/DESIGN-vz-guest-2026-10-23.md`
//! §5.1): the lab copies the `aterm-vz-host` it built to `vz.noindex/host/<sha256>/`, the
//! SHA-256 of the built file, and signs the copy there with `codesign --force --sign -
//! --entitlements` and [`ENTITLEMENTS`] (ad hoc signing allows the virtualization
//! entitlement, fact 33). Nothing is ever signed in place in `target/`.
//!
//! The copy is made and signed in a partial directory and renamed whole into its name, so a
//! content-addressed directory holds a signed helper or does not exist. A copy is removed
//! ([`reclaim`]) only after a helper from it has written its first line (its `ran` marker,
//! [`mark_ran`]), and only when no record names it: AGENTS.md's rule for a Mach-O copied
//! and executed, which keeps syspolicyd off a file it may still be assessing. A copy that
//! was started ([`mark_started`]) and never proved it ran stays in place. The copies are
//! this user's, shared by every state directory, so a live helper also holds its marker
//! under a shared `flock` ([`InUse`]): a copy some helper runs from is kept whichever
//! state's record the placing read.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use crate::record::{DiskLock, Record};

/// The helper's one entitlement, as `codesign` takes it.
pub const ENTITLEMENTS: &str = include_str!("../aterm-vz-host.entitlements");

/// The helper's file name in its directory.
pub const EXE: &str = "aterm-vz-host";

/// The entitlements file beside it, which signed it.
pub const ENTITLEMENTS_FILE: &str = "aterm-vz-host.entitlements";

/// The marker the lab writes before it starts a helper from a copy.
pub const STARTED: &str = "started";

/// The marker the helper writes once it runs: the line it wrote from its copy.
pub const RAN: &str = "ran";

/// `codesign`'s arguments that sign `target` ad hoc with the entitlements at
/// `entitlements`.
#[must_use]
pub fn codesign_args(entitlements: &Path, target: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["--force", "--sign", "-", "--entitlements"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(entitlements.as_os_str().to_owned());
    args.push(target.as_os_str().to_owned());
    args
}

/// Sign `target` with the OS's `codesign` (KEEP-OS, init-system row 1), under a one-minute
/// hang detector.
///
/// # Errors
/// It could not start, did not finish, or exited unsuccessfully (with its stderr).
pub fn codesign(target: &Path, entitlements: &Path) -> Result<(), String> {
    let codesign = aterm_uds::os_path!("codesign");
    let mut cmd = aterm_uds::spawnfd::command(codesign);
    cmd.args(codesign_args(entitlements, target));
    let out = aterm_uds::deadline::output_within(&mut cmd, crate::detect::SPAN)
        .map_err(|e| format!("{codesign} {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{codesign} --sign - exited {:?} on {}: {}",
            out.status.code(),
            target.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// A helper placed under its content address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    /// The SHA-256 of the built file: the directory's name and the record's
    /// `helper_sha256`.
    pub sha256: String,
    /// The signed copy.
    pub exe: PathBuf,
    /// Whether this placing made it (else the same helper was there).
    pub fresh: bool,
}

/// Maximum helper executable size accepted by [`place`] and [`helper_sha256`].
const MAX_HELPER_BYTES: u64 = 256 * 1024 * 1024;

/// Copy `built` to `host/<sha256>/aterm-vz-host` and sign the copy there with `sign`
/// ([`codesign`] in the lab; the tests' records the argv), then make it 0555 and rename its
/// directory into place. A completed copy already there is kept; replacing an incomplete real
/// directory requires the same execution proof and in-use protection as [`reclaim`]. A rename
/// only ever puts a whole, signed copy under the name.
///
/// # Errors
/// `built` could not be read, the signing failed (nothing is placed), a write failed, or an
/// existing incomplete copy cannot be proven safe to replace.
pub fn place(
    built: &Path,
    host: &Path,
    sign: &mut dyn FnMut(&Path, &Path) -> Result<(), String>,
) -> Result<Placed, String> {
    let sha256 = crate::sha256_regular_bounded(built, MAX_HELPER_BYTES)
        .map_err(|e| format!("could not read {} ({e})", built.display()))?;
    let dir = host.join(&sha256);
    let exe = dir.join(EXE);
    let placed_regular = || {
        std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) && crate::is_regular_file(&exe)
    };
    if placed_regular() {
        return Ok(Placed {
            sha256,
            exe,
            fresh: false,
        });
    }
    let bytes = crate::read_regular_bounded(built, MAX_HELPER_BYTES)
        .map_err(|e| format!("could not read {} ({e})", built.display()))?;
    std::fs::create_dir_all(host)
        .map_err(|e| format!("could not make {} ({e})", host.display()))?;
    static PARTIAL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = PARTIAL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let partial = host.join(format!(".partial-{sha256}-{seq}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&partial);
    let written = (|| -> Result<bool, String> {
        let io = |what: &str, p: &Path, e: std::io::Error| {
            format!("could not {what} {} ({e})", p.display())
        };
        std::fs::create_dir(&partial).map_err(|e| io("make", &partial, e))?;
        let copy = partial.join(EXE);
        let ents = partial.join(ENTITLEMENTS_FILE);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&ents)
            .and_then(|mut f| f.write_all(ENTITLEMENTS.as_bytes()))
            .map_err(|e| io("write", &ents, e))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&copy)
            .map_err(|e| io("write", &copy, e))?;
        file.write_all(&bytes).map_err(|e| io("write", &copy, e))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o755))
            .map_err(|e| io("chmod", &copy, e))?;
        drop(file);
        sign(&copy, &ents)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&copy)
            .map_err(|e| io("open", &copy, e))?;
        if !file.metadata().map_err(|e| io("open", &copy, e))?.is_file() {
            return Err(format!(
                "could not open {} (not a regular file)",
                copy.display()
            ));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o555))
            .map_err(|e| io("chmod", &copy, e))?;
        aterm_durable::durable(&file).map_err(|e| io("flush", &copy, e))?;
        aterm_durable::durable_dir(&partial).map_err(|e| io("flush", &partial, e))?;
        let mut _retiring = None;
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) => {
                if meta.is_dir() && crate::is_regular_file(&exe) {
                    // Another lab placed this helper meanwhile: keep its copy, which a helper
                    // may already run from.
                    return Ok(false);
                }
                let res = if meta.is_dir() {
                    // A missing or nonregular executable says nothing about whether this
                    // directory held a started copy. Keep its retirement hold through rename.
                    _retiring = Some(removal_guard(&dir).ok_or_else(|| {
                        format!(
                            "could not replace {} (copy is unproven or in use)",
                            dir.display()
                        )
                    })?);
                    std::fs::remove_dir_all(&dir)
                } else {
                    // Unlink only this entry, including a hash-directory alias, never its target.
                    std::fs::remove_file(&dir)
                };
                res.map_err(|e| io("remove", &dir, e))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io("inspect", &dir, error)),
        }
        if let Err(e) = std::fs::rename(&partial, &dir) {
            return if placed_regular() {
                Ok(false)
            } else {
                Err(io("rename into", &dir, e))
            };
        }
        aterm_durable::durable_dir(host).map_err(|e| io("flush", host, e))?;
        Ok(true)
    })();
    let _ = std::fs::remove_dir_all(&partial);
    written.map(|fresh| Placed { sha256, exe, fresh })
}

/// The content-addressed directory `exe` runs from, when it is one (`host/<sha256>/`).
#[must_use]
pub fn content_dir(exe: &Path) -> Option<(&Path, &str)> {
    let dir = exe.parent()?;
    let name = dir.file_name()?.to_str()?;
    crate::is_hex(name, 64).then_some((dir, name))
}

/// The content address of the helper running from `exe`: its directory's name when it runs
/// from a placed copy (the SHA-256 of the file the lab built, before signing, which the lab's
/// `Attach` names), else the SHA-256 of `exe` itself (a helper run from where it was built).
///
/// # Errors
/// `exe` is outside a content-addressed directory and could not be read.
pub fn helper_sha256(exe: &Path) -> Result<String, String> {
    if let Some((_, sha)) = content_dir(exe) {
        return Ok(sha.to_string());
    }
    crate::sha256_regular_bounded(exe, MAX_HELPER_BYTES)
        .map_err(|e| format!("could not read {} ({e})", exe.display()))
}

/// The lab is about to start a helper from `exe`: its copy must now prove it ran before it
/// is removed. Nothing is written for a helper outside a content-addressed directory.
pub fn mark_started(exe: &Path) {
    if let Some((dir, _)) = content_dir(exe)
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(dir.join(STARTED))
        && file.metadata().is_ok_and(|m| m.is_file())
        && file.set_len(0).is_ok()
    {
        let _ = writeln!(file, "{}", std::process::id());
    }
}

/// A helper's hold on the copy it runs from: a shared `flock` on the copy's `ran` marker,
/// held for the helper's whole life and let go by `LOCK_UN` when this is dropped. [`reclaim`]
/// keeps a copy whose marker a live process holds.
#[derive(Debug)]
#[must_use = "the copy is held in use only while this lives"]
pub struct InUse {
    file: std::fs::File,
}

impl Drop for InUse {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The helper runs from `exe`: the line it writes from its copy, its first act, under the
/// shared `flock` it then holds while it lives ([`InUse`]). The lock is taken before the line
/// is written, so a marker with a line is held for as long as its helper lives. `None` for a
/// helper outside a content-addressed directory, or a marker that could not be written.
pub fn mark_ran(exe: &Path) -> Option<InUse> {
    let (dir, _) = content_dir(exe)?;
    let file = aterm_uds::kept_out(|| {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(dir.join(RAN))
    })
    .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard's own take: its Drop releases by LOCK_UN"
    )]
    file.lock_shared().ok()?;
    let held = InUse { file };
    held.file.set_len(0).ok()?;
    (&held.file)
        .write_all(format!("pid {}\n", std::process::id()).as_bytes())
        .ok()?;
    Some(held)
}

/// Whether a helper from the copy at `dir` proved it ran: its `ran` marker holds a line.
fn ran(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(RAN)).is_ok_and(|m| m.is_file() && m.len() > 0)
}

struct RemovalGuard {
    _ran: Option<DiskLock>,
}

/// Unknown or nonregular markers are not evidence that a copy was never started. A proved
/// run also needs its marker held exclusively, so another state's live helper keeps the copy.
fn removal_guard(dir: &Path) -> Option<RemovalGuard> {
    if ran(dir) {
        let held = DiskLock::try_take(&dir.join(RAN)).ok().flatten()?;
        return ran(dir).then_some(RemovalGuard { _ran: Some(held) });
    }
    let absent = |name| {
        matches!(std::fs::symlink_metadata(dir.join(name)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    };
    (absent(STARTED) && absent(RAN)).then_some(RemovalGuard { _ran: None })
}

/// Remove the copies under `host` that no helper needs: every directory other than `keep`
/// that `record` does not name, that was never started or whose helper proved it ran, and
/// whose `ran` marker no live helper holds ([`InUse`], whatever state it serves); also a
/// partial directory a dead placing left. Returns what was removed.
#[must_use]
pub fn reclaim(host: &Path, keep: &str, record: Option<&Record>) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(host) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        // While a copy is removed, its marker is held exclusively, so no helper takes it.
        let mut held = None;
        let path = if let Some(rest) = name.strip_prefix(".partial-") {
            // `.partial-<sha256>-<pid>`: a placing whose process is gone; nothing ran from it.
            let dead = rest
                .rsplit_once('-')
                .map(|(_, pid)| pid)
                .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|pid| pid.parse::<u32>().ok())
                .is_some_and(|pid| !aterm_uds::process::pid_alive(pid));
            if !dead {
                continue;
            }
            entry.path()
        } else if crate::is_hex(name, 64) {
            let named = record.is_some_and(|r| r.helper_sha256 == name);
            if name == keep || named {
                continue;
            }
            let path = entry.path();
            held = removal_guard(&path);
            if held.is_none() {
                continue;
            }
            path
        } else {
            continue;
        };
        if std::fs::remove_dir_all(&path).is_ok() {
            removed.push(path);
        }
        drop(held);
    }
    removed.sort_unstable();
    removed
}

#[cfg(test)]
#[path = "sign_tests.rs"]
mod tests;
