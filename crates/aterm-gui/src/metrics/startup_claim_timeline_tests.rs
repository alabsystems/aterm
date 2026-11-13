// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ClaimStamp, ClaimTimelineInputs, StartupAttachMilestones, claim_timeline_points,
    claim_worker_vs_join,
};
use std::time::{Duration, Instant};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn every_admitted_transport_records_milestones_without_waiting_for_proof() {
    for rendezvous in [false, true] {
        let startup = super::Startup::new();
        let mut observed = Vec::new();
        startup.stamp_claim_with(ClaimStamp::Warmed, &mut |s| observed.push(s));
        assert!(observed.is_empty(), "ordinary boot has no handoff records");
        if rendezvous {
            startup.claim_at.set(Instant::now()).unwrap();
        }
        startup.begin_handoff_diagnostics_with(&mut |s| observed.push(s));
        startup.begin_handoff_diagnostics_with(&mut |s| observed.push(s));
        startup.stamp_claim_with(ClaimStamp::Intake, &mut |s| observed.push(s));
        startup.stamp_claim_with(ClaimStamp::Intake, &mut |s| observed.push(s));
        startup.stamp_claim_with(ClaimStamp::Control, &mut |s| observed.push(s));
        assert_eq!(
            observed.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            ["warmed", "intake", "control"]
        );
        assert!(
            startup.claim_stamps[ClaimStamp::Proof.index()]
                .get()
                .is_none(),
            "progress survives without ever reaching the success-only timeline"
        );
    }
}

#[test]
fn activation_and_a_worker_milestone_cannot_duplicate_a_journal_record() {
    let startup = super::Startup::new();
    let observed = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        scope.spawn(|| {
            startup
                .begin_handoff_diagnostics_with(&mut |s| observed.lock().unwrap().push(s.as_str()))
        });
        scope.spawn(|| {
            startup.stamp_claim_with(ClaimStamp::Control, &mut |s| {
                observed.lock().unwrap().push(s.as_str())
            })
        });
    });
    assert_eq!(*observed.lock().unwrap(), ["control"]);
}

fn a_boot() -> ClaimTimelineInputs {
    let exec = Instant::now();
    let dial = exec + ms(31);
    let claim = dial + ms(5);
    let mut own = [None; ClaimStamp::COUNT];
    own[ClaimStamp::Intake.index()] = Some(claim + ms(12));
    own[ClaimStamp::Journal.index()] = Some(claim + ms(48));
    own[ClaimStamp::Adopt0.index()] = Some(claim + ms(60));
    own[ClaimStamp::LoopBuild.index()] = Some(claim + ms(66));
    own[ClaimStamp::EventLoopBuilt.index()] = Some(claim + ms(80));
    own[ClaimStamp::Services.index()] = Some(claim + ms(99));
    own[ClaimStamp::Control.index()] = Some(claim + ms(103));
    own[ClaimStamp::ConfigJoined.index()] = Some(claim + ms(104));
    own[ClaimStamp::AppBuilt.index()] = Some(claim + ms(105));
    own[ClaimStamp::FirstPresent.index()] = Some(claim + ms(203));
    own[ClaimStamp::RestoreStart.index()] = Some(claim + ms(204));
    own[ClaimStamp::RestoreDrained.index()] = Some(claim + ms(219));
    own[ClaimStamp::Proof.index()] = Some(claim + ms(223));
    own[ClaimStamp::Commit.index()] = Some(claim + ms(256));
    let attach = [110, 115, 135, 140, 141, 160, 170].map(|n| claim + ms(n));
    ClaimTimelineInputs {
        claim,
        exec: Some(exec),
        warm: None,
        dial: Some(dial),
        park: Some(claim - ms(2)),
        own,
        worker_spawn: Some(claim + ms(32)),
        worker_done: Some(claim + ms(90)),
        run_app: Some(claim + ms(106)),
        resumed: Some(claim + ms(108)),
        attach: Some(StartupAttachMilestones::new(attach)),
        surface_ready: Some(claim + ms(172)),
        visible: Some(claim + ms(205)),
    }
}

#[test]
fn every_boundary_is_signed_ms_after_the_claim_in_boot_order() {
    let points = claim_timeline_points(&a_boot());
    let labels: Vec<_> = points.iter().map(|(label, _)| *label).collect();
    assert_eq!(
        labels,
        [
            "main",
            "dial",
            "park",
            "intake",
            "worker_spawn",
            "journal",
            "adopt0",
            "loop_build",
            "event_loop",
            "services",
            "control",
            "config_joined",
            "app_built",
            "run_app",
            "resumed",
            "attach",
            "window_create",
            "window_created",
            "join",
            "worker_done",
            "joined",
            "surface_create",
            "surface_created",
            "surface_ready",
            "first_present",
            "visible",
            "restore",
            "drained",
            "proof",
            "commit",
        ]
    );
    let at = |label: &str| points.iter().find(|(l, _)| *l == label).unwrap().1;
    assert_eq!(at("main"), -36_000);
    assert_eq!(at("dial"), -5_000);
    assert_eq!(at("intake"), 12_000);
    assert_eq!(at("surface_create"), 160_000);
    assert_eq!(at("surface_created"), 170_000);
    assert_eq!(at("proof"), 223_000);
    assert_eq!(at("commit") - at("proof"), 33_000);
    assert_eq!(at("park"), -2_000);
    assert_eq!(at("proof") - at("park"), 225_000, "park→proof");
}

#[test]
fn a_boundary_never_reached_is_left_out_not_printed_as_zero() {
    let mut boot = a_boot();
    boot.own[ClaimStamp::RestoreStart.index()] = None;
    boot.own[ClaimStamp::RestoreDrained.index()] = None;
    boot.attach = None;
    boot.park = None;
    let points = claim_timeline_points(&boot);
    for gone in [
        "park",
        "restore",
        "drained",
        "attach",
        "join",
        "joined",
        "surface_created",
    ] {
        assert!(points.iter().all(|(l, _)| *l != gone), "{gone}");
    }
    assert!(claim_worker_vs_join(&boot).is_none());
}

#[test]
fn the_worker_is_judged_against_the_join_on_both_sides() {
    let boot = a_boot();
    assert_eq!(claim_worker_vs_join(&boot), Some((1_000, 50_000)));
    let mut late = boot;
    late.worker_done = Some(boot.claim + ms(141));
    assert_eq!(claim_worker_vs_join(&late), Some((1_000, -1_000)));
}
