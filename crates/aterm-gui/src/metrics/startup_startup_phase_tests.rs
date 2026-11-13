// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn timeline() -> (StartupMilestones, StartupPresentTiming, Instant) {
    let rust_main = Instant::now();
    let gui_entry = rust_main + std::time::Duration::from_millis(1);
    let gui_ready = gui_entry + std::time::Duration::from_millis(2);
    let winit_resumed = gui_ready + std::time::Duration::from_millis(3);
    let surface_ready = winit_resumed + std::time::Duration::from_millis(4);
    let frame_started = surface_ready + std::time::Duration::from_millis(5);
    let pre_present = frame_started + std::time::Duration::from_millis(6);
    let surface_return = pre_present + std::time::Duration::from_millis(7);
    let published_at = surface_return + std::time::Duration::from_millis(8);
    (
        StartupMilestones {
            rust_main: Some(rust_main),
            gui_entry: Some(gui_entry),
            gui_ready: Some(gui_ready),
            winit_resumed: Some(winit_resumed),
            surface_ready: Some(surface_ready),
        },
        StartupPresentTiming::new(frame_started, pre_present, surface_return),
        published_at,
    )
}

fn attach_timeline() -> (Instant, StartupAttachMilestones, Instant) {
    let winit_resumed = Instant::now();
    let points = std::array::from_fn(|index| {
        winit_resumed + std::time::Duration::from_millis((index + 1) as u64)
    });
    let surface_ready = winit_resumed + std::time::Duration::from_millis(8);
    (
        winit_resumed,
        StartupAttachMilestones::new(points),
        surface_ready,
    )
}

#[test]
fn startup_phases_are_an_exact_exclusive_partition() {
    let (milestones, timing, published_at) = timeline();
    let phases = derive_startup_phases(milestones, timing, published_at);
    assert!(phases.valid);
    assert_eq!(
        [
            phases.router_ns,
            phases.gui_prepare_ns,
            phases.winit_dispatch_ns,
            phases.initial_surface_attach_ns,
            phases.surface_to_successful_redraw_ns,
            phases.successful_compose_ns,
            phases.successful_surface_transaction_ns,
            phases.successful_finalize_ns,
        ],
        [
            1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000, 6_000_000, 7_000_000, 8_000_000,
        ]
    );
    assert_eq!(phases.total_ns(), Some(36_000_000));
    assert_eq!(
        phases
            .total_ns()
            .and_then(|total| total.checked_sub(phases.router_ns)),
        Some(35_000_000)
    );
}

#[test]
fn startup_attach_is_an_exact_exclusive_parent_partition() {
    let (winit_resumed, milestones, surface_ready) = attach_timeline();
    let attach = derive_startup_attach(Some(winit_resumed), Some(milestones), Some(surface_ready));
    assert!(attach.valid);
    assert_eq!(
        [
            attach.dispatch_ns,
            attach.prepare_ns,
            attach.window_create_ns,
            attach.window_setup_ns,
            attach.backend_finalize_ns,
            attach.chrome_geometry_ns,
            attach.surface_create_ns,
            attach.finish_ns,
        ],
        [1_000_000; 8]
    );
    assert_eq!(attach.total_ns(), Some(8_000_000));
    assert_eq!(
        attach.total_ns(),
        duration_ns(winit_resumed, surface_ready),
        "the drill-down must equal its initial-surface-attach parent"
    );
}

#[test]
fn startup_attach_fails_closed_on_missing_reordered_or_overflowing_data() {
    let (winit_resumed, milestones, surface_ready) = attach_timeline();
    assert_eq!(
        derive_startup_attach(Some(winit_resumed), None, Some(surface_ready)),
        StartupAttachSample::default()
    );
    let mut reversed = milestones;
    reversed.points.swap(3, 4);
    assert_eq!(
        derive_startup_attach(Some(winit_resumed), Some(reversed), Some(surface_ready)),
        StartupAttachSample::default()
    );
    let overflow = StartupAttachSample {
        valid: true,
        dispatch_ns: u64::MAX,
        finish_ns: 1,
        ..StartupAttachSample::default()
    };
    assert_eq!(overflow.total_ns(), None);
}

#[test]
fn phase_derivation_conforms_to_the_derived_publication_gate() {
    let model = aterm_models_core::startup_phase_publication_model();
    let (milestones, timing, published_at) = timeline();
    let mut incomplete_milestones = milestones;
    incomplete_milestones.surface_ready = None;

    let mut state = model.init_state();
    for _ in 0..7 {
        assert!(model.fire("Step", &mut state));
    }
    let host_valid = derive_startup_phases(incomplete_milestones, timing, published_at).valid;
    assert_eq!(
        !model.successors("Publish", &state).is_empty(),
        host_valid,
        "Tier-1: the real missing-milestone decision matches phase 7"
    );

    assert!(model.fire("Step", &mut state));
    let host_valid = derive_startup_phases(milestones, timing, published_at).valid;
    assert_eq!(
        !model.successors("Publish", &state).is_empty(),
        host_valid,
        "Tier-1: the complete real partition matches phase 8"
    );
    assert!(model.fire("Publish", &mut state));

    let reversed = StartupPresentTiming::new(
        timing.pre_present,
        timing.frame_started,
        timing.surface_return,
    );
    assert!(
        !derive_startup_phases(milestones, reversed, published_at).valid,
        "negative control: real out-of-order stamps must fail closed"
    );

    let attach_model = aterm_models_core::startup_phase_publication_model();
    let (winit_resumed, attach_milestones, surface_ready) = attach_timeline();
    let mut attach_state = attach_model.init_state();
    for _ in 0..7 {
        assert!(attach_model.fire("Step", &mut attach_state));
    }
    let incomplete_attach =
        derive_startup_attach(Some(winit_resumed), Some(attach_milestones), None).valid;
    assert_eq!(
        !attach_model.successors("Publish", &attach_state).is_empty(),
        incomplete_attach,
        "Tier-1: the missing surface-ready boundary matches attach phase 7"
    );
    assert!(attach_model.fire("Step", &mut attach_state));
    let complete_attach = derive_startup_attach(
        Some(winit_resumed),
        Some(attach_milestones),
        Some(surface_ready),
    )
    .valid;
    assert_eq!(
        !attach_model.successors("Publish", &attach_state).is_empty(),
        complete_attach,
        "Tier-1: the complete attach partition matches phase 8"
    );
    let mut reversed_attach = attach_milestones;
    reversed_attach.points.swap(1, 2);
    assert!(
        !derive_startup_attach(
            Some(winit_resumed),
            Some(reversed_attach),
            Some(surface_ready),
        )
        .valid,
        "negative control: an out-of-order attach stamp must fail closed"
    );
}
