// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-attempt attribution, separate from the advisory quota snapshot. One private
//! JSON-lines file per UTC hour on this machine retains concurrent processes' rows.
//! Rows contain no request headers, query strings or bodies. A response's rate counter
//! is a shared-identity observation, so its delta must never be charged to this caller.

use aterm_json::Value;

use super::RateBook;
use crate::error::{Error, Result};

/// Maximum bytes retained in one machine-hour. Crossing it reports incomplete
/// attribution at the caller; it never suppresses a GitHub operation.
#[cfg(unix)]
const HOUR_MAX: usize = 64 * 1024 * 1024;

impl RateBook {
    /// Append one attempt's attribution. The caller reports any failure, including
    /// the explicit 64 MiB/hour storage bound, separately from its GitHub result.
    pub(crate) fn record_call(&self, now: u64, row: &Value) -> Result<()> {
        #[cfg(unix)]
        {
            use aterm_uds::private_file::{
                PrivateLock, create_private_dir, open_private_rw, write_private,
            };
            use std::io::{Seek, SeekFrom, Write};
            use std::time::Duration;

            let Some(root) = &self.dir else { return Ok(()) };
            // The rate book normally creates its parents while recording a snapshot.
            // An answer without rate headers still deserves an attribution row.
            if let Some(parent) = root.parent() {
                if let Some(grand) = parent.parent() {
                    std::fs::create_dir_all(grand).map_err(local)?;
                }
                create_private_dir(parent).map_err(local)?;
            }
            create_private_dir(root).map_err(local)?;
            let dir = root.join("calls");
            create_private_dir(&dir).map_err(local)?;
            let hour = now / 3600;
            let _lock = PrivateLock::take(
                &dir,
                &format!("{hour}.lock"),
                Duration::from_secs(60),
                "rate attribution hour is still locked after 60 seconds",
            )
            .map_err(local)?;
            let mut file = open_private_rw(&dir, &format!("{hour}.jsonl")).map_err(local)?;
            let mut bytes = aterm_json::to_vec(row).map_err(local)?;
            bytes.push(b'\n');
            let len = file.metadata().map_err(local)?.len();
            if len.saturating_add(bytes.len() as u64) > HOUR_MAX as u64 {
                write_private(
                    &dir,
                    &format!("{hour}.incomplete"),
                    b"machine-hour storage bound reached",
                )
                .map_err(local)?;
                return Err(Error::local(format!(
                    "rate attribution exceeds the {HOUR_MAX}-byte machine-hour bound at {}",
                    dir.join(format!("{hour}.jsonl")).display(),
                )));
            }
            file.seek(SeekFrom::End(0)).map_err(local)?;
            if let Err(error) = file.write_all(&bytes) {
                file.set_len(len).map_err(local)?;
                return Err(local(error));
            }
        }
        #[cfg(not(unix))]
        let _ = (now, row);
        Ok(())
    }

    /// Read every attempt recorded in one UTC hour (`Unix seconds / 3600`).
    /// The files remain on their originating machine until explicitly removed.
    /// # Errors
    /// Unreadable, oversized or incomplete attribution, or an unavailable rate book.
    pub fn calls_in_hour(&self, hour: u64) -> Result<Vec<Value>> {
        #[cfg(unix)]
        {
            use aterm_uds::private_file::{PrivateFileError, PrivateLock, read_private};
            let root = self
                .dir
                .as_ref()
                .ok_or_else(|| Error::local("rate attribution has no configured directory"))?;
            let dir = root.join("calls");
            let absent = || {
                Error::could_not_run(format!(
                    "no rate attribution was recorded for UTC hour {hour}"
                ))
            };
            if !dir.try_exists().map_err(local)? {
                return Err(absent());
            }
            let _lock = PrivateLock::take(
                &dir,
                &format!("{hour}.lock"),
                std::time::Duration::from_secs(60),
                "rate attribution hour is still locked after 60 seconds",
            )
            .map_err(local)?;
            match read_private(&dir, &format!("{hour}.incomplete"), 4096) {
                Err(PrivateFileError::Missing) => {}
                Ok(_) => {
                    return Err(Error::could_not_run(format!(
                        "rate attribution for UTC hour {hour} is incomplete; its storage bound was reached"
                    )));
                }
                Err(error) => return Err(local(error)),
            }
            let bytes = match read_private(&dir, &format!("{hour}.jsonl"), HOUR_MAX) {
                Ok(bytes) => bytes,
                Err(PrivateFileError::Missing) => return Err(absent()),
                Err(error) => return Err(local(error)),
            };
            if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
                return Err(Error::local(
                    "rate attribution has an incomplete final record",
                ));
            }
            bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| aterm_json::from_slice(line).map_err(local))
                .collect()
        }
        #[cfg(not(unix))]
        {
            let _ = hour;
            Err(Error::local("rate attribution requires Unix"))
        }
    }
}

#[cfg(unix)]
fn local(error: impl std::fmt::Display) -> Error {
    Error::local(format!("rate attribution: {error}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn concurrent_rows_are_complete_and_storage_bound_is_explicit() {
        let root = crate::test_fixtures::scratch_dir("rate-usage");
        let book = RateBook::at(root.path().join("rate"));
        std::thread::scope(|scope| {
            for caller in 0..8 {
                let book = &book;
                scope.spawn(move || {
                    for at in 0..16 {
                        book.record_call(at, &aterm_json::json!({"caller": caller, "at": at}))
                            .unwrap();
                    }
                });
            }
        });
        let rows = book.calls_in_hour(0).unwrap();
        assert_eq!(rows.len(), 128);
        let distinct: std::collections::BTreeSet<_> = rows
            .iter()
            .map(|row| (row["caller"].as_u64().unwrap(), row["at"].as_u64().unwrap()))
            .collect();
        assert_eq!(distinct.len(), 128);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("rate/calls/0.jsonl"))
            .unwrap();
        file.set_len(HOUR_MAX as u64).unwrap();
        let error = book
            .record_call(0, &aterm_json::json!({"at": 0}))
            .unwrap_err();
        assert!(error.why.contains("67108864-byte machine-hour bound"));
        assert!(
            book.calls_in_hour(0)
                .unwrap_err()
                .why
                .contains("incomplete")
        );
        // A full hour leaves the next hour writable.
        book.record_call(3600, &aterm_json::json!({"at": 3600}))
            .unwrap();
        assert_eq!(book.calls_in_hour(1).unwrap().len(), 1);
    }
}
