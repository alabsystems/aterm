// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unix seconds of a written time: an RFC 3339 UTC stamp, as transcripts carry, and a process
//! start as a snapshot and the session file's `procStart` spell it.

/// Unix seconds of an RFC 3339 UTC stamp (`2026-09-24T04:18:02.796Z`, as transcripts carry), via [`aterm_types::rfc3339::parse_utc_fractional`].
#[must_use]
pub fn parse_utc(s: &str) -> Option<u64> {
    u64::try_from(aterm_types::rfc3339::parse_utc_fractional(s)?).ok()
}

/// Unix seconds of a process start as the snapshot and the session file's `procStart` spell it
/// (`upgrade_drive::kernel_start`): a `ps -o lstart=` stamp in UTC, C locale; on Windows the
/// decimal count of its creation `FILETIME`, its second floored as
/// `aterm_uds::process::read_process_birth` floors it.
#[must_use]
pub fn parse_lstart(s: &str) -> Option<u64> {
    #[cfg(windows)]
    if let Some(ticks) = crate::parse_uint(s) {
        return filetime_seconds(ticks);
    }
    let w: Vec<&str> = s.split_whitespace().collect();
    let [_, mon, day, hms, year] = w.as_slice() else {
        return None;
    };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let m = MONTHS.iter().position(|x| x == mon)? + 1;
    let day: u32 = crate::parse_uint(day)?;
    let year: u32 = crate::parse_uint(year)?;
    parse_utc(&format!("{year:04}-{m:02}-{day:02}T{hms}Z"))
}

/// Unix seconds of a `FILETIME`, 100 ns ticks since 1601-01-01 UTC; `None` before 1970.
#[cfg(any(windows, test))]
const fn filetime_seconds(ticks: u64) -> Option<u64> {
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    match ticks.checked_sub(UNIX_EPOCH_TICKS) {
        Some(since) => Some(since / 10_000_000),
        None => None,
    }
}

#[cfg(test)]
#[path = "unix_time_tests.rs"]
mod tests;
