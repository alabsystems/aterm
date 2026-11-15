// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's start stamps (ATERM-KI-1111-2) on the command itself: the fabric bench only
//! reports a split, so a broker that stopped stamping, or stamped a phase out of its order, would
//! leave the bench's split empty and pass. This holds the line's presence and order; it reads no
//! time.

use std::path::Path;

use aterm_link::start_stamps::{ORDER, Stamps};

use crate::harness;

/// **`aterm link broker` STAMPS EVERY PHASE OF ITS START, IN ORDER.** Started as a launcher starts
/// it, through `aterm`'s main, it writes its stamps' line on its stderr before its start line:
/// every phase of [`ORDER`] is on it, and each is no earlier than the one before it, the first no
/// earlier than this test's read of the same clock before the spawn and the last no later than its
/// read once it had the `listening` line.
#[test]
fn the_broker_command_stamps_every_phase_of_its_start_in_order() {
    let spawned = aterm_uds::process::continuous_ns().expect("the machine's continuous clock");
    let dir = aterm_tempfile::Builder::new()
        .prefix("atl-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch dir");
    let said = |dir: &Path| std::fs::read_to_string(dir.join("broker.err")).unwrap_or_default();
    let err = std::fs::File::create(dir.path().join("broker.err")).expect("broker.err");
    let sock = dir.path().join("b.sock").to_string_lossy().into_owned();
    let log = dir.path().join("b.log").to_string_lossy().into_owned();
    let mut broker = harness::spawn_broker(dir.path(), &[&sock, &log], |cmd| {
        cmd.stdout(aterm_uds::spawnfd::Stdio::piped()).stderr(err);
    });
    let first = harness::first_line_within(&mut broker.child, "its readiness line");
    let seen = aterm_uds::process::continuous_ns();
    assert_eq!(
        first.trim_end(),
        format!("listening {sock}"),
        "{}",
        said(dir.path())
    );
    // The stamps' line is written before the start line, so a whole start line means a whole
    // stamps' line.
    let stderr = harness::until("the broker's whole start line", || {
        let text = said(dir.path());
        text.split_inclusive('\n')
            .any(|line| {
                line.starts_with("aterm link broker: unix socket on") && line.ends_with('\n')
            })
            .then_some(text)
    });
    harness::kill(i32::try_from(broker.child.id()).expect("pid"), 15);
    harness::until("the broker to stop", || {
        broker.child.try_wait().ok().flatten()
    });
    let stamps = Stamps::find(&stderr).unwrap_or_else(|| panic!("no stamps' line: {stderr}"));
    let missing: Vec<&str> = ORDER
        .into_iter()
        .filter(|phase| stamps.at(phase).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "phases not stamped: {missing:?} ({})",
        stamps.line()
    );
    assert!(
        stamps.steps(spawned, seen).is_some(),
        "stamps out of the start's order, or outside spawn {spawned} .. seen {seen:?}: {}",
        stamps.line()
    );
}
