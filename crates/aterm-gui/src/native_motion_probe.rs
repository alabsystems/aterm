// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Times a native page's paint-only motion frame (`docs/DESIGN-init-on-by-default-2026-10-25.md`
//! §5.1, §5.6, whose budgets are ≤ 0.3 ms p50 for an 1800×96 px band and ≤ 0.05 ms for a 32×32
//! spinner, over the whole path).
//!
//! Two kinds of case. The engine on a band: a 900×48-point moving node over a page of text at
//! 2x (an 1800×96 px band), each frame its repaint, splice and region raster, the copy of its
//! rows into the window's card, and the GPU arm's own copy of those rows (what the Metal arm
//! does with a motion frame's rows besides handing them to the texture). And the System State
//! page's working spinner through the host: each frame the motion tick, then the window's whole
//! native frame (its stamp, the paint-only raster, the card hand-over, the cell grid and the
//! tab strip), next to the same frame through the content path the paint-only path replaced
//! (a recompile and a regional raster, and a clone of the card). Headless: no GPU presents, so
//! the present's own upload is the copy the engine case times.

use super::*;

use std::time::{Duration, Instant};

/// One case: µs per frame at p50, p90 and p99.
pub(crate) struct MotionCost {
    pub(crate) what: String,
    pub(crate) p50_us: f64,
    pub(crate) p90_us: f64,
    pub(crate) p99_us: f64,
}

fn cost(what: String, mut micros: Vec<f64>) -> MotionCost {
    micros.sort_by(f64::total_cmp);
    let at = |q: f64| micros[((micros.len() - 1) as f64 * q) as usize];
    MotionCost {
        what,
        p50_us: at(0.5),
        p90_us: at(0.9),
        p99_us: at(0.99),
    }
}

/// One spinner frame and a hair: each frame of a case shows the next step.
const STEP_MS: u64 = 84;

pub(crate) fn native_motion_cost(frames: usize) -> Vec<MotionCost> {
    let mut out = vec![band_case(frames)];
    out.extend(page_cases(frames));
    out
}

/// A 900×644-point page of text rows with a 900×48-point moving band under its heading.
fn band_page() -> native_ui::CompiledUi {
    use native_ui::{
        AuditedCustomNode, GroupSpec, Layout, Length, LogicalRect, NodeMotion,
        SYSTEM_STATE_MARK_AUDIT, SemanticRole, TextSpec, UiContent, UiNode, UiTree,
    };
    let band = UiNode::new(
        "band",
        UiContent::Custom(AuditedCustomNode {
            audit_id: SYSTEM_STATE_MARK_AUDIT,
            role: SemanticRole::Group,
            label: String::new(),
            paint: Some(system_state::working_spinner()),
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
            progress: None,
            motion: Some(NodeMotion::endless(
                0,
                aterm_messages::motion::Cadence::SPINNER,
            )),
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(900.0))
            .height(Length::Fixed(48.0)),
    )
    .paint_only();
    let mut rows = vec![
        UiNode::new(
            "heading",
            UiContent::Text(TextSpec::text("Setting up this Mac")),
        ),
        band,
    ];
    rows.extend((0..24).map(|row| {
        UiNode::new(
            format!("row/{row}"),
            UiContent::Text(TextSpec::text(format!(
                "Row {row}: the Command Line Tools, Homebrew, the vault and the fleet"
            ))),
        )
    }));
    UiTree::new(
        UiNode::new(
            "page",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::column().gap(4.0))
        .children(rows),
    )
    .compile(LogicalRect::new(0.0, 0.0, 900.0, 644.0))
    .expect("the band page compiles")
}

fn band_case(frames: usize) -> MotionCost {
    let compiled = band_page();
    let theme = aterm_render::Theme::default();
    let palette = (chrome_band::ChromePalette::from(theme), theme);
    let (scale, size) = (2.0, (1_800, 1_288));
    let (tray, spans) = compiled.tray_with_motion(theme, theme, 13.0, 0);
    let mut rgba =
        tray_raster::rasterize_tray_pixels(&tray.prims, size.0, size.1, scale, [0, 0, 0, 0]);
    let mut card = rgba.clone();
    let mut held = rgba.clone();
    let mut motion = app_native_motion::MotionTray::new(tray.prims, spans);
    let stride = size.0 as usize * 4;
    let mut frame = |t_ms: u64| {
        let regions = motion.paint(
            &compiled,
            (t_ms - STEP_MS, t_ms),
            palette,
            scale,
            &mut rgba,
            size,
        );
        assert!(
            app_native_motion::copy_card_regions(&mut card, &rgba, size.0, &regions),
            "the card takes the band"
        );
        let rows = app_native_motion::rows_of(&regions);
        let band = aterm_gpu::tray_rows_to_upload(&held, &card, stride, rows)
            .expect("the band's rows upload");
        let (from, to) = (band.0 as usize * stride, band.1 as usize * stride);
        held[from..to].copy_from_slice(&card[from..to]);
        regions
    };
    let mut t_ms = STEP_MS;
    for _ in 0..16 {
        let _ = frame(t_ms);
        t_ms += STEP_MS;
    }
    let mut micros = Vec::with_capacity(frames);
    let mut region = None;
    for _ in 0..frames {
        let started = Instant::now();
        let regions = std::hint::black_box(frame(t_ms));
        micros.push(started.elapsed().as_secs_f64() * 1e6);
        region = regions.first().copied();
        t_ms += STEP_MS;
    }
    let region = region.expect("every frame repaints the band");
    cost(
        format!(
            "band {}x{} px: paint+card+upload copy",
            region.width, region.height
        ),
        micros,
    )
}

/// The System State page showing a run, at 2x in a focused window with glass.
fn running_page() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).expect("the headless window");
        // 975×840 points of the cells a 2x window's font makes (15×28 px, 130×60 of them): the
        // running row's spinner sits at y 680, so it shows.
        window.metrics.font_px = crate::font_px_for_scale(window.metrics.font_px, 2.0);
        window.rows = 60;
        window.cols = 130;
        window.scale = 2.0;
        window.os_window_for_test = true;
        window.focused = true;
    }
    // The frame is what is timed, not this machine's power: hold the Low Power sample, which a
    // busy machine's thermal state would otherwise turn on mid-run (and still the spinner).
    app.low_power = false;
    app.low_power_sampled_at = Some(Instant::now() + Duration::from_secs(86_400));
    assert!(app.open_system_state_tab(), "System State opens");
    // The scenario is published as the dev seam's fixture is (`poll_system_state_status`), not
    // received as a worker's read: a read is a yes's input, which only the event loop takes.
    app.publish_system_state(
        system_state::fixture_status("running").expect("the running scenario"),
    );
    assert!(app.prepare_native_input_scratch(wid), "the page paints");
    let (_, view) = app.active_native_view(wid).expect("the page is in front");
    assert!(
        app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .is_some_and(|raster| !raster.compiled.motion.is_empty()),
        "the running page shows a moving spinner: {:?}, spinners {:?}, viewport {:?}",
        app.native_view_motion_cx(wid, view).page(),
        app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .map(|raster| {
                raster
                    .compiled
                    .paint
                    .iter()
                    .filter(|node| node.key.as_str().ends_with("/mark"))
                    .map(|node| (node.key.as_str().to_owned(), node.motion().is_some()))
                    .collect::<Vec<_>>()
            }),
        app.native_ui_full_viewport(wid)
    );
    (app, wid)
}

/// Move the motion clock on by one spinner step.
fn step(app: &mut App) {
    app.lat_epoch = app
        .lat_epoch
        .checked_sub(Duration::from_millis(STEP_MS))
        .expect("the epoch steps back");
}

fn page_cases(frames: usize) -> Vec<MotionCost> {
    let (mut app, wid) = running_page();
    let (_, view) = app.active_native_view(wid).expect("the page is in front");
    let raster = |app: &App| {
        app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .map(|raster| {
                (
                    raster.last_work,
                    raster.full_rasters,
                    raster.regional_rasters,
                )
            })
            .expect("the page's raster")
    };
    // Paint-only: the tick, then the frame.
    let mut ticks = Vec::with_capacity(frames);
    let mut micros = Vec::with_capacity(frames);
    let mut region = None;
    let (_, full_before, regional_before) = raster(&app);
    for round in 0..frames + 16 {
        step(&mut app);
        let started = Instant::now();
        assert!(
            app.native_motion_due(wid, Instant::now()),
            "a spinner step is owed"
        );
        let ticked = started.elapsed();
        let started = Instant::now();
        assert!(app.prepare_native_input_scratch(wid));
        let painted = started.elapsed();
        let (work, _, _) = raster(&app);
        let NativeRasterWork::Motion { rect, .. } = work else {
            panic!("a paint-only frame, not {work:?}");
        };
        region = Some(rect);
        if round >= 16 {
            ticks.push(ticked.as_secs_f64() * 1e6);
            micros.push(painted.as_secs_f64() * 1e6);
        }
    }
    let (_, full_after, regional_after) = raster(&app);
    assert_eq!(
        (full_before, regional_before),
        (full_after, regional_after),
        "no motion frame rastered through the content path"
    );
    let region = region.expect("a frame ran");
    let mut out = vec![
        cost(
            format!(
                "spinner {}x{} px: whole native frame",
                region.width, region.height
            ),
            micros,
        ),
        cost("spinner: motion tick".to_owned(), ticks),
    ];
    // The content path a spinner step took before paint-only frames: the step's rect as the
    // leaf's damage, so the frame recompiles, rasters that rect and clones the card.
    let mut micros = Vec::with_capacity(frames);
    for round in 0..frames + 16 {
        step(&mut app);
        if let Some(cache) = app
            .windows
            .get_mut(&wid)
            .and_then(|window| window.leaf_render_cache.get_mut(&view))
        {
            cache.native_damage = Some(native_app::DamageRegion::Rect {
                x: (region.x as f32 / 2.0) as u32,
                y: (region.y as f32 / 2.0) as u32,
                width: (region.width as f32 / 2.0).ceil() as u32,
                height: (region.height as f32 / 2.0).ceil() as u32,
            });
        }
        let started = Instant::now();
        assert!(app.prepare_native_input_scratch(wid));
        if round >= 16 {
            micros.push(started.elapsed().as_secs_f64() * 1e6);
        }
    }
    out.push(cost("spinner: content path (before)".to_owned(), micros));
    out
}
