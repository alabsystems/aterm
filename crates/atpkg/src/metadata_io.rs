// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded, same-handle reads and atomic publication for package metadata.

use std::io::{self, Read as _};
use std::path::Path;

/// Replace metadata with a complete private file. The caller prepares and
/// hardens the parent directory according to its store layout.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    PendingMetadata::prepare(path, bytes, Publication::Private)?.commit()
}

/// Atomically publish a record with a 0644 creation mode, honoring the caller's
/// umask, without adding a content-flush obligation.
pub(crate) fn write_record_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    PendingMetadata::prepare(path, bytes, Publication::Record)?.commit()
}

/// Flush a record's contents before atomically publishing it. Its 0644 creation
/// mode honors the caller's umask; directory ordering remains with the caller.
pub(crate) fn write_record_durably(path: &Path, bytes: &[u8]) -> io::Result<()> {
    PendingMetadata::prepare(path, bytes, Publication::DurableRecord)?.commit()
}

#[derive(Clone, Copy)]
enum Publication {
    Private,
    Record,
    DurableRecord,
}

impl Publication {
    const fn mode(self) -> u32 {
        match self {
            Self::Private => 0o600,
            Self::Record | Self::DurableRecord => 0o644,
        }
    }
}

struct PendingMetadata {
    directory: aterm_tempfile::TempDir,
    destination: std::path::PathBuf,
}

impl PendingMetadata {
    fn prepare(path: &Path, bytes: &[u8], publication: Publication) -> io::Result<Self> {
        Self::prepare_with(path, bytes, publication, |file| {
            aterm_durable::refusal_accepted(crate::platform::sync_file_contents(file))
        })
    }

    fn prepare_with(
        path: &Path,
        bytes: &[u8],
        publication: Publication,
        flush: impl FnOnce(&std::fs::File) -> io::Result<()>,
    ) -> io::Result<Self> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "metadata has no parent directory",
            )
        })?;
        let directory = aterm_tempfile::Builder::new()
            .prefix("package-metadata-")
            .suffix(".noindex")
            .tempdir_in(parent)?;
        let mut file = crate::platform::open_create_new_write(
            &directory.path().join("data"),
            publication.mode(),
        )?;
        if matches!(publication, Publication::Private) {
            crate::platform::set_mode_on(&file, 0o600)?;
        }
        std::io::Write::write_all(&mut file, bytes)?;
        if matches!(publication, Publication::DurableRecord) {
            flush(&file)?;
        }
        Ok(Self {
            directory,
            destination: path.to_path_buf(),
        })
    }

    fn commit(self) -> io::Result<()> {
        #[cfg(test)]
        seam::before_commit(&self.destination)?;
        // Each pending write owns its path until rename. Concurrent writers may
        // replace the destination, but cannot unlink one another's staging file.
        std::fs::rename(self.directory.path().join("data"), &self.destination)
    }
}

#[cfg(test)]
pub(crate) mod seam {
    use super::*;
    type Callback = Box<dyn Fn(&Path) -> io::Result<()>>;
    thread_local! {
        static BEFORE_COMMIT: std::cell::RefCell<Option<Callback>> = const { std::cell::RefCell::new(None) };
    }
    pub(crate) struct Guard(Option<Callback>);
    impl Drop for Guard {
        fn drop(&mut self) {
            BEFORE_COMMIT.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    pub(crate) fn before_publish(callback: impl Fn(&Path) -> io::Result<()> + 'static) -> Guard {
        Guard(BEFORE_COMMIT.with(|slot| slot.replace(Some(Box::new(callback)))))
    }
    pub(super) fn before_commit(path: &Path) -> io::Result<()> {
        BEFORE_COMMIT.with(|slot| {
            slot.borrow()
                .as_ref()
                .map_or(Ok(()), |callback| callback(path))
        })
    }
}

fn too_large(limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("package metadata exceeds the {limit}-byte limit"),
    )
}

fn not_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "package metadata is not a regular non-link file",
    )
}

/// Opens an existing regular metadata file without following its final link.
#[cfg(unix)]
pub(crate) fn open_regular_with_meta(
    path: &Path,
) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(not_regular());
    }
    Ok((file, metadata))
}

/// Windows form of the existing regular-file admission, refusing reparse points.
#[cfg(windows)]
pub(crate) fn open_regular_with_meta(
    path: &Path,
) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(not_regular());
    }
    Ok((file, metadata))
}

/// Portable fallback with before/after regular-file checks on other targets.
#[cfg(not(any(unix, windows)))]
pub(crate) fn open_regular_with_meta(
    path: &Path,
) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    let before = std::fs::symlink_metadata(path)?;
    if !before.file_type().is_file() {
        return Err(not_regular());
    }
    let file = std::fs::File::open(path)?;
    let opened = file.metadata()?;
    let after = std::fs::symlink_metadata(path)?;
    if !opened.file_type().is_file()
        || !after.file_type().is_file()
        || before.len() != opened.len()
        || after.len() != opened.len()
        || before.modified().ok() != opened.modified().ok()
        || after.modified().ok() != opened.modified().ok()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "package metadata changed while it was opened",
        ));
    }
    Ok((file, opened))
}

pub(crate) fn open_regular(path: &Path) -> io::Result<std::fs::File> {
    open_regular_with_meta(path).map(|(file, _)| file)
}

#[cfg(unix)]
pub(crate) fn set_regular_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    open_regular(path)?.set_permissions(std::fs::Permissions::from_mode(mode))
}

/// Copy one regular, non-link file to a fresh regular destination without following a final
/// symlink at either endpoint, preserving source permissions.
pub(crate) fn copy_regular_file(src: &Path, dst: &Path) -> io::Result<u64> {
    let (mut reader, meta) = open_regular_with_meta(src)?;
    let _ = std::fs::remove_file(dst);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let mut writer = opts.open(dst)?;
    let copied = std::io::copy(&mut reader, &mut writer)?;
    writer.set_permissions(meta.permissions())?;
    Ok(copied)
}

/// Read one regular, non-link file through the same handle used for admission.
pub(crate) fn read_bounded_regular(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let (file, before) = open_regular_with_meta(path)?;
    read_bounded_regular_file(file, &before, limit)
}

/// Read one already-opened regular, non-link file bounded by `limit`.
pub(crate) fn read_bounded_regular_file(
    mut file: std::fs::File,
    before: &std::fs::Metadata,
    limit: usize,
) -> io::Result<Vec<u8>> {
    let limit_u64 = u64::try_from(limit).unwrap_or(u64::MAX);
    if before.len() > limit_u64 {
        return Err(too_large(limit));
    }
    let sentinel = limit.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "package metadata byte limit cannot be represented",
        )
    })?;
    let initial_capacity = usize::try_from(before.len().min(limit_u64).saturating_add(1))
        .unwrap_or(sentinel)
        .min(sentinel);
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(initial_capacity).map_err(|error| {
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            format!("package metadata allocation refused: {error}"),
        )
    })?;
    std::io::Read::by_ref(&mut file)
        .take(u64::try_from(sentinel).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(too_large(limit));
    }
    let after = file.metadata()?;
    if !after.file_type().is_file() || after.len() != bytes.len() as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "package metadata changed while it was read",
        ));
    }
    Ok(bytes)
}

/// Reads at most `limit` leading bytes of a regular, non-link file.
pub(crate) fn read_head(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    read_head_with_meta(path, limit).map(|(head, _meta)| head)
}

/// [`read_head`] together with the opened descriptor's `fstat` metadata.
pub(crate) fn read_head_with_meta(
    path: &Path,
    limit: usize,
) -> io::Result<(Vec<u8>, std::fs::Metadata)> {
    let (file, before) = open_regular_with_meta(path)?;
    let max = limit.saturating_add(1);
    let cap = usize::try_from(before.len().saturating_add(1))
        .unwrap_or(max)
        .min(max);
    let mut head = Vec::with_capacity(cap);
    file.take(u64::try_from(limit).unwrap_or(u64::MAX))
        .read_to_end(&mut head)?;
    Ok((head, before))
}

/// Reduce a regular file one bounded UTF-8 line at a time. The admitted handle's initial
/// length bounds total IO; growth, truncation and overlong records refuse instead of returning
/// a partial answer. `visit` returns false once its leading evidence is complete. The caller
/// owns any accumulated output, whose size may be proportional to the number of records.
pub(crate) fn read_regular_lines(
    path: &Path,
    line_bytes: usize,
    mut visit: impl FnMut(&str) -> io::Result<bool>,
) -> io::Result<()> {
    use std::io::BufRead as _;
    let (file, before) = open_regular_with_meta(path)?;
    let budget = before
        .len()
        .checked_add(1)
        .ok_or_else(|| too_large(usize::MAX))?;
    let line_budget = u64::try_from(line_bytes)
        .unwrap_or(u64::MAX)
        .checked_add(1)
        .ok_or_else(|| too_large(line_bytes))?;
    let mut reader = std::io::BufReader::new(file.take(budget));
    let mut seen = 0_u64;
    let mut eof = false;
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        let count = std::io::Read::by_ref(&mut reader)
            .take(line_budget)
            .read_until(b'\n', &mut bytes)?;
        seen = seen
            .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
            .ok_or_else(|| too_large(line_bytes))?;
        if count > line_bytes || seen > before.len() {
            return Err(too_large(line_bytes));
        }
        if count == 0 {
            eof = true;
            break;
        }
        let line = std::str::from_utf8(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if !visit(line)? {
            break;
        }
    }
    let file = reader.into_inner().into_inner();
    let after = file.metadata()?;
    if after.len() != before.len()
        || (eof && seen != before.len())
        || after.modified().ok() != before.modified().ok()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "package metadata changed while it was read",
        ));
    }
    Ok(())
}

/// Read one admitted metadata file and require complete UTF-8.
pub(crate) fn read_bounded_regular_utf8(path: &Path, limit: usize) -> io::Result<String> {
    String::from_utf8(read_bounded_regular(path, limit)?).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("package metadata is not UTF-8: {error}"),
        )
    })
}

/// Resolves a bounded chain of final-component symlinks and reads the target as bounded UTF-8.
pub(crate) fn read_bounded_regular_utf8_follow_final_links(
    path: &Path,
    limit: usize,
) -> io::Result<String> {
    const MAX_LINKS: usize = 16;

    let mut resolved = path.to_path_buf();
    for _ in 0..MAX_LINKS {
        let metadata = match std::fs::symlink_metadata(&resolved) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return read_bounded_regular_utf8(&resolved, limit);
            }
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_symlink() {
            return read_bounded_regular_utf8(&resolved, limit);
        }
        let target = std::fs::read_link(&resolved)?;
        resolved = if target.is_absolute() {
            target
        } else {
            resolved
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(target)
        };
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "package metadata symlink chain exceeds 16 links",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn record_publication_respects_a_restrictive_umask() {
        if !crate::testdir::isolated_lifecycle(
            "metadata_io::tests::record_publication_respects_a_restrictive_umask",
        ) {
            return;
        }
        struct RestoreMask(libc::mode_t);
        impl Drop for RestoreMask {
            fn drop(&mut self) {
                // SAFETY: this exact-test child owns its process-wide mask for this scope.
                unsafe { libc::umask(self.0) };
            }
        }
        // SAFETY: the lifecycle helper runs only this case in its owned child process.
        let _restore = RestoreMask(unsafe { libc::umask(0o077) });
        use std::os::unix::fs::PermissionsExt as _;
        let root = fixture("restricted-mask");
        let private = root.join("private");
        let buffered = root.join("buffered");
        let durable = root.join("durable");
        write_private_atomic(&private, b"private bytes").unwrap();
        write_record_atomic(&buffered, b"buffered bytes").unwrap();
        write_record_durably(&durable, b"durable bytes").unwrap();
        for path in [private, buffered, durable] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn publication_policy_preserves_permissions_and_flush_boundary() {
        for (policy, expected_flushes, expected_mode) in [
            (Publication::Private, 0, 0o600),
            (Publication::Record, 0, 0o644),
            (Publication::DurableRecord, 1, 0o644),
        ] {
            let root = fixture("flush-boundary");
            let path = root.join("record");
            std::fs::write(&path, b"previous bytes").unwrap();
            let flushes = std::cell::Cell::new(0);
            let pending =
                PendingMetadata::prepare_with(&path, b"complete new bytes", policy, |file| {
                    assert_eq!(file.metadata()?.len(), b"complete new bytes".len() as u64);
                    assert_eq!(
                        std::fs::read(&path)?,
                        b"previous bytes",
                        "flush precedes publication"
                    );
                    flushes.set(flushes.get() + 1);
                    Ok(())
                })
                .unwrap();
            assert_eq!(flushes.get(), expected_flushes);
            assert_eq!(std::fs::read(&path).unwrap(), b"previous bytes");
            pending.commit().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"complete new bytes");
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
                let expected_mode = if matches!(policy, Publication::Private) {
                    expected_mode
                } else {
                    let control = root.join("prior-creation-policy");
                    drop(
                        std::fs::OpenOptions::new()
                            .mode(expected_mode)
                            .create_new(true)
                            .write(true)
                            .open(&control)
                            .unwrap(),
                    );
                    std::fs::metadata(control).unwrap().permissions().mode() & 0o777
                };
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    expected_mode
                );
            }
            #[cfg(not(unix))]
            let _ = expected_mode;
        }
    }

    #[test]
    fn failed_content_flush_keeps_old_record_and_removes_staging() {
        let root = fixture("flush-failure");
        let path = root.join("record");
        std::fs::write(&path, b"original").unwrap();
        let result = PendingMetadata::prepare_with(
            &path,
            b"replacement",
            Publication::DurableRecord,
            |_| Err(io::Error::other("injected content flush failure")),
        );
        assert!(
            result.is_err(),
            "a required flush failure refuses publication"
        );
        assert_eq!(std::fs::read(path).unwrap(), b"original");
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "failed staging is cleaned"
        );
    }

    #[test]
    fn overlapping_metadata_writes_publish_complete_bytes_and_keep_independent_staging() {
        let root = fixture("overlap");
        let path = root.join("metadata");
        std::fs::write(&path, b"original").unwrap();
        let first =
            PendingMetadata::prepare(&path, b"first complete version", Publication::Private)
                .unwrap();
        let second =
            PendingMetadata::prepare(&path, b"second complete version", Publication::Private)
                .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        first.commit().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first complete version");
        second.commit().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second complete version");
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "staging is cleaned"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn abandoned_and_failed_metadata_writes_preserve_destination_and_remove_staging() {
        let root = fixture("abandon");
        let path = root.join("metadata");
        std::fs::write(&path, b"original").unwrap();
        drop(PendingMetadata::prepare(&path, b"abandoned", Publication::Private).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        let blocked = root.join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        assert!(write_private_atomic(&blocked, b"cannot replace a directory").is_err());
        assert!(blocked.is_dir());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
    }

    fn fixture(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-metadata-{label}-"))
    }

    #[test]
    fn exact_boundary_is_admitted_and_one_byte_over_is_rejected() {
        let root = fixture("boundary");
        let exact = root.join("exact");
        let over = root.join("over");
        std::fs::write(&exact, b"12345678").unwrap();
        std::fs::write(&over, b"123456789").unwrap();
        assert_eq!(read_bounded_regular(&exact, 8).unwrap(), b"12345678");
        let error = read_bounded_regular(&over, 8).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn line_reduction_bounds_records_and_detects_changes_after_partial_observation() {
        let root = fixture("line-reduction");
        let path = root.join("records");
        std::fs::write(&path, b"one\ntwo\n").unwrap();
        let mut names = Vec::new();
        read_regular_lines(&path, 4, |line| {
            names.push(line.trim().to_string());
            Ok(true)
        })
        .unwrap();
        assert_eq!(names, ["one", "two"]);
        assert!(read_regular_lines(&path, 3, |_| Ok(true)).is_err());
        std::fs::write(&path, [b'o', b'k', b'\n', 0xff]).unwrap();
        assert!(read_regular_lines(&path, 4, |_| Ok(true)).is_err());
        std::fs::write(&path, b"one\ntwo\n").unwrap();
        assert!(
            read_regular_lines(&path, 4, |_| {
                std::fs::write(&path, b"replacement is larger\n")?;
                Ok(false)
            })
            .is_err(),
            "even a completed prefix is refused if its admitted file changes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn writerless_fifo_and_final_symlink_return_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = fixture("special");
        let fifo = root.join("fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert_eq!(
            read_bounded_regular(&fifo, 64).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        let target = root.join("target");
        let link = root.join("link");
        std::fs::write(&target, b"status").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_bounded_regular(&link, 64).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn explicit_follow_reader_supports_and_safely_retargets_config_symlinks() {
        let root = fixture("follow-link");
        let first = root.join("first.toml");
        let second = root.join("second.toml");
        let logical = root.join("aterm.toml");
        std::fs::write(&first, "[packages]\nchannel = \"first\"\n").unwrap();
        std::fs::write(&second, "[packages]\nchannel = \"second\"\n").unwrap();
        std::os::unix::fs::symlink(&first, &logical).unwrap();
        assert!(
            read_bounded_regular_utf8_follow_final_links(&logical, 128)
                .unwrap()
                .contains("first")
        );
        std::fs::remove_file(&logical).unwrap();
        std::os::unix::fs::symlink(&second, &logical).unwrap();
        assert!(
            read_bounded_regular_utf8_follow_final_links(&logical, 128)
                .unwrap()
                .contains("second")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
