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
#[path = "metadata_io_seam_tests.rs"]
pub(crate) mod seam;

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
///
/// A directory opens too (`FILE_FLAG_BACKUP_SEMANTICS`), as `open(2)` opens one on Unix, so
/// it is refused as not regular by its own handle's metadata. Without the flag Windows refuses
/// the open itself as access denied, an error a reader cannot tell from an unreadable file:
/// [`crate::store::build_is_complete`] keeps such a marker as complete.
#[cfg(windows)]
pub(crate) fn open_regular_with_meta(
    path: &Path,
) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
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
#[path = "metadata_io_tests.rs"]
mod tests;
