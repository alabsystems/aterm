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
    let bytes =
        std::fs::read(built).map_err(|e| format!("could not read {} ({e})", built.display()))?;
    let mut h = aterm_digest::Sha256::new();
    h.update(&bytes);
    let sha256 = crate::hex(&h.finalize());
    let dir = host.join(&sha256);
    let exe = dir.join(EXE);
    let placed = |fresh| Placed {
        sha256: sha256.clone(),
        exe: exe.clone(),
        fresh,
    };
    let placed_regular = || {
        std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) && crate::is_regular_file(&exe)
    };
    if placed_regular() {
        return Ok(placed(false));
    }
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
        std::fs::write(&ents, ENTITLEMENTS).map_err(|e| io("write", &ents, e))?;
        let mut file = std::fs::File::create(&copy).map_err(|e| io("write", &copy, e))?;
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
    written.map(placed)
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
    let bytes =
        std::fs::read(exe).map_err(|e| format!("could not read {} ({e})", exe.display()))?;
    let mut h = aterm_digest::Sha256::new();
    h.update(&bytes);
    Ok(crate::hex(&h.finalize()))
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
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(dir.join(RAN))
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    aterm_uds::keep_out_of_children(&file).ok()?;
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
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        // While a copy is removed, its marker is held exclusively, so no helper takes it.
        let mut held = None;
        let removable = if let Some(rest) = name.strip_prefix(".partial-") {
            // `.partial-<sha256>-<pid>`: a placing whose process is gone; nothing ran from it.
            rest.rsplit_once('-')
                .map(|(_, pid)| pid)
                .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|pid| pid.parse::<u32>().ok())
                .is_some_and(|pid| !aterm_uds::process::pid_alive(pid))
        } else if crate::is_hex(&name, 64) {
            let named = record.is_some_and(|r| r.helper_sha256 == name);
            if name != keep && !named {
                held = removal_guard(&path);
                held.is_some()
            } else {
                false
            }
        } else {
            false
        };
        if removable && std::fs::remove_dir_all(&path).is_ok() {
            removed.push(path);
        }
        drop(held);
    }
    removed.sort();
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signing_argv_is_ad_hoc_with_the_helpers_entitlements_only() {
        let args = codesign_args(
            Path::new("/h/x/aterm-vz-host.entitlements"),
            Path::new("/h/x/aterm-vz-host"),
        );
        let args: Vec<_> = args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--force",
                "--sign",
                "-",
                "--entitlements",
                "/h/x/aterm-vz-host.entitlements",
                "/h/x/aterm-vz-host"
            ]
        );
        assert_eq!(aterm_uds::os_path!("codesign"), "/usr/bin/codesign");
        // The plist names the virtualization entitlement and nothing else.
        let keys: Vec<_> = ENTITLEMENTS
            .lines()
            .filter_map(|l| l.trim().strip_prefix("<key>"))
            .collect();
        assert_eq!(keys, ["com.apple.security.virtualization</key>"]);
        assert!(ENTITLEMENTS.contains("<true/>"));
        // codesign's own XML reader (AMFIUnserializeXML) refuses a comment holding `--`,
        // which plutil passes (measured: "syntax error near line 8").
        let comment = ENTITLEMENTS
            .split_once("<!--")
            .and_then(|(_, rest)| rest.split_once("-->"))
            .map(|(inside, _)| inside)
            .expect("the plist's header comment");
        assert!(!comment.contains("--"), "a double hyphen in {comment:?}");
    }

    fn sha(bytes: &[u8]) -> String {
        let mut h = aterm_digest::Sha256::new();
        h.update(bytes);
        crate::hex(&h.finalize())
    }

    #[test]
    fn the_copy_is_signed_in_its_partial_directory_then_placed_under_the_builds_hash() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let built = tmp.path().join("target/aterm-vz-host");
        std::fs::create_dir_all(built.parent().unwrap()).unwrap();
        std::fs::write(&built, b"\xcf\xfa\xed\xfe helper").unwrap();
        let host = tmp.path().join("vz.noindex/host");
        let mut signed = Vec::new();
        let placed = place(&built, &host, &mut |copy, ents| {
            signed.push((copy.to_path_buf(), ents.to_path_buf()));
            assert!(
                copy.parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(".partial-")
            );
            assert_eq!(std::fs::read_to_string(ents).unwrap(), ENTITLEMENTS);
            // What codesign does: rewrite the file in place.
            std::fs::write(copy, b"\xcf\xfa\xed\xfe helper, signed").unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(signed.len(), 1);
        assert!(placed.fresh);
        assert_eq!(
            placed.sha256,
            sha(b"\xcf\xfa\xed\xfe helper"),
            "the built file's hash"
        );
        assert_eq!(placed.exe, host.join(&placed.sha256).join(EXE));
        assert_eq!(
            std::fs::read(&placed.exe).unwrap(),
            b"\xcf\xfa\xed\xfe helper, signed"
        );
        let mode = std::fs::metadata(&placed.exe).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o555);
        assert_eq!(
            std::fs::read(&built).unwrap(),
            b"\xcf\xfa\xed\xfe helper",
            "nothing is signed in place in target/"
        );
        // Placed again: kept, never signed twice.
        let again = place(&built, &host, &mut |_, _| panic!("signed twice")).unwrap();
        assert!(!again.fresh);
        assert_eq!(again.exe, placed.exe);
        let names: Vec<_> = std::fs::read_dir(&host)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "no partial directory is left: {names:?}");
        // The helper names itself as the lab's `Attach` does: the built file's hash, which
        // its signed copy's bytes no longer have; one run from target/ hashes its own file.
        assert_eq!(helper_sha256(&placed.exe).unwrap(), placed.sha256);
        assert_eq!(helper_sha256(&built).unwrap(), placed.sha256);
        assert!(
            helper_sha256(&tmp.path().join("gone"))
                .unwrap_err()
                .contains("could not read")
        );
    }

    #[test]
    fn a_failed_signing_places_nothing() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let built = tmp.path().join("aterm-vz-host");
        std::fs::write(&built, b"helper").unwrap();
        let host = tmp.path().join("host");
        let err = place(&built, &host, &mut |_, _| Err("codesign said no".into())).unwrap_err();
        assert_eq!(err, "codesign said no");
        assert_eq!(std::fs::read_dir(&host).unwrap().count(), 0);
    }

    fn copy(host: &Path, name: &str, markers: &[&str]) -> PathBuf {
        let dir = host.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(EXE), b"helper").unwrap();
        for m in markers {
            std::fs::write(dir.join(m), b"pid 1\n").unwrap();
        }
        dir
    }

    #[test]
    fn a_copy_is_reclaimed_only_once_it_proved_it_ran_and_no_record_names_it() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let host = tmp.path();
        let current = copy(host, &"a".repeat(64), &[STARTED, RAN]);
        let never_started = copy(host, &"b".repeat(64), &[]);
        let ran = copy(host, &"c".repeat(64), &[STARTED, RAN]);
        let unproven = copy(host, &"d".repeat(64), &[STARTED]);
        let named = copy(host, &"e".repeat(64), &[STARTED, RAN]);
        let other = host.join("not-a-hash");
        std::fs::create_dir_all(&other).unwrap();
        let record = Record {
            pid: 1,
            uniq: 1,
            birth_us: 1,
            helper_sha256: "e".repeat(64),
            proto: crate::proto::PROTO,
            live_disk: "/l".into(),
            started: 0,
            boot_id: None,
            agent_sha256: None,
        };
        let removed = reclaim(host, &"a".repeat(64), Some(&record));
        assert_eq!(removed, [never_started.clone(), ran.clone()]);
        for kept in [&current, &unproven, &named, &other] {
            assert!(kept.exists(), "{} kept", kept.display());
        }
        // The marker's own path: a helper outside a content-addressed directory writes none.
        let loose = tmp.path().join("loose/aterm-vz-host");
        assert_eq!(content_dir(&loose), None);
        assert_eq!(
            content_dir(&unproven.join(EXE)).map(|(_, n)| n.len()),
            Some(64)
        );
        let in_use = mark_ran(&unproven.join(EXE)).expect("the marker is held");
        assert!(
            std::fs::read_to_string(unproven.join(RAN))
                .unwrap()
                .starts_with("pid ")
        );
        // A helper still running from it holds its marker: kept, though no record names it
        // (its helper serves another state directory).
        assert!(reclaim(host, &"a".repeat(64), Some(&record)).is_empty());
        assert!(unproven.exists());
        // The negative control: once its helper is gone, the copy goes.
        drop(in_use);
        assert_eq!(reclaim(host, &"a".repeat(64), Some(&record)), [unproven]);
        // A marker with no line (a helper that died before it wrote one) proves nothing.
        let empty = copy(host, &"9".repeat(64), &[STARTED]);
        std::fs::write(empty.join(RAN), b"").unwrap();
        assert!(reclaim(host, &"a".repeat(64), Some(&record)).is_empty());
        assert!(empty.exists());
        // A symlinked `ran` marker proves nothing and is refused by `mark_ran`.
        let linked = copy(host, &"8".repeat(64), &[STARTED]);
        let outside = tmp.path().join("outside-ran");
        std::fs::write(&outside, b"pid 1\n").unwrap();
        std::os::unix::fs::symlink(&outside, linked.join(RAN)).unwrap();
        assert!(!super::ran(&linked));
        assert!(mark_ran(&linked.join(EXE)).is_none());
        assert_eq!(std::fs::read(&outside).unwrap(), b"pid 1\n");
    }

    #[test]
    fn unknown_started_markers_keep_copies_until_their_run_is_proven_and_unheld() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let host = tmp.path().join("host");
        let never = copy(&host, &"a".repeat(64), &[]);
        let proved = copy(&host, &"b".repeat(64), &[STARTED, RAN]);
        assert_eq!(reclaim(&host, "", None), [never, proved]);

        for kind in ["symlink", "dangling", "directory"] {
            let dir = copy(&host, &sha(kind.as_bytes()), &[]);
            let marker = dir.join(STARTED);
            let outside = tmp.path().join(format!("outside-{kind}"));
            if kind == "directory" {
                std::fs::create_dir(&marker).unwrap();
            } else {
                if kind == "symlink" {
                    std::fs::write(&outside, b"retained outside marker\n").unwrap();
                }
                std::os::unix::fs::symlink(&outside, &marker).unwrap();
            }
            assert!(std::fs::symlink_metadata(&marker).is_ok());
            assert!(!super::ran(&dir));
            assert!(
                reclaim(&host, "", None).is_empty(),
                "a {kind} marker proves no safe retirement"
            );
            assert_eq!(std::fs::read(dir.join(EXE)).unwrap(), b"helper");
            assert!(std::fs::symlink_metadata(&marker).is_ok());

            // These inert bytes are never executed: only the normal marker/lock API runs.
            let held = mark_ran(&dir.join(EXE)).expect("a witnessed marker is held");
            assert!(reclaim(&host, "", None).is_empty());
            drop(held);
            assert_eq!(reclaim(&host, "", None), [dir]);
            if kind == "symlink" {
                assert_eq!(
                    std::fs::read(&outside).unwrap(),
                    b"retained outside marker\n"
                );
            }
        }
    }

    #[test]
    fn replacing_a_nonregular_executable_requires_safe_retirement_of_its_real_directory() {
        for state in ["never", "ran", "unproven", "in-use"] {
            let tmp = aterm_tempfile::tempdir().unwrap();
            let built = tmp.path().join("built");
            std::fs::write(&built, b"inert built helper").unwrap();
            let host = tmp.path().join("host");
            let dir = host.join(sha(b"inert built helper"));
            std::fs::create_dir_all(&dir).unwrap();
            let decoy = tmp.path().join("inert-decoy");
            std::fs::write(&decoy, b"retained decoy").unwrap();
            std::os::unix::fs::symlink(&decoy, dir.join(EXE)).unwrap();
            let payload = dir.join("retained-state");
            std::fs::write(&payload, b"unproven state must survive").unwrap();
            if state != "never" {
                std::fs::write(dir.join(STARTED), b"synthetic started\n").unwrap();
            }
            let held = matches!(state, "ran" | "in-use")
                .then(|| mark_ran(&dir.join(EXE)).expect("the inert copy's marker"));
            let held = if state == "ran" {
                drop(held);
                None
            } else {
                held
            };
            assert!(std::fs::symlink_metadata(&dir).unwrap().is_dir());
            assert!(
                std::fs::symlink_metadata(dir.join(EXE))
                    .unwrap()
                    .is_symlink()
            );
            let result = place(&built, &host, &mut |copy, _| {
                std::fs::write(copy, b"inert signed replacement").unwrap();
                Ok(())
            });
            if matches!(state, "never" | "ran") {
                let placed = result.expect("safe, unused copies can be replaced");
                assert!(placed.fresh);
                assert_eq!(
                    std::fs::read(&placed.exe).unwrap(),
                    b"inert signed replacement"
                );
            } else {
                assert!(
                    result.is_err(),
                    "the {state} real directory must be retained"
                );
                assert!(
                    std::fs::symlink_metadata(dir.join(EXE))
                        .unwrap()
                        .is_symlink()
                );
                assert_eq!(
                    std::fs::read(&payload).unwrap(),
                    b"unproven state must survive"
                );
                assert_eq!(
                    std::fs::read(dir.join(STARTED)).unwrap(),
                    b"synthetic started\n"
                );
            }
            assert_eq!(std::fs::read(&decoy).unwrap(), b"retained decoy");
            let names: Vec<_> = std::fs::read_dir(&host)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(names.len(), 1, "partial copies are not retained: {names:?}");
            drop(held);
        }
    }

    #[test]
    fn place_replaces_a_symlinked_dir_or_exe_with_a_real_signed_copy() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let built = tmp.path().join("target/aterm-vz-host");
        std::fs::create_dir_all(built.parent().unwrap()).unwrap();
        std::fs::write(&built, b"helper-bytes").unwrap();
        let digest = sha(b"helper-bytes");
        let host = tmp.path().join("host");
        std::fs::create_dir_all(&host).unwrap();
        let decoy_dir = tmp.path().join("decoy");
        std::fs::create_dir_all(&decoy_dir).unwrap();
        let decoy_exe = decoy_dir.join(EXE);
        std::fs::write(&decoy_exe, b"unsigned-decoy").unwrap();
        std::fs::write(decoy_dir.join(STARTED), b"unproven decoy\n").unwrap();
        // A symlink at `host/<sha256>` pointing to `decoy_dir` must not be trusted as placed.
        std::os::unix::fs::symlink(&decoy_dir, host.join(&digest)).unwrap();
        let placed = place(&built, &host, &mut |copy, _| {
            std::fs::write(copy, b"signed-real").unwrap();
            Ok(())
        })
        .unwrap();
        assert!(placed.fresh);
        assert!(
            std::fs::symlink_metadata(host.join(&digest))
                .unwrap()
                .is_dir()
        );
        assert_eq!(std::fs::read(&placed.exe).unwrap(), b"signed-real");
        assert_eq!(std::fs::read(&decoy_exe).unwrap(), b"unsigned-decoy");
        assert_eq!(
            std::fs::read(decoy_dir.join(STARTED)).unwrap(),
            b"unproven decoy\n"
        );
    }

    #[test]
    fn a_dead_placings_partial_directory_is_reclaimed_and_a_live_ones_kept() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let mine = tmp.path().join(format!(
            ".partial-{}-{}",
            "f".repeat(64),
            std::process::id()
        ));
        std::fs::create_dir_all(&mine).unwrap();
        // pid 0 is never a live placing (`pid_alive` refuses it).
        let dead = tmp.path().join(format!(".partial-{}-0", "f".repeat(64)));
        std::fs::create_dir_all(&dead).unwrap();
        let non_canonical = tmp.path().join(format!(".partial-{}-+0", "f".repeat(64)));
        std::fs::create_dir_all(&non_canonical).unwrap();
        assert_eq!(reclaim(tmp.path(), "", None), [dead]);
        assert!(mine.exists());
        assert!(non_canonical.exists());
    }
}
