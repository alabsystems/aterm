// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_metric_is_its_whole_field_and_a_latency_needs_its_decimal_point() {
    assert_eq!(
        metric_u64("OK backend=gpu frames=17 present_drops=0", "frames"),
        Some(17)
    );
    assert_eq!(
        metric_u64("OK max_frames=9", "frames"),
        None,
        "max_frames is not frames"
    );
    assert_eq!(metric_u64("OK frames=0", "frames"), Some(0));
    assert_eq!(metric_u64("OK frames=", "frames"), None);
    assert_eq!(metric_u64("OK", "frames"), None);
    assert_eq!(metric_u64("OK frames=3 frames=9", "frames"), Some(9));
    let m = "OK frames=31 max_input_present_ms=12.480 sync_rel_timeout=0 ";
    assert_eq!(metric_ms_whole(m, "max_input_present_ms"), Some(12));
    assert_eq!(
        metric_ms_whole("OK max_input_present_ms=530 ", "max_input_present_ms"),
        None,
        "no decimal point is an unparseable reply, not a latency finding"
    );
    assert_eq!(
        metric_ms_whole("OK max_input_present_ms=.5 ", "max_input_present_ms"),
        None
    );
    assert_eq!(
        metric_ms_whole("OK other=1.0", "max_input_present_ms"),
        None
    );
}

/// `script` under `/bin/sh` as a [`SmokeChild`], and the first line it printed.
fn smoke_child(script: &str) -> (SmokeChild, String) {
    use std::io::BufRead as _;
    let mut child = SmokeChild::spawn(
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
        |_| {},
    )
    .expect("spawn");
    let mut line = String::new();
    std::io::BufReader::new(child.child.stdout.take().expect("piped stdout"))
        .read_line(&mut line)
        .expect("read the child's word");
    (child, line)
}

#[test]
fn retiring_a_live_child_terms_it_and_reaps_exactly_once() {
    let (child, ready) = smoke_child("echo up; exec sleep 30");
    assert_eq!(ready, "up\n");
    let pid = i32::try_from(child.id()).expect("pid");
    assert!(
        child.retire(|_| {}, |_| {}),
        "a TERM we sent is a clean retirement"
    );
    assert_eq!(
        aterm_uds::process::process_presence(pid),
        aterm_uds::process::ProcessPresence::Absent,
        "the leader is reaped"
    );
}

#[test]
fn a_child_that_ignores_term_is_killed_within_the_budget() {
    let (child, ready) = smoke_child("trap '' TERM; echo ignoring; exec sleep 120");
    assert_eq!(ready, "ignoring\n", "the child set its trap");
    let start = std::time::Instant::now();
    assert!(
        child.retire(|_| {}, |_| {}),
        "KILL after the budget is still a retirement we caused"
    );
    assert!(
        start.elapsed() < GROUP_GONE,
        "the run is never hung by teardown: {:?}",
        start.elapsed()
    );
}

/// The subject's `atpkg machine` read, started as the smoke's window went, outlived it in the
/// window's group. Here the helper ignores `SIGTERM` and sleeps on, so it is still there,
/// held, when the instance dies: only the teardown can end it. The kernel's answer about the
/// whole group ([`crate::exec::group::empty`]) reads it not empty while its leader is
/// unreaped, and, on Apple, whose teardown waits until the group lists nothing but the leader,
/// empty once the leader is reaped (on Linux, whose listing skips a zombie, the helper may
/// still be one then, so that answer is only read); on every host it reads empty once the
/// helper is gone too. NEGATIVE CONTROL: answer `Some(false)` for `ESRCH` in
/// [`crate::exec::group::empty`], and the emptied group reads not empty.
#[test]
fn retiring_the_instance_ends_the_helper_it_left_in_its_group() {
    let (child, line) = smoke_child(
        "(trap '' TERM; exec /bin/sh -c 'echo $$; exec /bin/sleep 600') & exec /bin/sleep 600",
    );
    let helper: i32 = line.trim().parse().expect("the helper's pid");
    let leader = i32::try_from(child.id()).expect("pid");
    let (mut swept, mut reaped) = (None, None);
    assert!(
        child.retire(
            |pgid| {
                swept = Some((
                    aterm_uds::process::group_members(pgid),
                    crate::exec::group::empty(pgid),
                ));
            },
            |pgid| reaped = Some(crate::exec::group::empty(pgid)),
        ),
        "a TERM we sent is a clean retirement"
    );
    let (listed, unreaped) = swept.expect("the sweep ran");
    assert_eq!(unreaped, Some(false), "the unreaped leader keeps its group");
    let reaped = reaped.expect("the reap was told");
    if cfg!(target_vendor = "apple") {
        assert_eq!(
            listed.map(|m| m.pids),
            Some(vec![leader]),
            "the sweep finds the unreaped leader alone in its group"
        );
        assert_eq!(reaped, Some(true), "reaped, the group holds no one");
    } else {
        assert!(reaped.is_some(), "the kernel answers about the group");
    }
    assert!(
        matches!(
            aterm_uds::process::process_presence(helper),
            aterm_uds::process::ProcessPresence::Absent
                | aterm_uds::process::ProcessPresence::Exited
        ),
        "the helper (pid {helper}) outlived the smoke's instance"
    );
    // Its new parent reaps the ended helper; then no process holds the group's id.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the ended helper's reap",
        || {
            (aterm_uds::process::process_presence(helper)
                == aterm_uds::process::ProcessPresence::Absent)
                .then_some(())
        },
    );
    assert_eq!(
        crate::exec::group::empty(u32::try_from(leader).expect("a pid")),
        Some(true),
        "with its leader reaped and its helper gone, the group holds no one"
    );
}
