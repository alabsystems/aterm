// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local pin state for programs frozen against `update` and `sync`.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::store::Layout;

const MAX_PIN_FILE_BYTES: usize = 256 * 1024;
const MAX_PINNED_PROGRAMS: usize = 2048;

fn pins_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("pins")
}

/// Returns the set of locally pinned program names.
#[must_use]
pub fn pinned_set(layout: &Layout) -> BTreeSet<String> {
    let Ok(body) =
        crate::metadata_io::read_bounded_regular_utf8(&pins_path(layout), MAX_PIN_FILE_BYTES)
    else {
        return BTreeSet::new();
    };
    let mut out = BTreeSet::new();
    for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if out.insert(line.to_string()) && out.len() > MAX_PINNED_PROGRAMS {
            return BTreeSet::new();
        }
    }
    out
}

/// Whether `program` is locally pinned (held against update/sync).
#[must_use]
pub fn is_pinned(layout: &Layout, program: &str) -> bool {
    pinned_set(layout).contains(program)
}

/// Sets or clears the local pin for `program`, returning `Ok(changed)`.
pub fn set_pinned(layout: &Layout, program: &str, pinned: bool) -> io::Result<bool> {
    static PIN_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    if program.is_empty()
        || program == "."
        || program == ".."
        || program.trim() != program
        || program.contains('/')
        || program.contains('\\')
        || program.contains('\n')
        || program.contains('\r')
        || program.contains('\0')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program name must be a single safe path component (no separators, `.`, `..`, \
             outer whitespace, CR/LF, or NUL)",
        ));
    }
    let mut set = pinned_set(layout);
    let changed = if pinned {
        set.insert(program.to_string())
    } else {
        set.remove(program)
    };
    if set.len() > MAX_PINNED_PROGRAMS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot pin more than {MAX_PINNED_PROGRAMS} programs"),
        ));
    }
    let body: String = set.iter().map(|p| format!("{p}\n")).collect();
    if body.len() > MAX_PIN_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("pin file exceeds {MAX_PIN_FILE_BYTES}-byte limit"),
        ));
    }
    layout.ensure_dir(&layout.prefix)?;
    let dest = pins_path(layout);
    let seq = PIN_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = layout
        .prefix
        .join(format!(".pins.tmp-{}-{seq}", std::process::id()));
    fs::write(&tmp, body.as_bytes())?;
    if let Err(err) = crate::platform::harden_file(&tmp).and_then(|()| fs::rename(&tmp, &dest)) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-pin-{label}-"))
    }

    #[test]
    fn set_is_pinned_roundtrip_and_0600() {
        let l = layout("roundtrip");
        assert!(!is_pinned(&l, "ay"));
        assert!(set_pinned(&l, "ay", true).unwrap(), "first pin is a change");
        assert!(is_pinned(&l, "ay"));
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
        for bad in [
            "", ".", "..", "a/b", "a\\b", "x\ny", "x\ry", "x\0y", " ay", "ay ", "\tay", "   ",
        ] {
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
    fn oversized_sparse_and_overcount_pin_files_fail_closed() {
        let l = layout("bounded");
        let path = pins_path(&l);
        let file = fs::File::create(&path).unwrap();
        file.set_len((MAX_PIN_FILE_BYTES + 1) as u64).unwrap();
        assert!(pinned_set(&l).is_empty());

        let exact_with_dup: String = (0..MAX_PINNED_PROGRAMS)
            .map(|index| format!("p{index}\n"))
            .chain(std::iter::once("p0\n".to_string()))
            .collect();
        fs::write(&path, exact_with_dup).unwrap();
        assert_eq!(
            pinned_set(&l).len(),
            MAX_PINNED_PROGRAMS,
            "duplicate lines do not push a 2048-member set over the cap"
        );
        let err = set_pinned(&l, "p2048", true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            pinned_set(&l).len(),
            MAX_PINNED_PROGRAMS,
            "refusing the 2049th pin preserves all 2048 existing pins"
        );

        let body: String = (0..=MAX_PINNED_PROGRAMS)
            .map(|index| format!("p{index}\n"))
            .collect();
        fs::write(&path, body).unwrap();
        assert!(pinned_set(&l).is_empty(), "over-count is never partial");
        let _ = fs::remove_dir_all(&l.prefix);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_and_symlink_pin_files_return_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let l = layout("special");
        let path = pins_path(&l);
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        assert!(pinned_set(&l).is_empty());
        fs::remove_file(&path).unwrap();
        let target = l.prefix.join("target-pins");
        fs::write(&target, "ay\n").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(pinned_set(&l).is_empty(), "pin links are not followed");
        let _ = fs::remove_dir_all(&l.prefix);
    }
}
