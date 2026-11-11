// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    #[cfg(unix)]
    {
        let target = cache.join("real-tag");
        std::fs::rename(cache.join("CACHEDIR.TAG"), &target).unwrap();
        std::os::unix::fs::symlink(&target, cache.join("CACHEDIR.TAG")).unwrap();
        assert!(
            !transient(&inside.join("aterm"), &elsewhere),
            "a symlinked CACHEDIR.TAG is rejected by O_NOFOLLOW"
        );
    }
    std::fs::remove_dir_all(&cache).unwrap();

    let dup_roots = temp_roots_from(PathBuf::from("/tmp"));
    for (i, root) in dup_roots.iter().enumerate() {
        assert!(
            !dup_roots[i + 1..].contains(root),
            "duplicate temporary root {root:?} in {dup_roots:?}"
        );
    }
}
