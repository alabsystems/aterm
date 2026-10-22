// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wake-reason word on recorded reasons, and (macOS) the timeline held
//! to the live machine's clock and to `sysctl(8)`'s read of the same
//! records (its oracle, run here only).

use super::*;

#[test]
fn a_wake_reason_is_named_by_its_last_word() {
    // m22's, 2026-10-20 (`kern.wakereason`; RD §5.1 reads the trackpad).
    assert_eq!(
        wake_word("MTP.DOCK.CHANNELS.AP0.IRQ nub-spmi-a0.0x0B RTP.multi-touch").as_deref(),
        Some("RTP.multi-touch")
    );
    assert_eq!(wake_word("XHC1").as_deref(), Some("XHC1"));
    // A trailing word that is not one is passed over.
    assert_eq!(
        wake_word("EC.LidOpen (User)").as_deref(),
        Some("EC.LidOpen")
    );
}

/// The negative control: a reason with no word that is one names none.
#[test]
fn a_wake_reason_with_no_word_names_none() {
    assert_eq!(wake_word(""), None);
    assert_eq!(wake_word("   "), None);
    assert_eq!(wake_word("(User) a/b caf\u{e9}"), None);
    let long = "A".repeat(WAKE_WORD_CAP + 1);
    assert_eq!(wake_word(&long), None);
    assert_eq!(
        wake_word(&"A".repeat(WAKE_WORD_CAP)).map(|w| w.len()),
        Some(WAKE_WORD_CAP)
    );
}

#[test]
fn a_zero_time_is_none_since_boot() {
    assert_eq!(recorded(Some(0)), None);
    assert_eq!(recorded(Some(-1)), None);
    assert_eq!(recorded(None), None);
    assert_eq!(recorded(Some(1_792_517_313_580)), Some(1_792_517_313_580));
}

#[cfg(target_os = "macos")]
mod live {
    use super::*;

    /// `sysctl -n` of the four records the timeline reads: its lines. A
    /// sysctl that has not answered in a minute is hung.
    fn sysctl() -> Vec<String> {
        let out = aterm_uds::deadline::output_within(
            aterm_uds::spawnfd::command("/usr/sbin/sysctl").args([
                "-n",
                "kern.boottime",
                "kern.sleeptime",
                "kern.waketime",
                "kern.wakereason",
            ]),
            std::time::Duration::from_secs(60),
        )
        .expect("run sysctl");
        assert!(out.status.success(), "sysctl: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// A `struct timeval` as `sysctl -n` prints it (`{ sec = 1792512373,
    /// usec = 683868 } Tue Oct 20 09:06:13 2026`), unix milliseconds.
    fn timeval_ms(line: &str) -> i64 {
        let field = |key: &str| -> i64 {
            line.split_once(key)
                .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| panic!("{key:?} in {line:?}"))
        };
        field("{ sec = ") * 1000 + field(", usec = ") / 1000
    }

    #[test]
    fn the_timeline_is_the_kernels_record() {
        let before = sysctl();
        let t = timeline().expect("the kernel answers kern.boottime");
        let after = sysctl();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970")
            .as_millis();
        let now_ms = i64::try_from(now_ms).expect("fits");
        assert!(0 < t.boot_ms && t.boot_ms < now_ms, "{t:?} {now_ms}");
        for at in [t.sleep_ms, t.wake_ms].into_iter().flatten() {
            assert!(t.boot_ms <= at && at <= now_ms, "{t:?} {now_ms}");
        }
        // The same records on both sides: equal unless the machine slept,
        // woke or stepped its clock between the reads.
        if before == after {
            let [boot, slept, woke, ..] = before.as_slice() else {
                panic!("four records: {before:?}");
            };
            let want = Timeline {
                boot_ms: timeval_ms(boot),
                sleep_ms: recorded(Some(timeval_ms(slept))),
                wake_ms: recorded(Some(timeval_ms(woke))),
                wake_reason: before.get(3).map(String::as_str).and_then(wake_word),
            };
            assert_eq!(t, want, "{before:?}");
        }
        // `kern.waketime` only moves forward.
        if let Some(woke) = t.wake_ms {
            assert!(waketime_ms().is_some_and(|w| w >= woke), "{t:?}");
        }
    }
}
