// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn scoped_metadata_and_known_children_refuse_replaced_ancestors() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let base = scratch.path().canonicalize().unwrap();
    let selected = base.join("selected");
    let foreign = base.join("foreign");
    std::fs::create_dir_all(selected.join("objects/aa")).unwrap();
    std::fs::create_dir_all(foreign.join("objects/bb")).unwrap();
    std::fs::write(selected.join("objects/file"), b"ordinary control").unwrap();
    std::os::unix::fs::symlink(foreign.join("objects/bb"), selected.join("objects/cc")).unwrap();
    let before = observe(&selected).unwrap();
    let names = vec!["aa".into(), "bb".into(), "cc".into(), "file".into()];
    assert_eq!(
        directories(&selected.join("objects"), before.stat.uid, &names).unwrap(),
        vec!["aa"]
    );
    assert!(
        directories(
            &selected.join("objects"),
            before.stat.uid,
            &["../foreign".into()]
        )
        .is_err()
    );
    assert!(directories(&selected.join("objects"), before.stat.uid, &["aa/".into()]).is_err());
    assert!(
        directories(
            &selected.join("objects"),
            before.stat.uid.wrapping_add(1),
            &names
        )
        .is_err()
    );
    std::fs::rename(&selected, base.join("held")).unwrap();
    std::os::unix::fs::symlink(&foreign, &selected).unwrap();
    assert!(observe(&selected.join("objects")).is_err());
    assert!(directories(&selected.join("objects"), before.stat.uid, &names).is_err());
}

/// A scope its owner may search but not list (`--x`) is observed and its known children
/// found, as on macOS: the walk holds it as a path (`O_PATH`), and both only `fstat` it or
/// look a name up under it; a link in its place still answers `ELOOP`. The control: that
/// folder cannot be opened to read, as the walk opened it before.
#[cfg(target_os = "linux")]
#[test]
fn a_scope_that_may_only_be_searched_is_observed() {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    aterm_uds::host::could_not_run_as_root("a search-only scope");
    let scratch = aterm_tempfile::tempdir().unwrap();
    let base = scratch.path().canonicalize().unwrap();
    let scope = base.join("objects");
    std::fs::create_dir_all(scope.join("aa")).unwrap();
    std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o100)).unwrap();
    let observed = observe(&scope).map(|seen| (seen.stat.dir, seen.stat.mode));
    let uid = std::fs::metadata(&scope).unwrap().uid();
    let found = directories(&scope, uid, &["aa".into(), "bb".into()]);
    let listed = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(&scope);
    std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        listed.map(drop).map_err(|error| error.kind()),
        Err(io::ErrorKind::PermissionDenied),
        "control: the scope cannot be opened to read"
    );
    assert_eq!(observed.unwrap(), (true, 0o100));
    assert_eq!(found.unwrap(), ["aa"]);
    let linked = base.join("linked");
    std::os::unix::fs::symlink(&scope, &linked).unwrap();
    for error in [
        observe(&linked).unwrap_err(),
        directories(&linked, uid, &[]).unwrap_err(),
    ] {
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{error}");
    }
}
