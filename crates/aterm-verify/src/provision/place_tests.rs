// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_download_is_named_by_its_urls_last_segment_or_archive() {
    assert_eq!(
        file_name("https://x/releases/download/4.9.3/fish-4.9.3.tar.xz"),
        "fish-4.9.3.tar.xz"
    );
    for odd in [
        "https://x/",
        "https://x/..",
        "https://x/a b",
        "https://x/.hidden",
    ] {
        assert_eq!(file_name(odd), "archive", "{odd}");
    }
    assert_eq!(container_of("a.tar.gz"), Ok(Container::TarGz));
    assert_eq!(container_of("a.zip"), Ok(Container::Zip));
    assert!(
        container_of("a.tar.xz").is_err(),
        "xz goes through plain_tar"
    );
}

#[test]
fn a_settled_tree_has_one_time_for_every_file() {
    let tree = aterm_tempfile::tempdir().unwrap();
    let deep = tree.path().join("lib/sh");
    std::fs::create_dir_all(&deep).unwrap();
    let files = [
        tree.path().join("configure"),
        tree.path().join("aclocal.m4"),
        deep.join("anonfile.c"),
    ];
    let epoch = std::time::SystemTime::UNIX_EPOCH;
    for (n, file) in files.iter().enumerate() {
        std::fs::write(file, "x").unwrap();
        let at = epoch + std::time::Duration::from_secs(1_000_000 * (n as u64 + 1));
        std::fs::File::open(file).unwrap().set_modified(at).unwrap();
    }
    let time = |file: &Path| std::fs::metadata(file).unwrap().modified().unwrap();
    assert!(
        time(&files[0]) < time(&files[1]),
        "the control: configure is older"
    );
    settle_times(tree.path()).unwrap();
    assert!(files.iter().all(|file| time(file) == time(&files[0])));
}
