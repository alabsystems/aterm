// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn mac_alias_spellings_keep_component_boundaries_and_raw_nonprotected_names() {
    let home = Path::new("/Users//CaseUser");
    for path in [
        "/Users//CaseUser/documents/dots",
        "/users/CASEUSER/Documentſ/dots",
        "/USERS/caseuser/lIbRaRy/cLoUdStOrAgE/dots",
        "/Users//CaseUser/LIBRARY/GROUP CONTAINERS/dots",
        "/volumes/External/dots",
        "/system/volumes/DATA/users/caseuser/DOCUMENTS/dots",
        "/SYSTEM/Volumes/data/VOLUMES/External",
    ] {
        assert!(under_protected_root(home, Path::new(path)), "{path}");
    }
    assert!(under_protected_root(
        Path::new("/Users/Åhome"),
        Path::new("/users/a\u{30a}HOME/Documents/dots")
    ));
    assert!(under_protected_root(
        Path::new("/Users/ßhome"),
        Path::new("/users/SSHOME/Documents/dots")
    ));
    assert!(under_file_provider_domain(
        home,
        Path::new("/users/caseuser/LIBRARY/CLOUDSTORAGE/dots")
    ));
    assert!(under_app_data(
        home,
        Path::new("/users/caseuser/library/containers/app")
    ));
    for path in [
        "/Users//CaseUser/documents-backup/dots",
        "/Users//CaseUser/Library/CloudStorageElsewhere/dots",
        "/Users//CaseUser/Library/Application Support/aterm",
        "/Users//CaseUser/dot-雪",
        "/Users//CaseUser/Döcuments/dots",
        "/Users//CaseUserOther/Documents/dots",
        "/VolumesElsewhere/dots",
    ] {
        assert!(!under_protected_root(home, Path::new(path)), "{path}");
    }
    use std::os::unix::ffi::OsStringExt as _;
    let raw = home.join(std::ffi::OsString::from_vec(b"dot-\xff".to_vec()));
    assert!(!under_protected_root(home, &raw));
}
