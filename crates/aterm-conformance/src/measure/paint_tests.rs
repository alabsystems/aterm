// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Paint verdict tests over synthetic scan reports.

use super::*;
use crate::measure::scan::{Cell, DrivenWindow, FrameReport, Shape};

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
        reach: None,
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
        companion_height_frames: 0,
        companion_height_px: None,
        companion_min_h: None,
        companion_max_h: None,
        base_comp: Shape::NONE,
        typed_row: None,
        reach_frames: 0,
        reach_px: None,
        reach_min_px: None,
        reach_max_px: None,
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
        companion_height_frames: 4,
        companion_height_px: Some(15),
        companion_min_h: Some(14),
        companion_max_h: Some(15),
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
        ScanReport {
            pet_missing: 1,
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
        companion_height_frames: 3,
        companion_height_px: Some(16),
        companion_min_h: Some(16),
        companion_max_h: Some(16),
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
fn a_missing_pet_is_named_by_its_frame_and_claimed_pose() {
    let frame = |missing: bool| FrameReport {
        name: String::new(),
        metrics: Default::default(),
        companion: if missing {
            Shape::NONE
        } else {
            Shape::new(305, 33, 19)
        },
        duplicate: false,
        notes: if missing {
            vec![FrameNote::CompanionMissing]
        } else {
            Vec::new()
        },
    };
    let scan = ScanReport {
        per_frame: vec![frame(false), frame(true), frame(true), frame(true)],
        ..lit_burst()
    };
    let ledger = Ledger {
        rows: Some(vec![
            status("rainbow kitty pet", "pet_active=true pet_pose=pet_sleep_1"),
            status("rainbow kitty pet", "pet_active=true pet_pose=pet_hop"),
            status("rainbow kitty flying", "pet_active=false cat_active=true"),
            status("rainbow kitty pet", "pet_active=true"),
        ]),
        final_row: None,
        fx_under_frames: 40,
    };
    assert_eq!(pet_missing_at(&ledger, &scan), "1:pet_hop,3:-");
    let found = ScanReport {
        per_frame: vec![frame(false); 4],
        ..scan
    };
    assert_eq!(pet_missing_at(&ledger, &found), "none");
    assert_eq!(
        pet_missing_at(
            &Ledger {
                rows: None,
                ..ledger
            },
            &found
        ),
        "none"
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

fn reach_row(reach: Reach, style: Style) -> PaintRow {
    PaintRow {
        scene: Scene::Plane,
        reach: Some(reach),
        ..row(Expect::Ink, Capture::UnpinnedFocused, style)
    }
}

/// A lit take whose bound frames measured the band's top `px` above the typed row.
fn reached(px: usize) -> ScanReport {
    ScanReport {
        reach_frames: 30,
        reach_px: Some(px),
        reach_min_px: Some(px),
        reach_max_px: Some(px),
        ..lit_burst()
    }
}

#[test]
fn a_reach_row_is_red_for_a_body_too_short_or_too_tall() {
    let ledger = flying_rows(67, "");
    let tall = reach_row(Reach::Tall, Style::Flying);
    assert!(tall.validate().is_ok());
    let cell_h = lit_burst().cell.h;
    let (lo, hi) = Reach::Tall.bounds();
    let lo_px = (lo * cell_h).div_ceil(100);
    let hi_px = hi * cell_h / 100;
    assert!(
        lo_px <= hi_px,
        "the bounds leave a pixel at cell height {cell_h}"
    );
    for px in [lo_px, hi_px] {
        assert_eq!(
            verdict(&tall, &ledger, &reached(px)),
            ShapeVerdict::Pass,
            "{px} px"
        );
    }
    for px in [lo_px - 1, hi_px + 1, 0, 3 * cell_h] {
        assert_eq!(
            verdict(&tall, &ledger, &reached(px)),
            ShapeVerdict::Fail,
            "{px} px"
        );
    }
    let few = ScanReport {
        reach_frames: REACH_MIN_FRAMES - 1,
        ..reached(lo_px)
    };
    assert_eq!(verdict(&tall, &ledger, &few), ShapeVerdict::Fail);
    let unmeasured = ScanReport {
        reach_px: None,
        ..reached(lo_px)
    };
    assert_eq!(verdict(&tall, &ledger, &unmeasured), ShapeVerdict::Fail);
    let unlit = ScanReport {
        total_ink: 0,
        ..reached(lo_px)
    };
    assert_eq!(verdict(&tall, &ledger, &unlit), ShapeVerdict::Fail);
    let unbounded = row(Expect::Ink, Capture::UnpinnedFocused, Style::Flying);
    assert_eq!(
        verdict(&unbounded, &ledger, &reached(0)),
        ShapeVerdict::Pass,
        "a row with no reach is not judged on one"
    );
}

#[test]
fn a_reach_row_that_cannot_measure_its_body_is_refused() {
    assert!(reach_row(Reach::Tall, Style::Underline).validate().is_err());
    assert!(reach_row(Reach::Tall, Style::Off).validate().is_err());
    assert!(
        PaintRow {
            scene: Scene::Prompt,
            ..reach_row(Reach::Tall, Style::Flying)
        }
        .validate()
        .is_err()
    );
    assert!(
        PaintRow {
            capture: Capture::PinnedVideo,
            ..reach_row(Reach::Tall, Style::Flying)
        }
        .validate()
        .is_err()
    );
    assert!(
        PaintRow {
            split: Some(Split::V),
            ..reach_row(Reach::Tall, Style::Flying)
        }
        .validate()
        .is_err()
    );
    assert!(
        PaintRow {
            keys: "",
            ..reach_row(Reach::Tall, Style::Flying)
        }
        .validate()
        .is_err()
    );
    for style in [Style::Classic, Style::Default, Style::Flying, Style::Tall] {
        assert!(
            reach_row(Reach::Tall, style).validate().is_ok(),
            "{style:?}"
        );
    }
}

#[test]
fn a_companion_row_is_red_for_a_companion_too_short_or_too_tall() {
    let cell_h = lit_burst().cell.h;
    for (companion, style) in [
        (Companion::Pet, Style::Default),
        (Companion::Cat, Style::Flying),
    ] {
        let companion_row = PaintRow {
            scene: Scene::Prompt,
            companion: Some(companion),
            ..row(Expect::Ink, Capture::UnpinnedFocused, style)
        };
        assert!(companion_row.validate().is_ok());
        let style_name = match style {
            Style::Default => "rainbow kitty pet",
            Style::Flying => "rainbow kitty flying",
            _ => unreachable!(),
        };
        let ledger = Ledger {
            rows: Some(vec![status(style_name, ""); 36]),
            final_row: None,
            fx_under_frames: 40,
        };
        let (lo, hi) = companion.height_bounds();
        let lo_px = (lo * cell_h).div_ceil(100);
        let hi_px = hi * cell_h / 100;
        assert!(
            lo_px <= hi_px,
            "{companion:?} bounds leave a pixel at cell height {cell_h}"
        );
        let with_height =
            |min_h: Option<usize>, px: Option<usize>, max_h: Option<usize>| match companion {
                Companion::Pet => ScanReport {
                    frames: 36,
                    status_frames: 36,
                    pet_claimed: 36,
                    pet_final_run: 3,
                    companion_height_frames: 4,
                    companion_height_px: px,
                    companion_min_h: min_h,
                    companion_max_h: max_h,
                    base_comp: Shape::new(215, 24, px.unwrap_or(14)),
                    ..lit_burst()
                },
                Companion::Cat => ScanReport {
                    frames: 36,
                    status_frames: 36,
                    cat_claimed: 4,
                    companion_frames: 3,
                    cat_final_run: 3,
                    cat_last_present: true,
                    companion_height_frames: 3,
                    companion_height_px: px,
                    companion_min_h: min_h,
                    companion_max_h: max_h,
                    ..lit_burst()
                },
            };
        for px in [lo_px, hi_px] {
            assert_eq!(
                verdict(
                    &companion_row,
                    &ledger,
                    &with_height(Some(px), Some(px), Some(px))
                ),
                ShapeVerdict::Pass,
                "{companion:?} {px} px"
            );
        }
        for px in [lo_px - 1, hi_px + 1, 0, 3 * cell_h] {
            assert_eq!(
                verdict(
                    &companion_row,
                    &ledger,
                    &with_height(Some(px), Some(px), Some(px))
                ),
                ShapeVerdict::Fail,
                "{companion:?} {px} px"
            );
        }
        assert_eq!(
            verdict(
                &companion_row,
                &ledger,
                &with_height(Some(lo_px - 1), Some(lo_px), Some(hi_px))
            ),
            ShapeVerdict::Fail,
            "{companion:?} outlier low still"
        );
        assert_eq!(
            verdict(
                &companion_row,
                &ledger,
                &with_height(Some(lo_px), Some(hi_px), Some(hi_px + 1))
            ),
            ShapeVerdict::Fail,
            "{companion:?} outlier high still"
        );
        let few = ScanReport {
            companion_height_frames: COMPANION_HEIGHT_MIN_FRAMES - 1,
            ..with_height(Some(lo_px), Some(lo_px), Some(lo_px))
        };
        assert_eq!(verdict(&companion_row, &ledger, &few), ShapeVerdict::Fail);
        assert_eq!(
            verdict(&companion_row, &ledger, &with_height(None, None, None)),
            ShapeVerdict::Fail
        );
    }
}
