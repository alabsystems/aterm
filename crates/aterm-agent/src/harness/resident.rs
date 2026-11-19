// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resident's private working directory and versioned project instructions.

use aterm_dirfd::{CWD, Mode, OFlags};
use aterm_uds::private_file::{DirectoryView, PrivateDirectory, PrivateFileError};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

const FOLDER: &str = "resident";
const CONTEXT: &str = "CLAUDE.md";
const MAX_CONTEXT: usize = 16 * 1024 * 1024;
const PREFIX: &str = "<!-- aterm resident v";
const VERSION: u32 = aterm_primer::RESIDENT_BRIEF_VERSION;
const END: &str = "<!-- /aterm resident -->";

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Replace just the resident's marked lines. Reject ambiguous ownership before changing a byte.
fn next_context(previous: &str) -> io::Result<Option<String>> {
    let mut start = None;
    let mut end = None;
    let mut version = 0;
    let mut offset = 0;
    for line in previous.split_inclusive('\n') {
        let marker = line.trim();
        if let Some(rest) = marker.strip_prefix(PREFIX) {
            if start.is_some() {
                return Err(invalid("duplicate resident brief marker"));
            }
            version = rest
                .strip_suffix(" -->")
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| invalid("invalid resident brief version"))?;
            start = Some(offset);
        } else if marker == END {
            if start.is_none() || end.is_some() {
                return Err(invalid("unmatched resident brief closing marker"));
            }
            end = Some(offset + line.len());
        }
        offset += line.len();
    }
    let block = format!(
        "{PREFIX}{VERSION} -->\n{}\n{END}\n",
        aterm_primer::RESIDENT_BRIEF.trim_end()
    );
    match (start, end) {
        (Some(first), Some(last)) => {
            if version > VERSION
                || previous[first..last].trim_end_matches('\n') == block.trim_end_matches('\n')
            {
                return Ok(None);
            }
            let mut next = previous.to_owned();
            next.replace_range(first..last, &block);
            Ok(Some(next))
        }
        (None, None) => {
            let mut next = previous.to_owned();
            if !next.is_empty() && !next.ends_with('\n') {
                next.push('\n');
            }
            next.push_str(&block);
            Ok(Some(next))
        }
        _ => Err(invalid("unclosed resident brief marker")),
    }
}

fn open_dir(parent: impl AsFd, name: &Path) -> io::Result<File> {
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    aterm_uds::kept_out(|| {
        aterm_dirfd::openat(parent, name, flags, Mode::empty())
            .map(File::from)
            .map_err(io::Error::from)
    })
}

/// Make only the missing components under the nearest real directory, relative to its descriptor.
fn open_state(path: &Path) -> io::Result<File> {
    match open_dir(CWD, path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(error)?;
            let name = path
                .file_name()
                .ok_or_else(|| invalid("invalid harness directory name"))?;
            let base = open_state(parent)?;
            make_dir(&base, Path::new(name))
        }
        result => result,
    }
}

fn check_state_parent(parent: &File) -> io::Result<()> {
    match astream_sys::owner_private_dir(parent)? {
        Ok(_) => Ok(()),
        // A mode verdict already establishes the directory's owner and absence of ACL grants.
        // The harness directory may allow other accounts to read or traverse it.
        Err(astream_sys::NotPrivate::Mode(mode)) if mode & 0o022 == 0 => Ok(()),
        Err(why) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("harness state directory {why}"),
        )),
    }
}

fn make_dir(parent: &File, name: &Path) -> io::Result<File> {
    check_state_parent(parent)?;
    match aterm_dirfd::mkdirat(parent, name, Mode::from_bits(0o700)).map_err(io::Error::from) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        result => result?,
    }
    open_dir(parent, name)
}

fn context_bytes(view: &DirectoryView<'_>) -> io::Result<Option<Vec<u8>>> {
    match view.read(CONTEXT, MAX_CONTEXT) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(PrivateFileError::Missing) => Ok(None),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn prepare(harness_state: &Path) -> io::Result<PathBuf> {
    let root = open_state(harness_state)?;
    let metadata = root.metadata()?;
    let folder = harness_state.join(FOLDER);
    let directory = PrivateDirectory::from_file(make_dir(&root, Path::new(FOLDER))?)?;
    let lock_name = aterm_durable::write_lock::name(OsStr::new(CONTEXT));
    let _lock = directory.lock(
        &lock_name,
        Duration::ZERO,
        "resident context is being edited",
    )?;
    let view = directory.view()?;
    let previous = context_bytes(&view)?;
    let text =
        std::str::from_utf8(previous.as_deref().unwrap_or_default()).map_err(io::Error::other)?;
    let still_named = || {
        check_state_parent(&root)?;
        let named = std::fs::symlink_metadata(harness_state)?;
        if (named.dev(), named.ino()) == (metadata.dev(), metadata.ino())
            && directory.is_at(&folder)?
        {
            Ok(())
        } else {
            Err(io::Error::other(
                "the resident directory moved during preparation",
            ))
        }
    };
    if let Some(next) = next_context(text)? {
        if next.len() > MAX_CONTEXT {
            return Err(invalid("resident context exceeds the size limit"));
        }
        view.write_entry_landed(CONTEXT, next.as_bytes(), || {
            still_named()?;
            if context_bytes(&view)? == previous {
                Ok(())
            } else {
                Err(io::Error::other(
                    "resident context changed during preparation",
                ))
            }
        })??;
    }
    still_named()?;
    Ok(folder)
}

/// Prepare the resident's working directory beneath the harness state directory, creating missing parents.
/// An unchanged managed block keeps its file. A newer version survives an older app's startup.
///
/// # Errors
/// Returns the path and the failure when the directory, file or lock is inaccessible,
/// insufficiently private, concurrently changed or the marked text cannot be safely updated.
pub fn prepare_resident(state_root: &Path) -> Result<PathBuf, String> {
    prepare(state_root).map_err(|e| format!("resident in {}: {e}", state_root.display()))
}

#[cfg(test)]
#[path = "resident_tests.rs"]
mod tests;
