// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn list_jobs_reads_each_job_and_skips_what_is_not_one() {
    let text = "\
117 systemd-networkd-wait-online.service start running
 98 cloud-final.service                  start waiting
No jobs running.
x not.a job line
+117 systemd-networkd-wait-online.service start running
";
    assert_eq!(
        parse_jobs(text),
        [
            RawJob {
                id: 117,
                unit: "systemd-networkd-wait-online.service".into(),
                kind: "start".into(),
                state: "running".into(),
            },
            RawJob {
                id: 98,
                unit: "cloud-final.service".into(),
                kind: "start".into(),
                state: "waiting".into(),
            },
        ]
    );
    assert_eq!(parse_jobs(""), []);
}

#[test]
fn failed_units_are_each_lines_unit() {
    let text = "fwupd.service loaded failed failed Firmware update daemon\n\
                snapd.seeded.service loaded failed failed Wait until snapd is fully seeded\n";
    assert_eq!(
        parse_failed(text),
        ["fwupd.service", "snapd.seeded.service"]
    );
    assert_eq!(parse_failed("\n"), Vec::<String>::new());
}

#[test]
fn uptime_reads_seconds_to_milliseconds() {
    assert_eq!(parse_uptime_ms("1.83 3.20\n"), Some(1830));
    assert_eq!(parse_uptime_ms("12345.6 1.0"), Some(12_345_600));
    assert_eq!(parse_uptime_ms("7 0"), Some(7000));
    assert_eq!(parse_uptime_ms("0.0049 0"), Some(4));
    assert_eq!(parse_uptime_ms("+7 0"), None);
    assert_eq!(parse_uptime_ms("1. 0"), None);
    assert_eq!(parse_uptime_ms(".5 0"), None);
    assert_eq!(parse_uptime_ms("1.234xyz 0"), None);
    assert_eq!(parse_uptime_ms("x"), None);
    assert_eq!(parse_uptime_ms(""), None);
}

#[test]
fn only_a_clock_past_the_slack_is_stepped() {
    let host = 1_792_000_000_000_000_000;
    assert!(!needs_step(host, host));
    assert!(!needs_step(host - CLOCK_SLACK_NS, host));
    assert!(!needs_step(host + CLOCK_SLACK_NS, host));
    assert!(needs_step(host - CLOCK_SLACK_NS - 1, host));
    assert!(needs_step(host + CLOCK_SLACK_NS + 1, host));
    // The guest's measured lag: about two minutes behind (no RTC).
    assert!(needs_step(host - 120_000_000_000, host));
}

#[test]
fn only_running_or_degraded_is_up() {
    let at = |state: &str| Booted {
        state: state.into(),
        failed_units: Vec::new(),
    };
    assert!(at("running").is_up());
    assert!(at("degraded").is_up());
    for other in [
        "starting",
        "maintenance",
        "stopping",
        "offline",
        "unknown",
        "",
    ] {
        assert!(!at(other).is_up(), "{other}");
    }
}
