// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual original-child native SETEXEC, not an environment policy receipt.
use super::*;
use std::io::Write as _;
use std::time::Duration;

const PAYLOAD: &str = "spawnfd::footprint_tests::native_footprint_payload";
const ROLE: &str = "ATERM_TEST_NATIVE_FOOTPRINT_ROLE";

#[test]
fn native_footprint_payload() {
    let Some(role) = std::env::var_os(ROLE) else {
        return;
    };
    let identity = || {
        let pid = std::process::id();
        let birth = crate::process::read_birth(pid as i32).unwrap().0;
        println!("native-footprint-identity {pid} {birth}");
        std::io::stdout().flush().unwrap();
    };
    identity();
    if role == "plain-exec" || role == "limited-exec" {
        let mut next = command(std::env::current_exe().unwrap());
        next.args(["--exact", PAYLOAD, "--nocapture"])
            .env_clear()
            .env(ROLE, "allocate");
        if role == "limited-exec" {
            next.footprint_limit(FootprintLimit::new(64).unwrap());
        }
        let error = next.exec();
        panic!("actual native final exec failed: {error}");
    }
    assert_eq!(role, "allocate");
    // Always finite, including a broken/missing setter: this cannot grow into
    // an unbounded machine-wide OOM exercise.
    let mut pages = vec![0_u8; 256 * 1024 * 1024];
    for page in pages.chunks_mut(4096) {
        page[0] = 1;
    }
    std::hint::black_box(&pages);
    std::thread::sleep(Duration::from_millis(250));
}

#[test]
fn footprint_policy_applies_at_final_setexec() {
    if !crate::test_binary::isolated_lifecycle(
        "spawnfd::footprint_tests::footprint_policy_applies_at_final_setexec",
    ) {
        return;
    }
    let run = |role: &str| {
        let mut initial = command(std::env::current_exe().unwrap());
        initial
            .args(["--exact", PAYLOAD, "--nocapture"])
            .env_clear()
            .env(ROLE, role);
        assert!(
            initial.get_footprint_limit().is_none(),
            "only final SETEXEC receives policy"
        );
        crate::deadline::output_while_process_progressing(
            &mut initial,
            Duration::from_secs(60),
            format!("native final footprint {role}"),
        )
        .unwrap()
    };
    let baseline = run("plain-exec");
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    let identities = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .filter(|line| line.starts_with("native-footprint-identity "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let baseline_ids = identities(&baseline.stdout);
    assert_eq!(baseline_ids.len(), 2);
    assert_eq!(
        baseline_ids[0], baseline_ids[1],
        "SETEXEC preserves original PID/birth"
    );
    let limited = run("limited-exec");
    assert_eq!(
        limited.status.signal(),
        Some(libc::SIGKILL),
        "limited actual child: {:?}\n{}",
        limited.status,
        String::from_utf8_lossy(&limited.stderr)
    );
    let limited_ids = identities(&limited.stdout);
    assert_eq!(
        limited_ids.len(),
        2,
        "final selected image actually entered before allocation"
    );
    assert_eq!(limited_ids[0], limited_ids[1]);
}
