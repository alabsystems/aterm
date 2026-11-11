// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const S: Duration = Duration::from_secs(1);

fn job(id: u32, unit: &str, elapsed_s: u64) -> Job {
    Job {
        id,
        unit: unit.into(),
        kind: "start".into(),
        state: "running".into(),
        elapsed_ms: elapsed_s * 1000,
    }
}

#[test]
fn before_the_agent_only_a_new_console_line_is_progress() {
    let w = Watch::new(S * 10);
    assert_eq!(w.check(S * 69, None), None);
    assert_eq!(w.check(S * 70, None), Some(Fired::BootBeforeAgent));
    // A console line at 40 s moves the span; a line older than the start does not.
    assert_eq!(w.check(S * 99, Some(S * 40)), None);
    assert_eq!(w.check(S * 100, Some(S * 40)), Some(Fired::BootBeforeAgent));
    assert_eq!(w.check(S * 70, Some(S)), Some(Fired::BootBeforeAgent));
}

#[test]
fn the_same_jobs_for_the_span_fire_it_while_console_lines_keep_arriving() {
    let mut w = Watch::new(Duration::ZERO);
    w.hello(S * 2);
    let wait = job(117, "systemd-networkd-wait-online.service", 0);
    for t in (2..=62).step_by(5) {
        let mut j = wait.clone();
        j.elapsed_ms = (t - 2) * 1000;
        w.booting(&[j], S * u32::try_from(t).unwrap());
    }
    // Console lines at every second count for nothing once the agent speaks.
    assert_eq!(w.check(S * 61, Some(S * 61)), None);
    let fired = w.check(S * 62, Some(S * 62)).expect("fired");
    let Fired::BootAfterAgent { jobs } = &fired else {
        panic!("{fired:?}")
    };
    assert_eq!(jobs[0].unit, "systemd-networkd-wait-online.service");
    let why = fired.why();
    assert!(
        why.starts_with("NOT RUN — the boot hung after the agent")
            && why.contains("systemd-networkd-wait-online.service start running (60 s)")
            && why.ends_with("VM stopped"),
        "{why}"
    );
}

#[test]
fn the_negative_control_a_job_set_that_changes_within_each_span_never_fires() {
    let mut w = Watch::new(Duration::ZERO);
    w.hello(Duration::ZERO);
    for t in 0..600u32 {
        // A different job every 50 s: each span sees a change.
        let id = t / 50;
        w.booting(&[job(id, &format!("unit-{id}.service"), 0)], S * t);
        assert_eq!(w.check(S * t, None), None, "at {t} s");
    }
}

#[test]
fn an_agent_that_goes_quiet_after_hello_fires_the_boot_detector() {
    let mut w = Watch::new(Duration::ZERO);
    w.hello(S * 5);
    assert_eq!(w.check(S * 64, None), None);
    assert_eq!(
        w.check(S * 65, None),
        Some(Fired::BootAfterAgent { jobs: Vec::new() })
    );
    assert!(
        w.check(S * 65, None)
            .unwrap()
            .why()
            .contains("no start job")
    );
}

#[test]
fn after_ready_only_a_missing_beat_fires() {
    let mut w = Watch::new(Duration::ZERO);
    w.hello(S);
    w.ready(S * 30);
    assert_eq!(w.check(S * 89, None), None);
    for t in (35..300).step_by(5) {
        w.beat(S * t);
        assert_eq!(w.check(S * (t + 4), None), None);
    }
    assert_eq!(w.check(S * (295 + 59), None), None);
    assert_eq!(w.check(S * (295 + 60), None), Some(Fired::Heartbeat));
    assert_eq!(
        Fired::Heartbeat.why(),
        "the guest stopped answering (no heartbeat for 60 s); VM stopped"
    );
}

#[test]
fn free_space_is_read_every_tick_and_stops_below_the_run_floor() {
    let mut s = Space::new(Duration::ZERO);
    assert!(s.due(Duration::ZERO));
    assert!(!s.due(S * 4));
    assert!(s.due(S * 5));
    let free = |bytes| crate::space::Free {
        bytes,
        volume: "/System/Volumes/Data (container:disk3)".into(),
    };
    assert_eq!(s.read(free(12 << 30)), None);
    assert_eq!(s.last_bytes(), Some(12 << 30));
    assert_eq!(
        s.read(free(3 << 30)).as_deref(),
        Some("the volume holding aterm-linux.img has 3.0 GiB free; VM stopped")
    );
}
