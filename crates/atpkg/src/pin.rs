// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Consumer-side LOCAL pin state (§11) — the set of programs the user has frozen against
//! `update`/`sync`.
//!
//! A pin is **upgrade-suppression ONLY**. It is a local file, never signed trust input; it
//! can only cause an apply to SKIP an upgrade — never grant, move, or downgrade a build —
//! and it is always consulted strictly AFTER [`crate::gate::decide`], so a Tombstone / floor
//! always wins (a pin can never keep a revoked or below-floor build running). The state
//! lives in a `0600` newline-delimited file directly under the hardened prefix and is
//! mutated atomically (temp + rename), mirroring [`crate::store::mark_build_ready`].

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::platform::ensure_private_dir;
use crate::store::Layout;

/// `pins` — the `0600` newline-delimited pin-state file directly under the vetted prefix.
fn pins_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("pins")
}

/// The set of pinned program names. An absent or unreadable file reads as EMPTY (a pin is
/// suppression state, not trust input — its absence must never be an error).
#[must_use]
pub fn pinned_set(layout: &Layout) -> BTreeSet<String> {
    fs::read_to_string(pins_path(layout))
        .map(|body| {
            body.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `program` is locally pinned (held against update/sync).
#[must_use]
pub fn is_pinned(layout: &Layout, program: &str) -> bool {
    pinned_set(layout).contains(program)
}

/// Set (or clear) the local pin for `program`, returning `Ok(changed)`. Rejects an unsafe
/// program name (the same shape rule [`crate::ops::uninstall`] / [`crate::store::shim_allowed`]
/// enforce, PLUS a newline guard so a crafted name cannot inject a phantom pin line) with
/// [`io::ErrorKind::InvalidInput`] and writes nothing. Hardens the prefix (`0700`), writes a
/// `0600` temp file, and atomically renames it into place so a crash leaves the old file or
/// none — never a torn / loose-perm one.
pub fn set_pinned(layout: &Layout, program: &str, pinned: bool) -> io::Result<bool> {
    if program.is_empty()
        || program == "."
        || program == ".."
        || program.contains('/')
        || program.contains('\\')
        || program.contains('\n')
        || program.contains('\0')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program name must be a single safe path component (no separators, `.`, `..`, \
             newline, or NUL)",
        ));
    }
    let mut set = pinned_set(layout);
    let changed = if pinned {
        set.insert(program.to_string())
    } else {
        set.remove(program)
    };
    // Harden the (vetted) prefix and write atomically.
    ensure_private_dir(&layout.prefix)?;
    let dest = pins_path(layout);
    let tmp = layout
        .prefix
        .join(format!(".pins.tmp-{}", std::process::id()));
    let body: String = set.iter().map(|p| format!("{p}\n")).collect();
    fs::write(&tmp, body.as_bytes())?;
    crate::platform::harden_file(&tmp)?;
    fs::rename(&tmp, &dest)?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs::Permissions;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-pin-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&p, Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    #[test]
    fn set_is_pinned_roundtrip_and_0600() {
        let l = layout("roundtrip");
        assert!(!is_pinned(&l, "ay"));
        assert!(set_pinned(&l, "ay", true).unwrap(), "first pin is a change");
        assert!(is_pinned(&l, "ay"));
        // 0600 — Unix-only mode check.
        #[cfg(unix)]
        {
            let mode = fs::metadata(pins_path(&l)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "pin file is 0600");
        }
        assert!(
            !set_pinned(&l, "ay", true).unwrap(),
            "re-pinning is a no-op change"
        );
        assert!(set_pinned(&l, "ay", false).unwrap(), "unpin is a change");
        assert!(!is_pinned(&l, "ay"));
        let _ = fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn rejects_unsafe_program_names() {
        let l = layout("unsafe");
        for bad in ["", ".", "..", "a/b", "a\\b", "x\ny", "x\0y"] {
            let err = set_pinned(&l, bad, true).unwrap_err();
            assert_eq!(
                err.kind(),
                io::ErrorKind::InvalidInput,
                "{bad:?} must be rejected"
            );
        }
        assert!(!pins_path(&l).exists(), "a rejected name writes no file");
        let _ = fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn pinned_set_reads_all() {
        let l = layout("reads-all");
        set_pinned(&l, "ay", true).unwrap();
        set_pinned(&l, "trust", true).unwrap();
        assert_eq!(
            pinned_set(&l),
            BTreeSet::from(["ay".to_string(), "trust".to_string()])
        );
        let _ = fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn absent_file_is_empty_not_error() {
        let l = layout("absent");
        assert!(!is_pinned(&l, "ay"));
        assert!(pinned_set(&l).is_empty());
        let _ = fs::remove_dir_all(&l.prefix);
    }
}
