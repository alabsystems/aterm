// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `com.apple.provenance` — the extended attribute macOS stamps on every file a
//! provenance-TRACKED process writes, and the predicate every surface in aterm uses to
//! see it.
//!
//! # What was measured (2026-09-12, macOS 26 / Darwin 25.6, this repo's
//! `scratchpad/provenance/probe.sh` + `probe2.sh`)
//!
//! * A process is tracked when its EXECUTABLE carries the tag, or when its PARENT is
//!   tracked. Everything a tracked process creates or touches is tagged: `open(O_CREAT)`,
//!   but also `rename`, `link`, `chmod`, `utimes`, `setxattr` and an append — a tracked
//!   parent that merely renames a clean file tags it. Reads, `stat` and `fsync` do not.
//! * The tag is inherited across `fork`, `posix_spawn`, `setsid`, `osascript`,
//!   `launchctl asuser`, and it SURVIVES `exec` into an untagged image: a tagged
//!   `#!/bin/sh` shim that `exec`s a clean tool still produces tagged output. The shims
//!   atpkg lays in `bin/` are exactly that shape.
//! * A job launchd spawns (`launchctl submit`, `launchctl bootstrap gui/<uid>`) is NOT a
//!   child of the submitter: with an untagged executable it writes clean files even when
//!   submitted by a tracked process, even into a tagged directory, even through a tagged
//!   symlink. With a tagged executable it writes tagged files — the tag follows the
//!   executable.
//! * A byte copy of a tagged executable made by an untracked process (`cat a > b`) is
//!   clean and runs clean; `cp`/`ditto` copy the attribute along with the bytes.
//! * `xattr -d com.apple.provenance` and `xattr -c` exit 0 and remove nothing.
//!
//! # Why aterm cares
//!
//! The release cutter builds with the Trust toolchain atpkg installs. If that
//! `trustc`/`targo` carries the tag, every object file, the proof snapshot and the
//! release artifacts inherit it, and `tools/proof_snapshot.py` refuses "published proof
//! snapshot has extended metadata" AFTER the ledger claim — a burned build number
//! (v0.83.0, 2026-09-12: bundle `trust/8590`, seeded from a shell that was itself
//! tracked). The 02:27 seed of `trust/8589` by the app's own update lane, from an
//! untracked app, came out clean, and the cut succeeded once that bundle was first on
//! PATH. Three surfaces read this module: the cutter's pre-claim gate
//! (`aterm-release::gates::provenance_gate`), `aterm pkg doctor`, and the store's own
//! staging lane, which hands extraction to an untracked launchd job when the installer
//! measures itself as tracked ([`crate::stage_helper`]).

use std::io;
use std::path::{Path, PathBuf};

/// The attribute name, exactly as `xattr -l` prints it.
pub const PROVENANCE_XATTR: &str = "com.apple.provenance";

/// What the tag does to a release cut, in one sentence, for every refusal that names it.
pub const WHAT_IT_BREAKS: &str = "the tag follows the executable and the parent process: every \
    file a tagged trustc/targo (or a cutter descended from a tagged process) writes inherits \
    it, `xattr -d` exits 0 and removes nothing, and tools/proof_snapshot.py refuses a tagged \
    proof snapshot AFTER the ledger claim — a burned build number (v0.83.0, 2026-09-12)";

/// The cure, for every refusal that names it.
pub const REMEDY: &str = "re-seed the bundle untagged — `aterm pkg uninstall <program> && \
    aterm pkg install <program>` (this atpkg stages through an untracked launchd job when it \
    measures itself as tracked) — or point TRUST_STAGE2_BIN at an untagged bundle's bin/ \
    (`xattr -l <bin>/trustc` prints nothing on a clean one), and run the cut itself via \
    `launchctl submit` from an untagged cutter (docs/RELEASING.md)";

/// The extended-attribute NAMES on `path` (symlinks followed), in the order the kernel
/// lists them.
///
/// `Ok(empty)` on a filesystem that has no extended attributes (`ENOTSUP`), and on every
/// non-macOS platform — the tag is a macOS phenomenon and there is nothing to read. A
/// path the kernel refuses to inspect (`ENOENT`, `EACCES`) is an error, not "clean": a
/// predicate that answers `false` on a failure to look is the bound-returned-as-a-fact
/// defect this repo has paid for before.
#[cfg(target_os = "macos")]
pub fn xattr_names(path: &Path) -> io::Result<Vec<String>> {
    use std::os::unix::ffi::OsStrExt as _;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // The list can grow between the size query and the read (another writer adding an
    // attribute); `ERANGE` then says so and the loop asks again. Bounded so a pathological
    // writer cannot hold this in a spin.
    for _ in 0..4 {
        // SAFETY: `c_path` is a NUL-terminated C string that outlives both calls; a null
        // buffer with size 0 is the documented size query; `options = 0` follows symlinks,
        // which is what a check on an executable the kernel will exec wants.
        let needed = unsafe { libc::listxattr(c_path.as_ptr(), std::ptr::null_mut(), 0, 0) };
        if needed < 0 {
            return unsupported_as_empty(io::Error::last_os_error());
        }
        if needed == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; needed as usize];
        // SAFETY: `buf` is a live, writable allocation of exactly `buf.len()` bytes, which
        // is the size passed; `c_path` as above.
        let got = unsafe {
            libc::listxattr(
                c_path.as_ptr(),
                buf.as_mut_ptr().cast::<libc::c_char>(),
                buf.len(),
                0,
            )
        };
        if got < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ERANGE) {
                continue;
            }
            return unsupported_as_empty(err);
        }
        buf.truncate(got as usize);
        return Ok(buf
            .split(|b| *b == 0)
            .filter(|name| !name.is_empty())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect());
    }
    Err(io::Error::other(
        "listxattr kept answering ERANGE — the attribute list would not hold still",
    ))
}

/// See the macOS body: nothing to read here.
#[cfg(not(target_os = "macos"))]
pub fn xattr_names(_path: &Path) -> io::Result<Vec<String>> {
    Ok(Vec::new())
}

/// `ENOTSUP` (a volume without extended attributes) is an honest empty answer; anything
/// else is a failure to look and stays an error.
#[cfg(target_os = "macos")]
fn unsupported_as_empty(err: io::Error) -> io::Result<Vec<String>> {
    if matches!(
        err.raw_os_error(),
        Some(libc::ENOTSUP) | Some(libc::EOPNOTSUPP)
    ) {
        Ok(Vec::new())
    } else {
        Err(err)
    }
}

/// Whether `path` carries the extended attribute `attr`. A path that cannot be inspected
/// reads as NOT carrying it — callers that must distinguish "clean" from "could not look"
/// use [`xattr_names`] directly (the cutter's gate does).
#[must_use]
pub fn carries(path: &Path, attr: &str) -> bool {
    xattr_names(path).is_ok_and(|names| names.iter().any(|n| n == attr))
}

/// Whether `path` carries `com.apple.provenance`.
#[must_use]
pub fn carries_provenance(path: &Path) -> bool {
    carries(path, PROVENANCE_XATTR)
}

/// The regular files DIRECTLY under `dir` that carry `attr`, and how many regular files
/// there were to look at — the shape a `bin/` scan reports ("3 of 31").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scan {
    /// Carriers, sorted by name.
    pub carriers: Vec<PathBuf>,
    /// Regular files inspected.
    pub total: usize,
}

/// Scan one directory level for regular files carrying `attr` (see [`Scan`]). An
/// unreadable `dir` is an empty scan: `total == 0` says nothing was inspected.
#[must_use]
pub fn tagged_files_in(dir: &Path, attr: &str) -> Scan {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Scan::default();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
        .collect();
    paths.sort();
    let total = paths.len();
    let carriers = paths.into_iter().filter(|p| carries(p, attr)).collect();
    Scan { carriers, total }
}

/// Whether THIS process is provenance-tracked — MEASURED, by writing a probe file into
/// `scratch` and reading the attribute back, never inferred from the binary's own
/// attributes (a clean binary under a tracked parent is tracked; that is the whole
/// incident). Answered once per process: tracking is decided at exec and does not change.
///
/// `false` when the probe cannot be written or inspected: a lane that cannot measure
/// must not claim tracking and route an install through machinery it has no evidence it
/// needs. The probe is removed before returning.
#[must_use]
pub fn process_is_tracked(scratch: &Path) -> bool {
    static MEASURED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MEASURED.get_or_init(|| measure_tracked(scratch).unwrap_or(false))
}

/// The un-cached measurement behind [`process_is_tracked`]: `Some(tagged)` when a probe
/// could be written and read, `None` when it could not.
pub fn measure_tracked(scratch: &Path) -> Option<bool> {
    if !cfg!(target_os = "macos") {
        return Some(false);
    }
    let probe = scratch.join(format!(".provenance-probe-{}", std::process::id()));
    std::fs::write(&probe, b"probe\n").ok()?;
    let verdict = xattr_names(&probe)
        .ok()
        .map(|names| names.iter().any(|n| n == PROVENANCE_XATTR));
    let _ = std::fs::remove_file(&probe);
    verdict
}

/// Set an extended attribute — TEST SCAFFOLDING for the synthetic-attribute tests here
/// and in `doctor.rs`/`install.rs`: `com.apple.provenance` itself cannot be minted by
/// hand (`xattr -w` is refused), so the predicate is exercised with `user.*` names
/// through the very `listxattr` path production reads.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn set_xattr_for_test(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let c_name = std::ffi::CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
    // SAFETY: both C strings and the value slice outlive the call; the length passed is
    // the slice's own.
    let rc = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            value.as_ptr().cast::<libc::c_void>(),
            value.len(),
            0,
            0,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("atpkg-provenance-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The name this process cannot mint is read through the same syscall as a name it
    /// can: a synthetic `user.*` attribute is listed, and only where it was set.
    #[test]
    fn a_synthetic_attribute_is_listed_where_set_and_nowhere_else() {
        let d = tmp("listed");
        let tagged = d.join("tagged");
        let clean = d.join("clean");
        std::fs::write(&tagged, b"x").unwrap();
        std::fs::write(&clean, b"x").unwrap();
        set_xattr_for_test(&tagged, "user.aterm.probe", b"1").unwrap();
        assert!(carries(&tagged, "user.aterm.probe"));
        assert!(!carries(&clean, "user.aterm.probe"));
        // A name that is NOT set is not reported, whatever else the file carries.
        assert!(!carries(&tagged, "user.aterm.other"));
        // The listing names the attribute exactly as `xattr -l` would.
        assert!(
            xattr_names(&tagged)
                .unwrap()
                .iter()
                .any(|n| n == "user.aterm.probe")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// "Could not look" is an error, never "clean".
    #[test]
    fn a_missing_path_is_an_error_not_a_clean_answer() {
        let d = tmp("missing");
        let err = xattr_names(&d.join("nope")).expect_err("ENOENT must surface");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!carries(&d.join("nope"), PROVENANCE_XATTR));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The directory scan counts regular files only, names the carriers sorted, and
    /// ignores symlinks and subdirectories.
    #[test]
    fn the_bin_scan_counts_regular_files_and_names_the_carriers() {
        let d = tmp("scan");
        let bin = d.join("bin");
        std::fs::create_dir_all(bin.join("subdir")).unwrap();
        for name in ["trustc", "targo", "ay"] {
            std::fs::write(bin.join(name), b"#!/bin/true\n").unwrap();
        }
        std::os::unix::fs::symlink("trustc", bin.join("rustc")).unwrap();
        set_xattr_for_test(&bin.join("trustc"), "user.aterm.probe", b"1").unwrap();
        set_xattr_for_test(&bin.join("ay"), "user.aterm.probe", b"1").unwrap();
        let scan = tagged_files_in(&bin, "user.aterm.probe");
        assert_eq!(scan.total, 3, "three regular files, one link, one dir");
        assert_eq!(scan.carriers, vec![bin.join("ay"), bin.join("trustc")]);
        // A different attribute name finds nothing.
        assert_eq!(
            tagged_files_in(&bin, "user.aterm.other").carriers,
            Vec::<PathBuf>::new()
        );
        // An unreadable dir inspects nothing — and says so through `total`.
        assert_eq!(
            tagged_files_in(&d.join("absent"), "user.aterm.probe"),
            Scan::default()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The measurement leaves no probe behind and agrees with a direct read of a file
    /// this process writes — whichever way this test happens to be run (tracked or not),
    /// the two must say the same thing.
    #[test]
    fn the_tracking_measurement_matches_a_direct_write_and_cleans_up() {
        let d = tmp("measure");
        let measured = measure_tracked(&d).expect("a writable scratch dir must be measurable");
        let witness = d.join("witness");
        std::fs::write(&witness, b"w").unwrap();
        assert_eq!(measured, carries_provenance(&witness));
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().starts_with(".provenance-probe-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "the probe must be removed: {leftovers:?}"
        );
        // The cached answer agrees with the fresh one.
        assert_eq!(process_is_tracked(&d), measured);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An unwritable scratch cannot be measured — `None`, so the cached predicate answers
    /// `false` (never routes on a measurement it did not take).
    #[test]
    fn an_unwritable_scratch_is_unmeasurable() {
        let d = tmp("unwritable");
        assert_eq!(measure_tracked(&d.join("absent").join("deeper")), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
