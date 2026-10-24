// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Store layout paths for [`crate::lease`] and [`crate::discovery`].
//!
//! THE STORE'S SPOTLIGHT LAYOUT (macOS). Spotlight skips a folder named `*.noindex`, so a
//! build that ships a Rust sysroot ([`ships_rust_sysroot`]: thousands of crate files under
//! `lib/rustlib`) lives in `store/<program>/<build>.noindex`, and `store/<program>/<build>`
//! is a symbolic link whose target is exactly `<build>.noindex` ([`is_linked_aside`]).
//! Every path through `<build>` (the `current` link, the shims, the rustup view laid from it,
//! a lease) reaches the build unchanged; a compiler that canonicalizes its sysroot reports
//! the `.noindex` folder, which every reader of a store path takes for the build. A real directory at `store/<program>/<build>` that ships a sysroot is a
//! build not moved yet ([`spotlight_exposed_builds`]): atpkg installs every new one aside,
//! and moves an older one once no `current` link or shim names it and nothing runs from it.
//! A reader that cannot link this crate reads the same contract from the directory alone:
//! a plain numeric directory is not moved yet, a symbolic link is moved.

use std::path::{Path, PathBuf};

/// `<prefix>/store` — the versioned, immutable builds, `store/<program>/<build>`.
pub const STORE_DIR: &str = "store";

/// `<prefix>/compat` — the store's exec roots, one per build.
pub const COMPAT_DIR: &str = "compat";

/// `<prefix>/rustup` — the seam's views, one per rustup toolchain name.
pub const VIEWS_DIR: &str = "rustup";

/// `<prefix>/store/<program>/<build>`.
#[inline]
#[must_use]
pub fn build_dir(prefix: &Path, program: &str, build: u64) -> PathBuf {
    prefix.join(STORE_DIR).join(program).join(build.to_string())
}

/// `<prefix>/rustup/<name>` — the view the seam `name` targets.
#[inline]
#[must_use]
pub fn view_dir(prefix: &Path, name: &str) -> PathBuf {
    prefix.join(VIEWS_DIR).join(name)
}

/// `<name>.noindex` beside the store build `build_dir`: where its tree lives once laid aside.
#[must_use]
pub fn aside_of(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?.to_str()?;
    Some(build_dir.with_file_name(crate::build_cache::noindex(name)))
}

/// Whether `build_dir` is a store build laid aside: a symbolic link (by `lstat`) whose target
/// is exactly its own `<name>.noindex`, relative ([`aside_of`]).
#[must_use]
pub fn is_linked_aside(build_dir: &Path) -> bool {
    let Some(aside) = aside_of(build_dir) else {
        return false;
    };
    std::fs::symlink_metadata(build_dir).is_ok_and(|m| m.file_type().is_symlink())
        && std::fs::read_link(build_dir)
            .is_ok_and(|target| Some(target.as_os_str()) == aside.file_name())
}

/// Whether the build tree at `tree` ships a Rust sysroot: `lib/` and `lib/rustlib` are real
/// directories (`tree` itself may be reached through a link).
#[must_use]
pub fn ships_rust_sysroot(tree: &Path) -> bool {
    let real_dir = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    real_dir(&tree.join("lib")) && real_dir(&tree.join("lib").join("rustlib"))
}

/// A store build Spotlight still indexes: a real directory at `store/<program>/<build>` that
/// ships a Rust sysroot ([`spotlight_exposed_builds`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposedBuild {
    pub program: String,
    pub build: u64,
    /// `<prefix>/store/<program>/<build>`.
    pub dir: PathBuf,
    /// Whether `store/<program>/current` names it: the build in use, which stays where it is
    /// until an update makes another current.
    pub active: bool,
}

/// Every store build under `prefix` not yet laid aside ([`ExposedBuild`]), by program then
/// build; empty when the store cannot be read.
#[must_use]
pub fn spotlight_exposed_builds(prefix: &Path) -> Vec<ExposedBuild> {
    let mut out = Vec::new();
    let Ok(programs) = std::fs::read_dir(prefix.join(STORE_DIR)) else {
        return out;
    };
    for program_dir in programs.flatten() {
        if !program_dir.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Some(program) = program_dir.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // Resolved whole: a current build laid aside resolves to `<build>.noindex`, which is
        // no plain build, as it is no exposed one.
        let current = std::fs::canonicalize(program_dir.path().join("current"))
            .ok()
            .and_then(|target| {
                target
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(plain_build_name)
            });
        let Ok(entries) = std::fs::read_dir(program_dir.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(build) = entry.file_name().to_str().and_then(plain_build_name) else {
                continue;
            };
            let dir = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) && ships_rust_sysroot(&dir) {
                out.push(ExposedBuild {
                    program: program.clone(),
                    build,
                    dir,
                    active: current == Some(build),
                });
            }
        }
    }
    out.sort_by(|a, b| (&a.program, a.build).cmp(&(&b.program, b.build)));
    out
}

/// A build directory's name as the store writes one: digits, no leading zero, not zero.
fn plain_build_name(name: &str) -> Option<u64> {
    let digits = !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit());
    let build = (digits && !(name.len() > 1 && name.starts_with('0')))
        .then(|| name.parse::<u64>().ok())
        .flatten()?;
    is_valid_build_id(build).then_some(build)
}

/// Return `true` when `build` is a valid non-zero store build identifier.
#[inline]
#[must_use]
pub const fn is_valid_build_id(build: u64) -> bool {
    build != 0
}

/// Encode a toolchain lease record (`build_id`, `holder_pid`, `epoch_secs`) into 16 LE bytes.
#[inline]
#[must_use]
pub const fn encode_lease_record_le(build_id: u64, holder_pid: u32, epoch_secs: u32) -> [u8; 16] {
    [
        build_id as u8,
        (build_id >> 8) as u8,
        (build_id >> 16) as u8,
        (build_id >> 24) as u8,
        (build_id >> 32) as u8,
        (build_id >> 40) as u8,
        (build_id >> 48) as u8,
        (build_id >> 56) as u8,
        holder_pid as u8,
        (holder_pid >> 8) as u8,
        (holder_pid >> 16) as u8,
        (holder_pid >> 24) as u8,
        epoch_secs as u8,
        (epoch_secs >> 8) as u8,
        (epoch_secs >> 16) as u8,
        (epoch_secs >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into a toolchain lease record (`build_id`, `holder_pid`, `epoch_secs`).
#[inline]
#[must_use]
pub const fn decode_lease_record_le(bytes: [u8; 16]) -> (u64, u32, u32) {
    let build_id = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let holder_pid = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let epoch_secs = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (build_id, holder_pid, epoch_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store that holds every shape [`spotlight_exposed_builds`] tells apart.
    #[cfg(unix)]
    #[test]
    fn exposed_builds_are_the_plain_sysroot_folders_and_say_which_is_current() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("atc-layout-exposed-")
            .tempdir()
            .expect("a scratch prefix");
        let prefix = scratch.path();
        let sysroot = |program: &str, name: &str| {
            let tree = prefix.join(STORE_DIR).join(program).join(name);
            std::fs::create_dir_all(tree.join("lib/rustlib/aarch64-apple-darwin/lib")).unwrap();
            std::fs::create_dir_all(tree.join("bin")).unwrap();
            tree
        };
        let trust = prefix.join(STORE_DIR).join("trust");
        sysroot("trust", "9192");
        sysroot("trust", "15698");
        sysroot("trust", "20001.noindex");
        std::os::unix::fs::symlink("20001.noindex", trust.join("20001")).unwrap();
        std::os::unix::fs::symlink(trust.join("15698"), trust.join("current")).unwrap();
        sysroot("trust-mc", "7");
        std::fs::create_dir_all(prefix.join(STORE_DIR).join("claude/31/bin")).unwrap();
        std::fs::write(trust.join("9192.ready"), "ok\n").unwrap();
        std::fs::create_dir_all(trust.join("0812/lib/rustlib")).unwrap();

        let found: Vec<(String, u64, bool)> = spotlight_exposed_builds(prefix)
            .into_iter()
            .map(|b| {
                assert_eq!(b.dir, build_dir(prefix, &b.program, b.build));
                (b.program, b.build, b.active)
            })
            .collect();
        assert_eq!(
            found,
            [
                ("trust".to_owned(), 9192, false),
                ("trust".to_owned(), 15698, true),
                ("trust-mc".to_owned(), 7, false),
            ],
            "a link is moved, a build with no sysroot is not Rust's, and a name the store \
             never writes is not a build"
        );
        assert!(is_linked_aside(&trust.join("20001")));
        assert!(!is_linked_aside(&trust.join("9192")) && !is_linked_aside(&trust.join("current")));
        assert_eq!(
            aside_of(&trust.join("9192")),
            Some(trust.join("9192.noindex"))
        );

        std::fs::remove_file(trust.join("current")).unwrap();
        std::os::unix::fs::symlink("20001", trust.join("current")).unwrap();
        assert!(
            spotlight_exposed_builds(prefix).iter().all(|b| !b.active),
            "current names the moved build"
        );
    }

    #[test]
    fn lease_record_le_round_trips() {
        assert!(!is_valid_build_id(0));
        assert!(is_valid_build_id(9192));
        let enc = encode_lease_record_le(9192, 4242, 1_700_000_000);
        assert_eq!(decode_lease_record_le(enc), (9192, 4242, 1_700_000_000));
    }
}
