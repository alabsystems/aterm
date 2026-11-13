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
const HOUR_MAX: usize = aterm_ctl::github_usage::HOUR_MAX;

impl RateBook {
    /// Append one attempt's attribution. The caller reports any failure, including
    /// the explicit per-hour storage bound, separately from its GitHub result.
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
                "calls.lock",
                Duration::from_secs(60),
                "rate attribution hour is still locked after 60 seconds",
            )
            .map_err(local)?;
            // One lock covers append and retention across processes. Keep exactly the
            // current UTC hour and its preceding 23 hours, including failure markers.
            for entry in std::fs::read_dir(&dir).map_err(local)? {
                let entry = entry.map_err(local)?;
                let name = entry.file_name();
                let Some((stem, ext)) = name.to_str().and_then(|s| s.split_once('.')) else {
                    continue;
                };
                if matches!(ext, "jsonl" | "incomplete" | "lock")
                    && let Ok(old) = stem.parse::<u64>()
                    && (old < hour.saturating_sub(23) || old > hour)
                {
                    std::fs::remove_file(entry.path()).map_err(local)?;
                }
            }
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
    /// The last 24 UTC hours remain on their originating machine.
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
                "calls.lock",
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
#[path = "usage_tests.rs"]
mod tests;
