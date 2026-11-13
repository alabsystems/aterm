// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
#[test]
fn a_same_size_rewrite_with_restored_mtime_tracks_the_change_timestamp() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = crate::testdir::dir("atpkg-probe-cache-rewrite-");
    let bin = dir.join("compiler");
    std::fs::write(&bin, b"old!").unwrap();
    let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    let restore = || {
        std::fs::File::options()
            .write(true)
            .open(&bin)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    };
    restore();
    let before = std::fs::metadata(&bin).unwrap();
    let old = identity(&bin).unwrap();
    let cache = dir.join("doctor/readings");
    put(&cache, &old, "old answer");
    std::fs::write(&bin, b"new!").unwrap();
    restore();
    let after = std::fs::metadata(&bin).unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.mode(),
            before.uid(),
            before.gid(),
            before.len(),
            before.modified().unwrap()
        ),
        (
            after.dev(),
            after.ino(),
            after.mode(),
            after.uid(),
            after.gid(),
            after.len(),
            after.modified().unwrap()
        )
    );
    let changed = (before.ctime(), before.ctime_nsec()) != (after.ctime(), after.ctime_nsec());
    let current = identity(&bin).unwrap();
    if changed {
        assert_ne!(current, old);
        assert_eq!(get(&cache, &current), None, "served the pre-rewrite answer");
    } else {
        // A rapid rewrite can share a filesystem timestamp tick. Metadata
        // identity intentionally cannot distinguish identical observations.
        assert_eq!(current, old);
        assert_eq!(get(&cache, &current).as_deref(), Some("old answer"));
    }
    put(&cache, &current, "new answer");
    assert_eq!(get(&cache, &current).as_deref(), Some("new answer"));
    assert_eq!(identity(&bin).unwrap(), current);
}

#[test]
fn malformed_cache_records_are_misses_and_a_new_answer_repairs_them() {
    let dir = crate::testdir::dir("atpkg-probe-cache-records-");
    let file = dir.join("doctor/readings");
    put(&file, "k", "old");
    let good = format!("k{RECORD}old\n");
    let too_many = (0..=ENTRIES)
        .map(|n| format!("k{n}{RECORD}v\n"))
        .collect::<String>();
    for bad in [
        format!("k{RECORD}partial"),
        format!("{good}k{RECORD}different\n"),
        format!("{good}{good}"),
        format!("{good}{RECORD}empty-key\n"),
        format!("{good}other{RECORD}two{RECORD}values\n"),
        format!("k{RECORD}old\r\n"),
        format!("{good}unfinished"),
        format!("{good}{too_many}"),
    ] {
        std::fs::write(&file, &bad).unwrap();
        assert_eq!(get(&file, "k"), None, "accepted malformed cache {bad:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), bad);
        put(&file, "fresh", "answer");
        assert_eq!(get(&file, "fresh").as_deref(), Some("answer"));
        assert_eq!(get(&file, "k"), None, "retained a malformed old file");
    }
    put(&file, "empty", "");
    assert_eq!(get(&file, "empty"), Some(String::new()));
    assert_eq!(get(&file, "fresh").as_deref(), Some("answer"));
}

#[test]
fn cache_writes_fit_the_read_limit_without_partial_records() {
    let dir = crate::testdir::dir("atpkg-probe-cache-bytes-");
    let file = dir.join("doctor/readings");
    let large = "é".repeat(MAX_PROBE_CACHE_BYTES / 4);
    put(&file, "oldest", &large);
    put(&file, "newest", &large);
    assert!(
        file.metadata().unwrap().len() <= MAX_PROBE_CACHE_BYTES as u64,
        "the writer exceeded the reader's byte limit"
    );
    assert!(get(&file, "newest").is_some_and(|value| value == large));
    assert_eq!(get(&file, "oldest"), None);
    let before = std::fs::read(&file).unwrap();
    let oversized = "é".repeat(MAX_PROBE_CACHE_BYTES / 2);
    for (key, value) in [("too-big", oversized.as_str()), (oversized.as_str(), "x")] {
        put(&file, key, value);
        assert!(
            std::fs::read(&file).unwrap() == before,
            "an oversized record replaced readable cache entries"
        );
    }
    let exact_key = "exact";
    let exact = "y".repeat(MAX_PROBE_CACHE_BYTES - exact_key.len() - RECORD.len_utf8() - 1);
    put(&file, exact_key, &exact);
    assert_eq!(file.metadata().unwrap().len(), MAX_PROBE_CACHE_BYTES as u64);
    assert!(get(&file, exact_key).is_some_and(|value| value == exact));
    assert_eq!(get(&file, "newest"), None);
    put(&file, "tail", "small");
    assert_eq!(get(&file, "tail").as_deref(), Some("small"));
    assert_eq!(get(&file, exact_key), None);
}

#[test]
fn empty_key_fields_keep_their_positions_and_cannot_reuse_old_records() {
    let shapes: &[&[&str]] = &[
        &["x"],
        &["", "x"],
        &["x", ""],
        &["", "x", ""],
        &[],
        &[""],
        &["", ""],
        &["x", "y"],
        &["x", "", "y"],
    ];
    let mut keys = Vec::new();
    for parts in shapes {
        let encoded = key(parts.iter().copied().map(Some)).unwrap();
        assert!(!keys.contains(&encoded), "key fields collapsed: {parts:?}");
        keys.push(encoded);
    }
    let dir = crate::testdir::dir("atpkg-probe-key-fields-");
    let file = dir.join("doctor/readings");
    put(&file, "x", "old ambiguous encoding");
    assert_eq!(get(&file, &keys[0]), None);
    for (index, encoded) in keys.iter().enumerate() {
        put(&file, encoded, &index.to_string());
    }
    for (index, encoded) in keys.iter().enumerate() {
        assert_eq!(get(&file, encoded), Some(index.to_string()));
    }
}

#[test]
fn a_reading_is_found_only_under_its_whole_key_and_the_newest_stay() {
    let dir = crate::testdir::dir("atpkg-probe-cache-");
    let file = dir.join("doctor").join("readings");
    assert_eq!(get(&file, "a"), None, "nothing kept yet");
    put(&file, "a", "1");
    put(&file, "b", "2");
    put(&file, "a", "3");
    assert_eq!(get(&file, "a").as_deref(), Some("3"));
    assert_eq!(get(&file, "b").as_deref(), Some("2"));
    assert_eq!(get(&file, "ab"), None);
    for n in 0..ENTRIES {
        put(&file, &format!("k{n}"), "v");
    }
    assert_eq!(get(&file, "a"), None, "the oldest key went first");
    assert_eq!(
        get(&file, &format!("k{}", ENTRIES - 1)).as_deref(),
        Some("v")
    );
    put(&file, "c", "two\nlines");
    assert_eq!(
        get(&file, "c"),
        None,
        "a value with a line break is not kept"
    );
    assert_eq!(key([Some("x"), None]), None, "an unread part leaves no key");
    put(&file, "x\u{1f}y", "old unframed encoding");
    let joined = key([Some("x"), Some("y")]).expect("two read parts");
    assert_eq!(get(&file, &joined), None);
    put(&file, &joined, "joined");
    assert_eq!(
        get(&file, &joined).as_deref(),
        Some("joined"),
        "a joined key is kept"
    );
    assert_eq!(get(&file, "x"), None, "a key's first part is not the key");
    assert_eq!(super::file(""), None);
    assert_eq!(super::file("."), None);
    assert_eq!(super::file(".."), None);
    assert_eq!(super::file("a/b"), None);
    assert_eq!(super::file("a\\b"), None);
}

#[test]
#[cfg(unix)]
fn native_paths_remain_distinct_for_the_same_file_metadata() {
    use std::os::unix::ffi::OsStringExt as _;

    let dir = crate::testdir::dir("atpkg-probe-cache-native-id-");
    let file = dir.join("compiler");
    std::fs::write(&file, "compiler fixture").unwrap();
    let metadata = file.metadata().unwrap();
    let first = dir.join(std::ffi::OsString::from_vec(b"compiler-\xff".to_vec()));
    let second = dir.join(std::ffi::OsString::from_vec(b"compiler-\xfe".to_vec()));
    assert_eq!(first.display().to_string(), second.display().to_string());
    let first_id = metadata_identity(&first, &metadata).unwrap();
    assert_eq!(metadata_identity(&first, &metadata).unwrap(), first_id);
    assert_ne!(first_id, metadata_identity(&second, &metadata).unwrap());
    let alias = dir.join("alias");
    std::os::unix::fs::symlink(&file, &alias).unwrap();
    assert_eq!(identity(&alias).unwrap(), identity(&file).unwrap());
}

#[test]
fn a_rewritten_file_has_another_identity() {
    let dir = crate::testdir::dir("atpkg-probe-cache-id-");
    let file = dir.join("bin");
    std::fs::write(&file, "one").unwrap();
    let first = identity(&file).expect("a file has an identity");
    assert_eq!(identity(&file).as_deref(), Some(first.as_str()));
    std::fs::write(&file, "three").unwrap();
    assert_ne!(identity(&file).as_deref(), Some(first.as_str()));
    assert_eq!(identity(&dir.join("gone")), None);
}

#[cfg(unix)]
/// NEGATIVE CONTROL: `probe_cache` must refuse symlinked directories, symlinked cache files,
/// and group/world-writable cache files so a cached compiler or doctor verdict cannot be
/// poisoned or redirected.
#[test]
fn get_and_put_refuse_symlinked_dirs_symlinked_files_and_writable_modes() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = crate::testdir::dir("atpkg-probe-cache-admit-");
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let victim = outside.join("readings");
    std::fs::write(&victim, format!("k{RECORD}poison\n")).unwrap();

    let link_dir = root.join("doctor-link");
    std::os::unix::fs::symlink(&outside, &link_dir).unwrap();
    let via_link_dir = link_dir.join("readings");
    assert_eq!(get(&via_link_dir, "k"), None);
    put(&via_link_dir, "k", "safe");
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        format!("k{RECORD}poison\n")
    );

    let clean_dir = root.join("doctor");
    let file = clean_dir.join("readings");
    put(&file, "k", "clean");
    assert_eq!(get(&file, "k").as_deref(), Some("clean"));

    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(
        get(&file, "k"),
        None,
        "world-writable cache file must not be trusted"
    );
    put(&file, "k2", "fresh");
    assert_eq!(get(&file, "k"), None, "untrusted old entries are discarded");
    assert_eq!(get(&file, "k2").as_deref(), Some("fresh"));

    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(&victim, &file).unwrap();
    assert_eq!(get(&file, "k"), None);
    put(&file, "k", "overwrite");
    assert!(
        std::fs::symlink_metadata(&file)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        format!("k{RECORD}poison\n")
    );
}
