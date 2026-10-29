// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's judged wait ([`crate::harness::recv_judged`]) and its fixture config
//! ([`crate::harness::prepare_fixture_config`]), checked; mounted only by the ordinary
//! fabric_world test target.

use std::time::{Duration, Instant};

use crate::harness::recv_judged;

/// A wait judged at 50 ms (the timeout branch under test, not a budget) ends COULD NOT RUN naming
/// the pid when the reader says the child is held, fails naming what never came otherwise, and
/// returns an item that came.
#[test]
fn a_wait_that_runs_out_on_a_held_child_ends_could_not_run_and_any_other_silence_fails() {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let pid = 4242;
    let hang = Duration::from_millis(50);
    let ended = |held: Option<bool>| {
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            recv_judged(
                &rx,
                Instant::now(),
                hang,
                pid,
                |p| held.filter(|_| p == pid),
                || "the child's readiness line".to_owned(),
            )
        }));
        let why = out.expect_err("nothing came");
        why.downcast_ref::<String>().cloned().unwrap_or_default()
    };
    let held = ended(Some(true));
    assert!(
        held.starts_with(aterm_uds::host::COULD_NOT_RUN) && held.contains("(pid 4242)"),
        "expected COULD NOT RUN, got: {held}"
    );
    for not in [Some(false), None] {
        let failed = ended(not);
        assert!(
            failed.starts_with("the child's readiness line"),
            "{not:?}: {failed}"
        );
    }
    tx.send("listening".into()).expect("the channel is open");
    assert_eq!(
        recv_judged(&rx, Instant::now(), hang, pid, |_| Some(true), String::new),
        "listening"
    );
}

/// THE FABRIC WORLDS DECLINE INSTALLS, so a spawn in one runs the world's stand-in and never
/// starts a download of the agent CLI: the config every fixture seeds
/// ([`crate::harness::prepare_fixture_config`]) says no to unattended installs by atpkg's own
/// reading ([`atpkg::adoption::unattended_install_off`]), which a spawn's install obeys.
#[test]
fn the_fixture_config_declines_installs() {
    let root = aterm_tempfile::Builder::new()
        .prefix("atl-consent-")
        .tempdir()
        .expect("scratch root");
    crate::harness::prepare_fixture_config(root.path());
    let written = std::fs::read_to_string(root.path().join("cfg/aterm/aterm.toml"))
        .expect("the fixture config");
    assert_eq!(written, crate::harness::FIXTURE_GUI_CONFIG);
    let store = atpkg::store::Layout {
        prefix: root.path().join("pkg"),
    };
    assert_eq!(
        atpkg::adoption::unattended_install_off(&store, &atpkg::config::parse_packages(&written)),
        Some(atpkg::adoption::UPDATES_OFF),
        "{written}"
    );
}
