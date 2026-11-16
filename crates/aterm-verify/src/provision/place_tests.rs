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

/// The Linux lane's two rows (`aterm_vz::pins`) placed and stamped in a scratch tooling cache
/// as `verify provision --linux` leaves them, their files sparse.
#[cfg(unix)]
fn place_linux_rows(tooling: &Path) -> (PathBuf, PathBuf) {
    use aterm_uds::host::PIN_STAMP;
    use aterm_vz::pins::{INITRD, PLATFORM, RELEASE, ROOT_ARCHIVE, ROOT_MEMBER, VMLINUZ};
    let stamp = |row: &str, sha256: &str| {
        format!(
            "{{\"row\":\"{row}\",\"version\":\"{RELEASE}\",\"sha256\":\"{sha256}\",\
             \"anchor\":\"sha256\",\"platform\":\"{PLATFORM}\",\"placed\":1790000000}}\n"
        )
    };
    let root = aterm_vz::pins::root_tool().pinned_root(tooling).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::File::create(root.join(ROOT_MEMBER))
        .unwrap()
        .set_len(aterm_vz::pins::ROOT_MEMBER_BYTES)
        .unwrap();
    std::fs::write(
        root.join(PIN_STAMP),
        stamp("ubuntu-root", ROOT_ARCHIVE.sha256),
    )
    .unwrap();
    let kernel = aterm_vz::pins::kernel_tool().pinned_root(tooling).unwrap();
    std::fs::create_dir_all(&kernel).unwrap();
    for file in [VMLINUZ, INITRD] {
        std::fs::File::create(kernel.join(file.name()))
            .unwrap()
            .set_len(file.bytes)
            .unwrap();
    }
    std::fs::write(
        kernel.join(PIN_STAMP),
        stamp("ubuntu-kernel", &aterm_vz::pins::kernel_stamp_sha256()),
    )
    .unwrap();
    (root, kernel)
}

/// Every file and folder of `dir` and `dir` itself last touched at `at`.
#[cfg(unix)]
fn age(dir: &Path, at: std::time::SystemTime) {
    let times = std::fs::FileTimes::new().set_accessed(at).set_modified(at);
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        std::fs::File::open(entry.path())
            .unwrap()
            .set_times(times)
            .unwrap();
    }
    std::fs::File::open(dir).unwrap().set_times(times).unwrap();
}

/// atpkg's tooling reclaim keeps a Linux-lane row that an aterm-vz bring-up read, though
/// reading it changes no file's time. The negative control: the row not read yet is still
/// listed. It lives here, where both crates are dependencies, so aterm-vz's own tests do not
/// rerun on every atpkg change.
#[cfg(unix)]
#[test]
fn a_bring_up_marks_its_rows_used_so_the_tooling_reclaim_keeps_them() {
    let home = aterm_tempfile::tempdir().unwrap();
    let tooling = home.path().join("tooling.noindex");
    let (root, kernel) = place_linux_rows(&tooling);
    let now = std::time::SystemTime::now();
    let month_ago = now - atpkg::build_cache::RECLAIM_IDLE - std::time::Duration::from_secs(86_400);
    age(&root, month_ago);
    age(&kernel, month_ago);
    let listed = || -> Vec<PathBuf> {
        atpkg::build_cache::tooling::plan(&tooling, now)
            .into_iter()
            .map(|p| p.path)
            .collect()
    };
    assert_eq!(
        listed(),
        [kernel.clone(), root.clone()],
        "both idle a month"
    );

    let lookup = aterm_uds::host::Lookup {
        named: None,
        tooling: Some(tooling.clone()),
        path: None,
    };
    aterm_vz::pins::base(&lookup).unwrap();
    assert_eq!(
        listed(),
        std::slice::from_ref(&kernel),
        "the base was read; the kernel row was not, and is still listed"
    );
    aterm_vz::pins::kernel_files(&lookup).unwrap();
    assert_eq!(listed(), Vec::<PathBuf>::new(), "both were read");
}
