// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unix seconds of a written time: an RFC 3339 UTC stamp, as transcripts carry, and a
//! `ps -o lstart=` process start.

/// Unix seconds of an RFC 3339 UTC stamp (`2026-09-24T04:18:02.796Z`, as transcripts carry), via [`aterm_types::rfc3339::parse_utc_fractional`].
#[must_use]
pub fn parse_utc(s: &str) -> Option<u64> {
    u64::try_from(aterm_types::rfc3339::parse_utc_fractional(s)?).ok()
}

/// Unix seconds of a `ps -o lstart=` stamp in UTC, C locale — the session file's `procStart`.
#[must_use]
pub fn parse_lstart(s: &str) -> Option<u64> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_and_lstart_stamps_parse() {
        assert_eq!(parse_utc("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_utc("2026-09-24T04:18:02.796Z"), Some(1_790_223_482));
        assert_eq!(parse_utc("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_utc("garbage"), None);
        assert_eq!(parse_utc("2026-09-24T04:18:02+05:30"), None);
        assert_eq!(parse_utc("2026-09-24T04:18:02.796"), None);
        assert_eq!(parse_utc("2026-09-24T04:18:02Z"), Some(1_790_223_482));
        assert_eq!(
            parse_lstart("Thu Sep 24 23:21:50 2026"),
            parse_utc("2026-09-24T23:21:50Z")
        );
        assert_eq!(
            parse_lstart("Thu Sep  4 03:01:00 2026"),
            parse_utc("2026-09-04T03:01:00Z")
        );
        assert_eq!(parse_lstart("garbage"), None);
    }
}
