// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
