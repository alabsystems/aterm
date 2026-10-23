// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Paint verdict tests over synthetic scan reports.

use super::*;
use crate::measure::scan::{Cell, DrivenWindow, Shape};

const NOMINAL: i64 = 20_000;

fn row(expect: Expect, capture: Capture, style: Style) -> PaintRow {
    PaintRow {
        scene: Scene::FakeClaude,
        keys: "a,b,c",
        expect,
        capture,
        style,
        companion: None,
        split: None,
    }
}

fn lit_burst() -> ScanReport {
    ScanReport {
        frames: 67,
        cell: Cell::new(7, 14).expect("cell"),
        best_ink: 2_500,
        best_hues: 11,
        total_ink: 100_000,
        union_hues: 11,
        cyan_px: 0,
        cyan_frames: 0,
        cyan_total: 0,
        cyan_hue: None,
        rainbow_frames: 65,
        rainbow_run: 65,
        rainbow_gap: 0,
        driven: DrivenWindow::NOT_MEASURED,
        status_frames: 67,
        ribbon_claimed: 40,
        ribbon_bound: 30,
        ribbon_window_hues: 10,
        ribbon_dark: 0,
        pet_claimed: 0,
        pet_missing: 0,
        pet_final_run: 0,
        cat_claimed: 0,
        cat_missing: 0,
        cat_gap: 0,
        cat_last_present: false,
        cat_final_run: 0,
        companion_frames: 0,
        companion_stack: 0,
        companion_fused: 0,
        base_comp: Shape::NONE,
        per_frame: Vec::new(),
    }
}

fn lit_video() -> ScanReport {
    ScanReport {
        status_frames: 0,
        ribbon_claimed: 0,
        ribbon_bound: 0,
        ribbon_window_hues: 0,
        driven: DrivenWindow {
            dark_us: 0,
            frames: 200,
            sampling_hole_us: NOMINAL,
            nominal_dt_us: NOMINAL,
            mature_lead_us: 130_000,
        },
        ..lit_burst()
    }
}

fn status(style: &str, extra: &str) -> StatusRow {
    StatusRow::new(format!(
        "trail style=\"{style}\" resolved=rainbow-kitty effective=true ribbon_look=tall {extra}"
    ))
}

fn flying_rows(frames: usize, extra: &str) -> Ledger {
    Ledger {
        rows: Some(vec![status("rainbow kitty flying", extra); frames]),
        final_row: None,
        fx_under_frames: 40,
    }
}

const QUIET: &str = "licensed=0 spawns=0 ribbon_active=false sparks=0 glow_active=false \
                     pet_active=false cat_active=false";

fn verdict(row: &PaintRow, ledger: &Ledger, scan: &ScanReport) -> ShapeVerdict {
    judge(row, ledger, scan).expect("decided")
}

#[test]
fn a_healthy_burst_is_ink_and_quiet_is_its_exact_negation() {
    let (ink, quiet) = (
        row(Expect::Ink, Capture::UnpinnedFocused, Style::Flying),
        row(Expect::Quiet, Capture::UnpinnedFocused, Style::Flying),
    );
    let ledger = flying_rows(67, "licensed=9 spawns=9");
    assert_eq!(verdict(&ink, &ledger, &lit_burst()), ShapeVerdict::Pass);
    assert_eq!(verdict(&quiet, &ledger, &lit_burst()), ShapeVerdict::Fail);
    let one_hue = ScanReport {
        union_hues: 1,
        ..lit_burst()
    };
    assert_eq!(verdict(&ink, &ledger, &one_hue), ShapeVerdict::Fail);
    assert_eq!(verdict(&quiet, &ledger, &one_hue), ShapeVerdict::Pass);
}

#[test]
fn the_coverage_floor_binds_ink_but_never_buys_the_quiet_control() {
    let uncovered = ScanReport {
        ribbon_bound: 0,
        ..lit_burst()
    };
    let ledger = flying_rows(67, "");
    let ink = row(Expect::Ink, Capture::UnpinnedFocused, Style::Flying);
    let quiet = row(Expect::Quiet, Capture::UnpinnedFocused, Style::Flying);
    assert_eq!(verdict(&ink, &ledger, &uncovered), ShapeVerdict::Fail);
    assert_eq!(verdict(&quiet, &ledger, &uncovered), ShapeVerdict::Fail);
}

#[test]
fn ink_needs_the_compose_side_witness() {
    let ink = row(Expect::Ink, Capture::UnpinnedFocused, Style::Flying);
    let no_witness = Ledger {
        fx_under_frames: 1,
        ..flying_rows(67, "")
    };
    assert_eq!(verdict(&ink, &no_witness, &lit_burst()), ShapeVerdict::Fail);
}

#[test]
fn a_driven_claim_over_an_unlit_raster_is_red() {
    let ink = row(Expect::Ink, Capture::UnpinnedFocused, Style::Flying);
    let dark = ScanReport {
        ribbon_dark: 2,
        ..lit_burst()
    };
    assert_eq!(
        verdict(&ink, &flying_rows(67, ""), &dark),
        ShapeVerdict::Fail
    );
    let mono_arc = ScanReport {
        ribbon_window_hues: 3,
        ..lit_burst()
    };
    assert_eq!(
        verdict(&ink, &flying_rows(67, ""), &mono_arc),
        ShapeVerdict::Fail
    );
}

#[test]
fn recorded_continuity_tolerates_one_dark_frame_and_refuses_two() {
    let ink = row(Expect::Ink, Capture::PinnedVideo, Style::Flying);
    let ledger = Ledger {
        rows: None,
        final_row: Some(status("rainbow kitty flying", "")),
        fx_under_frames: 40,
    };
    for (dark_us, want) in [
        (NOMINAL, ShapeVerdict::Pass),
        (2 * NOMINAL, ShapeVerdict::Fail),
    ] {
        let mut scan = lit_video();
        scan.driven.dark_us = dark_us;
        assert_eq!(verdict(&ink, &ledger, &scan), want, "{dark_us}");
    }
    let mut dead_start = lit_video();
    dead_start.driven.mature_lead_us = 3_000_000;
    assert_eq!(verdict(&ink, &ledger, &dead_start), ShapeVerdict::Fail);
}

#[test]
fn a_recorded_take_that_lost_its_bracket_decides_nothing() {
    let ink = row(Expect::Ink, Capture::PinnedVideo, Style::Flying);
    let ledger = flying_rows(67, "");
    let mut lost = lit_video();
    lost.driven = DrivenWindow::NOT_MEASURED;
    assert!(judge(&ink, &ledger, &lost).is_err());
    let burst = row(Expect::Ink, Capture::UnpinnedFocused, Style::Flying);
    let gapped = ScanReport {
        rainbow_gap: 2,
        ..lit_burst()
    };
    assert_eq!(verdict(&burst, &ledger, &gapped), ShapeVerdict::Fail);
}

#[test]
fn a_near_empty_take_is_never_a_dark_pass() {
    let dark = row(Expect::Dark, Capture::UnpinnedFocused, Style::Flying);
    let empty = ScanReport {
        frames: 2,
        total_ink: 0,
        ..lit_burst()
    };
    assert!(judge(&dark, &flying_rows(2, QUIET), &empty).is_err());
}

#[test]
fn trail_dark_reads_the_engine_ledger_and_refuses_without_one() {
    let cold = row(Expect::TrailDark, Capture::UnpinnedFocused, Style::Flying);
    let scan = ScanReport {
        total_ink: 400,
        ribbon_claimed: 0,
        ..lit_burst()
    };
    assert_eq!(
        verdict(&cold, &flying_rows(67, QUIET), &scan),
        ShapeVerdict::Pass
    );
    let licensed = flying_rows(67, &QUIET.replace("licensed=0", "licensed=1"));
    assert_eq!(verdict(&cold, &licensed, &scan), ShapeVerdict::Fail);
    let blind = Ledger {
        rows: None,
        final_row: None,
        fx_under_frames: 0,
    };
    assert_eq!(verdict(&cold, &blind, &scan), ShapeVerdict::Fail);
}

#[test]
fn effect_off_needs_typed_pixels_a_quiet_ledger_and_no_witness() {
    let off = row(Expect::EffectOff, Capture::UnpinnedFocused, Style::Off);
    let scan = ScanReport {
        total_ink: 2_744,
        rainbow_frames: 0,
        ribbon_claimed: 0,
        ..lit_burst()
    };
    let rows = |extra: &str| Ledger {
        rows: Some(vec![
            StatusRow::new(format!(
                "trail style=\"off\" effective=false {extra}"
            ));
            67
        ]),
        final_row: None,
        fx_under_frames: 0,
    };
    assert_eq!(verdict(&off, &rows(QUIET), &scan), ShapeVerdict::Pass);
    let ribbon = ScanReport {
        rainbow_frames: 1,
        ..scan.clone()
    };
    assert_eq!(verdict(&off, &rows(QUIET), &ribbon), ShapeVerdict::Fail);
    let witnessed = Ledger {
        fx_under_frames: 3,
        ..rows(QUIET)
    };
    assert_eq!(verdict(&off, &witnessed, &scan), ShapeVerdict::Fail);
    let nothing_typed = ScanReport {
        total_ink: 0,
        ..scan
    };
    assert_eq!(
        verdict(&off, &rows(QUIET), &nothing_typed),
        ShapeVerdict::Fail
    );
}

#[test]
fn a_recorded_effect_off_window_must_read_dead() {
    let off = row(Expect::EffectOff, Capture::PinnedVideo, Style::Off);
    let ledger = Ledger {
        rows: None,
        final_row: Some(StatusRow::new(format!(
            "trail style=\"off\" effective=false {QUIET}"
        ))),
        fx_under_frames: 0,
    };
    let mut scan = ScanReport {
        rainbow_frames: 0,
        ..lit_video()
    };
    scan.driven.dark_us = 1_400_000;
    assert_eq!(verdict(&off, &ledger, &scan), ShapeVerdict::Pass);
    scan.driven.dark_us = NOMINAL;
    assert_eq!(verdict(&off, &ledger, &scan), ShapeVerdict::Fail);
}

#[test]
fn a_spelling_that_resolved_elsewhere_is_red() {
    let tall = row(Expect::Ink, Capture::UnpinnedFocused, Style::Tall);
    let resolved = |resolved: &str| Ledger {
        rows: Some(vec![
            StatusRow::new(format!(
                "trail style=\"rainbow kitty tall\" resolved={resolved} ribbon_look=tall"
            ));
            67
        ]),
        final_row: None,
        fx_under_frames: 40,
    };
    assert_eq!(
        verdict(&tall, &resolved("rainbow-kitty"), &lit_burst()),
        ShapeVerdict::Pass
    );
    assert_eq!(
        verdict(&tall, &resolved("lumen"), &lit_burst()),
        ShapeVerdict::Fail
    );
    assert_eq!(
        verdict(
            &tall,
            &resolved("rainbow-kitty"),
            &ScanReport {
                frames: 68,
                ..lit_burst()
            }
        ),
        ShapeVerdict::Fail
    );
}

#[test]
fn companion_obligations_ride_on_top_of_ink() {
    let pet = PaintRow {
        scene: Scene::Prompt,
        companion: Some(Companion::Pet),
        ..row(Expect::Ink, Capture::UnpinnedFocused, Style::Default)
    };
    let ledger = Ledger {
        rows: Some(vec![status("rainbow kitty pet", ""); 36]),
        final_row: None,
        fx_under_frames: 40,
    };
    let healthy = ScanReport {
        frames: 36,
        status_frames: 36,
        pet_claimed: 36,
        pet_final_run: 3,
        base_comp: Shape::new(215, 24, 14),
        ..lit_burst()
    };
    assert_eq!(verdict(&pet, &ledger, &healthy), ShapeVerdict::Pass);
    for broken in [
        ScanReport {
            companion_stack: 1,
            ..healthy.clone()
        },
        ScanReport {
            cat_claimed: 1,
            ..healthy.clone()
        },
        ScanReport {
            base_comp: Shape::NONE,
            ..healthy.clone()
        },
        ScanReport {
            pet_final_run: 2,
            ..healthy.clone()
        },
    ] {
        assert_eq!(verdict(&pet, &ledger, &broken), ShapeVerdict::Fail);
    }
    let cat = PaintRow {
        companion: Some(Companion::Cat),
        style: Style::Flying,
        ..pet
    };
    let earned = ScanReport {
        frames: 36,
        status_frames: 36,
        cat_claimed: 4,
        companion_frames: 3,
        cat_final_run: 3,
        cat_last_present: true,
        ..lit_burst()
    };
    let flying = Ledger {
        rows: Some(vec![status("rainbow kitty flying", ""); 36]),
        ..ledger
    };
    assert_eq!(verdict(&cat, &flying, &earned), ShapeVerdict::Pass);
    assert_eq!(
        verdict(
            &cat,
            &flying,
            &ScanReport {
                cat_last_present: false,
                ..earned.clone()
            }
        ),
        ShapeVerdict::Fail
    );
    assert_eq!(
        verdict(
            &cat,
            &flying,
            &ScanReport {
                pet_claimed: 1,
                ..earned
            }
        ),
        ShapeVerdict::Fail
    );
}

#[test]
fn self_contradicting_rows_are_refused() {
    let off_without_keys = PaintRow {
        keys: "",
        ..row(Expect::EffectOff, Capture::UnpinnedFocused, Style::Off)
    };
    assert!(off_without_keys.validate().is_err());
    let video_companion = PaintRow {
        scene: Scene::Prompt,
        companion: Some(Companion::Pet),
        ..row(Expect::Ink, Capture::PinnedVideo, Style::Default)
    };
    assert!(video_companion.validate().is_err());
    let cat_wrong_style = PaintRow {
        scene: Scene::Prompt,
        companion: Some(Companion::Cat),
        ..row(Expect::Ink, Capture::UnpinnedFocused, Style::Classic)
    };
    assert!(cat_wrong_style.validate().is_err());
}
