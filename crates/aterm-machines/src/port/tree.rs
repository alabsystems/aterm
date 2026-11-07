// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole-folder steps for a root-owned copy of an app bundle ([`super::System::clone_tree`],
//! [`super::System::root_own`], [`super::System::exchange`]): APFS `clonefile(2)`, an owner and
//! mode walk that never follows a link, and `renamex_np`'s atomic swap.

use std::io;
use std::path::Path;

#[cfg(target_os = "macos")]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path holds a NUL"))
}

/// Clone the real directory held open at `from` to the new name `to`, owned by the caller.
/// Source links are refused and a replacement pathname cannot redirect the clone.
///
/// # Errors
/// Unsafe source shape, or the original filesystem error (`EXDEV` remains distinguishable).
pub(super) fn clone(from: &Path, to: &Path) -> Result<(), aterm_uds::app_image::Error> {
    aterm_uds::app_image::clone(from, to)
}

/// Exchange the names `a` and `b` atomically (`renamex_np` with `RENAME_SWAP`): each then names
/// what the other did. Off macOS it refuses (`Unsupported`).
///
/// # Errors
/// The call's error, `ENOENT` when either is missing.
pub(super) fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let (a, b) = (c_path(a)?, c_path(b)?);
        // SAFETY: both pointers are NUL-terminated strings that outlive the call; RENAME_SWAP
        // makes it an exchange of the two existing names.
        if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (a, b);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Make `path` (and, with `tree`, everything below it) `uid:gid` with mode bits no wider than
/// `0755`, never following a link: the group and other write bits, setuid, setgid and sticky are
/// cleared, and any extended ACL is removed, so a bundle the person's account
/// made can carry no grant of its own into a root-owned clone (`ditto`'s copy keeps ACLs and set-id
/// bits). The outer image must be a real directory. Internal links must stay within it;
/// they are re-owned and stripped of ACLs without following them.
///
/// # Errors
/// Unsafe shape or escaping link, or the first failing filesystem/ACL operation.
pub(super) fn own(
    path: &Path,
    uid: u32,
    gid: u32,
    tree: bool,
) -> Result<(), aterm_uds::app_image::Error> {
    aterm_uds::app_image::seal(path, uid, gid, tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The walk clears group and other write below the folder and leaves a link's target alone.
    /// NEGATIVE CONTROL: without `tree`, only the folder itself changes.
    #[test]
    fn the_walk_clears_shared_write_and_never_follows_a_link() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = aterm_tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o666)).unwrap();
        let app = dir.path().join("a.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        let exe = app.join("Contents/MacOS/aterm");
        std::fs::write(&exe, b"").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::os::unix::fs::symlink("MacOS/aterm", app.join("Contents/link")).unwrap();
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().mode() & 0o7777;
        let (uid, gid) = {
            let meta = std::fs::metadata(dir.path()).unwrap();
            (meta.uid(), meta.gid())
        };

        own(&app, uid, gid, false).unwrap();
        assert_eq!((mode(&app), mode(&exe)), (0o755, 0o777));
        own(&app, uid, gid, true).unwrap();
        assert_eq!((mode(&app), mode(&exe)), (0o755, 0o755));
        assert_eq!(mode(&outside), 0o666);
        std::os::unix::fs::symlink(&outside, app.join("Contents/escaped")).unwrap();
        assert!(
            own(&app, uid, gid, true).is_err(),
            "a sealed root image cannot keep a link into another account's writable files"
        );
        assert_eq!(mode(&outside), 0o666);
        std::fs::remove_file(app.join("Contents/escaped")).unwrap();
        std::os::unix::fs::symlink("missing/target", app.join("Contents/dangling")).unwrap();
        own(&app, uid, gid, true).unwrap();
        std::os::unix::fs::symlink(".", app.join("x")).unwrap();
        std::fs::create_dir(app.join("d")).unwrap();
        std::os::unix::fs::symlink("../x/..", app.join("d/composed")).unwrap();
        assert!(
            own(&app, uid, gid, true).is_err(),
            "relative links cannot compose an outside traversal"
        );
        assert_eq!(mode(&outside), 0o666);
    }

    /// The seal removes an ACL the person's account put on a file, a folder or a link, and clears
    /// setuid and setgid: what `ditto` carries into a copy. NEGATIVE CONTROL: before the seal,
    /// `ls -le` shows each ACL and the set-id bits are there.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_seal_removes_acls_and_set_id_bits() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = aterm_tempfile::tempdir().unwrap();
        let app = dir.path().join("a.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("aterm");
        let helper = macos.join("helper");
        std::fs::write(&exe, b"").unwrap();
        std::fs::write(&helper, b"").unwrap();
        // A scratch file takes its folder's group, which can be one we do not belong to (`wheel`
        // under /private/tmp); the kernel then clears SGID on chmod. Keep the helper in our
        // effective group before setting it.
        // SAFETY: getegid reads this process's effective group and has no preconditions.
        let egid = unsafe { libc::getegid() };
        std::os::unix::fs::chown(&helper, None, Some(egid)).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o4755)).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o2755)).unwrap();
        let link = app.join("Contents/link");
        std::os::unix::fs::symlink("MacOS/aterm", &link).unwrap();
        let chmod = |args: &[&std::ffi::OsStr]| {
            let ran = aterm_uds::spawnfd::command("/bin/chmod")
                .args(args)
                .output()
                .unwrap();
            assert!(ran.status.success(), "{ran:?}");
        };
        let grant = std::ffi::OsStr::new("everyone allow write,delete,append");
        chmod(&["+a".as_ref(), grant, exe.as_os_str()]);
        chmod(&["+a".as_ref(), grant, macos.as_os_str()]);
        chmod(&["-h".as_ref(), "+a".as_ref(), grant, link.as_os_str()]);
        let acls = |p: &Path| {
            let ran = aterm_uds::spawnfd::command("/bin/ls")
                .arg("-led")
                .arg(p)
                .output()
                .unwrap();
            assert!(ran.status.success(), "{ran:?}");
            String::from_utf8_lossy(&ran.stdout)
                .lines()
                .filter(|line| line.trim_start().starts_with("0:"))
                .count()
        };
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().mode() & 0o7777;
        assert_eq!((acls(&exe), acls(&macos), acls(&link)), (1, 1, 1));
        assert_eq!((mode(&exe), mode(&helper)), (0o4755, 0o2755));
        // The seal must also target a group we can assign, rather than the scratch
        // folder's inherited group: the helper now belongs to our effective group.
        let uid = std::fs::metadata(dir.path()).unwrap().uid();
        own(&app, uid, egid, true).unwrap();
        assert_eq!((acls(&exe), acls(&macos), acls(&link)), (0, 0, 0));
        assert_eq!((mode(&exe), mode(&helper)), (0o755, 0o755));
    }

    /// A clone of a folder holds its files, and an exchange swaps two names whole.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_folder_clones_and_two_folders_swap() {
        use crate::port::System as _;
        use std::os::unix::fs::MetadataExt as _;

        let dir = aterm_tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a.app"),
            dir.path().join("b.app"),
            dir.path().join("c.app"),
        );
        std::fs::create_dir_all(a.join("Contents")).unwrap();
        std::fs::write(a.join("Contents/x"), b"a").unwrap();
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(c.join("y"), b"c").unwrap();
        clone(&a, &b).unwrap();
        assert_eq!(std::fs::read(b.join("Contents/x")).unwrap(), b"a");
        assert!(
            matches!(clone(&a, &b), Err(aterm_uds::app_image::Error::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        exchange(&b, &c).unwrap();
        assert_eq!(std::fs::read(b.join("y")).unwrap(), b"c");
        assert_eq!(std::fs::read(c.join("Contents/x")).unwrap(), b"a");

        // The real strict checker accepts an intact owned clone and keeps a
        // modified resource unverified. Capturing this requirement precedes
        // either copy or mutation; candidate bytes never choose their own policy.
        aterm_uds::deadline::require_trust_service();
        let signed = dir.path().join("signed.app");
        let executable = signed.join("Contents/MacOS/aterm");
        let resource = Path::new("Contents/Resources/payload");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::create_dir_all(signed.join(resource).parent().unwrap()).unwrap();
        std::fs::copy("/usr/bin/true", &executable).unwrap();
        std::fs::write(
            signed.join("Contents/Info.plist"),
            b"<plist version=\"1.0\"><dict>\
              <key>CFBundleExecutable</key><string>aterm</string>\
              <key>CFBundleIdentifier</key><string>com.aterm.stage-fixture</string>\
              <key>CFBundlePackageType</key><string>APPL</string>\
              </dict></plist>",
        )
        .unwrap();
        std::fs::write(signed.join(resource), b"intact").unwrap();
        let signed_result = aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--timestamp=none"])
            .arg(&signed)
            .output()
            .unwrap();
        assert!(signed_result.status.success(), "{signed_result:?}");
        let identity = aterm_keeper::identity::Identity::for_code_at(&signed).unwrap();
        let staged = dir.path().join("staged.app");
        clone(&signed, &staged).unwrap();
        let metadata = std::fs::metadata(dir.path()).unwrap();
        own(&staged, metadata.uid(), metadata.gid(), true).unwrap();
        identity
            .admits_code_at_strictly(&staged)
            .expect("the intact staged clone satisfies the previously captured requirement");
        let foreign = crate::port::Live::new().check_app_copy(&staged);
        assert!(
            matches!(foreign, Err(aterm_keeper::identity::CheckError::Refused(_))),
            "the System checker uses its loaded identity, never the candidate's: {foreign:?}"
        );
        std::fs::write(staged.join(resource), b"modified after copying").unwrap();
        let verdict = identity.admits_code_at_strictly(&staged);
        eprintln!("owned staged resource after mutation: {verdict:?}");
        assert!(
            matches!(
                verdict,
                Err(aterm_keeper::identity::CheckError::Refused(_)
                    | aterm_keeper::identity::CheckError::Unchecked(_))
            ),
            "a modified staged resource is never admitted: {verdict:?}"
        );

        // This is the exact pathname the caller canonicalized. A replacement
        // before cloning cannot turn the root image into an outer symlink.
        let captured = std::fs::canonicalize(&a).unwrap();
        let moved = dir.path().join("moved.app");
        std::fs::rename(&a, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &a).unwrap();
        let rebound = dir.path().join("rebound.app");
        assert!(
            clone(&captured, &rebound).is_err(),
            "a canonical source pathname rebound to a link is not a real bundle snapshot"
        );
        assert!(!rebound.exists());
    }
}
