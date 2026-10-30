// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether a copy of a program lasts: the one rule atpkg's shims (`atpkg::stub`), the vault
//! subscriber's launchd job and `aterm init`'s Dock pin name a program by. A copy on a disk
//! image (`/Volumes/…`), an App Translocation copy, one in a temporary directory, or one in a
//! cache directory (a `CACHEDIR.TAG` above it) goes away on its own.

use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The first line of a cache directory's `CACHEDIR.TAG` (bford.info/cachedir).
pub const CACHEDIR_TAG_SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55";

/// Whether `path` goes away on its own, with `temp_roots` the machine's temporary directories
/// ([`temp_roots`]).
#[must_use]
pub fn transient(path: &Path, temp_roots: &[PathBuf]) -> bool {
    let text = path.to_string_lossy();
    text.contains("/AppTranslocation/")
        || text.starts_with("/Volumes/")
        || temp_roots.iter().any(|root| path.starts_with(root))
        || path.ancestors().skip(1).any(is_cache_dir)
}

/// Whether `path` is not [`transient`] among this machine's temporary directories.
#[must_use]
pub fn lasting(path: &Path) -> bool {
    !transient(path, &temp_roots())
}

/// This machine's temporary directories: the process's, `/tmp` and `/var/tmp`, each as named
/// and as resolved.
#[must_use]
pub fn temp_roots() -> Vec<PathBuf> {
    temp_roots_from(std::env::temp_dir())
}

/// [`temp_roots`] with `temp_dir` the process's temporary directory; never the filesystem's
/// root.
#[must_use]
pub fn temp_roots_from(temp_dir: PathBuf) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push = |candidate: PathBuf| {
        if candidate.is_absolute() && candidate.parent().is_some() && !roots.contains(&candidate) {
            roots.push(candidate);
        }
    };
    for root in [temp_dir, PathBuf::from("/tmp"), PathBuf::from("/var/tmp")] {
        if let Ok(real) = std::fs::canonicalize(&root) {
            push(real);
        }
        push(root);
    }
    roots
}

/// Whether `dir` holds a `CACHEDIR.TAG`: a regular file, read no further than its first 4 KiB,
/// that starts with [`CACHEDIR_TAG_SIGNATURE`].
fn is_cache_dir(dir: &Path) -> bool {
    let tag = dir.join("CACHEDIR.TAG");
    if !std::fs::symlink_metadata(&tag).is_ok_and(|meta| meta.file_type().is_file()) {
        return false;
    }
    let Ok(file) = std::fs::File::open(&tag) else {
        return false;
    };
    let mut head = Vec::with_capacity(CACHEDIR_TAG_SIGNATURE.len());
    file.take(4096).read_to_end(&mut head).is_ok()
        && head.starts_with(CACHEDIR_TAG_SIGNATURE.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_images_translocation_temporaries_and_caches_do_not_last() {
        let roots = temp_roots_from(PathBuf::from("/"));
        for gone in [
            "/Volumes/aterm/aterm.app/Contents/MacOS/aterm",
            "/private/var/folders/x/AppTranslocation/Y/d/aterm.app/Contents/MacOS/aterm",
            "/tmp/x/aterm.app/Contents/MacOS/aterm",
            "/var/tmp/x/aterm",
        ] {
            assert!(transient(Path::new(gone), &roots), "{gone}");
        }
        for lasts in [
            "/Applications/aterm.app/Contents/MacOS/aterm",
            "/Users//bob/Applications/aterm.app/Contents/MacOS/aterm",
        ] {
            assert!(!transient(Path::new(lasts), &roots), "{lasts}");
        }
        let cache = std::env::temp_dir().join(format!("lasting-cache-{}", std::process::id()));
        let inside = cache.join("bin");
        std::fs::create_dir_all(&inside).unwrap();
        // A cache directory outside every temporary root: only its tag makes it go away.
        let elsewhere: Vec<PathBuf> = Vec::new();
        assert!(!transient(&inside.join("aterm"), &elsewhere));
        std::fs::write(
            cache.join("CACHEDIR.TAG"),
            format!("{CACHEDIR_TAG_SIGNATURE}\n# a cache directory tag\n"),
        )
        .unwrap();
        assert!(transient(&inside.join("aterm"), &elsewhere));
        std::fs::remove_dir_all(&cache).unwrap();

        let dup_roots = temp_roots_from(PathBuf::from("/tmp"));
        for (i, root) in dup_roots.iter().enumerate() {
            assert!(
                !dup_roots[i + 1..].contains(root),
                "duplicate temporary root {root:?} in {dup_roots:?}"
            );
        }
    }
}
