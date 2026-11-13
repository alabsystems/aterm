// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_render::{FaceId, GlowQuad, GlyphClass, StyleBits};

use super::{FRAME_GROUPS, GROUP_SRGB, WindowGpu};

/// A cold build through `new_loading`, which every native constructor runs, records every
/// startup leg, so no leg the GUI reads (`startup_gpu_*`) is a 0 nothing timed. A host with
/// no GPU or no system monospace font could not run it.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_cold_build_records_every_startup_leg() {
    use super::{GpuContext, GpuRenderer, Renderer, Theme};
    use crate::startup_probe::{Leg, leg_ns};
    if let Err(e) = GpuContext::new_headless() {
        aterm_spec::host::could_not_run(format!("no GPU: {e}"));
    }
    if Renderer::from_system(16.0, Theme::default()).is_none() {
        aterm_spec::host::could_not_run("no system monospace font");
    }
    let _gpu = GpuRenderer::new_headless(16.0, Theme::default())
        .expect("a host with a GPU and a font builds a renderer");
    for leg in [Leg::Context, Leg::FontThread, Leg::FontJoin, Leg::Assemble] {
        assert_ne!(leg_ns(leg), 0, "{leg:?} was not recorded");
    }
}

/// A MOTION FRAME'S ROWS UPLOAD WITHOUT A COMPARE (MOT-1): rows a card names as all it
/// changed since the card held upload as named, clamped to the card, and none where they
/// fall outside it; a card that names none is compared whole. NEGATIVE CONTROL: an unnamed
/// card whose rows match uploads nothing.
#[test]
fn a_motion_frames_rows_upload_as_named_and_an_unnamed_card_is_compared() {
    let stride = 4 * 4;
    let old = vec![0u8; stride * 8];
    let mut new = old.clone();
    new[stride * 5] = 9;
    assert_eq!(
        super::tray_rows_to_upload(&old, &new, stride, None),
        Some((5, 6))
    );
    assert_eq!(super::tray_rows_to_upload(&old, &old, stride, None), None);
    assert_eq!(
        super::tray_rows_to_upload(&old, &new, stride, Some((2, 4))),
        Some((2, 4)),
        "named rows are trusted"
    );
    assert_eq!(
        super::tray_rows_to_upload(&old, &new, stride, Some((6, 99))),
        Some((6, 8))
    );
    assert_eq!(
        super::tray_rows_to_upload(&old, &new, stride, Some((8, 9))),
        None
    );
}

#[test]
fn a_minimized_window_parks_all_but_a_captures_present() {
    let mut win = WindowGpu::new();
    assert!(!win.parks_while_minimized(), "a visible window presents");
    win.set_occluded_hint(true);
    assert!(win.parks_while_minimized(), "a minimized one parks");
    win.set_capture_present(true);
    assert!(!win.parks_while_minimized(), "but presents for a capture");
    win.set_capture_present(false);
    assert!(win.parks_while_minimized(), "and parks again after it");
    win.set_occluded_hint(false);
    win.set_capture_present(true);
    assert!(!win.parks_while_minimized());
}

#[test]
fn band_shift_ops_states_the_staged_move_law() {
    use super::{BandShift, band_shift_ops};
    assert_eq!(
        band_shift_ops(10, 30, 3),
        Some(BandShift {
            src_y: 13,
            dst_y: 10,
            moved: 17
        })
    );
    assert_eq!(
        band_shift_ops(10, 30, -3),
        Some(BandShift {
            src_y: 10,
            dst_y: 13,
            moved: 17
        })
    );
    assert_eq!(band_shift_ops(10, 30, 0), None);
    assert_eq!(band_shift_ops(10, 10, 5), None);
    assert_eq!(band_shift_ops(30, 10, 5), None);
    assert_eq!(band_shift_ops(10, 30, 20), None);
    assert_eq!(band_shift_ops(10, 30, -20), None);
    assert_eq!(
        band_shift_ops(0, 8, 7),
        Some(BandShift {
            src_y: 7,
            dst_y: 0,
            moved: 1
        })
    );
}

#[test]
fn sdr_glow_attack_is_owned_per_window() {
    let now = aterm_time::Instant::now();
    let mut a = WindowGpu::new();
    let mut b = WindowGpu::new();

    let expected_first = 0.30 * (1.0 - (-0.016f32 / 0.045).exp());
    let a_first = a.advance_sdr_glow_budget(true, 0.30, now);
    assert!(
        (a_first - expected_first).abs() <= f32::EPSILON,
        "fresh attack takes the 16 ms step: got {a_first}, want {expected_first}"
    );
    assert_eq!(
        a.advance_sdr_glow_budget(true, 0.30, now),
        a_first,
        "zero elapsed dt holds the current attack level"
    );
    let t1 = now + std::time::Duration::from_millis(16);
    let a_second = a.advance_sdr_glow_budget(true, 0.30, t1);
    assert!(a_second > a_first && a_second <= 0.30);

    assert_eq!(b.advance_sdr_glow_budget(false, 0.30, now), 0.0);
    assert_eq!(
        a.sdr_glow_level, a_second,
        "an empty present in window B cannot reset window A's crown"
    );
    assert!(a.sdr_glow_level_at.is_some());

    let b_first = b.advance_sdr_glow_budget(true, 0.30, now);
    assert!(
        (b_first - a_first).abs() <= f32::EPSILON,
        "window B begins its own attack instead of inheriting A's level"
    );

    let snapped = a.advance_sdr_glow_budget(true, 0.05, t1 + std::time::Duration::from_millis(16));
    assert_eq!(
        snapped, 0.05,
        "lowering the target below the current level snaps down immediately"
    );

    assert_eq!(a.advance_sdr_glow_budget(false, 0.30, t1), 0.0);
    assert_eq!(a.sdr_glow_level, 0.0);
    assert!(a.sdr_glow_level_at.is_none());
    let a_restarted = a.advance_sdr_glow_budget(true, 0.30, t1 + std::time::Duration::from_secs(5));
    assert!(
        (a_restarted - expected_first).abs() <= f32::EPSILON,
        "after reset, the next lit frame takes a fresh 16 ms attack step"
    );
    assert_eq!(
        b.sdr_glow_level, b_first,
        "resetting and restarting window A cannot disturb window B"
    );
}

#[test]
fn frame_passes_coalesce_maximally_without_reordering() {
    for bits in 0u32..(1 << FRAME_GROUPS) {
        let mut enabled = [false; FRAME_GROUPS];
        for (g, e) in enabled.iter_mut().enumerate() {
            *e = bits & (1 << g) != 0;
        }
        let (pass_of, pass_srgb, passes) = super::coalesce_frame_passes(&enabled, &GROUP_SRGB);
        let live: Vec<usize> = (0..FRAME_GROUPS).filter(|&g| enabled[g]).collect();
        assert_eq!(
            passes,
            live.windows(2)
                .filter(|w| GROUP_SRGB[w[0]] != GROUP_SRGB[w[1]])
                .count()
                + usize::from(!live.is_empty()),
            "pass count must be exactly one plus the number of VIEW CHANGES \
             between consecutive live groups (bits {bits:#010b})"
        );
        for &g in &live {
            assert_eq!(
                pass_srgb[pass_of[g]], GROUP_SRGB[g],
                "group {g} must be emitted into a pass attaching ITS view"
            );
        }
        for w in live.windows(2) {
            let (a, b) = (w[0], w[1]);
            if GROUP_SRGB[a] == GROUP_SRGB[b] {
                assert_eq!(
                    pass_of[a], pass_of[b],
                    "same-view neighbours {a}/{b} must share one pass — an \
                     unmerged boundary is a whole-framebuffer tile load+store"
                );
            } else {
                assert_eq!(
                    pass_of[b],
                    pass_of[a] + 1,
                    "a view change must advance by exactly one pass, never \
                     reorder ({a} -> {b})"
                );
            }
        }
        for g in 0..FRAME_GROUPS {
            if !enabled[g] {
                assert_eq!(
                    pass_of[g],
                    usize::MAX,
                    "a disabled group must match no pass index"
                );
            }
        }
    }
}

#[test]
fn the_unorm_ablation_drops_or_thins_exactly_the_one_one_groups() {
    let counts = [Some(7u32); STREAM_COUNT];
    let ctx = |ablation| FramePlanCtx {
        has_image_plane: true,
        has_deco: true,
        has_wallpaper: true,
        cursor_opaque: true,
        ablation,
        under_opaque: false,
    };
    let passes_of = |plan: &[DrawItem]| super::plan_passes(plan).2;
    let plan = |ablation| {
        let mut plan = Vec::new();
        let record = build_frame_plan(&mut plan, &counts, ctx(ablation));
        let unorm: Vec<&DrawItem> = plan
            .iter()
            .filter(|i| !GROUP_SRGB[i.group as usize])
            .collect();
        assert_eq!(
            record,
            PlanUnorm {
                ablation,
                draws: unorm.len() as u32,
                instances: unorm.iter().map(|i| u64::from(i.count)).sum(),
                route: UnormRoute::Split,
                under_store: false,
            }
        );
        plan
    };
    let (full, dropped, thinned) = (
        plan(UnormAblation::Off),
        plan(UnormAblation::Drop),
        plan(UnormAblation::Thin),
    );
    let unorm = |i: &DrawItem| !GROUP_SRGB[i.group as usize];
    assert!(
        full.iter().any(unorm),
        "the full plan draws One/One streams, or there is nothing to ablate"
    );
    let kept: Vec<(u8, StreamId)> = full
        .iter()
        .filter(|i| !unorm(i))
        .map(|i| (i.group, i.stream))
        .collect();
    let got: Vec<(u8, StreamId)> = dropped.iter().map(|i| (i.group, i.stream)).collect();
    assert_eq!(got, kept, "Drop removes only Unorm items, in order");
    assert_eq!(
        passes_of(&dropped),
        1,
        "all-sRGB groups coalesce to one pass"
    );
    assert!(
        passes_of(&full) > passes_of(&dropped),
        "the Unorm groups cost passes: {} vs {}",
        passes_of(&full),
        passes_of(&dropped)
    );
    assert_eq!(
        thinned
            .iter()
            .map(|i| (i.group, i.stream))
            .collect::<Vec<_>>(),
        full.iter().map(|i| (i.group, i.stream)).collect::<Vec<_>>(),
        "Thin keeps every item, in order"
    );
    assert_eq!(passes_of(&thinned), passes_of(&full), "and so every pass");
    for (thin, item) in thinned.iter().zip(&full) {
        let want = if unorm(item) { 1 } else { item.count };
        assert_eq!(thin.count, want, "{:?}", item.stream);
    }
}

/// The fetch route moves exactly the Unorm-law draws onto their twins, in order and count, so a
/// full plan is one pass.
#[cfg(target_os = "macos")]
#[test]
fn the_fetch_route_draws_every_unorm_law_row_through_its_twin_in_one_pass() {
    let counts = [Some(7u32); STREAM_COUNT];
    let ctx = FramePlanCtx {
        has_image_plane: true,
        has_deco: true,
        has_wallpaper: true,
        cursor_opaque: false,
        ablation: UnormAblation::Off,
        under_opaque: false,
    };
    let (mut split, mut fetch) = (Vec::new(), Vec::new());
    let mut split_record = build_frame_plan(&mut split, &counts, ctx);
    route_frame_plan(&mut split, &mut split_record, UnormRoute::Split);
    let mut fetch_record = build_frame_plan(&mut fetch, &counts, ctx);
    route_frame_plan(&mut fetch, &mut fetch_record, UnormRoute::Fetch);
    assert_eq!(
        fetch_record,
        PlanUnorm {
            route: UnormRoute::Fetch,
            ..split_record
        }
    );
    assert!(
        split_record.draws > 0,
        "the full plan holds Unorm-law draws"
    );
    let key = |i: &DrawItem| (i.group, i.pipe, i.atlas, i.stream, i.count);
    assert_eq!(
        fetch.iter().map(key).collect::<Vec<_>>(),
        split.iter().map(key).collect::<Vec<_>>(),
        "the route changes no draw but its view and pipeline"
    );
    let mut twins = std::collections::BTreeSet::new();
    for item in &fetch {
        let unorm = !GROUP_SRGB[item.group as usize];
        assert_eq!(item.fetch, unorm, "{:?}", item.stream);
        assert!(item.srgb(), "{:?} draws in the sRGB view", item.stream);
        if unorm {
            let row = item.pipe.row();
            let twin = pipeline_table::FetchTwin::of(row)
                .unwrap_or_else(|| panic!("{row:?} draws by the Unorm law but has no twin"));
            assert_eq!(twin.row(), row);
            let (spec, base) = (twin.spec(), row.spec());
            assert_eq!(spec.target, pipeline_table::TargetRole::OffscreenSrgb);
            assert!(spec.blend.is_none(), "{twin:?} blends in its fragment");
            assert_eq!(
                (spec.vs, spec.write_mask, spec.library),
                (base.vs, base.write_mask, base.library),
                "{twin:?} keeps its row's vertex stage, mask and library"
            );
            twins.insert(twin);
        }
    }
    assert_eq!(
        twins.into_iter().collect::<Vec<_>>(),
        pipeline_table::FetchTwin::ALL.to_vec(),
        "every twin stands in for a draw of the full plan"
    );
    assert!(!split.iter().any(|i| i.fetch), "the split routes nothing");
    assert!(super::plan_passes(&split).2 > 1);
    assert_eq!(
        super::plan_passes(&fetch).2,
        1,
        "the fetch route is one pass"
    );
}

/// Route (1): an opaque Exact Under fx stores at the end of the base pass only while no cursor
/// glow draws under the text.
#[test]
fn an_opaque_under_fx_stores_in_the_base_pass_only_without_a_glow_under() {
    let ctx = |under_opaque| FramePlanCtx {
        has_image_plane: false,
        has_deco: false,
        has_wallpaper: false,
        cursor_opaque: true,
        ablation: UnormAblation::Off,
        under_opaque,
    };
    let mut counts = [None; STREAM_COUNT];
    for id in [
        StreamId::Bg,
        StreamId::Trail,
        StreamId::FxUnder,
        StreamId::FireAdd,
        StreamId::Glyph,
    ] {
        counts[id as usize] = Some(3);
    }
    let order = |plan: &[DrawItem]| plan.iter().map(|i| i.stream).collect::<Vec<_>>();
    let under = |plan: &[DrawItem]| {
        let item = plan
            .iter()
            .find(|i| i.stream == StreamId::FxUnder)
            .expect("the Under fx draws");
        (usize::from(item.group), item.pipe)
    };
    let (mut kept, mut stored) = (Vec::new(), Vec::new());
    let kept_rec = build_frame_plan(&mut kept, &counts, ctx(false));
    let stored_rec = build_frame_plan(&mut stored, &counts, ctx(true));
    assert!(!kept_rec.under_store && stored_rec.under_store);
    assert_eq!(under(&kept), (G_GLOW_UNDER, DrawPipe::Fx));
    assert_eq!(under(&stored), (G_BASE_BG, DrawPipe::FxStore));
    assert_eq!(
        order(&stored),
        order(&kept),
        "the store draws where the CPU draws the layer"
    );
    assert_eq!(stored_rec.draws + 1, kept_rec.draws);
    assert_eq!(
        (super::plan_passes(&kept).2, super::plan_passes(&stored).2),
        (3, 3),
        "the fire still opens its own group"
    );
    counts[StreamId::FireAdd as usize] = None;
    build_frame_plan(&mut kept, &counts, ctx(false));
    build_frame_plan(&mut stored, &counts, ctx(true));
    assert_eq!(
        (super::plan_passes(&kept).2, super::plan_passes(&stored).2),
        (3, 1),
        "with the Under group empty its passes merge away"
    );

    counts[StreamId::GlowUnder as usize] = Some(1);
    let mut glow = Vec::new();
    let glow_rec = build_frame_plan(&mut glow, &counts, ctx(true));
    assert!(!glow_rec.under_store);
    assert_eq!(
        under(&glow),
        (G_GLOW_UNDER, DrawPipe::Fx),
        "a cursor glow under the text keeps the Under fx after it"
    );

    counts[StreamId::GlowUnder as usize] = None;
    counts[StreamId::FxUnder as usize] = None;
    let mut none = Vec::new();
    assert!(
        !build_frame_plan(&mut none, &counts, ctx(true)).under_store,
        "a frame without Under fx stores nothing"
    );
}

#[test]
fn the_unorm_route_seam_reads_its_modes() {
    assert_eq!(UnormRoute::from_seam("split"), UnormRoute::Split);
    assert_eq!(UnormRoute::from_seam("fetch"), UnormRoute::Fetch);
    assert_eq!(UnormRoute::from_seam(""), UnormRoute::Fetch);
    for route in [UnormRoute::Split, UnormRoute::Fetch] {
        assert_eq!(UnormRoute::from_seam(route.name()), route);
    }
}

#[test]
fn the_ablation_seam_reads_its_modes() {
    assert_eq!(UnormAblation::from_seam("drop"), UnormAblation::Drop);
    assert_eq!(UnormAblation::from_seam("1"), UnormAblation::Drop);
    assert_eq!(UnormAblation::from_seam("thin"), UnormAblation::Thin);
    assert_eq!(UnormAblation::from_seam("0"), UnormAblation::Off);
    assert_eq!(UnormAblation::from_seam(""), UnormAblation::Off);
    for mode in [UnormAblation::Off, UnormAblation::Drop, UnormAblation::Thin] {
        let back = UnormAblation::from_seam(mode.name());
        assert_eq!(back, mode, "{}", mode.name());
    }
}

#[test]
fn union_rect_opt_is_conservative_and_clamped() {
    let dims = (100, 50);
    assert_eq!(super::union_rect_opt(None, Some([0, 0, 1, 1]), dims), None);
    assert_eq!(super::union_rect_opt(Some([0, 0, 1, 1]), None, dims), None);
    assert_eq!(
        super::union_rect_opt(Some([0, 0, 0, 0]), Some([2, 3, 4, 5]), dims),
        Some([2, 3, 4, 5])
    );
    assert_eq!(
        super::union_rect_opt(Some([2, 3, 4, 5]), Some([9, 9, 9, 9]), dims),
        Some([2, 3, 4, 5])
    );
    assert_eq!(
        super::union_rect_opt(Some([1, 2, 3, 4]), Some([10, 0, 200, 80]), dims),
        Some([1, 0, 100, 50])
    );
}

#[test]
#[cfg(target_os = "macos")]
fn copy_src_is_armed_only_by_a_live_tap_on_a_capable_surface() {
    let wants = super::GpuRenderer::tap_wants_copy_src;
    assert!(!wants(false, false, false), "idle: no readback, no cost");
    assert!(
        wants(true, false, false),
        "a live recording needs the readback"
    );
    assert!(
        wants(false, true, false),
        "a ONE-SHOT still needs it just as much"
    );
    assert!(
        wants(true, true, false),
        "a still taken mid-recording needs it too"
    );
    assert!(wants(false, false, true), "a recent still holds it");
}

#[test]
#[cfg(target_os = "macos")]
fn polled_snapshots_inside_the_hold_reconfigure_the_swapchain_once() {
    struct Swapchain {
        copyable: bool,
        reconfigures: u32,
    }
    impl Swapchain {
        fn present(&mut self, hold: super::CopySrcHold, snapshot: bool, at: aterm_time::Instant) {
            let want = super::GpuRenderer::tap_wants_copy_src(false, snapshot, hold.holds(at));
            if want != self.copyable {
                self.copyable = want;
                self.reconfigures += 1;
            }
        }
    }
    let frame = std::time::Duration::from_millis(16);
    let t0 = aterm_time::Instant::now();
    let mut hold = super::CopySrcHold::default();
    let mut swapchain = Swapchain {
        copyable: false,
        reconfigures: 0,
    };
    let mut capture = |swapchain: &mut Swapchain, at: aterm_time::Instant| {
        hold.arm(at);
        swapchain.present(hold, true, at);
        swapchain.present(hold, false, at + frame);
        hold
    };

    capture(&mut swapchain, t0);
    let hold = capture(&mut swapchain, t0 + std::time::Duration::from_millis(500));
    assert_eq!(
        swapchain.reconfigures, 1,
        "two polled captures: one reconfigure"
    );
    assert!(swapchain.copyable);

    let expired = t0 + std::time::Duration::from_millis(500) + super::SNAPSHOT_COPY_SRC_HOLD;
    swapchain.present(hold, false, expired);
    assert!(!swapchain.copyable, "past the hold the bit drops");
    capture(&mut swapchain, expired + frame);
    assert_eq!(
        swapchain.reconfigures, 3,
        "a capture after the hold reconfigures again"
    );
}

use super::*;
use aterm_core::terminal::Terminal;
use aterm_render::Theme;

#[test]
fn present_crop_normalizes_only_after_logical_validation() {
    let crop = PresentCrop {
        source_y: 3,
        height: 92,
    };
    assert_eq!(
        normalize_present_crop(crop, 100, 100),
        Some(crop),
        "an entirely resident crop is unchanged"
    );
    assert_eq!(
        normalize_present_crop(crop, 100, 64),
        Some(PresentCrop {
            source_y: 3,
            height: 61,
        }),
        "a valid oversized-grid crop is trimmed to the resident top prefix"
    );

    for malformed in [
        PresentCrop {
            source_y: 0,
            height: 0,
        },
        PresentCrop {
            source_y: 100,
            height: 1,
        },
        PresentCrop {
            source_y: 99,
            height: 2,
        },
        PresentCrop {
            source_y: u32::MAX,
            height: u32::MAX,
        },
    ] {
        assert_eq!(
            normalize_present_crop(malformed, 100, 64),
            None,
            "malformed logical crop {malformed:?} must be rejected"
        );
    }
    assert_eq!(
        normalize_present_crop(
            PresentCrop {
                source_y: 70,
                height: 10,
            },
            100,
            64,
        ),
        None,
        "a logically valid interval wholly below the resident prefix is empty"
    );
}

#[test]
fn visible_source_scissor_preserves_signed_odd_crop_intersections() {
    let uniform = present_blit_uniform(
        false,
        None,
        Some(PresentCrop {
            source_y: 1,
            height: 6,
        }),
        10,
        11,
        7,
        8,
        0,
    );
    if cfg!(target_os = "linux") {
        assert_eq!(uniform.content_off, [-2.0, 0.0]);
        assert_eq!(visible_source_scissor(&uniform, 7, 8), Some((0, 1, 7, 6)));
    } else {
        assert_eq!(uniform.content_off, [-2.0, -2.0]);
        assert_eq!(visible_source_scissor(&uniform, 7, 8), Some((0, 0, 7, 5)));
    }

    let wholly_above = present_blit_uniform(
        false,
        None,
        Some(PresentCrop {
            source_y: 0,
            height: 1,
        }),
        10,
        11,
        1,
        1,
        0,
    );
    if cfg!(target_os = "linux") {
        assert_eq!(
            visible_source_scissor(&wholly_above, 1, 1),
            Some((0, 0, 1, 1)),
            "the pinned top row stays on screen"
        );
    } else {
        assert_eq!(
            visible_source_scissor(&wholly_above, 1, 1),
            None,
            "a fully clipped crown must not issue a zero/underflowed scissor"
        );
    }
}

#[test]
fn crown_scissor_clips_both_scroll_directions_to_independent_destination_bounds() {
    let mut uniform = present_blit_uniform(false, None, None, 40, 60, 40, 60, 0);
    let up = BandShift {
        src_y: 13,
        dst_y: 10,
        moved: 17,
    };
    let down = BandShift {
        src_y: 10,
        dst_y: 13,
        moved: 17,
    };
    assert_eq!(
        crown_band_scissor(&uniform, None, 40, 60),
        Some((0, 0, 40, 60))
    );
    assert_eq!(
        crown_band_scissor(&uniform, Some(up), 40, 60),
        Some((0, 10, 40, 17))
    );
    assert_eq!(
        crown_band_scissor(&uniform, Some(down), 40, 60),
        Some((0, 13, 40, 17))
    );

    uniform.visible_y = 15.0;
    uniform.visible_h = 5.0;
    for shift in [up, down] {
        assert_eq!(
            crown_band_scissor(&uniform, Some(shift), 40, 60),
            Some((0, 15, 40, 5))
        );
    }
    uniform.visible_y = 30.0;
    uniform.visible_h = 10.0;
    for shift in [up, down] {
        assert_eq!(crown_band_scissor(&uniform, Some(shift), 40, 60), None);
    }

    uniform.content_off = [-3.0, -12.0];
    uniform.visible_y = 0.0;
    uniform.visible_h = 60.0;
    assert_eq!(
        crown_band_scissor(&uniform, Some(up), 30, 30),
        Some((0, 0, 30, 15))
    );
    assert_eq!(
        crown_band_scissor(&uniform, Some(down), 30, 30),
        Some((0, 1, 30, 17))
    );
    assert_eq!(
        crown_band_scissor(&uniform, Some(up), 30, 3),
        Some((0, 0, 30, 3))
    );
    assert_eq!(
        crown_band_scissor(&uniform, Some(down), 30, 3),
        Some((0, 1, 30, 2))
    );
}

#[test]
fn crown_band_scissor_and_present_uniforms_track_fractional_scroll_band() {
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.set_hdr_glow(true);
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"hello");
    let mut input = term.cell_frame(rows, cols);
    input.grid_top_row = 1;
    input.grid_bot_row = rows - 1;
    input.cursor_glow_add.push(GlowQuad {
        x: 8,
        y: 24,
        w: 8,
        h: 16,
        color: 0xffffff,
        alpha: 0,
        color2: 0xffffff,
        alpha2: 0,
        row: 2,
    });
    let (fw, fh) = gpu.frame_size(rows, cols);
    let (fw, fh) = (fw as u32, fh as u32);
    let (dw, dh) = (fw + 10, fh + 6);
    let mut win = WindowGpu::new();
    win.edr_max = 2.0;

    input.scroll_frac_px = 0;
    let base = gpu.present_uniforms(
        &mut win,
        &input,
        false,
        None,
        None,
        (fw, fh),
        (dw, dh),
        true,
        false,
        false,
    );
    let base_crown = base.crown.expect("HDR present with glow emits a crown");
    assert_eq!(base_crown.content_off, base.want.content_off);

    input.scroll_frac_px = 3;
    let shifted = gpu.present_uniforms(
        &mut win,
        &input,
        false,
        None,
        None,
        (fw, fh),
        (dw, dh),
        true,
        false,
        false,
    );
    let shifted_crown = shifted.crown.expect("HDR present with glow emits a crown");
    assert_eq!(
        shifted_crown.content_off,
        [base.want.content_off[0], base.want.content_off[1] - 3.0],
        "crown placement must translate with the sub-row scroll band"
    );
    let (band_y0, band_y1) = aterm_render::scroll_translate::grid_band_px(
        gpu.cpu.grid_top(),
        gpu.cell_size().1,
        input.grid_top_row,
        input.grid_bot_row,
        fh as usize,
    );
    let expected_shift =
        band_shift_ops(band_y0, band_y1, 3).expect("nonzero frac inside non-empty band");
    assert_eq!(
        shifted.crown_scissor,
        crown_band_scissor(&shifted.want, Some(expected_shift), dw, dh),
        "crown scissor must clip to the moved grid band"
    );
}

#[test]
fn atlas_texel_bytes_match_cpu_glyph_bytes_exactly() {
    let Some(mut cpu) = Renderer::from_system(18.0, Theme::default()) else {
        aterm_spec::host::could_not_run("no system monospace font");
    };

    cpu.debug_block_on_lazy_fallbacks();
    let keys: Vec<GlyphKey> = "RXab\u{65E5}\u{672C}"
        .chars()
        .map(|c| cpu.glyph_key(c))
        .collect();
    assert_atlas_holds_the_cpu_bytes(&mut cpu, keys, 5);
}

#[test]
fn fitted_fallback_styles_reach_the_atlas_as_the_cpu_fit_them() {
    let primary = aterm_tempfile::manifest_dir()
        .join("../aterm-render/assets/bundled/CascadiaMonoPL-Regular.ttf");
    let primary = primary.to_str().expect("a UTF-8 font path");
    let mut cpu = Renderer::from_system_with_family(Some(primary), 18.0, Theme::default())
        .unwrap_or_else(|| panic!("{primary} does not load"));
    cpu.debug_block_on_lazy_fallbacks();
    let warning = cpu.glyph_key('\u{26A0}');
    if !matches!(
        warning.source,
        FaceId::Fallback | FaceId::SymbolFallback | FaceId::RuntimeFallback | FaceId::DisplayMix
    ) || warning.glyph_class != GlyphClass::Mono
    {
        aterm_spec::host::could_not_run(format!(
            "this host serves U+26A0 from no mono fallback face ({warning:?})"
        ));
    }
    let (cell_w, cell_h) = cpu.cell_size();
    let mut keys = Vec::new();
    for style in [
        StyleBits::REGULAR,
        StyleBits::BOLD,
        StyleBits::ITALIC,
        StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
    ] {
        let key = cpu.glyph_key_styled('\u{26A0}', style);
        assert_eq!(key.cell_span, 1);
        let img = cpu.glyph_image(key);
        assert!(
            img.xmin() >= 0 && img.xmin() + img.width() as i32 <= cell_w as i32,
            "{style:?} warning escaped its one-cell CPU mask before atlas packing"
        );
        assert!(img.height() <= cell_h);
        assert_eq!(img.advance(), cell_w as f32);
        keys.push(key);
    }
    assert_atlas_holds_the_cpu_bytes(&mut cpu, keys, 1);
}

fn assert_atlas_holds_the_cpu_bytes(cpu: &mut Renderer, mut keys: Vec<GlyphKey>, at_least: usize) {
    keys.sort_unstable();
    keys.dedup();
    assert!(
        keys.len() >= at_least,
        "the fixture contributes at least {at_least} distinct glyphs, got {}",
        keys.len()
    );

    let atlas = build_atlas(cpu, &keys, u32::MAX);
    for &key in &keys {
        let slot = atlas
            .map
            .get(&key)
            .expect("every requested glyph gets a slot");
        let img = cpu.glyph_image(key).clone();
        let GlyphImage::Mono {
            width,
            height,
            bytes,
            ..
        } = &img
        else {
            panic!("char keys rasterize as Mono");
        };
        if *width == 0 || *height == 0 {
            assert_eq!(
                (slot.gw, slot.gh),
                (0, 0),
                "empty glyph must pack as an empty slot"
            );
            continue;
        }
        assert_eq!(
            (slot.gw as usize, slot.gh as usize),
            (*width, *height),
            "slot size differs from CPU bitmap for {:?}",
            key.chr()
        );
        for j in 0..slot.gh {
            for i in 0..slot.gw {
                let atlas_byte = atlas.data[((slot.ay + j) * atlas.width + slot.ax + i) as usize];
                let cpu_byte = bytes[(j * slot.gw + i) as usize];
                assert_eq!(
                    atlas_byte,
                    cpu_byte,
                    "atlas texel ({i},{j}) of {:?} differs from the CPU cache byte",
                    key.chr()
                );
            }
        }
    }
}

#[test]
fn vs15_and_bare_primary_glyphs_pack_distinct_atlas_entries() {
    let primary_bytes = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut primary = Renderer::from_bytes(primary_bytes, 18.0, Theme::default())
        .expect("fixture primary renderer");
    let bolt = '\u{26A1}';
    let bare_primary = primary.glyph_key(bolt);
    assert_eq!(bare_primary.source, FaceId::Primary);
    assert_eq!(bare_primary.glyph_class, GlyphClass::MonoGid);
    assert_eq!(bare_primary.cell_span, 0);

    let mut primary_term = Terminal::new(1, 3);
    primary_term.process("⚡\u{FE0E}".as_bytes());
    let primary_input = primary_term.cell_frame(1, 3);
    let text_primary = primary.resolve_cell_key_for_span(None, &primary_input.cells[0][0], 1);
    assert_eq!(text_primary.source, bare_primary.source);
    assert_eq!(text_primary.glyph_class, bare_primary.glyph_class);
    assert_eq!(text_primary.ch_or_id, bare_primary.ch_or_id);
    assert_eq!(text_primary.cell_span, 1);
    assert_ne!(text_primary, bare_primary);

    let primary_atlas = build_atlas(&mut primary, &[bare_primary, text_primary], u32::MAX);
    let bare_slot = primary_atlas
        .map
        .get(&bare_primary)
        .expect("bare primary key reaches mono atlas");
    let text_slot = primary_atlas
        .map
        .get(&text_primary)
        .expect("VS15 primary key reaches mono atlas");
    assert_ne!(
        (bare_slot.ax, bare_slot.ay),
        (text_slot.ax, text_slot.ay),
        "bare and VS15 primary rasters must occupy distinct atlas entries"
    );
}

#[test]
fn atlas_place_degrades_glyph_wider_than_atlas() {
    let px_q = GlyphKey::quantize_px(18.0);
    let ow = (ATLAS_WIDTH as usize) * 2 + 100;
    let overwide = GlyphImage::Mono {
        width: ow,
        height: 10,
        xmin: 3,
        ymin: 7,
        advance: 12.0,
        bytes: vec![0xAB; ow * 10],
    };

    let mut mono = Atlas {
        kind: AtlasKind::Mono,
        width: ATLAS_WIDTH,
        height: 1,
        data: vec![0u8; ATLAS_WIDTH as usize],
        map: FxHashMap::default(),
        px: 0,
        py: 0,
        shelf_h: 0,
        write_gen: 0,
    };
    let key = GlyphKey::mono_char(
        aterm_render::FaceId::Primary,
        'W',
        aterm_render::StyleBits::REGULAR,
        px_q,
    );
    assert!(
        mono.place(key, &overwide).is_none(),
        "an over-wide glyph must not claim a packed band"
    );
    assert_eq!(
        (mono.px, mono.py, mono.shelf_h),
        (0, 0, 0),
        "cursor unmoved"
    );
    let slot = *mono.map.get(&key).expect("Mono records every key");
    assert_eq!((slot.gw, slot.gh), (0, 0), "degraded slot is zero-sized");
    assert_eq!(
        (slot.xmin, slot.ymin),
        (3, 7),
        "placement offsets preserved"
    );
    let before = mono.data.clone();
    mono.blit(&overwide, &slot);
    assert_eq!(mono.data, before, "zero-slot blit writes nothing");

    let mut color = Atlas {
        kind: AtlasKind::Color,
        width: ATLAS_WIDTH,
        height: 1,
        data: vec![0u8; (ATLAS_WIDTH * 4) as usize],
        map: FxHashMap::default(),
        px: 0,
        py: 0,
        shelf_h: 0,
        write_gen: 0,
    };
    let overwide_rgba = GlyphImage::Rgba {
        width: ow,
        height: 10,
        xmin: 0,
        ymin: 0,
        advance: 12.0,
        bytes: vec![0u8; ow * 10 * 4],
    };
    let ckey = GlyphKey::rgba_char(aterm_render::FaceId::Primary, '\u{1F600}', px_q);
    assert!(
        color.place(ckey, &overwide_rgba).is_none(),
        "an over-wide colour glyph must not claim a packed band"
    );
    assert_eq!(
        (color.px, color.py, color.shelf_h),
        (0, 0, 0),
        "cursor unmoved"
    );
    assert!(
        color.map.is_empty(),
        "Color skips an over-wide glyph entirely (no slot)"
    );
}

#[test]
fn set_face_preserves_text_shaping() {
    let theme = Theme::default();
    let mut gpu = crate::renderer::gpu_renderer_for_test(Some("Menlo"), 16.0, theme);
    gpu.set_text_shaping(aterm_render::TextShapingConfig {
        ligature_mode: aterm_types::text_shaping::LigatureMode::Disabled,
        ..Default::default()
    });
    let Some(face) = Renderer::from_system(24.0, theme) else {
        aterm_spec::host::could_not_run("no system monospace font");
    };
    gpu.set_face(face, theme);
    assert_eq!(
        gpu.cpu.text_shaping().ligature_mode,
        aterm_types::text_shaping::LigatureMode::Disabled,
        "shaping must survive the face rebuild, not reset to Enabled"
    );
}

/// A prepared face draws the weight it was prepared at: a config reload of
/// `font_weight`, and the headless build's deferred GPU, install a face that already
/// carries the config's variation requests. Before, installing it put back the
/// requests of the face it replaced, so a weight changed in Settings (or a headless
/// instance's configured weight) never reached the GPU's glyphs.
#[test]
fn a_prepared_face_keeps_the_weight_it_was_prepared_at() {
    let theme = Theme::default();
    let mut gpu = crate::renderer::gpu_renderer_for_test(Some("Menlo"), 16.0, theme);
    assert!(gpu.cpu.font_variation_requests().is_empty());
    let Some(mut prepared) = Renderer::from_system(16.0, theme) else {
        aterm_spec::host::could_not_run("no system monospace font");
    };
    let weight = [(aterm_render::variation::WGHT_TAG, 600.0)];
    let _ = prepared.set_font_variations(&weight, 0.0);
    gpu.install_prepared_font(None, prepared, theme);
    assert_eq!(gpu.cpu.font_variation_requests(), weight);
}

#[test]
fn deco_atlas_drops_on_oversized_cell_height_no_abort() {
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    let (cw, ch) = gpu.cell_size();
    gpu.ensure_deco_atlas(cw, ch);
    assert!(
        gpu.deco_atlas.is_some(),
        "a realistic {cw}x{ch} cell must build the deco atlas (guard must not over-trigger)"
    );
    gpu.ensure_deco_atlas(cw, 10_000);
    assert!(
        gpu.deco_atlas.is_none(),
        "an oversized cell height must DROP the deco atlas (graceful), not abort"
    );
}

#[test]
fn present_prev_is_per_window_no_cross_window_leak() {
    let theme = Theme::default();
    let px = 18.0;
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, px, theme);

    let (rows, cols) = (4usize, 10usize);
    let frame = |bytes: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25l");
        term.process(bytes);
        term.cell_frame(rows, cols)
    };
    let a_frames = [
        frame(b"AAAA"),
        frame(b"AAAA\r\nbbbb"),
        frame(b"AAAA\r\ncccc"),
    ];
    let b_frames = [
        frame(b"ZZZZ"),
        frame(b"ZZZZ\r\nyyyy"),
        frame(b"ZZZZ\r\nxxxx"),
    ];

    let mut win_a = WindowGpu::new();
    let mut win_b = WindowGpu::new();

    let mut last_a = None;
    let mut last_b = None;
    for i in 0..a_frames.len() {
        last_a = Some(gpu.present_input_readback(&mut win_a, &a_frames[i]));
        last_b = Some(gpu.present_input_readback(&mut win_b, &b_frames[i]));
    }
    let got_a = last_a.expect("at least one frame driven");
    let got_b = last_b.expect("at least one frame driven");

    assert!(
        gpu.scissor_taken() > 0,
        "scissor path must be exercised by the changed frames"
    );

    let mut ref_win_a = WindowGpu::new();
    let mut ref_win_b = WindowGpu::new();
    let want_a = gpu.render_input(&mut ref_win_a, &a_frames[a_frames.len() - 1], None);
    let want_b = gpu.render_input(&mut ref_win_b, &b_frames[b_frames.len() - 1], None);

    assert_eq!(
        (got_a.width, got_a.height),
        (want_a.width, want_a.height),
        "window A readback dims must match the reference"
    );
    assert_eq!(
        got_a.pixels, want_a.pixels,
        "window A's interleaved scissored readback must be byte-identical to a fresh full \
         render — a mismatch means window B's prior frame leaked into window A"
    );
    assert_eq!(
        (got_b.width, got_b.height),
        (want_b.width, want_b.height),
        "window B readback dims must match the reference"
    );
    assert_eq!(
        got_b.pixels, want_b.pixels,
        "window B's interleaved scissored readback must be byte-identical to a fresh full \
         render — a mismatch means window A's prior frame leaked into window B"
    );
}

#[test]
fn asymmetric_top_pad_change_invalidates_gpu_present_and_readback_caches() {
    let model = aterm_models_render::asymmetric_pad_layout_model();
    let picked = model
        .successors("PickLayout", &model.init_state())
        .into_iter()
        .find(|state| {
            state["pad"] == 3
                && state["head"] == 1
                && state["initial_request"] == 3
                && state["changed_request"] == 0
        })
        .expect("bounded GPU layout fixture");
    let initial = model.successors("ApplyInitialTop", &picked)[0].clone();
    let primed = model.successors("PrimeLayoutCache", &initial)[0].clone();
    let changed = model.successors("ApplyChangedTop", &primed)[0].clone();
    let decided = model.successors("RenderWithLayoutCache", &changed)[0].clone();
    assert_eq!((decided["cache_hit"], decided["full_repaint"]), (0, 1));

    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.set_pad(picked["pad"] as usize);
    gpu.set_head(picked["head"] as usize);
    gpu.set_pad_top(picked["initial_request"] as usize);
    assert_eq!(gpu.pad_top(), initial["pad_top"] as usize);
    gpu.set_pad(picked["pad"] as usize);
    assert_eq!(
        gpu.pad_top(),
        initial["pad_top"] as usize,
        "the GPU delegate must preserve an asymmetric origin on same-value set_pad"
    );
    let (rows, cols) = (4usize, 12usize);
    let frame_size = gpu.frame_size(rows, cols);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lgrid-origin");
    let input = term.cell_frame(rows, cols);
    let mut win = WindowGpu::new();

    let first = gpu.present_input_readback(&mut win, &input);
    let initial_projection = gpu.project_asymmetric_pad_layout(&win);
    assert_eq!(
        initial_projection.present_cached_grid_top,
        Some(primed["cached_grid_top"] as usize)
    );
    assert_eq!(initial_projection.grid_top, initial["grid_top"] as usize);
    let full_before = gpu.full_repaints();
    gpu.set_pad_top(picked["changed_request"] as usize);
    assert_eq!(gpu.frame_size(rows, cols), frame_size);
    let stale_present = gpu.project_asymmetric_pad_layout(&win);
    assert_eq!(stale_present.grid_top, changed["grid_top"] as usize);
    assert_eq!(
        stale_present.present_cached_grid_top,
        Some(changed["cached_grid_top"] as usize)
    );
    assert_ne!(
        stale_present.present_cached_grid_top,
        Some(stale_present.grid_top)
    );

    let dimension_only_mutant_hit = gpu.frame_size(rows, cols) == frame_size;
    assert!(dimension_only_mutant_hit);
    assert_ne!(
        usize::from(dimension_only_mutant_hit),
        decided["cache_hit"] as usize
    );

    let moved = gpu.present_input_readback(&mut win, &input);
    assert!(
        gpu.full_repaints() > full_before,
        "grid-origin change must force a full GPU repaint"
    );
    let moved_projection = gpu.project_asymmetric_pad_layout(&win);
    assert_eq!(
        moved_projection.present_cached_grid_top,
        Some(changed["grid_top"] as usize)
    );
    let mut reference = WindowGpu::new();
    let fresh = gpu.render_input(&mut reference, &input, None);
    assert_eq!(moved.pixels, fresh.pixels);
    assert_ne!(first.pixels, moved.pixels, "the visible grid must move");

    let mut gate_win = WindowGpu::new();
    gpu.set_pad_top(picked["initial_request"] as usize);
    let gated_first = gpu
        .render_input_cached(&mut gate_win, &input)
        .pixels()
        .to_vec();
    let gate_primed = gpu.project_asymmetric_pad_layout(&gate_win);
    assert_eq!(
        gate_primed.gate_cached_grid_top,
        Some(primed["cached_grid_top"] as usize)
    );
    let hits_before = gpu.gate_hits();
    let _ = gpu.render_input_cached(&mut gate_win, &input);
    assert_eq!(gpu.gate_hits(), hits_before + 1, "identical layout may hit");

    gpu.set_pad_top(picked["changed_request"] as usize);
    let stale_gate = gpu.project_asymmetric_pad_layout(&gate_win);
    assert_eq!(stale_gate.grid_top, changed["grid_top"] as usize);
    assert_eq!(
        stale_gate.gate_cached_grid_top,
        Some(changed["cached_grid_top"] as usize)
    );
    let misses_before = gpu.gate_misses();
    let gated_moved = gpu
        .render_input_cached(&mut gate_win, &input)
        .pixels()
        .to_vec();
    assert_eq!(
        gpu.gate_misses(),
        misses_before + 1,
        "origin change must miss the readback gate"
    );
    assert_eq!(gpu.gate_hits(), hits_before + 1);
    assert_eq!(
        gpu.project_asymmetric_pad_layout(&gate_win)
            .gate_cached_grid_top,
        Some(changed["grid_top"] as usize)
    );
    let mut gate_reference = WindowGpu::new();
    let gate_fresh = gpu.render_input(&mut gate_reference, &input, None);
    assert_eq!(gated_moved, gate_fresh.pixels);
    assert_ne!(gated_first, gated_moved);
}

#[test]
fn bloom_glow_frames_scissor_and_match_full_render() {
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    assert!(gpu.bloom_enabled(), "precondition: bloom defaults ON");
    gpu.set_shimmer_phase_for_test(Some(0.33));
    gpu.set_pad(14);
    let pad = gpu.pad();
    let (cw, ch) = gpu.cell_size();
    let (rows, cols) = (10usize, 12usize);

    let frame = |bytes: &[u8], glow: Vec<GlowQuad>| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25l");
        term.process(bytes);
        let mut input = term.cell_frame(rows, cols);
        input.cursor_glow_add = glow;
        input
    };
    let quad = |row: usize, x: usize, y: usize, h: usize| GlowQuad {
        row: row as u16,
        x: (pad + x) as u16,
        y: (pad + row * ch + y) as u16,
        w: cw as u16,
        h: h as u16,
        color: 0x0030_50A0, // premultiplied ADDITIVE light (alpha 0)
        alpha: 0,
        color2: 0x0030_50A0,
        alpha2: 0,
    };
    let inner = |row: usize, dx: usize| quad(row, 2 * cw + dx, 2, ch - 4);
    let seq = [
        (None, frame(b"AAAA", Vec::new())),
        (None, frame(b"AAAA\r\nbbbb", vec![inner(4, 0)])),
        (
            Some(8.0),
            frame(b"AAAA\r\nbbbb", vec![inner(4, 3), inner(5, 1)]),
        ),
        (None, frame(b"AAAA\r\nbbbb", Vec::new())),
        (None, frame(b"AAAA\r\ncccc", vec![quad(0, cw, 1, ch - 2)])),
        (None, frame(b"AAAA\r\ncccc", Vec::new())),
    ];
    let mut win = WindowGpu::new();
    let mut scissored = 0;
    for (i, (radius, input)) in seq.iter().enumerate() {
        if let Some(radius) = radius {
            gpu.set_bloom_params(BLOOM_STRENGTH, *radius);
        }
        let before = gpu.scissor_taken();
        let got = gpu.present_input_readback(&mut win, input);
        scissored += gpu.scissor_taken() - before;
        let mut ref_win = WindowGpu::new();
        let want = gpu.render_input(&mut ref_win, input, None);
        assert_eq!(
            (got.width, got.height, got.pixels == want.pixels),
            (want.width, want.height, true),
            "frame {i}: the scissored bloom present must be byte-identical to \
             a fresh full render (halo ghost, accumulation or old-radius fringe?)"
        );
    }
    assert_eq!(
        scissored as usize,
        seq.len() - 1,
        "every frame after the first must keep the scissored path, not force Full \
         (full_repaints={})",
        gpu.full_repaints(),
    );
}

/// A shrunk window keeps its GL bloom half texture, and the halo drawn through that roomy
/// storage matches a never-resized window's out to the frame's edge.
#[cfg(gl_arm)]
#[test]
fn a_bloom_through_kept_storage_matches_one_never_resized() {
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    assert!(gpu.bloom_enabled(), "precondition: bloom defaults ON");
    let pad = gpu.pad();
    let (cw, ch) = gpu.cell_size();
    let frame = |rows: usize, cols: usize| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lAAAA\r\nbbbb");
        let mut input = term.cell_frame(rows, cols);
        // In the last column, so the halo reaches the half texture's last texels in use.
        input.cursor_glow_add = vec![GlowQuad {
            row: 1,
            x: (pad + (cols - 1) * cw) as u16,
            y: (pad + ch + 2) as u16,
            w: cw as u16,
            h: (ch - 4) as u16,
            color: 0x0030_50A0,
            alpha: 0,
            color2: 0x0030_50A0,
            alpha2: 0,
        }];
        input
    };
    let (large, small) = (frame(14, 20), frame(12, 18));
    let mut shrunk = WindowGpu::new();
    let _ = gpu.present_input_readback(&mut shrunk, &large);
    let got = gpu.present_input_readback(&mut shrunk, &small);
    let half = shrunk
        .gl_bloom_half
        .as_ref()
        .expect("a bloom frame on the GL arm draws its half texture");
    assert!(
        half.roomy(),
        "the shrink kept the larger frame's storage: {half:?}"
    );
    let want = gpu.present_input_readback(&mut WindowGpu::new(), &small);
    assert_eq!((got.width, got.height), (want.width, want.height));
    assert_eq!(
        got.pixels, want.pixels,
        "the halo through kept storage must be the never-resized window's"
    );
}

#[test]
fn tray_upload_skips_unchanged_card_bytes() {
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lhello");
    let input = term.cell_frame(rows, cols);
    let mut win = WindowGpu::new();

    let (pw, ph) = (32u32, 16u32);
    let red: Vec<u8> = [200, 40, 40, 255].repeat((pw * ph) as usize);
    let green: Vec<u8> = [40, 200, 40, 255].repeat((pw * ph) as usize);
    fn tray(rgba: &[u8], pw: u32, ph: u32) -> TrayQuad<'_> {
        TrayQuad {
            rgba,
            pw,
            ph,
            dx: 4,
            dy: 4,
            scrim: None,
            serial: 0,
            delta: None,
            above: &[],
        }
    }

    let first = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph)));
    assert_eq!(gpu.tray_uploads(), 1, "first card present must upload");

    let second = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph)));
    assert_eq!(
        gpu.tray_uploads(),
        1,
        "identical card bytes must skip the write_texture re-upload"
    );
    assert_eq!(
        first.pixels, second.pixels,
        "the skipped upload must not change the composited output"
    );

    let third = gpu.render_input(&mut win, &input, Some(tray(&green, pw, ph)));
    assert_eq!(gpu.tray_uploads(), 2, "changed card bytes must upload");
    assert_ne!(
        second.pixels, third.pixels,
        "the new card must actually reach the frame"
    );

    let swapped = TrayQuad {
        rgba: &green,
        pw: ph,
        ph: pw,
        dx: 4,
        dy: 4,
        scrim: None,
        serial: 0,
        delta: None,
        above: &[],
    };
    let _ = gpu.render_input(&mut win, &input, Some(swapped));
    assert_eq!(gpu.tray_uploads(), 3, "a recreate must always upload");
}

/// A tracked card's serial spares the compare: the same serial draws the card it
/// holds, a band counted from it uploads that band alone, and only an untracked card (or a
/// band from a serial it does not hold) is compared; a new size makes a new card.
#[cfg(target_os = "macos")]
#[test]
fn a_tray_card_update_compares_only_what_it_cannot_name() {
    let (pw, ph) = (8u32, 6u32);
    let rgba = vec![0u8; (pw * ph * 4) as usize];
    let quad = |serial, delta| TrayQuad {
        rgba: &rgba,
        pw,
        ph,
        dx: 0,
        dy: 0,
        scrim: None,
        serial,
        delta,
        above: &[],
    };
    let delta = |since, rows| Some(TrayDelta { since, rows });
    let card = Some((pw, ph, 7));
    let full = u64::from(pw * ph * 4);
    let mut compared = 0;
    // What each case's compare finds; `None`: the case must not compare (a debug build
    // compares a named band only to check it, uncounted).
    type Case<'a> = (
        Option<(u32, u32, u64)>,
        TrayQuad<'a>,
        Option<Option<(u32, u32)>>,
        TrayCardUpdate,
        u64,
    );
    let cases: [Case<'_>; 7] = [
        (card, quad(7, None), None, TrayCardUpdate::Keep, 0),
        (
            card,
            quad(8, delta(7, (1, 3))),
            Some(Some((1, 2))),
            TrayCardUpdate::Rows(1, 3),
            0,
        ),
        (
            card,
            quad(8, delta(7, (2, 99))),
            Some(Some((2, 6))),
            TrayCardUpdate::Rows(2, ph),
            0,
        ),
        (
            card,
            quad(8, delta(7, (2, 2))),
            Some(None),
            TrayCardUpdate::Keep,
            0,
        ),
        (
            card,
            quad(8, delta(6, (1, 3))),
            Some(Some((0, 1))),
            TrayCardUpdate::Rows(0, 1),
            full,
        ),
        (
            card,
            quad(0, None),
            Some(Some((0, 1))),
            TrayCardUpdate::Rows(0, 1),
            full,
        ),
        (
            Some((pw, ph + 1, 7)),
            quad(7, None),
            None,
            TrayCardUpdate::Whole,
            0,
        ),
    ];
    for (i, (resident, t, finds, want, bytes)) in cases.into_iter().enumerate() {
        let before = compared;
        let compare = || finds.unwrap_or_else(|| panic!("case {i} compares"));
        assert_eq!(
            tray_card_update(resident, &t, compare, &mut compared),
            want,
            "case {i}"
        );
        assert_eq!(compared - before, bytes, "case {i}: bytes compared");
    }
    assert_eq!(
        tray_card_update(
            None,
            &quad(7, None),
            || panic!("no card compares"),
            &mut compared
        ),
        TrayCardUpdate::Whole,
        "no card yet"
    );
}

/// By the renderer's counters: a card presented again under its serial is neither
/// compared nor uploaded, a change counted from the serial the window holds uploads its
/// band (after the ring's second slot is minted whole), and the frames show each card.
#[cfg(target_os = "macos")]
#[test]
fn an_unchanged_card_is_neither_compared_nor_uploaded() {
    if crate::metal::ffi::Device::preferred().is_none() {
        aterm_spec::host::could_not_run("no Metal device");
    }
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lhello");
    let input = term.cell_frame(rows, cols);
    let mut win = WindowGpu::new();
    let (pw, ph) = (32u32, 16u32);
    let stride = u64::from(pw * 4);
    let red: Vec<u8> = [200, 40, 40, 255].repeat((pw * ph) as usize);
    let mut banded = red.clone();
    for px in banded[(4 * stride) as usize..(8 * stride) as usize]
        .as_chunks_mut::<4>()
        .0
    {
        *px = [40, 200, 40, 255];
    }
    fn tray(rgba: &[u8], pw: u32, ph: u32, serial: u64, delta: Option<TrayDelta>) -> TrayQuad<'_> {
        TrayQuad {
            rgba,
            pw,
            ph,
            dx: 4,
            dy: 4,
            scrim: None,
            serial,
            delta,
            above: &[],
        }
    }
    let band = |since| {
        Some(TrayDelta {
            since,
            rows: (4, 8),
        })
    };
    let counters = |gpu: &GpuRenderer| {
        (
            gpu.tray_uploads(),
            gpu.tray_upload_bytes(),
            gpu.tray_compare_bytes(),
        )
    };
    let full = stride * u64::from(ph);

    let first = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph, 7, None)));
    assert!(gpu.metal_render_armed(), "the frame took the Metal arm");
    assert_eq!(counters(&gpu), (1, full, 0), "the first card uploads whole");
    for again in 0..3 {
        let same = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph, 7, None)));
        assert_eq!(counters(&gpu), (1, full, 0), "present {again} of serial 7");
        assert_eq!(
            same.pixels, first.pixels,
            "present {again} draws the card it holds"
        );
    }
    let changed = gpu.render_input(&mut win, &input, Some(tray(&banded, pw, ph, 8, band(7))));
    assert_eq!(
        counters(&gpu),
        (2, 2 * full, 0),
        "the ring's second slot is minted and filled whole"
    );
    assert_ne!(
        changed.pixels, first.pixels,
        "CONTROL: the band reaches the frame"
    );
    let back = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph, 9, band(8))));
    assert_eq!(
        counters(&gpu),
        (3, 2 * full + 4 * stride, 0),
        "a change counted from the serial held uploads its four rows"
    );
    assert_eq!(
        back.pixels, first.pixels,
        "the band alone restores the card"
    );
    let _ = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph, 0, None)));
    assert_eq!(
        counters(&gpu),
        (3, 2 * full + 4 * stride, full),
        "CONTROL: an untracked card is compared whole"
    );
}

/// Asking whether the renderer folds a tray stack never waits on the tray prims' shader
/// compile: the first ask starts the build off this thread and says no while it is under
/// way (the caller folds on the CPU), and once it lands the renderer stacks. A renderer that
/// compiled on the ask (the mutant) would not return while the build is held, which the
/// hang detector ends.
#[cfg(target_os = "macos")]
#[test]
fn the_tray_stack_never_waits_for_the_tray_prims_build() {
    use std::sync::atomic::{AtomicBool, Ordering};
    if crate::metal::ffi::Device::preferred().is_none_or(|d| !crate::metal::fetch::capable(&d)) {
        aterm_spec::host::could_not_run("no Metal device with framebuffer fetch");
    }
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    gpu.set_tray_prim_law(&crate::tray_prims::TrayPrimLaw {
        to_linear: [0.0; 256],
        unit: [0.0; 256],
        thresholds: [0.0; 256],
    });
    let hold = std::sync::Arc::new(std::sync::Mutex::new(Some(
        crate::metal::tray_prims::held::hold(),
    )));
    let hung = std::sync::Arc::new(AtomicBool::new(false));
    let detector = {
        let (hold, hung) = (std::sync::Arc::clone(&hold), std::sync::Arc::clone(&hung));
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(60));
            if let Some(h) = hold.lock().unwrap_or_else(|e| e.into_inner()).take() {
                hung.store(true, Ordering::SeqCst);
                drop(h);
            }
        })
    };
    let first = gpu.tray_stack_supported();
    let again = gpu.tray_stack_supported();
    assert!(
        !hung.load(Ordering::SeqCst),
        "the ask waited on the held build"
    );
    assert!(!first && !again, "under way, the renderer folds no stack");
    drop(hold.lock().unwrap_or_else(|e| e.into_inner()).take());
    assert!(gpu.tray_prims_supported(), "the build lands");
    assert!(gpu.tray_stack_supported(), "and then the renderer stacks");
    drop(detector);
}

/// A band whose ring-slot mint fails leaves the card holding the serial its pixels are:
/// the same quad presented again uploads the band and shows it, where a card that claimed
/// the lost serial would keep drawing the old bytes.
#[cfg(target_os = "macos")]
#[test]
fn a_failed_ring_mint_leaves_the_card_on_the_serial_it_holds() {
    if crate::metal::ffi::Device::preferred().is_none() {
        aterm_spec::host::could_not_run("no Metal device");
    }
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lhello");
    let input = term.cell_frame(rows, cols);
    let (pw, ph) = (32u32, 16u32);
    let stride = (pw * 4) as usize;
    let red: Vec<u8> = [200, 40, 40, 255].repeat((pw * ph) as usize);
    let mut banded = red.clone();
    for px in banded[4 * stride..8 * stride].as_chunks_mut::<4>().0 {
        *px = [40, 200, 40, 255];
    }
    fn tray(rgba: &[u8], pw: u32, ph: u32, serial: u64) -> TrayQuad<'_> {
        TrayQuad {
            rgba,
            pw,
            ph,
            dx: 4,
            dy: 4,
            scrim: None,
            serial,
            delta: (serial == 8).then_some(TrayDelta {
                since: 7,
                rows: (4, 8),
            }),
            above: &[],
        }
    }
    let want = gpu
        .render_input(
            &mut WindowGpu::new(),
            &input,
            Some(tray(&banded, pw, ph, 1)),
        )
        .pixels;

    let mut win = WindowGpu::new();
    let first = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph, 7)));
    assert_ne!(first.pixels, want, "CONTROL: the band changes the frame");
    INJECTED_TRAY_RING_MINT_FAILURE.with(|f| f.set(true));
    let _lost = gpu.render_input(&mut win, &input, Some(tray(&banded, pw, ph, 8)));
    assert!(
        !INJECTED_TRAY_RING_MINT_FAILURE.with(std::cell::Cell::get),
        "the band's present reached the ring-slot mint"
    );
    let uploads = gpu.tray_uploads();
    let again = gpu.render_input(&mut win, &input, Some(tray(&banded, pw, ph, 8)));
    assert_eq!(
        gpu.tray_uploads(),
        uploads + 1,
        "the serial the failed present lost is uploaded again"
    );
    assert_eq!(
        again.pixels, want,
        "the card shows the band once the mint succeeds"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_tray_card_drawn_and_waited_inline_records_no_reader() {
    if crate::metal::ffi::Device::preferred_listed().is_none() {
        aterm_spec::host::could_not_run("no Metal device");
    }
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lhello");
    let input = term.cell_frame(rows, cols);
    let mut win = WindowGpu::new();
    let (pw, ph) = (32u32, 16u32);
    let cards: Vec<Vec<u8>> = [[200, 40, 40, 255], [40, 200, 40, 255], [40, 40, 200, 255]]
        .iter()
        .map(|rgba| rgba.repeat((pw * ph) as usize))
        .collect();
    fn tray(rgba: &[u8], pw: u32, ph: u32) -> TrayQuad<'_> {
        TrayQuad {
            rgba,
            pw,
            ph,
            dx: 4,
            dy: 4,
            scrim: None,
            serial: 0,
            delta: None,
            above: &[],
        }
    }
    let readers = |win: &WindowGpu| {
        let card = win.metal_tray_card.as_ref().expect("the armed card");
        (0..crate::tray_card_ring::TRAY_CARD_SLOTS)
            .filter_map(|slot| card.ring.slot(slot))
            .map(|slot| slot.last_read)
            .collect::<Vec<_>>()
    };
    for (i, card) in cards.iter().cycle().take(4).enumerate() {
        let _ = gpu.render_input(&mut win, &input, Some(tray(card, pw, ph)));
        assert!(gpu.metal_render_armed(), "the frame took the Metal arm");
        assert!(
            readers(&win).iter().all(Option::is_none),
            "bake {i}: {:?}",
            readers(&win)
        );
    }
    assert_eq!(
        readers(&win).len(),
        2,
        "the ring rotated through both slots"
    );
    for (i, card) in cards.iter().take(2).enumerate() {
        assert!(gpu.present_virtual(&mut win, &input, false, None, Some(tray(card, pw, ph))));
        assert!(
            readers(&win).iter().all(Option::is_none),
            "virtual present {i}: {:?}",
            readers(&win)
        );
    }
}

#[test]
fn fractional_scroll_keeps_tray_pixels_pinned() {
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lrow zero\r\nrow one\r\nrow two\r\nrow three");
    let mut input = term.cell_frame(rows, cols);
    input.grid_top_row = 1;
    input.grid_bot_row = rows - 1;
    let (cw, ch) = gpu.cell_size();
    let grid_top = gpu.cpu.grid_top();
    let (pw, ph) = ((cw * 3) as u32, ch.saturating_sub(4).max(4) as u32);
    let (dx, dy) = (cw as u32, (grid_top + ch * 2 + 2) as u32);
    let magenta: Vec<u8> = [220, 30, 180, 255].repeat((pw * ph) as usize);
    let tray = || TrayQuad {
        rgba: &magenta,
        pw,
        ph,
        dx,
        dy,
        scrim: None,
        serial: 0,
        delta: None,
        above: &[],
    };

    input.scroll_frac_px = 0;
    let mut zero_win = WindowGpu::new();
    let zero = gpu.render_input(&mut zero_win, &input, Some(tray()));
    input.scroll_frac_px = 3;
    let mut shifted_win = WindowGpu::new();
    let shifted = gpu.render_input(&mut shifted_win, &input, Some(tray()));
    assert_eq!((zero.width, zero.height), (shifted.width, shifted.height));

    for y in dy as usize..(dy + ph) as usize {
        let x0 = y * zero.width + dx as usize;
        let x1 = x0 + pw as usize;
        assert_eq!(
            &shifted.pixels[x0..x1],
            &zero.pixels[x0..x1],
            "tray row {y} moved with the terminal scroll band"
        );
    }
    assert_ne!(
        shifted.pixels, zero.pixels,
        "negative control: terminal pixels outside the pinned tray must move"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_failed_first_frame_warm_leaves_the_inline_mint_and_its_failure_route() {
    let dev = crate::metal::ffi::Device::preferred_listed()
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"));
    static LATCH: crate::metal::warm::WarmLatch =
        crate::metal::warm::WarmLatch::new(crate::metal::warm::DEADLINE);
    let renderer = || {
        let mut g = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        g.arm_metal_for_test();
        g.set_first_frame_latch_for_test(&LATCH);
        g
    };
    let mut gpu = renderer();
    let mut warm = LATCH.begin().expect("the latch's first warm");
    let failed = warm.run_on(
        &dev,
        crate::metal::warm::FirstFrameKnobs::default(),
        |_, _| Err("injected warm failure".to_owned()),
    );
    assert_eq!(failed, Err("injected warm failure".to_owned()));
    drop(warm);
    assert!(
        gpu.metal_arm_ready(),
        "the mint after a failed warm compiles inline: {}",
        gpu.metal_start_error()
    );
    assert!(gpu.metal_render_armed());

    let mut gpu = renderer();
    INJECTED_MINT_FAILURE.with(|f| f.set(Some("injected mint failure")));
    assert!(!gpu.metal_arm_ready(), "a failed mint is not ready");
    assert!(!gpu.metal_armed, "a failed mint disarms");
    assert_eq!(
        gpu.metal_start_error(),
        "the Metal renderer could not start: injected mint failure"
    );
}

/// A small text frame lit by one cursor-aurora quad: Unorm-law light for the route to place.
#[cfg(target_os = "macos")]
fn glow_over_text(gpu: &GpuRenderer) -> RenderInput {
    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"glow over text");
    let mut input = term.cell_frame(rows, cols);
    let (cw, ch) = gpu.cpu.cell_size();
    input.cursor_glow_add.push(GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: (4 * cw) as u16,
        h: ch as u16,
        color: 0x0040_6080,
        alpha: 0,
        color2: 0x0040_6080,
        alpha2: 0,
    });
    input
}

/// One [`glow_over_text`] frame on a fresh armed renderer: its shape and the fetch twins built.
#[cfg(target_os = "macos")]
fn glow_frame_on(setup: impl FnOnce(&mut GpuRenderer)) -> (FrameShape, usize) {
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    setup(&mut gpu);
    let input = glow_over_text(&gpu);
    let mut win = WindowGpu::new();
    let _ = gpu.render_input(&mut win, &input, None);
    assert!(gpu.metal_render_armed(), "the frame minted the Metal arm");
    let twins = gpu
        .metal_built_for_test()
        .into_iter()
        .filter(|item| matches!(item, crate::metal::warm::WarmItem::Twin(_)))
        .count();
    (gpu.frame_shape(), twins)
}

/// A device whose fetch verdict is anything but Passed draws its Unorm-law light on the split
/// whatever route the renderer asks for, building no twin.
#[cfg(target_os = "macos")]
#[test]
fn a_device_that_did_not_pass_the_probe_draws_its_unorm_light_on_the_split() {
    let dev = crate::metal::ffi::Device::preferred_listed()
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"));
    let failed = FetchProbe::Failed {
        stage: crate::FetchStage::InPass,
        channel: 2,
        want: 9,
        got: 10,
    };
    for verdict in [failed, FetchProbe::Refused, FetchProbe::Unsupported] {
        let (shape, twins) = glow_frame_on(|gpu| {
            gpu.set_unorm_route(UnormRoute::Fetch);
            gpu.set_fetch_probe_seam(Some(verdict));
        });
        assert_eq!(shape.route, UnormRoute::Split, "{verdict:?}");
        assert!(
            shape.unorm_draws > 0 && shape.passes > 1,
            "{verdict:?}: the light draws on the Unorm view, {} passes",
            shape.passes
        );
        assert_eq!(twins, 0, "{verdict:?}: no twin is built");
    }
    if !crate::metal::fetch::capable(&dev) {
        aterm_spec::host::could_not_run("the device has no framebuffer fetch");
    }
    let (shape, twins) = glow_frame_on(|gpu| {
        gpu.set_unorm_route(UnormRoute::Fetch);
        gpu.set_fetch_probe_seam(Some(FetchProbe::Passed));
    });
    assert_eq!(
        (shape.route, shape.passes),
        (UnormRoute::Fetch, 1),
        "the same frame under a passed verdict takes the fetch: the verdict kept the split"
    );
    assert!(twins > 0);
}

/// An unconfigured renderer routes Unorm-law light through fetch, every Apple GPU has fetch,
/// and on a device that passes its probe the default glow frame is one pass.
#[cfg(target_os = "macos")]
#[test]
fn the_default_route_is_the_fetch_and_this_device_takes_it() {
    use std::ffi::OsStr;
    assert_eq!(UnormRoute::initial(None), UnormRoute::Fetch);
    assert_eq!(
        UnormRoute::initial(Some(OsStr::new("split"))),
        UnormRoute::Split
    );
    assert_eq!(
        UnormRoute::initial(Some(OsStr::new("fetch"))),
        UnormRoute::Fetch
    );
    let dev = crate::metal::ffi::Device::preferred_listed()
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"));
    if cfg!(target_arch = "aarch64") {
        assert!(
            crate::metal::fetch::capable(&dev),
            "{}: an Apple GPU has framebuffer fetch",
            dev.name()
        );
    } else if !crate::metal::fetch::capable(&dev) {
        aterm_spec::host::could_not_run("the device has no framebuffer fetch");
    }
    if aterm_types::dev_seam!("ATERM_GPU_UNORM_ROUTE").is_some() {
        aterm_spec::host::could_not_run("ATERM_GPU_UNORM_ROUTE chooses the route here");
    }
    let (shape, twins) = glow_frame_on(|_| {});
    assert_eq!(
        (shape.route, shape.passes),
        (UnormRoute::Fetch, 1),
        "{}: the default glow frame",
        dev.name()
    );
    assert!(twins > 0, "it drew through a twin");
}

/// An armed renderer with every present pass on (bloom, shimmer, both crowns, a window on
/// screen that may present f16), routing Unorm-law light through framebuffer fetch.
#[cfg(target_os = "macos")]
fn every_pass_on() -> GpuRenderer {
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    gpu.metal_on_screen = true;
    gpu.set_unorm_route(UnormRoute::Fetch);
    gpu.set_bloom(true);
    gpu.set_bloom_params(0.85, 2.2);
    gpu.set_shimmer(true);
    gpu.set_hdr_glow(true);
    gpu.set_sdr_glow_boost(0.25);
    gpu
}

/// A default window's first frames on `gpu`, presented to a fresh `format` swapchain on a
/// screen of `edr` headroom: a blank grid, the first prompt (cursor aurora and a Claude card's
/// fx layer), then the prompt with an emoji.
#[cfg(target_os = "macos")]
fn present_first_frames(gpu: &mut GpuRenderer, format: crate::device_layer::TexelFormat, edr: f32) {
    let (rows, cols) = (24usize, 80usize);
    let mut win = WindowGpu::new();
    win.set_edr_max(edr);
    let blank = Terminal::new(rows as u16, cols as u16).cell_frame(rows, cols);
    let (cw, ch) = gpu.cpu.cell_size();
    let card = aterm_core::render::FxLayer::new(
        1,
        std::sync::Arc::new(aterm_core::render::FxContent::new(
            vec![aterm_core::render::FxItem::Rect(
                aterm_core::render::FxRect {
                    x: 0,
                    y: (2 * ch) as i32,
                    w: (20 * cw) as u16,
                    h: 2,
                    ink: 0,
                },
            )],
            vec![
                aterm_core::render::FxInk::add(0x0020_4060),
                aterm_core::render::FxInk::add(0x0060_4020),
            ],
        )),
        aterm_core::render::FxSlot::Under,
        aterm_core::render::FxSpace::Window,
        aterm_core::render::FxBlend::Exact,
    );
    let prompt = |text: &str| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(text.as_bytes());
        let mut input = term.cell_frame(rows, cols);
        input.cursor_glow_add.push(aterm_render::GlowQuad {
            row: 0,
            x: (14 * cw) as u16,
            y: 0,
            w: (2 * cw) as u16,
            h: ch as u16,
            color: 0x0040_6080,
            alpha: 0,
            color2: 0x0040_6080,
            alpha2: 0,
        });
        input.fx_layers.push(card.clone());
        input
    };
    let (fw, fh) = gpu.frame_size(rows, cols);
    for input in [&blank, &prompt("user@host ~ % "), &prompt("\u{1f680} ~ % ")] {
        gpu.metal_present_standalone_for_test(
            &mut win,
            input,
            (fw as u32, fh as u32),
            (false, true),
            format,
            |_, _| Ok(()),
        )
        .expect("the armed present");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_first_frame_warm_builds_what_a_default_windows_first_frames_build() {
    use crate::device_layer::TexelFormat;
    use crate::metal::warm::{FirstFrameKnobs, WarmItem, first_frame_rows, first_frame_twins};
    if crate::metal::ffi::Device::preferred_listed().is_none() {
        aterm_spec::host::could_not_run("no Metal device");
    }
    let mut built: Vec<WarmItem> = Vec::new();
    let mut fetch = false;
    for (format, edr) in [
        (TexelFormat::Bgra8Unorm, 1.0),
        (TexelFormat::Rgba16Float, 2.0),
    ] {
        let mut gpu = every_pass_on();
        present_first_frames(&mut gpu, format, edr);
        for item in gpu.metal_built_for_test() {
            if !built.contains(&item) {
                built.push(item);
            }
        }
        fetch = gpu.fetch_probe() == Some(FetchProbe::Passed);
    }
    let knobs = FirstFrameKnobs {
        aurora: true,
        bloom: true,
        hdr_glow: true,
        sdr_glow: true,
        fx: true,
    };
    let mut warmed: Vec<WarmItem> = Vec::new();
    for (row, present) in first_frame_rows(knobs, fetch) {
        for item in [
            WarmItem::Library(row.spec().library),
            WarmItem::pipeline(row, present),
        ] {
            if !warmed.contains(&item) {
                warmed.push(item);
            }
        }
    }
    warmed.extend(
        first_frame_twins(knobs, fetch)
            .into_iter()
            .map(WarmItem::Twin),
    );
    let missed: Vec<&WarmItem> = built.iter().filter(|i| !warmed.contains(i)).collect();
    let idle: Vec<&WarmItem> = warmed.iter().filter(|i| !built.contains(i)).collect();
    assert!(
        missed.is_empty() && idle.is_empty(),
        "the first frames built {built:?}\nthe warm builds {warmed:?}\nmissed by the \
         warm: {missed:?}\nbuilt by the warm for nothing: {idle:?}"
    );
}

/// A warm asked for before the arm minted starts from the mint and hands over every row and
/// twin, so a default window's first frames build no pipeline on the render thread; without
/// the warm they build their own.
#[cfg(target_os = "macos")]
#[test]
fn a_warmed_arm_builds_no_pipeline_on_the_render_thread() {
    use crate::device_layer::TexelFormat;
    use crate::metal::warm::{PresentKnobs, bindable};
    if crate::metal::ffi::Device::preferred_listed().is_none() {
        aterm_spec::host::could_not_run("no Metal device");
    }
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);
    let on = PresentKnobs {
        bloom: true,
        shimmer: true,
        hdr_glow: true,
        sdr_glow: true,
    };
    for (format, edr) in [
        (TexelFormat::Bgra8Unorm, 1.0),
        (TexelFormat::Rgba16Float, 2.0),
    ] {
        let mut gpu = every_pass_on();
        gpu.warm_effect_pipelines();
        assert!(
            gpu.metal_arm_ready(),
            "the mint: {}",
            gpu.metal_start_error()
        );
        let twins = gpu.fetch_probe().is_none_or(|v| v == FetchProbe::Passed);
        assert_eq!(
            gpu.metal_built_for_test(),
            vec![crate::metal::warm::WarmItem::Library(ShaderLibrary::Cell)],
            "the mint built nothing"
        );
        assert_eq!(
            gpu.await_pipeline_warm_for_test(HANG),
            Vec::new(),
            "builds the warm failed"
        );
        present_first_frames(&mut gpu, format, edr);
        let held = gpu.metal_built_for_test();
        let untaken: Vec<_> = bindable(on, twins)
            .into_iter()
            .filter(|item| !held.contains(item))
            .collect();
        assert!(
            untaken.is_empty(),
            "the frames' first miss took every build the warm made: {untaken:?} untaken"
        );
        assert_eq!(
            gpu.metal_built_here_for_test(),
            Vec::new(),
            "{format:?}: built on the render thread after the warm"
        );

        let mut cold = every_pass_on();
        present_first_frames(&mut cold, format, edr);
        assert!(
            !cold.metal_built_here_for_test().is_empty(),
            "{format:?}: without the warm the frames build their own"
        );
    }
}

/// A pass whose switch was off when the warm ran is warmed when the switch turns it on, and
/// only then.
#[cfg(target_os = "macos")]
#[test]
fn a_pass_switched_on_after_the_warm_is_warmed_then() {
    use crate::metal::ffi::PixelFormat;
    use crate::metal::warm::WarmItem;
    if crate::metal::ffi::Device::preferred_listed().is_none() {
        aterm_spec::host::could_not_run("no Metal device");
    }
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);
    let shimmer = WarmItem::pipeline(Pipeline::Shimmer, PixelFormat::Rgba8Unorm);
    let mut gpu = every_pass_on();
    gpu.set_shimmer(false);
    gpu.warm_effect_pipelines();
    assert!(
        gpu.metal_arm_ready(),
        "the mint: {}",
        gpu.metal_start_error()
    );
    assert!(
        !gpu.metal_warm_owed_for_test().is_empty()
            && !gpu.metal_warm_owed_for_test().contains(&shimmer),
        "the warm left the shimmer it found off"
    );
    gpu.set_shimmer(false);
    assert!(!gpu.metal_warm_owed_for_test().contains(&shimmer));
    gpu.set_shimmer(true);
    assert!(
        gpu.metal_warm_owed_for_test().contains(&shimmer),
        "turning the shimmer on warms it"
    );
    assert_eq!(gpu.await_pipeline_warm_for_test(HANG), Vec::new());
}

#[cfg(target_os = "macos")]
#[test]
fn armed_present_composes_the_stale_rect_and_matches_a_full_copy() {
    if crate::metal::ffi::Device::preferred_listed().is_none() {
        aterm_spec::host::could_not_run("no Metal device");
    }
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lrow zero\r\nrow one\r\nrow two\r\nrow three");
    let glide_input = |term: &mut Terminal| {
        let mut input = term.cell_frame(rows, cols);
        input.grid_top_row = 1;
        input.grid_bot_row = rows - 1;
        input.scroll_frac_px = 3;
        input
    };
    let mut input = glide_input(&mut term);
    let mut win = WindowGpu::new();
    input.scroll_frac_px = 0;
    let first = gpu.render_input(&mut win, &input, None);
    assert!(
        gpu.metal_render_armed(),
        "the first frame must mint the arm"
    );
    let (fw, fh) = (first.width as u32, first.height as u32);
    let dest = (fw + 9, fh + 7);
    input.scroll_frac_px = 3;

    let primed = gpu
        .metal_present_bytes_with_scope_for_test(&mut win, &input, dest, false, true)
        .expect("armed present");
    assert_eq!(
        win.metal_last_compose_rect,
        Some([0, 0, fw, fh]),
        "the first sync into a fresh copy is the full frame"
    );
    let mut flat = input.clone();
    flat.scroll_frac_px = 0;
    let flat = gpu
        .metal_present_bytes_with_scope_for_test(&mut WindowGpu::new(), &flat, dest, false, true)
        .expect("armed present");
    assert_ne!(
        primed, flat,
        "a frac 3 present must move terminal pixels on glass"
    );

    term.process(b"\x1b[3;1HX");
    let changed = glide_input(&mut term);
    let scissors_before = gpu.scissor_taken();
    let incremental = gpu
        .metal_present_bytes_with_scope_for_test(&mut win, &changed, dest, false, false)
        .expect("armed present");
    assert!(
        gpu.scissor_taken() > scissors_before,
        "non-vacuity: the second armed encode must take the scissor path"
    );
    let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
        gpu.cpu.grid_top(),
        gpu.cell_size().1,
        1,
        rows - 1,
        fh as usize,
    );
    let band = [0, y0 as u32, fw, y1 as u32];
    assert!(
        band[3] - band[1] < fh,
        "fixture: the band {band:?} must be smaller than the {fw}x{fh} frame"
    );
    assert_eq!(
        win.metal_last_compose_rect,
        Some(band),
        "the compose must re-copy the stale union (the previous shift's band, \
         which contains the dirty row), never the whole {fw}x{fh} frame"
    );
    assert_ne!(
        incremental, primed,
        "negative control: the changed cell must reach the glass"
    );

    let mut fresh = WindowGpu::new();
    let oracle = gpu
        .metal_present_bytes_with_scope_for_test(&mut fresh, &changed, dest, false, true)
        .expect("armed present");
    assert_eq!(
        fresh.metal_last_compose_rect,
        Some([0, 0, fw, fh]),
        "fixture: the oracle is a full-frame compose"
    );
    assert_eq!(
        incremental, oracle,
        "the stale-rect compose must present the full-copy bytes"
    );

    assert_eq!(
        win.offscreen_dirty_since_sync,
        Some([0, 0, 0, 0]),
        "fixture: the compose just synced the copy"
    );
    gpu.shift_offscreen_rows(&mut win, 1, rows);
    assert_eq!(
        win.offscreen_dirty_since_sync, None,
        "the armed E7 shift must invalidate the incremental sync"
    );
}

/// A present whose one effect is the tray card composes the offscreen into the present copy
/// by a draw in the tray's pass, so it opens no blit encoder (a blit wakes a second class of
/// the GPU driver's working memory, kept resident while the GPU stays busy), and the copy is
/// texel-exact: outside the card the glass is the bare frame's, on the first full sync and
/// on an incremental one.
#[cfg(target_os = "macos")]
#[test]
fn a_tray_present_composes_by_draw_with_no_blit_and_every_texel_kept() {
    tray_present_composes_by_draw_at(1.0);
}

/// On a translucent window the compose keeps the offscreen's alpha: outside the card the
/// glass is the bare translucent frame's, alpha and all, so a deck never makes the window
/// opaque.
#[cfg(target_os = "macos")]
#[test]
fn a_translucent_tray_present_keeps_the_offscreen_alpha() {
    tray_present_composes_by_draw_at(0.5);
}

/// The compose draw `read`s the offscreen at every texel of its rect, so a rect that leaves
/// the offscreen is refused before anything encodes, as the blit it replaced refused it; one
/// inside both textures encodes.
#[cfg(target_os = "macos")]
#[test]
fn a_compose_rect_past_the_offscreen_is_refused() {
    use crate::device_layer::{TexUsage, TexelFormat};
    use crate::metal::present::{PostPass, PresentSequence, encode_present_sequence};
    if crate::metal::ffi::Device::preferred_listed().is_none() {
        aterm_spec::host::could_not_run("no Metal device");
    }
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    let input = Terminal::new(2, 4).cell_frame(2, 4);
    let _ = gpu.render_input(&mut WindowGpu::new(), &input, None);
    assert!(
        gpu.metal_render_armed(),
        "fixture: the frame minted the arm"
    );
    let live = gpu.metal_arm.as_mut().expect("armed").live_mut();
    let mint = |live: &MetalArmLive, label, format, side: u32| {
        live.layer()
            .try_create_texture_2d(label, format, side, side, TexUsage::OFFSCREEN, None)
            .map(crate::device_layer::LayerTexture::into_metal)
            .expect("a scratch texture")
    };
    let offscreen = mint(live, "test offscreen", TexelFormat::Rgba8Unorm, 8);
    let copy = mint(live, "test present copy", TexelFormat::Rgba8Unorm, 16);
    let dest = mint(live, "test drawable", TexelFormat::Bgra8Unorm, 16);
    let compose_pso = live
        .present_pso(Pipeline::Blit, crate::metal::ffi::PixelFormat::Rgba8Unorm)
        .expect("the compose row");
    let blit_pso = live
        .present_pso(Pipeline::Blit, crate::metal::ffi::PixelFormat::Bgra8Unorm)
        .expect("the present row");
    let uniform = live.ensure_compose_copy_u().expect("the compose uniform");
    let binds = Pipeline::Blit.spec().binds;
    let post = |pso, tex| PostPass {
        pso,
        tex,
        tex_slot: binds.fragment_textures[0] as usize,
        sampler: &live.nearest,
        sampler_slot: binds.fragment_samplers[0] as usize,
        uniform: &uniform,
        uniform_slot: binds.fragment_buffers[0] as usize,
    };
    let encode = |copy_rect| {
        let seq = PresentSequence {
            offscreen: &offscreen,
            present_off: Some(&copy),
            copy_rect,
            compose: Some(post(&compose_pso, offscreen.obj())),
            bloom: None,
            shimmer: None,
            band_shift: None,
            apron: None,
            stack: None,
            tray: None,
            blit: post(&blit_pso, copy.obj()),
            clear: crate::metal::ffi::ClearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            crown: None,
        };
        let mut cb = live.session.begin().expect("a command buffer");
        let blits_before = crate::metal::encoder::blit_census::opened();
        let out = encode_present_sequence(&mut cb, &dest, (16, 16), &seq);
        assert_eq!(
            crate::metal::encoder::blit_census::opened(),
            blits_before,
            "the compose blitted"
        );
        out
    };
    let refused = encode([0, 0, 16, 16]).expect_err("a rect past the 8x8 offscreen");
    assert!(
        refused.contains("leaves the 8x8 offscreen"),
        "the refusal names the offscreen: {refused}"
    );
    encode([0, 0, 8, 8]).expect("negative control: a rect inside both textures encodes");
}

/// [`a_tray_present_composes_by_draw_with_no_blit_and_every_texel_kept`] at `opacity`,
/// presented translucent below 1.
#[cfg(target_os = "macos")]
fn tray_present_composes_by_draw_at(opacity: f32) {
    use crate::device_layer::TexelFormat;
    if crate::metal::ffi::Device::preferred_listed().is_none() {
        aterm_spec::host::could_not_run("no Metal device");
    }
    let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    gpu.set_background_opacity(opacity);
    let translucent = opacity < 1.0;
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25lrow zero\r\nrow one\r\nrow two\r\nrow three");
    let input = term.cell_frame(rows, cols);
    let probe = gpu.render_input(&mut WindowGpu::new(), &input, None);
    assert!(gpu.metal_render_armed(), "the frame took the Metal arm");
    let dest = (probe.width as u32, probe.height as u32);
    let (pw, ph, dx, dy) = (24u32, 10u32, 3u32, 2u32);
    let card: Vec<u8> = [30, 200, 120, 255].repeat((pw * ph) as usize);
    let tray = || {
        Some(TrayQuad {
            rgba: &card,
            pw,
            ph,
            dx,
            dy,
            scrim: None,
            serial: 0,
            delta: None,
            above: &[],
        })
    };
    let present = |gpu: &mut GpuRenderer,
                   win: &mut WindowGpu,
                   input: &RenderInput,
                   with_tray: bool,
                   full: bool| {
        gpu.metal_present_standalone_with_tray_for_test(
            win,
            input,
            dest,
            (translucent, full),
            (
                TexelFormat::Bgra8Unorm,
                if with_tray { tray() } else { None },
            ),
            |gpu, target| gpu.metal_read_bgra_target_for_test(target, dest),
        )
        .expect("armed present")
    };
    let in_card = |i: usize| {
        let (x, y) = ((i % dest.0 as usize) as u32, (i / dest.0 as usize) as u32);
        (dx..dx + pw).contains(&x) && (dy..dy + ph).contains(&y)
    };
    let outside_card_matches = |got: &[u8], want: &[u8]| {
        got.as_chunks::<4>()
            .0
            .iter()
            .zip(want.as_chunks::<4>().0)
            .enumerate()
            .filter(|(i, (g, w))| !in_card(*i) && g != w)
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    };

    let mut bare_win = WindowGpu::new();
    let bare = present(&mut gpu, &mut bare_win, &input, false, true);
    assert_eq!(
        bare_win.metal_last_compose_rect, None,
        "fixture: a frame with no effect draws the offscreen with no copy"
    );
    assert_eq!(
        bare.as_chunks::<4>().0.iter().any(|px| px[3] < 255),
        translucent,
        "fixture: the bare frame's alpha is below opaque exactly when translucent"
    );

    let mut win = WindowGpu::new();
    let carded = present(&mut gpu, &mut win, &input, true, true);
    assert_eq!(
        win.metal_last_compose_rect,
        Some([0, 0, dest.0, dest.1]),
        "fixture: the first sync into a fresh copy is the whole frame"
    );
    assert_eq!(
        win.metal_last_present_blits, 0,
        "the tray present composed through a blit encoder"
    );
    assert_eq!(
        outside_card_matches(&carded, &bare),
        Vec::<usize>::new(),
        "texels outside the card left the composed copy changed"
    );
    assert!(
        carded
            .as_chunks::<4>()
            .0
            .iter()
            .zip(bare.as_chunks::<4>().0)
            .enumerate()
            .any(|(i, (c, b))| in_card(i) && c != b),
        "negative control: the card must reach the glass"
    );

    term.process(b"\x1b[3;1HX");
    let changed = term.cell_frame(rows, cols);
    let incremental = present(&mut gpu, &mut win, &changed, true, false);
    let rect = win.metal_last_compose_rect.expect("the copy composed");
    assert!(
        rect != [0, 0, dest.0, dest.1] && rect[2] > rect[0] && rect[3] > rect[1],
        "fixture: the second sync copies a part of the frame, not {rect:?}"
    );
    assert_eq!(
        win.metal_last_present_blits, 0,
        "the incremental compose blitted"
    );
    let mut fresh = WindowGpu::new();
    let oracle = present(&mut gpu, &mut fresh, &changed, true, true);
    assert_eq!(
        incremental, oracle,
        "the incremental compose must present the full sync's bytes"
    );
    assert_ne!(
        incremental, carded,
        "negative control: the changed cell must reach the glass"
    );
    let bare_changed = present(&mut gpu, &mut WindowGpu::new(), &changed, false, true);
    assert_eq!(
        outside_card_matches(&incremental, &bare_changed),
        Vec::<usize>::new(),
        "texels outside the card left the incremental copy changed"
    );
}

#[test]
fn hdr_upgrade_publishes_linear_capture_metadata_with_safe_defaults() {
    use crate::video_tap::CaptureColorSpace;

    let mut win = WindowGpu::new();
    win.set_capture_color_space(CaptureColorSpace::Srgb);
    win.set_sdr_white_scale(3.0);
    win.set_edr_max(4.0);
    win.apply_hdr_surface_upgrade();
    assert_eq!(
        win.capture_color_space(),
        CaptureColorSpace::ExtendedLinearSrgb
    );
    assert_eq!(win.sdr_white_scale(), 1.0);
    assert_eq!(win.edr_max(), 0.0);
}

#[cfg(target_os = "macos")]
#[test]
fn the_screen_upgrade_moves_the_layer_before_it_promotes_the_metadata() {
    use crate::device_layer::TexelFormat;
    use crate::video_tap::CaptureColorSpace;

    let _pool = crate::metal::ffi::AutoreleasePool::new();
    let parent = crate::metal::test_support::plain_calayer();
    let mut gpu =
        crate::renderer::gpu_renderer_for_test(None, 18.0, aterm_render::Theme::default());
    gpu.set_hdr_glow(true);
    let mut win = WindowGpu::new();
    let mut surf = gpu
        .attach_metal_window_surface(&parent, TexelFormat::Bgra8Unorm, 64, 48)
        .expect("an 8-bit attach under a stand-in parent layer");
    let linear = |win: &WindowGpu| win.capture_color_space == CaptureColorSpace::ExtendedLinearSrgb;
    let layer = |surf: &super::GpuSurface| {
        surf.metal
            .as_ref()
            .expect("the attach armed a swapchain")
            .config()
            .format
    };
    let held_8bit = |surf: &super::GpuSurface, why: &str| {
        assert_eq!(
            surf.neutral.format,
            TexelFormat::Bgra8Unorm,
            "{why}: retained"
        );
        assert_eq!(
            layer(surf),
            TexelFormat::Bgra8Unorm.metal(),
            "{why}: live layer"
        );
    };

    for potential in [Some(1.0), None] {
        assert!(
            !gpu.upgrade_surface_on_screen(&mut win, &mut surf, || potential),
            "{potential:?} is no evidence of an EDR screen"
        );
        held_8bit(&surf, "no EDR screen");
        assert!(!linear(&win), "no EDR screen promotes no metadata");
    }

    let arm = gpu.metal_arm.take();
    assert!(!gpu.upgrade_surface_on_screen(&mut win, &mut surf, || Some(2.0)));
    held_8bit(&surf, "a refused flip");
    assert!(!linear(&win), "a refused flip promotes no metadata");
    gpu.metal_arm = arm;

    assert!(
        gpu.upgrade_surface_on_screen(&mut win, &mut surf, || Some(2.0)),
        "an EDR screen upgrades an opted-in 8-bit window"
    );
    assert_eq!(surf.neutral.format, TexelFormat::Rgba16Float);
    assert_eq!(
        layer(&surf),
        TexelFormat::Rgba16Float.metal(),
        "the live layer itself moved, not only the retained desire"
    );
    assert!(linear(&win), "the metadata follows the flipped layer");

    let mut read = false;
    assert!(!gpu.upgrade_surface_on_screen(&mut win, &mut surf, || {
        read = true;
        Some(1.0)
    }));
    assert!(!read, "an f16 surface never reads the screen");
    assert_eq!(
        layer(&surf),
        TexelFormat::Rgba16Float.metal(),
        "one direction only"
    );
}
