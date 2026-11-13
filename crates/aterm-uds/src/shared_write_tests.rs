// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn account(name: &str, gid: u32) -> Account {
    Account {
        name: name.into(),
        gid,
    }
}

fn group(name: &str, members: &[&str]) -> Group {
    Group {
        name: name.into(),
        members: members.iter().map(|m| m.as_bytes().to_vec()).collect(),
    }
}

#[test]
fn only_a_group_no_other_account_can_hold_is_private() {
    let me = account("ann", 1000);
    let own = group("ann", &[]);
    assert!(user_private_group(
        1000,
        1000,
        Some(&me),
        Some(&own),
        &[1000]
    ));
    assert!(
        user_private_group(1000, 1000, Some(&me), Some(&group("ann", &["ann"])), &[]),
        "listing its owner as a member adds no one"
    );
    for (why, uid, gid, account, group, primaries) in [
        (
            "root",
            0,
            0,
            Some(account("root", 0)),
            Some(group("root", &[])),
            vec![0],
        ),
        (
            "not the primary group",
            1000,
            1001,
            Some(me.clone()),
            Some(group("ann", &[])),
            vec![],
        ),
        (
            "another name",
            1000,
            1000,
            Some(me.clone()),
            Some(group("staff", &[])),
            vec![],
        ),
        (
            "a member",
            1000,
            1000,
            Some(me.clone()),
            Some(group("ann", &["bob"])),
            vec![],
        ),
        (
            "another's primary",
            1000,
            1000,
            Some(me.clone()),
            Some(own.clone()),
            vec![1000, 1001],
        ),
        ("no account", 1000, 1000, None, Some(own.clone()), vec![]),
        ("no group", 1000, 1000, Some(me.clone()), None, vec![]),
    ] {
        assert!(
            !user_private_group(uid, gid, account.as_ref(), group.as_ref(), &primaries),
            "{why}"
        );
    }
}

#[test]
fn the_other_accounts_primary_groups_are_read_last_and_decide() {
    let me = || Some(account("ann", 1000));
    let own = |_: u32| Some(group("ann", &[]));
    assert!(private_group_by(
        1000,
        1000,
        |_| me(),
        own,
        |_| Some(vec![1000])
    ));
    for (why, primaries) in [
        ("another account's primary group", Some(vec![1000, 1001])),
        ("an enumeration that failed", None),
    ] {
        assert!(
            !private_group_by(1000, 1000, |_| me(), own, |_| primaries),
            "{why}"
        );
    }
    assert!(
        !private_group_by(
            1000,
            1000,
            |_| me(),
            |_| Some(group("staff", &[])),
            |_| unreachable!()
        ),
        "another group's name: no enumeration"
    );
    assert!(
        !private_group_by(1000, 1000, |_| None, own, |_| unreachable!()),
        "no account: no enumeration"
    );
}

#[test]
fn only_a_private_groups_write_is_the_owners_own() {
    let private = |gid: u32| gid == 1000;
    for (why, owner, gid, mode, widened, shared) in [
        ("owner-only", 1000, 1000, 0o100_644, false, false),
        ("private-group write", 1000, 1000, 0o100_664, false, false),
        (
            "a 0775 directory in the private group",
            1000,
            1000,
            0o040_775,
            false,
            false,
        ),
        ("another group's write", 1000, 100, 0o100_664, false, true),
        ("world write", 1000, 1000, 0o100_666, false, true),
        ("world write alone", 1000, 1000, 0o100_646, false, true),
        ("another owner's file", 1001, 1000, 0o100_664, false, true),
        ("a sticky shared directory", 0, 0, 0o041_777, false, true),
        (
            "private-group write, widened",
            1000,
            1000,
            0o100_664,
            true,
            true,
        ),
        (
            "a 0775 directory, widened",
            1000,
            1000,
            0o040_775,
            true,
            true,
        ),
        (
            "an ACL whose mask grants no write",
            1000,
            1000,
            0o100_644,
            true,
            false,
        ),
        ("world write, widened", 1000, 1000, 0o100_646, true, true),
    ] {
        assert_eq!(
            others_can_write_as(1000, owner, gid, mode, widened, private),
            shared,
            "{why}"
        );
    }
    assert!(
        !others_can_write_as(1000, 1000, 100, 0o100_644, false, |_| unreachable!()),
        "no write bit for others: the group is never looked up"
    );
    assert!(
        others_can_write_as(1000, 1000, 1000, 0o100_664, true, |_| unreachable!()),
        "widened: the group is never looked up"
    );
}

#[test]
fn an_access_acl_is_either_attributes() {
    const EIO: i32 = 5;
    const EACCES: i32 = 13;
    for (why, posix, nfs4, carried) in [
        ("ext4 without an ACL", Some(ENODATA), Some(ENOTSUP), false),
        (
            "a filesystem that keeps none",
            Some(ENOTSUP),
            Some(ENOTSUP),
            false,
        ),
        ("FUSE without either", Some(ENODATA), Some(ENODATA), false),
        ("a POSIX ACL", None, Some(ENOTSUP), true),
        ("an NFSv4 ACL", Some(ENOTSUP), None, true),
        ("an unreadable POSIX ACL", Some(EACCES), Some(ENODATA), true),
        (
            "an unreachable NFSv4 server",
            Some(ENOTSUP),
            Some(EIO),
            true,
        ),
    ] {
        let answer = acl_carried(|name| match name.to_bytes() {
            b"system.posix_acl_access" => posix,
            b"system.nfs4_acl" => nfs4,
            other => panic!("{why}: asked {other:?}"),
        });
        assert_eq!(answer, carried, "{why}");
    }
}

#[test]
fn a_remote_or_unknown_filesystem_widens_the_mode() {
    assert!(remote(None), "an unreadable type");
    for magic in REMOTE_FILESYSTEMS {
        assert!(remote(Some(magic)), "{magic:#x}");
    }
    for magic in [
        0xEF53,
        0x0102_1994,
        0x9123_683E,
        0x5846_5342,
        0x794C_7630,
        0x9FA0,
    ] {
        assert!(!remote(Some(magic)), "{magic:#x}");
    }
}

#[test]
fn a_link_is_judged_as_it_stands_not_through() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-uds-shared-write.")
        .tempdir()
        .expect("scratch");
    let file = scratch.path().join("file");
    std::fs::write(&file, "").expect("write");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let link = scratch.path().join("link");
    std::os::unix::fs::symlink(&file, &link).expect("symlink");
    #[cfg(target_vendor = "apple")]
    {
        let c = std::ffi::CString::new(link.as_os_str().as_encoded_bytes()).expect("path");
        // SAFETY: a NUL-terminated path, live for the call.
        let rc =
            unsafe { libc::fchmodat(libc::AT_FDCWD, c.as_ptr(), 0o777, libc::AT_SYMLINK_NOFOLLOW) };
        assert_eq!(rc, 0, "lchmod: {}", std::io::Error::last_os_error());
    }
    assert!(!others_can_write_at(&file).expect("judged"));
    assert!(others_can_write_at(&link).expect("judged"));
}

/// The walk over the accounts: it keeps the uids whose primary group is the one asked,
/// grows the record buffer on `ERANGE` (the same entry asked again) up to the limit and
/// no further, and a failure at any step, though every entry before it admits the group,
/// is no answer rather than a short list.
#[cfg(target_os = "linux")]
#[test]
fn a_failure_mid_walk_is_no_answer() {
    use nss::{Step, walk};
    let script = |steps: Vec<Step>| {
        let mut steps = steps.into_iter();
        let mut sizes = Vec::new();
        let got = walk(1000, |size| {
            sizes.push(size);
            steps.next().expect("asked past the script")
        });
        (got, sizes)
    };
    let entry = |uid, gid| Step::Entry { uid, gid };
    let (got, _) = script(vec![
        entry(0, 0),
        entry(1000, 1000),
        entry(1001, 100),
        entry(1002, 1000),
        Step::End,
    ]);
    assert_eq!(got, Some(vec![1000, 1002]));
    let (got, _) = script(vec![entry(1000, 1000), Step::Failed]);
    assert_eq!(got, None, "a backend failure after the entries so far");
    let mut grown = vec![Step::TooSmall; 8];
    grown.extend([entry(1000, 1000), Step::End]);
    let (got, sizes) = script(grown);
    assert_eq!(got, Some(vec![1000]), "an entry a 1 MiB buffer holds");
    assert_eq!(sizes.first(), Some(&4096));
    assert_eq!(sizes[8], 1 << 20);
    let (got, sizes) = script(vec![Step::TooSmall; 9]);
    assert_eq!(got, None, "an entry no buffer up to the limit holds");
    assert_eq!(sizes.last(), Some(&(1 << 20)));
}

/// The enumeration on this machine reads the real accounts: root has gid 0, and this
/// user has their own primary group.
#[cfg(target_os = "linux")]
#[test]
fn the_enumeration_reads_this_machines_accounts() {
    let roots = nss::primary_members(0).expect("the enumeration");
    assert!(roots.contains(&0), "{roots:?}");
    let uid = crate::peer::our_uid();
    let mine = nss::account(uid).expect("this user's passwd entry");
    let members = nss::primary_members(mine.gid).expect("the enumeration");
    assert!(members.contains(&uid), "{members:?}");
}

/// The filesystem type is read where it is: `/proc` is procfs, through its path and
/// through a descriptor alike, and a path that does not exist has no type.
#[cfg(target_os = "linux")]
#[test]
fn the_filesystem_type_is_read() {
    const PROC_SUPER_MAGIC: u32 = 0x9FA0;
    assert_eq!(
        filesystem::at_path(std::path::Path::new("/proc")),
        Some(PROC_SUPER_MAGIC)
    );
    let status = std::fs::File::open("/proc/self/status").expect("open");
    assert_eq!(filesystem::of_file(&status), Some(PROC_SUPER_MAGIC));
    assert_eq!(
        filesystem::at_path(std::path::Path::new("/proc/self/no-such-entry")),
        None
    );
}

/// On this machine's own filesystem: a file and a directory on this process's primary
/// group are judged as the pure rule says for what the system answers (its private
/// group or not, on a local filesystem or not), and as it says for a stand-in NFS or
/// unreadable filesystem type in place of the real one; an access ACL that gives another
/// account the group class's bits, leaving the mode as it was, is seen through the
/// descriptor and through the path, and refuses the group-write the private group would
/// have admitted; and the mask without a write bit still admits.
#[cfg(target_os = "linux")]
#[test]
fn an_access_acl_takes_back_the_private_groups_exemption() {
    use std::os::unix::fs::PermissionsExt as _;
    let chmod = |path: &std::path::Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    };
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-uds-shared-write.")
        .tempdir()
        .expect("scratch");
    let local = !remote(filesystem::at_path(scratch.path()));
    let nobody = 65534;
    for (name, dir, mode) in [
        ("file-664", false, 0o664),
        ("file-644", false, 0o644),
        ("dir-775", true, 0o775),
        ("dir-755", true, 0o755),
    ] {
        let path = scratch.path().join(name);
        if dir {
            std::fs::create_dir(&path).expect("mkdir");
        } else {
            std::fs::write(&path, "").expect("write");
        }
        chmod(&path, mode);
        let gid = std::os::unix::fs::MetadataExt::gid(&std::fs::metadata(&path).expect("stat"));
        let private = is_user_private_group(gid);
        let open = || std::fs::File::open(&path).expect("open");
        assert!(!acl::on_file(&open()), "{name}: no ACL yet");
        assert!(!acl::at_path(&path), "{name}: no ACL yet");
        let plain = mode & 0o020 != 0 && !(private && local);
        assert_eq!(others_can_write(&open()).expect("judged"), plain, "{name}");
        assert_eq!(others_can_write_at(&path).expect("judged"), plain, "{name}");
        let md = std::fs::symlink_metadata(&path).expect("lstat");
        for (magic, remote) in [(Some(0x6969), true), (None, true), (Some(0xEF53), false)] {
            assert_eq!(
                judge(&md, false, magic),
                mode & 0o020 != 0 && !(private && !remote),
                "{name} on a stand-in filesystem {magic:x?}"
            );
        }
        grant_group_class_by_acl(&path, nobody)
            .unwrap_or_else(|e| panic!("{name}: an access ACL on ext4 or tmpfs: {e}"));
        assert_eq!(
            std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(&path).expect("stat")) & 0o777,
            mode,
            "{name}: the ACL leaves the mode bits as they were"
        );
        assert!(acl::on_file(&open()), "{name}: the descriptor sees the ACL");
        assert!(acl::at_path(&path), "{name}: the path sees the ACL");
        let shared = mode & 0o020 != 0;
        assert_eq!(
            others_can_write(&open()).expect("judged"),
            shared,
            "{name}, ACL"
        );
        assert_eq!(
            others_can_write_at(&path).expect("judged"),
            shared,
            "{name}, ACL"
        );
    }
}
