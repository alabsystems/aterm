// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance: `main_entry` reaches all ten `startup_prepare_*_ms` cuts in boot
//! order before winit runs, and `process_birth_to_rust_main_ms` shares a clock with the kernel.

#![cfg(unix)]

use crate::headless_boot;

use headless_boot::{ctl_ok, field};

const PHASES: [&str; 10] = [
    "startup_prepare_boot_ms",
    "startup_prepare_warm_ms",
    "startup_prepare_journal_ms",
    "startup_prepare_session0_ms",
    "startup_prepare_event_loop_ms",
    "startup_prepare_services_ms",
    "startup_prepare_control_ms",
    "startup_prepare_config_join_ms",
    "startup_prepare_app_ms",
    "startup_prepare_arm_ms",
];

#[test]
fn a_headless_boot_reaches_every_preparation_cut_in_order() {
    let inst = headless_boot::boot_with("aterm-startup-prepare", |_, _| {}, |_| true);
    ctl_ok(&inst, &["panes"]);
    let reply = ctl_ok(&inst, &["metrics"]);
    assert_eq!(
        field(&reply, "startup_prepare_schema"),
        Some("1"),
        "{reply}"
    );
    assert_eq!(
        field(&reply, "startup_prepare_valid"),
        Some("1"),
        "a cut was skipped or reached out of boot order: {reply}"
    );
    let ms = |name| {
        let value: f64 = field(&reply, name)
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("metrics names no number {name}=: {reply}"));
        assert!(value.is_finite() && value >= 0.0, "{name}={value}: {reply}");
        value
    };
    let sum: f64 = PHASES.into_iter().map(ms).sum();
    let parent = ms("startup_gui_prepare_ms");
    assert!(
        parent > 0.0,
        "completed preparation must be measured: {reply}"
    );
    assert!(
        (sum - parent).abs() <= 0.06,
        "preparation phases {sum}ms do not reconcile with parent {parent}ms: {reply}"
    );
    assert_eq!(field(&reply, "frames"), Some("0"), "{reply}");
    assert_eq!(field(&reply, "startup_phase_valid"), Some("0"), "{reply}");
    assert_eq!(ms("first_present_ms"), 0.0, "{reply}");
    let birth = field(&reply, "process_birth_to_rust_main_ms")
        .unwrap_or_else(|| panic!("metrics names no process_birth_to_rust_main_ms=: {reply}"));
    if cfg!(target_vendor = "apple") {
        let ms: f64 = birth
            .parse()
            .unwrap_or_else(|_| panic!("process_birth_to_rust_main_ms={birth}: {reply}"));
        assert!(ms >= 0.0, "process_birth_to_rust_main_ms={ms}: {reply}");
    } else {
        assert_eq!(birth, "none", "{reply}");
    }
}
