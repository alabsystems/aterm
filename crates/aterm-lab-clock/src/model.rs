// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Platform-free clock offset arithmetic and record formatting.

use core::fmt;

const NS: i128 = 1_000_000_000;
const US: i128 = 1_000_000;

/// `CLOCK_REALTIME`.
pub(crate) const CLOCK_REALTIME: u32 = 0;
/// `CLOCK_MONOTONIC_RAW`.
pub(crate) const CLOCK_MONOTONIC_RAW: u32 = 4;
/// `CLOCK_MONOTONIC_RAW_APPROX`.
pub(crate) const CLOCK_MONOTONIC_RAW_APPROX: u32 = 5;
/// `CLOCK_MONOTONIC`.
pub(crate) const CLOCK_MONOTONIC: u32 = 6;
/// `CLOCK_UPTIME_RAW`.
#[cfg(test)]
pub(crate) const CLOCK_UPTIME_RAW: u32 = 8;
/// `CLOCK_UPTIME_RAW_APPROX`.
#[cfg(test)]
pub(crate) const CLOCK_UPTIME_RAW_APPROX: u32 = 9;

/// The clock ids the record counts reads of (`clockid_t` values below this).
pub(crate) const NCLOCK: usize = 32;

/// Whether a read of `clock` is shifted.
pub(crate) const fn shifted(clock: u32, continuous: bool) -> bool {
    match clock {
        CLOCK_REALTIME => true,
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_RAW_APPROX => continuous,
        _ => false,
    }
}

/// The offset file's text read.
pub(crate) fn parse_offset(text: &[u8]) -> Option<i64> {
    let text = text.trim_ascii();
    let (negative, digits) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, text),
    };
    let (whole, frac) = match digits.iter().position(|&b| b == b'.') {
        Some(at) => {
            let (whole, dot_frac) = digits.split_at(at);
            (whole, dot_frac.get(1..).unwrap_or_default())
        }
        None => (digits, &[][..]),
    };
    if (whole.is_empty() && frac.is_empty())
        || frac.len() > 9
        || !whole.iter().chain(frac).all(u8::is_ascii_digit)
    {
        return None;
    }
    let mut ns: i64 = 0;
    for &d in whole {
        ns = ns.checked_mul(10)?.checked_add(i64::from(d - b'0'))?;
    }
    ns = ns.checked_mul(1_000_000_000)?;
    let mut part: i64 = 0;
    for place in 0..9 {
        let d = frac.get(place).map_or(0, |&d| i64::from(d - b'0'));
        part = part * 10 + d;
    }
    ns = ns.checked_add(part)?;
    Some(if negative { -ns } else { ns })
}

fn clamp(value: i128) -> i64 {
    i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX })
}

/// A `timespec` (`sec`, `nsec`) shifted by `off` nanoseconds, normalized (`0 <= nsec < 1e9`).
pub(crate) fn shift_timespec(sec: i64, nsec: i64, off: i64) -> (i64, i64) {
    let total = i128::from(sec) * NS + i128::from(nsec) + i128::from(off);
    (clamp(total.div_euclid(NS)), clamp(total.rem_euclid(NS)))
}

/// A `timeval` (`sec`, `usec`) shifted by `off` nanoseconds, normalized (`0 <= usec < 1e6`).
pub(crate) fn shift_timeval(sec: i64, usec: i64, off: i64) -> (i64, i64) {
    let total = i128::from(sec) * US + i128::from(usec) + i128::from(off / 1000);
    (clamp(total.div_euclid(US)), clamp(total.rem_euclid(US)))
}

/// A `clock_gettime_nsec_np` answer shifted by `off` nanoseconds.
pub(crate) const fn shift_nsec(value: u64, off: i64) -> u64 {
    if value == 0 {
        0
    } else {
        value.saturating_add_signed(off)
    }
}

/// `ns` nanoseconds in Mach absolute-time ticks of the timebase `numer/denom`.
pub(crate) fn ns_to_ticks(ns: i64, numer: u32, denom: u32) -> i64 {
    if numer == 0 || denom == 0 {
        return 0;
    }
    clamp(i128::from(ns) * i128::from(denom) / i128::from(numer))
}

/// A Mach tick count (`mach_continuous_time`) shifted by `off` nanoseconds.
pub(crate) fn shift_ticks(value: u64, off: i64, numer: u32, denom: u32) -> u64 {
    value.saturating_add_signed(ns_to_ticks(off, numer, denom))
}

/// A text of at most `N` bytes on the stack.
pub(crate) struct Text<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Text<N> {
    /// An empty text.
    pub(crate) const fn new() -> Self {
        Text {
            buf: [0; N],
            len: 0,
        }
    }

    /// What it holds.
    pub(crate) fn bytes(&self) -> &[u8] {
        self.buf.get(..self.len).unwrap_or_default()
    }

    /// Append `bytes`, or nothing when they do not fit.
    /// # Errors
    /// They do not fit.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> fmt::Result {
        let end = self.len.checked_add(bytes.len()).ok_or(fmt::Error)?;
        self.buf
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
}

impl<const N: usize> fmt::Write for Text<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push(s.as_bytes())
    }
}

/// What a process's `<pid>.txt` says.
pub(crate) struct Record<'a> {
    pub(crate) pid: u32,
    pub(crate) ppid: u32,
    pub(crate) exe: &'a str,
    pub(crate) offset_ns: i64,
    pub(crate) continuous: bool,
    pub(crate) file_reads: u64,
    pub(crate) gettimeofday: u64,
    pub(crate) time: u64,
    pub(crate) continuous_time: u64,
    pub(crate) continuous_approximate_time: u64,
    pub(crate) clock_gettime: [u64; NCLOCK],
    pub(crate) clock_gettime_nsec_np: [u64; NCLOCK],
}

/// The record's text.
/// # Errors
/// It does not fit in `out`.
pub(crate) fn render<const N: usize>(r: &Record<'_>, out: &mut Text<N>) -> fmt::Result {
    use fmt::Write as _;
    writeln!(out, "pid={} ppid={} exe={}", r.pid, r.ppid, r.exe)?;
    writeln!(
        out,
        "offset_ns={} continuous={} file_reads={}",
        r.offset_ns,
        u8::from(r.continuous),
        r.file_reads
    )?;
    writeln!(
        out,
        "gettimeofday={} time={} mach_continuous_time={} mach_continuous_approximate_time={}",
        r.gettimeofday, r.time, r.continuous_time, r.continuous_approximate_time
    )?;
    for (clock, (a, b)) in r
        .clock_gettime
        .iter()
        .zip(r.clock_gettime_nsec_np.iter())
        .enumerate()
    {
        if *a != 0 || *b != 0 {
            writeln!(
                out,
                "clock {clock}: clock_gettime={a} clock_gettime_nsec_np={b}"
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
