// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_pins_are_one_release_of_ubuntus_files() {
    assert_eq!(RELEASE, "20260926");
    assert_eq!(aterm_uds::host::pinned_version("ubuntu-kernel"), RELEASE);
    for file in [ROOT_ARCHIVE, VMLINUZ, INITRD] {
        assert!(crate::is_hex(file.sha256, 64), "{file:?}");
        assert!(
            file.url_at(RELEASE).starts_with(&format!(
                "https://{PRIMARY_HOST}/releases/noble/release-{RELEASE}/"
            )),
            "{file:?}"
        );
    }
    assert!(crate::is_hex(IMAGE_SHA256, 64));
    assert_eq!(root_tool().pin.map(|p| p.exe), Some(ROOT_MEMBER));
    assert_eq!(kernel_tool().pin.map(|p| p.exe), Some(VMLINUZ.name()));
    for tool in [root_tool(), kernel_tool()] {
        assert_eq!(tool.pin.map(|p| p.hosts), Some(&[PLATFORM][..]));
    }
}

/// The stamp `verify provision` writes for a row of this release, host and `sha256`.
fn stamp_text(row: &str, sha256: &str) -> String {
    format!(
        "{{\"row\":\"{row}\",\"version\":\"{RELEASE}\",\"sha256\":\"{sha256}\",\
         \"anchor\":\"sha256\",\"platform\":\"{PLATFORM}\",\"placed\":1790000000}}\n"
    )
}

/// Both rows placed and stamped in a scratch tooling cache, their files sparse.
fn place_both(tooling: &Path) -> (PathBuf, PathBuf) {
    let root = root_tool().pinned_root(tooling).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::File::create(root.join(ROOT_MEMBER))
        .unwrap()
        .set_len(ROOT_MEMBER_BYTES)
        .unwrap();
    std::fs::write(
        root.join(PIN_STAMP),
        stamp_text("ubuntu-root", ROOT_ARCHIVE.sha256),
    )
    .unwrap();
    let kernel = kernel_tool().pinned_root(tooling).unwrap();
    std::fs::create_dir_all(&kernel).unwrap();
    for file in [VMLINUZ, INITRD] {
        std::fs::File::create(kernel.join(file.name()))
            .unwrap()
            .set_len(file.bytes)
            .unwrap();
    }
    std::fs::write(
        kernel.join(PIN_STAMP),
        stamp_text("ubuntu-kernel", &kernel_stamp_sha256()),
    )
    .unwrap();
    (root, kernel)
}

fn scratch_lookup(tooling: &Path) -> Lookup {
    Lookup {
        named: None,
        tooling: Some(tooling.to_path_buf()),
        path: None,
    }
}

#[test]
fn a_placed_base_and_kernel_are_found_where_the_rows_put_them() {
    let home = aterm_tempfile::tempdir().unwrap();
    let tooling = home.path().join("tooling.noindex");
    let lookup = scratch_lookup(&tooling);
    match base(&lookup) {
        Err(Error::Input(why)) => assert!(
            why.contains("`targo --unverified verify provision --linux` fetches the pinned"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    let (root, kernel) = place_both(&tooling);
    let member = root.join(ROOT_MEMBER);
    assert_eq!(base(&lookup), Ok(member.clone()));
    assert_eq!(
        kernel_files(&lookup),
        Ok((kernel.join(VMLINUZ.name()), kernel.join(INITRD.name())))
    );

    std::fs::remove_file(root.join(PIN_STAMP)).unwrap();
    assert!(
        base(&lookup).is_err(),
        "a tree without the row's stamp is not placed"
    );
    std::fs::write(
        root.join(PIN_STAMP),
        stamp_text("ubuntu-root", &"e".repeat(64)),
    )
    .unwrap();
    assert!(
        matches!(base(&lookup), Err(Error::Input(why)) if why.contains("is not the pinned")),
        "a stamp of another archive is not this pin's"
    );
    std::fs::write(
        root.join(PIN_STAMP),
        stamp_text("ubuntu-root", ROOT_ARCHIVE.sha256),
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&member)
        .unwrap()
        .set_len(ROOT_MEMBER_BYTES - 1)
        .unwrap();
    assert!(
        matches!(base(&lookup), Err(Error::Input(why)) if why.contains("not the pin's")),
        "the negative control: a member of another size"
    );

    std::fs::remove_file(kernel.join(INITRD.name())).unwrap();
    assert!(matches!(kernel_files(&lookup), Err(Error::Input(_))));
}

#[test]
fn a_base_the_test_variable_names_is_refused_however_it_is_sized() {
    let home = aterm_tempfile::tempdir().unwrap();
    let tooling = home.path().join("tooling.noindex");
    let (root, _) = place_both(&tooling);
    let stand_in = home.path().join("not-ubuntu.img");
    std::fs::File::create(&stand_in)
        .unwrap()
        .set_len(ROOT_MEMBER_BYTES)
        .unwrap();
    let named = Lookup {
        named: Some(stand_in.clone().into_os_string()),
        ..scratch_lookup(&tooling)
    };
    match base(&named) {
        Err(Error::Input(why)) => assert!(
            why.starts_with("ATERM_TEST_UBUNTU_ROOT names ")
                && why.contains(
                    "takes only what `targo --unverified verify provision --linux` placed"
                ),
            "{why}"
        ),
        other => panic!("a stand-in of the member's size is no base: {other:?}"),
    }
    let on_path = Lookup {
        path: Some(home.path().as_os_str().to_owned()),
        ..scratch_lookup(&home.path().join("empty"))
    };
    std::fs::rename(&stand_in, home.path().join("ubuntu-root")).unwrap();
    assert!(
        matches!(base(&on_path), Err(Error::Input(why)) if why.starts_with("no Ubuntu")),
        "nor is a file on $PATH"
    );
    assert_eq!(
        base(&scratch_lookup(&tooling)),
        Ok(root.join(ROOT_MEMBER)),
        "the negative control: the placed row is the base"
    );
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

/// The tooling reclaim keeps a row a bring-up read, though reading it changes no file's
/// time. The negative control: the row not read yet is still listed.
#[cfg(unix)]
#[test]
fn a_bring_up_marks_its_rows_used_so_the_tooling_reclaim_keeps_them() {
    let home = aterm_tempfile::tempdir().unwrap();
    let tooling = home.path().join("tooling.noindex");
    let (root, kernel) = place_both(&tooling);
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

    let lookup = scratch_lookup(&tooling);
    base(&lookup).unwrap();
    assert_eq!(
        listed(),
        std::slice::from_ref(&kernel),
        "the base was read; the kernel row was not, and is still listed"
    );
    kernel_files(&lookup).unwrap();
    assert_eq!(listed(), Vec::<PathBuf>::new(), "both were read");
}

#[test]
fn an_archived_release_names_the_archive_host_and_the_bump() {
    let url = ROOT_ARCHIVE.url_at(RELEASE);
    let curl = "/usr/bin/curl exited Some(1): curl: (1) Protocol \"http\" disabled (in redirect)";
    let words = archived(&url, curl).expect("the archive's redirect");
    for needle in [
        "release-20260926",
        ARCHIVE_HOST,
        "plain http only",
        "Bump the image pin",
        "crates/aterm-vz/src/pins.rs",
        curl,
    ] {
        assert!(words.contains(needle), "{needle}: {words}");
    }
    assert_eq!(
        archived(
            &url,
            "/usr/bin/curl exited Some(6): curl: (6) Could not resolve host"
        ),
        None,
        "the negative control: another failure is curl's own"
    );
    assert_eq!(
        archived("https://github.com/x/y.tar.gz", curl),
        None,
        "another host's redirect is not Ubuntu's archive"
    );
}
