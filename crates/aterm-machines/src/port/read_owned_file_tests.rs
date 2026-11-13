// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Live, Stat, System};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

#[test]
fn observed_owned_reads_refuse_in_place_mutation_between_descriptor_stats() {
    use std::io::Read as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("profile");
    std::fs::write(&path, b"AAAA").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let uid = path.metadata().unwrap().uid();
    let good = observed(&path, uid, 16, false).unwrap();
    assert_eq!(good.bytes, b"AAAA");
    assert_eq!(good.metadata, metadata(&path, uid, 16, false).unwrap());
    assert!(!format!("{good:?}").contains("AAAA"));
    let changed = observed_with(open(&path).unwrap(), uid, 16, false, |file, bound| {
        let mut prefix = [0; 2];
        file.read_exact(&mut prefix)?;
        std::fs::write(&path, b"BBBBBBBB")?;
        let mut bytes = prefix.to_vec();
        file.take(bound as u64).read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    assert_eq!(changed.unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(observed(&path, uid, 16, false).unwrap().bytes, b"BBBBBBBB");
}

#[test]
fn scoped_owned_reads_refuse_a_linked_parent_at_the_final_open() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let selected = root.join("selected");
    let foreign = root.join("foreign");
    std::fs::create_dir(&selected).unwrap();
    std::fs::create_dir(&foreign).unwrap();
    let file = selected.join("control");
    std::fs::write(&file, b"owned control").unwrap();
    let uid = std::fs::metadata(&file).unwrap().uid();
    let live = Live::new();
    assert_eq!(
        live.read_owned_file_scoped(&file, uid, 13, false).unwrap(),
        b"owned control"
    );
    assert!(
        live.read_owned_file_scoped(&file, uid.wrapping_add(1), 13, false)
            .is_err()
    );
    assert!(live.read_owned_file_scoped(&file, uid, 1, false).is_err());
    std::fs::write(foreign.join("control"), b"other control").unwrap();
    std::fs::rename(&selected, root.join("saved-selected")).unwrap();
    std::os::unix::fs::symlink(&foreign, &selected).unwrap();
    assert!(live.read_owned_file_scoped(&file, uid, 13, false).is_err());
    assert_eq!(
        live.read_owned_file(&file, uid, 13, false).unwrap(),
        b"other control",
        "the existing unscoped caller policy is unchanged"
    );
}

#[test]
fn descriptor_relative_scoped_reads_never_follow_an_ancestor_or_leaf_link() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let directory = root.join("selected");
    std::fs::create_dir(&directory).unwrap();
    let file = directory.join("control");
    std::fs::write(&file, b"owned control").unwrap();
    let uid = std::fs::metadata(&file).unwrap().uid();
    assert_eq!(
        read_admitted(open_components(&file).unwrap(), uid, 13, false).unwrap(),
        b"owned control"
    );
    let leaf = directory.join("link");
    std::os::unix::fs::symlink(&file, &leaf).unwrap();
    assert!(open_components(&leaf).is_err());
    std::fs::rename(&directory, root.join("saved")).unwrap();
    std::os::unix::fs::symlink(root.join("saved"), &directory).unwrap();
    assert!(open_components(&file).is_err());
    assert!(open_components(Path::new("relative/control")).is_err());
    assert!(open_components(&root.join("selected/../saved/control")).is_err());
}

/// A folder on the way that may be searched but not listed (`--x`, as a home under a 0711
/// folder) is passed, as a plain open passes it: the walk holds it as a path (`O_PATH`).
/// The control: that folder cannot be opened to read, as the walk opened each before.
#[cfg(target_os = "linux")]
#[test]
fn a_folder_on_the_way_that_may_only_be_searched_is_passed() {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    aterm_uds::host::could_not_run_as_root("a search-only folder");
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let folder = root.join("search-only");
    std::fs::create_dir(&folder).unwrap();
    let file = folder.join("control");
    std::fs::write(&file, b"owned control").unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o100)).unwrap();
    let walked = open_components(&file).map(|mut file| {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map(|_| bytes)
    });
    let listed = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(&folder);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        listed.map(drop).map_err(|error| error.kind()),
        Err(io::ErrorKind::PermissionDenied),
        "control: the folder cannot be opened to read"
    );
    assert_eq!(walked.unwrap().unwrap(), b"owned control");
}

#[test]
fn native_owned_reads_refuse_unsafe_modes_owner_symlink_directory_and_oversize() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let path = temp.path().join("owned");
    std::fs::write(&path, b"private fixture").unwrap();
    // A scratch file can inherit a group we do not belong to; the kernel then clears
    // SGID on chmod. Keep this owned fixture in our effective group before setting it.
    // SAFETY: getegid reads this process's effective group and has no preconditions.
    let gid = unsafe { libc::getegid() };
    std::os::unix::fs::chown(&path, None, Some(gid)).unwrap();
    assert_eq!(path.metadata().unwrap().gid(), gid);
    let uid = path.metadata().unwrap().uid();
    let live = Live::new();
    let mode = |mode| {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            path.metadata().unwrap().mode() & 0o7777,
            mode,
            "the fixture must retain the requested permission bits before admission"
        );
    };
    mode(0o600);
    assert_eq!(
        live.read_owned_file(&path, uid, 15, true).unwrap(),
        b"private fixture"
    );
    assert!(live.read_owned_file(&path, uid, 14, true).is_err());
    assert!(
        live.read_owned_file(&path, uid.wrapping_add(1), 15, true)
            .is_err()
    );
    for unsafe_mode in [0o620, 0o602, 0o4600, 0o2600] {
        mode(unsafe_mode);
        assert!(
            live.read_owned_file(&path, uid, 15, false).is_err(),
            "{unsafe_mode:o}"
        );
    }
    mode(0o644);
    assert!(live.read_owned_file(&path, uid, 15, true).is_err());
    assert_eq!(
        live.read_owned_file(&path, uid, 15, false).unwrap(),
        b"private fixture"
    );
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(live.read_owned_file(&link, uid, 15, false).is_err());
    assert!(live.read_owned_file(temp.path(), uid, 15, false).is_err());
    assert!(live.read_owned_file(&path, uid, usize::MAX, false).is_err());
}

#[test]
fn native_owned_fifo_read_refuses_without_waiting_for_a_writer() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    let temp = aterm_tempfile::tempdir().unwrap();
    let path = temp.path().join("fifo");
    let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a live owned C string names an absent file in this private fixture.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let uid = path.metadata().unwrap().uid();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        send.send(Live::new().read_owned_file(&path, uid, 64, true).is_err())
            .unwrap();
    });
    assert!(
        receive
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("FIFO admission must not wait for a writer")
    );
}

#[test]
fn fake_owned_reads_admit_the_owner_and_bound_both_metadata_and_actual_bytes() {
    let sys = Fake::root();
    let path = Path::new("/owned");
    sys.file(path.to_str().unwrap(), b"private fixture");
    let safe = Stat {
        uid: 501,
        gid: 20,
        mode: 0o600,
        dir: false,
        symlink: false,
        len: 15,
    };
    sys.stat_of(path.to_str().unwrap(), safe);
    assert_eq!(
        sys.read_owned_file(path, 501, 15, true).unwrap(),
        b"private fixture"
    );
    assert_eq!(
        sys.read_owned_file_observed(path, 501, 15, true)
            .unwrap()
            .metadata
            .stat,
        safe
    );
    assert!(sys.read_owned_file(path, 501, 14, true).is_err());
    assert!(sys.read_owned_file(path, 502, 15, true).is_err());
    for changed in [
        Stat {
            mode: 0o620,
            ..safe
        },
        Stat {
            mode: 0o4600,
            ..safe
        },
        Stat {
            mode: 0o2600,
            ..safe
        },
        Stat { dir: true, ..safe },
        Stat {
            symlink: true,
            ..safe
        },
    ] {
        sys.stat_of(path.to_str().unwrap(), changed);
        assert!(sys.read_owned_file(path, 501, 15, false).is_err());
        assert!(sys.read_owned_file_observed(path, 501, 15, false).is_err());
    }
    sys.stat_of(
        path.to_str().unwrap(),
        Stat {
            mode: 0o644,
            ..safe
        },
    );
    assert!(sys.read_owned_file(path, 501, 15, true).is_err());
    assert!(sys.read_owned_file(path, 501, 15, false).is_ok());
    sys.stat_of(path.to_str().unwrap(), Stat { len: 1, ..safe });
    assert!(
        sys.read_owned_file(path, 501, 14, true).is_err(),
        "stale small metadata must not bypass the actual read bound"
    );
}
