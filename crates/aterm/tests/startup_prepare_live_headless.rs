// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance: A REAL BOOT SPLITS ITS GUI PREPARATION AT EVERY
//! CUT (CS-2 of the 2026-10-15 launch audit).
//!
//! `metrics` cuts GUI entry → ready for winit into ten exclusive phases at the
//! boot's own stamps (`startup_prepare_*_ms`), valid only when every cut was
//! reached in boot order, and reads the kernel's birth of the process → Rust
//! `main` (`process_birth_to_rust_main_ms`). The unit tests drive both
//! derivations with invented readings; only a real boot shows that
//! `main_entry` reaches every cut, in order, before winit runs, and that the
//! birth record and the Rust-main stamp share a clock. A cut moved into a
//! branch this boot skips, or below the cut after it, reads
//! `startup_prepare_valid=0` here.
//!
//! NEGATIVE CONTROL: with the `Journal` stamp deleted from `main_entry`, this
//! test failed on `startup_prepare_valid=0`.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, every automatic
//! lane off, `--no-reroute`, `SHELL=/bin/sh` and a lifeline
//! (`support/launch_isolation.rs`). A headless instance opens no window.

#![cfg(unix)]

use crate::headless_boot;

use headless_boot::{ctl_ok, field};

/// The ten phases, in boot order, as `metrics` names them.
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
    // `metrics` is answered on the control thread, which listens before winit
    // runs; `panes` is answered by the event loop, which runs only once ready
    // for winit is stamped. So after `panes`, every cut a boot reaches is in.
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
    // Each of the ten phases and the parent is rounded to 0.01 ms. This is
    // their quantization allowance, not a budget for how long startup takes.
    assert!(
        (sum - parent).abs() <= 0.06,
        "preparation phases {sum}ms do not reconcile with parent {parent}ms: {reply}"
    );
    // Preparation does not require a presentation and cannot validate one.
    assert_eq!(field(&reply, "frames"), Some("0"), "{reply}");
    assert_eq!(field(&reply, "startup_phase_valid"), Some("0"), "{reply}");
    assert_eq!(ms("first_present_ms"), 0.0, "{reply}");
    let birth = field(&reply, "process_birth_to_rust_main_ms")
        .unwrap_or_else(|| panic!("metrics names no process_birth_to_rust_main_ms=: {reply}"));
    if cfg!(target_vendor = "apple") {
        // The shipped binary stamps Rust main, and the kernel keeps this
        // process's birth on the same wall clock: a reading, never `none`.
        let ms: f64 = birth
            .parse()
            .unwrap_or_else(|_| panic!("process_birth_to_rust_main_ms={birth}: {reply}"));
        assert!(ms >= 0.0, "process_birth_to_rust_main_ms={ms}: {reply}");
    } else {
        assert_eq!(birth, "none", "{reply}");
    }
}
