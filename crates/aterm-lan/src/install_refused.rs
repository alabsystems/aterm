// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Root's record of the broker installs setup's root steps could not finish
//! (`aterm_types::macos_setup::refused::LAN_BROKER`), as the broker that serves reads it: once
//! the broker running from its installed image has served, the first time a peer it admitted
//! says its protocol's hello (`crate::server::Shared::after_first_hello`), it takes out every
//! line its own install's key names (a definite failure, a no-verdict, a stall), so a run nobody
//! asked for no longer leaves out an install that serves. A broker that binds and dies before
//! any hello, which root's run judged as serving in no round, leaves them. It keeps every other
//! line: another bundle's or build's failure still stands for that install, and only an apply
//! that succeeds removes the whole record.

use aterm_types::macos_setup::refused;
use std::collections::BTreeSet;
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// The most a record may hold before it reads as no record root wrote.
const MAX: usize = aterm_types::macos_setup::spotlight_scope::RECEIPT_MAX;

/// [`served`] at root's record, for the broker serving the install of `source` whose executable
/// is `code`; a record it could not read or change is logged, and left as it stands.
pub(crate) fn serving(source: &Path, code: &Path) {
    if let Err(error) = served(Path::new(refused::LAN_BROKER), source, code) {
        crate::server::log(format_args!(
            "{}: the install's own lines were not taken out: {error}",
            refused::LAN_BROKER
        ));
    }
}

/// Take out of the record at `record` every line whose key is the install of the bundle `source`
/// whose executable is `code` (`refused::broker_key` of its code), rewriting the rest in place,
/// or removing the record where nothing is left. `Ok(false)` where there is no record or no line
/// of the key. The record is read and changed only while this holds its lock
/// (`refused::hold`), which root's steps take to write a failure or remove the record on a
/// success, so a success that lands while the broker takes out its lines waits for that change
/// and then removes the record, and never sees it brought back. The key is read before the lock
/// is taken, so root waits on no reading of the bundle's code. A record gone by the time it is
/// removed (a root of a build before the lock removed it) is removed.
///
/// # Errors
/// `code`'s code could not be read, or names no key; the record's lock could not be held; the
/// record stands but is not this process's own regular file (root's, for the broker) with no
/// group or other write, or is larger than a record, or could not be read; or the record could
/// not be written or removed.
pub(crate) fn served(record: &Path, source: &Path, code: &Path) -> io::Result<bool> {
    served_between(record, source, code, || {})
}

/// [`served`], running `between` once the record is read and before it is changed: where a test
/// puts another writer.
fn served_between(
    record: &Path,
    source: &Path,
    code: &Path,
    between: impl FnOnce(),
) -> io::Result<bool> {
    // No record: nothing to take out, and no lock file made beside a record that is not there.
    match std::fs::symlink_metadata(record) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        _ => {}
    }
    let code = aterm_uds::lan::install::CodeDirectories::of_file(code)?;
    let key = refused::broker_key(source, &code)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no key for this bundle"))?;
    let _held = refused::hold(record)?;
    let (file, meta) = match aterm_uds::regular_file::open(record) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        opened => opened?,
    };
    if meta.uid() != aterm_uds::peer::our_uid() || meta.mode() & 0o6022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "not this process's own record",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "larger than a record",
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let lines: BTreeSet<PathBuf> = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    let Some(kept) = refused::without_key(&lines, &key) else {
        return Ok(false);
    };
    between();
    if kept.is_empty() {
        match std::fs::remove_file(record) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            removed => removed?,
        }
    } else {
        let text: String = kept
            .iter()
            .map(|line| format!("{}\n", line.display()))
            .collect();
        aterm_uds::atomic_file::write(record, text.as_bytes(), 0o644)?;
    }
    Ok(true)
}

#[cfg(test)]
#[path = "install_refused_tests.rs"]
mod tests;
