// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nonblocking observation of first-execution evidence before optional artifact removal.

use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

/// Missing is never-started; unreadable evidence is unknown, never an expired hold.
pub(super) fn hold(dir: &Path) -> Result<Option<Duration>, String> {
    let Some(stamp) = read(&dir.join(aterm_uds::deadline::EXEC_STAMP), 64)? else {
        return Ok(None);
    };
    // The existing witness can be empty. Its admitted regular file identity matters;
    // a directory, alias, multiply-linked file or refused read cannot stand in for it.
    if read(&dir.join(aterm_uds::deadline::RAN), 4096)?.is_some() {
        return Ok(None);
    }
    let at = std::str::from_utf8(&stamp)
        .ok()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or("first-execution marker is not an awake-clock reading")?;
    let awake = aterm_uds::process::awake_ms().ok_or("awake clock is unavailable")?;
    // Preserve the existing first-exec policy: a reading beyond this boot's awake clock
    // no longer holds a previous boot's executable. No parent-directory age substitutes
    // for an unreadable current reading.
    let Some(age) = awake.checked_sub(at) else {
        return Ok(None);
    };
    let left = aterm_uds::deadline::FIRST_EXEC_HANG.saturating_sub(Duration::from_millis(age));
    Ok((!left.is_zero()).then_some(left))
}

fn read(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, String> {
    let mut file = match astream_sys::open_nonblocking(path, astream_sys::Links::Refuse) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("first-execution evidence is unreadable: {error}")),
    };
    let before = file.metadata().map_err(|error| error.to_string())?;
    if !before.is_file() || before.len() > maximum {
        return Err("first-execution evidence is not a bounded regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if before.nlink() != 1 || before.uid() != aterm_uds::peer::our_uid() {
            return Err("first-execution evidence is not a singly-linked owned file".into());
        }
    }
    let modified = before.modified().map_err(|error| error.to_string())?;
    let mut bytes =
        Vec::with_capacity(usize::try_from(before.len()).unwrap_or(0).saturating_add(1));
    std::io::Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let after = file.metadata().map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if after.nlink() != 1 || after.uid() != aterm_uds::peer::our_uid() {
            return Err("first-execution evidence changed file admission while read".into());
        }
    }
    if bytes.len() as u64 != before.len()
        || after.len() != before.len()
        || after.modified().map_err(|error| error.to_string())? != modified
    {
        return Err("first-execution evidence changed while read".into());
    }
    Ok(Some(bytes))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn only_an_admitted_ran_witness_releases_a_marked_artifact() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = root.path();
        assert!(
            hold(dir).unwrap().is_none(),
            "genuine missing exec evidence"
        );
        let exec = dir.join(aterm_uds::deadline::EXEC_STAMP);
        let ran = dir.join(aterm_uds::deadline::RAN);
        std::fs::write(&exec, "0").unwrap();
        std::fs::write(&ran, "").unwrap();
        assert!(
            hold(dir).unwrap().is_none(),
            "ordinary empty witness is supported"
        );
        std::fs::remove_file(&ran).unwrap();

        let target = dir.join("other-witness");
        std::fs::write(&target, "unchanged").unwrap();
        std::os::unix::fs::symlink(&target, &ran).unwrap();
        assert!(
            hold(dir).is_err(),
            "an alias cannot supply witness identity"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
        std::fs::remove_file(&ran).unwrap();
        std::fs::hard_link(&target, &ran).unwrap();
        assert!(hold(dir).is_err(), "a multiply-linked witness is unknown");
        std::fs::remove_file(&ran).unwrap();
        std::fs::create_dir(&ran).unwrap();
        assert!(hold(dir).is_err(), "a directory cannot be a witness");
        std::fs::remove_dir(&ran).unwrap();
        std::fs::write(&exec, b"\xff").unwrap();
        assert!(hold(dir).is_err(), "invalid UTF-8 is unknown");
        std::fs::write(&exec, vec![b'1'; 65]).unwrap();
        assert!(hold(dir).is_err(), "oversized clock evidence is unknown");
        std::fs::write(&exec, "+1").unwrap();
        assert!(hold(dir).is_err(), "signed clock evidence is unknown");
    }
}
