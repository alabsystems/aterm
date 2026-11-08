// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor-bound observations for the private inventory's declared checkout scope.

use super::{ScopedPathMetadata, Stat};
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

fn open(path: &Path) -> io::Result<std::fs::File> {
    #[cfg(target_os = "macos")]
    {
        aterm_uds::nofollow::open_evtonly_path(path).map(std::fs::File::from)
    }
    #[cfg(not(target_os = "macos"))]
    {
        super::read_owned_file::hold_components(path)
    }
}

pub(super) fn observe(path: &Path) -> io::Result<ScopedPathMetadata> {
    use aterm_toolchain::stat_digest::{StatDigest, StatKey};
    let file = open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut digest = StatDigest::new(b"repository scoped metadata observation");
    digest.entry(b"", &StatKey::of(&metadata));
    Ok(ScopedPathMetadata {
        stat: Stat {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            dir: metadata.is_dir(),
            symlink: false,
            len: metadata.len(),
        },
        version: digest.finish().0,
    })
}

/// Probe only the supplied bounded single-component directory names, relative to a held
/// no-follow descriptor. No path-based namespace enumeration can escape after admission.
pub(super) fn directories(path: &Path, uid: u32, names: &[String]) -> io::Result<Vec<String>> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd as _;
    if names.len() > 256
        || names.iter().any(|name| {
            let mut parts = Path::new(name).components();
            !matches!(parts.next(), Some(std::path::Component::Normal(_)))
                || parts.next().is_some()
                || name.as_bytes().contains(&0)
                || name.as_bytes().contains(&b'/')
        })
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let file = open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut present = Vec::new();
    for name in names {
        let name_c = CString::new(name.as_bytes())?;
        // SAFETY: stat is plain storage which fstatat initializes on success. The held file
        // owns the directory descriptor; name_c is exactly one validated component.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: pointers remain live for the call and AT_SYMLINK_NOFOLLOW refuses the only
        // component that can be replaced. Ancestor names are not consulted by this lookup.
        let result = unsafe {
            libc::fstatat(
                file.as_raw_fd(),
                name_c.as_ptr(),
                &raw mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                continue;
            }
            return Err(error);
        }
        if stat.st_mode & libc::S_IFMT == libc::S_IFDIR
            && stat.st_uid == uid
            && stat.st_mode & 0o022 == 0
        {
            present.push(name.clone());
        }
    }
    Ok(present)
}

#[cfg(test)]
mod tests {
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
        std::os::unix::fs::symlink(foreign.join("objects/bb"), selected.join("objects/cc"))
            .unwrap();
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
}
