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
mod tests {
    use super::*;

    #[test]
    fn a_step_moves_the_wall_and_under_the_switch_the_sleep_clocks_and_never_uptime() {
        for continuous in [false, true] {
            assert!(shifted(CLOCK_REALTIME, continuous));
            for c in [
                CLOCK_MONOTONIC,
                CLOCK_MONOTONIC_RAW,
                CLOCK_MONOTONIC_RAW_APPROX,
            ] {
                assert_eq!(shifted(c, continuous), continuous, "clock {c}");
            }
            for c in [CLOCK_UPTIME_RAW, CLOCK_UPTIME_RAW_APPROX, 12, 16, 31, 99] {
                assert!(!shifted(c, continuous), "clock {c}");
            }
        }
    }

    #[test]
    fn the_offset_file_is_decimal_seconds() {
        assert_eq!(parse_offset(b"0\n"), Some(0));
        assert_eq!(parse_offset(b"1800\n"), Some(1_800_000_000_000));
        assert_eq!(parse_offset(b"  1800.250 "), Some(1_800_250_000_000));
        assert_eq!(parse_offset(b"1800.0"), Some(1_800_000_000_000));
        assert_eq!(parse_offset(b".5"), Some(500_000_000));
        assert_eq!(parse_offset(b"2."), Some(2_000_000_000));
        assert_eq!(parse_offset(b"-3"), Some(-3_000_000_000));
        assert_eq!(parse_offset(b"0.000000001"), Some(1));
        for bad in [
            &b""[..],
            b"\n",
            b".",
            b"-",
            b"1e3",
            b"12x",
            b"1.2.3",
            b"+5",
            b"0.0000000001",
            b"99999999999999999999",
            b"1 2",
        ] {
            assert_eq!(
                parse_offset(bad),
                None,
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn a_reading_is_shifted_and_normalized_in_each_unit() {
        assert_eq!(
            shift_timespec(1_000, 900_000_000, 86_400_000_000_000),
            (87_400, 900_000_000)
        );
        assert_eq!(
            shift_timespec(1_000, 900_000_000, 200_000_000),
            (1_001, 100_000_000)
        );
        assert_eq!(
            shift_timespec(1_000, 100_000_000, -200_000_000),
            (999, 900_000_000)
        );
        assert_eq!(shift_timespec(5, 0, 0), (5, 0));
        assert_eq!(shift_timespec(i64::MAX, 0, i64::MAX).0, i64::MAX);
        assert_eq!(shift_timeval(1_000, 999_999, 1_000), (1_001, 0));
        assert_eq!(shift_timeval(1_000, 0, 1_800_000_000_000), (2_800, 0));
        assert_eq!(shift_timeval(1_000, 0, -1_500), (999, 999_999));
        assert_eq!(shift_timeval(1_000, 5, 999), (1_000, 5));
        assert_eq!(shift_nsec(10, 5), 15);
        assert_eq!(shift_nsec(10, -20), 0);
        assert_eq!(shift_nsec(0, 1_000), 0);
        assert_eq!(shift_nsec(u64::MAX - 1, 5), u64::MAX);
    }

    #[test]
    fn nanoseconds_become_mach_ticks_of_the_timebase() {
        assert_eq!(ns_to_ticks(1_000_000_000, 125, 3), 24_000_000);
        assert_eq!(ns_to_ticks(1_800_000_000_000, 125, 3), 43_200_000_000);
        assert_eq!(ns_to_ticks(-1_000_000_000, 125, 3), -24_000_000);
        assert_eq!(ns_to_ticks(1_000_000_000, 1, 1), 1_000_000_000);
        assert_eq!(ns_to_ticks(5, 0, 3), 0);
        assert_eq!(shift_ticks(100, 1_000_000_000, 125, 3), 24_000_100);
        assert_eq!(shift_ticks(100, -1_000_000_000, 125, 3), 0);
    }

    #[test]
    fn a_text_takes_what_fits_and_refuses_the_rest() {
        use fmt::Write as _;
        let mut t: Text<8> = Text::new();
        assert!(write!(t, "abc{}", 12).is_ok());
        assert_eq!(t.bytes(), b"abc12");
        assert!(t.push(b"xyz").is_ok());
        assert!(t.push(b"!").is_err(), "nine bytes in eight");
        assert_eq!(t.bytes(), b"abc12xyz");
    }

    #[test]
    fn the_record_names_the_process_its_offset_and_the_clocks_it_read() {
        let mut cg = [0; NCLOCK];
        let mut np = [0; NCLOCK];
        cg[0] = 11_698;
        cg[8] = 124_638;
        np[6] = 89;
        let r = Record {
            pid: 95_226,
            ppid: 1,
            exe: "/lab/bin dir/aterm",
            offset_ns: 1_800_000_000_000,
            continuous: true,
            file_reads: 2,
            gettimeofday: 0,
            time: 3,
            continuous_time: 19,
            continuous_approximate_time: 0,
            clock_gettime: cg,
            clock_gettime_nsec_np: np,
        };
        let mut out: Text<4096> = Text::new();
        render(&r, &mut out).unwrap();
        assert_eq!(
            String::from_utf8_lossy(out.bytes()),
            "pid=95226 ppid=1 exe=/lab/bin dir/aterm\n\
             offset_ns=1800000000000 continuous=1 file_reads=2\n\
             gettimeofday=0 time=3 mach_continuous_time=19 mach_continuous_approximate_time=0\n\
             clock 0: clock_gettime=11698 clock_gettime_nsec_np=0\n\
             clock 6: clock_gettime=0 clock_gettime_nsec_np=89\n\
             clock 8: clock_gettime=124638 clock_gettime_nsec_np=0\n"
        );
        let mut small: Text<16> = Text::new();
        assert!(render(&r, &mut small).is_err());
    }
}
