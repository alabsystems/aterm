// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent test observation through the SAME live contents-directory borrow.
//! No filesystem path, printed report or observation creates an original owner.

use crate::actor::owner::bootstrap_graph::ArchiveOperation;
use aterm_types::resource_admission::Binding;
use aterm_uds::private_file::PrivateDirectory;
use std::{
    collections::BTreeSet,
    ffi::CString,
    fs::{File, Metadata},
    io,
    os::{
        fd::{AsRawFd as _, BorrowedFd, FromRawFd as _},
        unix::fs::{FileExt as _, MetadataExt as _},
    },
    sync::Mutex,
    time::Instant,
};
use trust_resource_controller::bootstrap_graph::archive;

struct Armed {
    binding: Binding,
    observed: Option<Result<u64, String>>,
}
static ARMED: Mutex<Option<Armed>> = Mutex::new(None);

pub(super) fn arm(binding: &Binding) {
    let mut armed = ARMED.lock().unwrap();
    assert!(armed.is_none());
    *armed = Some(Armed {
        binding: binding.clone(),
        observed: None,
    });
}

pub(super) fn require(binding: &Binding) -> u64 {
    let armed = ARMED.lock().unwrap().take().expect("archive oracle armed");
    assert_eq!(&armed.binding, binding);
    armed
        .observed
        .expect("actual successful native archive closure was not observed")
        .expect("actual original contents did not satisfy the independent oracle")
}

pub(in crate::actor::owner) fn observe(operation: &ArchiveOperation) {
    let mut armed = ARMED.lock().unwrap();
    let Some(armed) = armed.as_mut() else {
        return;
    };
    let binding = &operation.request.key.graph.query.binding;
    let result = (|| {
        if binding.session != armed.binding.session
            || binding.root_attempt != armed.binding.attempt
            || armed.observed.is_some()
        {
            return Err(invalid("foreign or repeated archive observation"));
        }
        operation.request.validate()?;
        let expected = operation
            .request
            .archives
            .iter()
            .enumerate()
            .map(|(index, pin)| Ok((operation.request.output_basename(index)?, pin.sha256)))
            .collect::<io::Result<Vec<_>>>()?;
        operation
            .inputs
            .with_contents_directory(operation.deadline, |contents| {
                inspect(contents, &expected, operation.deadline)
            })
    })();
    // Publish only after the SAME bundle's final source/configuration/workspace
    // bookends succeeded. Errors are retained for the fixture after actual cleanup.
    armed.observed = Some(result.map_err(|error| error.to_string()));
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}

fn open(contents: BorrowedFd<'_>, name: &CString) -> io::Result<File> {
    aterm_uds::kept_out(|| {
        // SAFETY: the directory borrow and NUL-terminated single leaf remain
        // live. Nonblocking/no-follow opening cannot wait on a substituted FIFO.
        let fd = unsafe {
            libc::openat(
                contents.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned this new owned descriptor on success.
        Ok(unsafe { File::from_raw_fd(fd) })
    })
}

fn version(metadata: &Metadata) -> (u64, u64, u64, u32, u32, u32, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mode(),
        metadata.uid(),
        metadata.gid(),
        metadata.nlink(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn names(
    directory: &PrivateDirectory,
    expected: &BTreeSet<Vec<u8>>,
    deadline: Instant,
) -> io::Result<()> {
    let mut actual = BTreeSet::new();
    for entry in directory.view()?.names()? {
        time(deadline)?;
        let entry = entry?;
        let name = entry.as_encoded_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if actual.len() == archive::ARCHIVE_COUNT || !actual.insert(name.to_vec()) {
            return Err(invalid("archive contents has extra or repeated entries"));
        }
    }
    if &actual != expected {
        return Err(invalid("archive contents has a missing or foreign entry"));
    }
    Ok(())
}

fn inspect(
    contents: BorrowedFd<'_>,
    expected: &[(Vec<u8>, [u8; 32])],
    deadline: Instant,
) -> io::Result<u64> {
    time(deadline)?;
    let expected_names = expected
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    if expected.len() != archive::ARCHIVE_COUNT
        || expected_names.len() != archive::ARCHIVE_COUNT
        || expected_names
            .iter()
            .any(|name| name.is_empty() || name.contains(&b'/') || name.contains(&0))
    {
        return Err(invalid(
            "oracle requires exactly nine distinct direct leaves",
        ));
    }
    // Duplicate only this borrowed description; this is no owner or pathname lookup.
    let directory = PrivateDirectory::from_file(File::from(contents.try_clone_to_owned()?))?;
    names(&directory, &expected_names, deadline)?;
    let mut total = 0_u64;
    let mut identities = BTreeSet::new();
    let mut buffer = [0; 64 * 1024];
    for (name, expected_hash) in expected {
        time(deadline)?;
        let name = CString::new(name.clone()).map_err(|_| invalid("invalid archive leaf"))?;
        let file = open(contents, &name)?;
        let before = file.metadata()?;
        if !before.is_file()
            || before.mode() & 0o777 != 0o600
            || before.nlink() != 1
            // SAFETY: geteuid has no preconditions and returns this process's uid.
            || before.uid() != unsafe { libc::geteuid() }
            || !(1..=archive::MAX_ARCHIVE_BYTES).contains(&before.len())
            || !identities.insert((before.dev(), before.ino()))
        {
            return Err(invalid(
                "archive leaf is not a distinct bounded private regular file",
            ));
        }
        trust_resource_provenance::require_no_acl_grants(&file)?;
        total = total
            .checked_add(before.len())
            .filter(|sum| *sum <= archive::MAX_TOTAL_ARCHIVE_BYTES)
            .ok_or_else(|| invalid("archive aggregate byte limit exceeded"))?;
        let mut hash = aterm_digest::Sha256::new();
        let mut offset = 0;
        while offset < before.len() {
            time(deadline)?;
            let count = (before.len() - offset).min(buffer.len() as u64) as usize;
            let read = match file.read_at(&mut buffer[..count], offset) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if read == 0 {
                return Err(invalid("archive leaf truncated during independent read"));
            }
            hash.update(&buffer[..read]);
            offset += read as u64;
        }
        if hash.finalize() != *expected_hash
            || version(&file.metadata()?) != version(&before)
            || version(&open(contents, &name)?.metadata()?) != version(&before)
        {
            return Err(invalid(
                "archive leaf changed or independent SHA256 differs",
            ));
        }
    }
    names(&directory, &expected_names, deadline)?;
    time(deadline)?;
    Ok(total)
}

#[test]
fn oracle_checks_all_nine_files_and_the_original_descriptor() {
    use std::os::{fd::AsFd as _, unix::fs::PermissionsExt as _};
    let temporary = aterm_tempfile::tempdir().unwrap();
    let directory = temporary.path().join("original");
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let expected = (0..archive::ARCHIVE_COUNT)
        .map(|index| {
            let name = format!("archive-{index}.tar.xz").into_bytes();
            let bytes = format!("actual independent archive {index}").into_bytes();
            let path = directory.join(std::str::from_utf8(&name).unwrap());
            std::fs::write(&path, &bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            (name, aterm_digest::Sha256::digest(&bytes))
        })
        .collect::<Vec<_>>();
    let held = File::open(&directory).unwrap();
    let until = Instant::now() + std::time::Duration::from_secs(10);
    assert!(inspect(held.as_fd(), &expected, until).unwrap() > 0);
    let moved = temporary.path().join("retained-original");
    std::fs::rename(&directory, &moved).unwrap();
    std::fs::create_dir(&directory).unwrap();
    assert!(inspect(held.as_fd(), &expected, until).unwrap() > 0);
    // A same-length corruption of the final archive must be read and rejected.
    let last = moved.join("archive-8.tar.xz");
    let original = std::fs::read(&last).unwrap();
    let mut corrupt = original.clone();
    corrupt[0] ^= 1;
    std::fs::write(&last, corrupt).unwrap();
    assert!(inspect(held.as_fd(), &expected, until).is_err());
    std::fs::write(&last, &original).unwrap();
    std::fs::write(moved.join("extra"), b"unrequested").unwrap();
    assert!(inspect(held.as_fd(), &expected, until).is_err());
    std::fs::remove_file(moved.join("extra")).unwrap();
    std::fs::remove_file(&last).unwrap();
    assert!(inspect(held.as_fd(), &expected, until).is_err());
    std::os::unix::fs::symlink(moved.join("archive-0.tar.xz"), &last).unwrap();
    assert!(inspect(held.as_fd(), &expected, until).is_err());
    std::fs::remove_file(&last).unwrap();
    std::fs::write(&last, original).unwrap();
    std::fs::set_permissions(&last, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(inspect(held.as_fd(), &expected, until).is_ok());
    assert!(inspect(held.as_fd(), &expected, Instant::now()).is_err());
}
