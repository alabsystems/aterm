// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
fn state_at(level: Option<&str>) -> aterm_tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let state = aterm_tempfile::tempdir().expect("a state folder");
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    if let Some(level) = level {
        let file = state.path().join(aterm_types::fleet_access::FILE);
        std::fs::write(&file, format!("level={level}\nat=1\nby=cli\n")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    state
}

/// An `aterm` image at `dir`: the one image that serves the source's name.
#[cfg(unix)]
fn aterm_in(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let exe = dir.join("aterm");
    std::fs::write(&exe, b"").unwrap();
    std::fs::canonicalize(exe).unwrap()
}

/// Only a Mac at Full Access names a source, its link runs the `aterm` that made it (by its real
/// path, through any link it was started by), each `aterm` keeps a link of its own, and a front
/// another image runs, or a call that names its own source, names none.
#[cfg(unix)]
#[test]
fn only_full_access_names_a_unit_source() {
    let images = aterm_tempfile::tempdir().expect("images");
    let exe = aterm_in(&images.path().join("one"));
    for level in [None, Some("read"), Some("write"), Some("nonsense")] {
        let state = state_at(level);
        assert_eq!(environment(state.path(), &exe), None, "{level:?}");
        assert!(!state.path().join(LINK_DIR).exists(), "{level:?}");
    }
    let state = state_at(Some("full"));
    let (name, link) = environment(state.path(), &exe).expect("a source at Full Access");
    assert_eq!(name, UNIT_SOURCE_ENV);
    let link = PathBuf::from(link);
    assert_eq!(link.file_name().unwrap(), LINK);
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
    // Started through a link (as the app's `atpkg` is), it names the image the link runs.
    let alias = images.path().join("atpkg");
    std::os::unix::fs::symlink(&exe, &alias).unwrap();
    assert_eq!(
        environment(state.path(), &alias).map(|(_, link)| PathBuf::from(link)),
        Some(link.clone())
    );
    // Another `aterm` keeps its own link, and the first one's stays.
    let other = aterm_in(&images.path().join("two"));
    let (_, again) = environment(state.path(), &other).unwrap();
    assert_ne!(PathBuf::from(&again), link);
    assert_eq!(std::fs::read_link(&again).unwrap(), other);
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
    // Another image serves no source's name.
    let atpkg = images.path().join("dev/atpkg");
    std::fs::create_dir_all(atpkg.parent().unwrap()).unwrap();
    std::fs::write(&atpkg, b"").unwrap();
    assert_eq!(environment(state.path(), &atpkg), None);
    // The front: a call that names its own keeps it; no state or no executable names none.
    assert_eq!(
        for_front(Some("/own".as_ref()), Some(state.path()), Some(&exe)),
        None
    );
    assert_eq!(for_front(None, None, Some(&exe)), None);
    assert_eq!(for_front(None, Some(state.path()), None), None);
    assert_eq!(
        for_front(None, Some(state.path()), Some(&exe)).map(|(_, link)| PathBuf::from(link)),
        Some(link)
    );
}

/// A unit laid out as targo's store lays it: `.lock`, `fingerprint/` and `out/`.
fn unit(root: &Path, dir: &str, record: &[u8]) -> PathBuf {
    let unit = root.join(dir);
    std::fs::create_dir_all(unit.join("fingerprint")).unwrap();
    std::fs::create_dir_all(unit.join("out")).unwrap();
    std::fs::write(unit.join(".lock"), b"").unwrap();
    std::fs::write(unit.join("fingerprint/lib-a.json"), record).unwrap();
    std::fs::write(unit.join("fingerprint/outputs-lib-a.json"), b"{}").unwrap();
    std::fs::write(unit.join("fingerprint/lib-a"), b"0123").unwrap();
    std::fs::write(unit.join("out/liba.rlib"), b"!<arch>\n").unwrap();
    unit
}

/// A member finds a unit by its record's key alone, in a plain or a Git source's folder (and a
/// registry crate named `git`), and answers none for a key no record has and for a unit a build
/// is writing. A miss is answered from the index until it is [`INDEX_FRESH`] old, and a record
/// written since is read again on the walk after. (One test: the index is the process's.)
#[test]
fn a_unit_is_found_by_its_records_key_under_its_shared_lock() {
    let store = aterm_tempfile::tempdir().expect("a store");
    let root = store.path().to_path_buf();
    unit(&root, "build/a/0011", b"{\"a\":1}");
    let git = unit(&root, "build/git/c0ffee/b/0022", b"{\"b\":2}");
    unit(&root, "build/git/0033", b"{\"git\":1}");
    let roots = [root.clone()];
    let found = held(&roots, &sha256_hex(b"{\"a\":1}")).expect("a's unit");
    assert_eq!(found.dir, "build/a/0011");
    let names: Vec<&str> = found.files.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(
        names,
        [
            "fingerprint/lib-a",
            "fingerprint/lib-a.json",
            "fingerprint/outputs-lib-a.json",
            "out/liba.rlib"
        ]
    );
    // Shared with a reader: a second answer of the same key is not refused.
    assert!(held(&roots, &sha256_hex(b"{\"a\":1}")).is_some());
    drop(found);
    assert_eq!(
        held(&roots, &sha256_hex(b"{\"b\":2}")).map(|held| held.dir),
        Some("build/git/c0ffee/b/0022".to_owned())
    );
    assert_eq!(
        held(&roots, &sha256_hex(b"{\"git\":1}")).map(|held| held.dir),
        Some("build/git/0033".to_owned()),
        "a registry crate named git"
    );
    assert!(
        held(&roots, &sha256_hex(b"{}")).is_none(),
        "a receipt is no record"
    );
    assert!(held(&roots, &sha256_hex(b"nothing")).is_none());
    let writer =
        crate::lock::Flock::try_lock(crate::metadata_io::open_regular(&git.join(".lock")).unwrap())
            .unwrap();
    assert!(held(&roots, &sha256_hex(b"{\"b\":2}")).is_none());
    drop(writer);
    // A unit with no lock is not served; the others still are.
    let unlocked = unit(&root, "build/d/0044", b"{\"d\":4}");
    std::fs::remove_file(unlocked.join(".lock")).unwrap();
    assert!(held_within(&roots, &sha256_hex(b"{\"d\":4}"), Duration::ZERO).is_none());
    assert!(held(&roots, &sha256_hex(b"{\"a\":1}")).is_some());
    // A record written since the walk: a miss inside the index's freshness is not walked for,
    // and the walk after reads it again.
    std::fs::write(git.join("fingerprint/lib-a.json"), b"{\"b\":3,\"more\":1}").unwrap();
    let rewritten = sha256_hex(b"{\"b\":3,\"more\":1}");
    assert!(held_within(&roots, &rewritten, Duration::from_secs(3600)).is_none());
    assert!(held_within(&roots, &rewritten, Duration::ZERO).is_some());
}
