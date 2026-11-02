// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The tray stack and the overlay-only present on a real Metal device
//! (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md` §3.3, step S4): a stack the renderer folds
//! presents the bytes the CPU fold presents; an overlay-only present shows what a full redraw of
//! the same state shows, and a recording sees every one; the renderer refuses one where an effect
//! of the last frame lies under the tray. Each reads back the composed destination, the texture
//! `image` and `video` read.

use crate::SettingsCard;
use crate::app_render::{present_tray_quad, tray_quad_below_y};
use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, OverlayRefusal, TrayLayer, TrayQuad, WindowGpu};
use aterm_render::RenderInput;

/// A Metal renderer that folds tray stacks, or the test could not run.
fn gpu() -> GpuRenderer {
    let theme = aterm_render::Theme::default();
    let cpu = aterm_render::Renderer::from_bytes(aterm_render::embedded_font(), 18.0, theme)
        .expect("the bundled font");
    let mut gpu = match aterm_gpu::GpuContext::new()
        .and_then(|ctx| GpuRenderer::from_parts(ctx, cpu, None, theme))
    {
        Ok(gpu) => gpu,
        Err(error) => {
            aterm_spec::host::could_not_run(format_args!("no GPU renderer on this host: {error}"))
        }
    };
    gpu.set_tray_prim_law(crate::tray_raster::gpu_prims::tray_prim_law());
    // The pipelines build off this thread; wait for them, as a frame never does.
    if !(gpu.tray_prims_supported() && gpu.tray_stack_supported()) {
        aterm_spec::host::could_not_run(format_args!(
            "this GPU folds no tray stack (no Metal arm with framebuffer fetch)"
        ));
    }
    gpu
}

/// A terminal frame of at least `w`×`h` px on `gpu`, with text on it.
fn frame(gpu: &GpuRenderer, w: u32, h: u32) -> RenderInput {
    let (cw, ch) = gpu.cell_size();
    let cols = (w as usize).div_ceil(cw) + 1;
    let rows = (h as usize).div_ceil(ch) + 1;
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    for row in 0..rows {
        term.process(format!("\x1b[{};1Hrow {row:03} the quick brown fox", row + 1).as_bytes());
    }
    term.cell_frame(rows, cols)
}

/// The window's composed destination after a present, straight sRGB RGBA8.
fn read_back(gpu: &GpuRenderer, win: &WindowGpu) -> Vec<u8> {
    gpu.virtual_presented_snapshot_current(win, win.resident_input_epoch(), 1)
        .expect("the destination reads back")
        .convert()
        .expect("the destination converts")
        .rgba
}

/// A full present of `input` under `tray`, read back.
fn full(
    gpu: &mut GpuRenderer,
    win: &mut WindowGpu,
    input: &RenderInput,
    tray: Option<TrayQuad<'_>>,
) -> Vec<u8> {
    let (fw, fh) = gpu.frame_size(input.rows, input.cols);
    let crop = aterm_gpu::PresentCrop {
        source_y: 0,
        height: fh as u32,
    };
    assert!(
        gpu.present_virtual_cropped_with_effects_transport(
            win,
            input,
            false,
            None,
            tray,
            crop,
            0,
            (fw as u32, fh as u32),
        ),
        "the full present cannot drop"
    );
    read_back(gpu, win)
}

/// An overlay-only present under `tray` (refused: `Err`), read back.
fn overlay(
    gpu: &mut GpuRenderer,
    win: &mut WindowGpu,
    input: &RenderInput,
    tray: Option<TrayQuad<'_>>,
) -> Result<Vec<u8>, OverlayRefusal> {
    let (fw, fh) = gpu.frame_size(input.rows, input.cols);
    let crop = aterm_gpu::PresentCrop {
        source_y: 0,
        height: fh as u32,
    };
    let shown =
        gpu.present_overlay_virtual(win, false, None, tray, Some(crop), (fw as u32, fh as u32))?;
    assert!(shown, "an admitted overlay-only present cannot drop");
    Ok(read_back(gpu, win))
}

/// How two read-backs differ: bytes off, and the largest difference.
fn differ(a: &[u8], b: &[u8]) -> (usize, u8) {
    assert_eq!(a.len(), b.len(), "the read-backs are one size");
    a.iter()
        .zip(b)
        .filter(|(x, y)| x != y)
        .fold((0, 0), |(n, m), (x, y)| (n + 1, m.max(x.abs_diff(*y))))
}

/// A card whose model is its pixels' hash (with `fp`), as a painted card's model names what it
/// painted.
fn card(rgba: Vec<u8>, (pw, ph): (u32, u32), (dx, dy): (u32, u32), fp: u64) -> SettingsCard {
    let fp = aterm_hash::fnv1a_64(&rgba) ^ fp;
    SettingsCard {
        rgba,
        pw,
        ph,
        dx,
        dy,
        fp,
        geom: fp,
        raster_fingerprint_once: std::sync::OnceLock::new(),
        scrim: None,
        serial: 0,
        delta: None,
    }
}

/// A top card in the shape of one the window draws over the deck (a route card, the Settings
/// panel, a build badge): every composite case in it, an opaque body, translucent edges and
/// clear corners, in colours that vary across it.
fn top_card(kind: &str, (pw, ph): (u32, u32), at: (u32, u32), seed: u64) -> SettingsCard {
    let mut rgba = Vec::with_capacity((pw * ph * 4) as usize);
    let mut z = seed;
    for y in 0..ph {
        for x in 0..pw {
            z = z
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let edge = x.min(y).min(pw - 1 - x).min(ph - 1 - y);
            let a = match edge {
                0 => 0,
                1..=6 => (edge * 36) as u8,
                _ if (x / 9 + y / 7) % 5 == 0 => 128 + (z >> 57) as u8,
                _ => 255,
            };
            rgba.extend_from_slice(&[
                (z >> 24) as u8,
                (x * 255 / pw) as u8,
                (y * 255 / ph) as u8,
                a,
            ]);
        }
    }
    let fp = aterm_hash::fnv1a_64(kind.as_bytes());
    card(rgba, (pw, ph), at, fp)
}

/// The deck's card for owner scene `o` in `form` and theme `light`, and the lens hung under it,
/// as the window rasters them.
fn deck_and_lens(
    o: crate::stats_deck::paint_tests::owner_scenes::Owner,
    form: crate::stats_plan::DeckForm,
    light: bool,
) -> (SettingsCard, Option<SettingsCard>) {
    use crate::stats_deck::paint_tests::light_inks;
    use crate::stats_deck::paint_tests::owner_scenes::{
        OWNER_W, owner_geom_at, owner_inks, owner_input, owner_scene,
    };
    let snap = owner_scene(o);
    let input = crate::stats_deck::DeckInput {
        inks: if light { light_inks() } else { owner_inks() },
        form,
        ..owner_input(&snap)
    };
    let g = owner_geom_at(form, OWNER_W, 2.0);
    let (prims, _, _) = crate::stats_deck::deck_paint_live(&g, &input);
    let (rgba, pw, ph) =
        crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, [0, 0, 0, 0]);
    let deck = card(rgba, (pw, ph), (0, 0), 0xDEC0 + u64::from(light));
    let lens = crate::stats_lens::lens_geom(
        crate::stats_deck::StatsBars::Deck,
        0,
        g.h,
        g.pad,
        g.h,
        g.w - 2.0 * g.pad,
        1_400.0,
        g.scale,
        g.font_px,
    )
    .map(|lg| {
        let pinned = crate::stats_lens::Lens {
            focus: crate::stats_paint::Instrument::Cpu,
            bar: crate::stats_paint::Bar::Global,
            how: crate::stats_lens::Opened::Pinned,
        };
        let laid = crate::stats_lens::lay_lens(
            &lg,
            &input,
            &pinned,
            &crate::deck_face::deck_metrics(),
            true,
        );
        let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
            &laid.prims,
            lg.w as u32,
            lg.h as u32,
            1.0,
            [0, 0, 0, 0],
        );
        card(rgba, (pw, ph), (lg.dx, lg.dy), 0x1E45)
    });
    (deck, lens)
}

/// A stack over the deck's scenes presents the bytes the CPU fold presents: the deck under the
/// lens, under each top card, and under both, and the lens and a top card with no deck.
#[test]
fn a_stacked_present_equals_the_fold() {
    use crate::stats_deck::paint_tests::owner_scenes::Owner;
    use crate::stats_plan::DeckForm;
    let mut gpu = gpu();
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let input = frame(&gpu, 2_100, 1_150);
    let (mut fold_win, mut stack_win) = (WindowGpu::new(), WindowGpu::new());
    let mut table = String::new();
    let mut presents = 0;
    let mut bytes = 0;
    for o in Owner::ALL {
        for form in [DeckForm::Full, DeckForm::Folded] {
            for light in [false, true] {
                let (deck, lens) = deck_and_lens(o, form, light);
                let lens = lens.expect("the owner's window hangs a lens");
                let tops = [
                    top_card("route", (1_200, 700), (420, 180), 1),
                    top_card("settings", (760, 900), (1_300, 60), 2),
                    top_card("badge", (260, 64), (24, 30), 3),
                ];
                let mut stacks: Vec<(String, [Option<&SettingsCard>; 3])> =
                    vec![("deck+lens".into(), [Some(&deck), Some(&lens), None])];
                for (top, kind) in tops.iter().zip(["route", "settings", "badge"]) {
                    stacks.push((format!("deck+{kind}"), [Some(&deck), None, Some(top)]));
                    stacks.push((
                        format!("deck+lens+{kind}"),
                        [Some(&deck), Some(&lens), Some(top)],
                    ));
                }
                stacks.push(("lens+badge".into(), [None, Some(&lens), Some(&tops[2])]));
                for (name, stack) in &stacks {
                    let cards: Vec<&SettingsCard> = stack.iter().flatten().copied().collect();
                    let folded = crate::stats_deck::fold_cards(&cards, 7).expect("the cards fold");
                    let want = full(
                        &mut gpu,
                        &mut fold_win,
                        &input,
                        tray_quad_below_y(&folded, 0),
                    );
                    let mut layers = Vec::new();
                    let quad = present_tray_quad(Some(*stack), None, None, 0, 0, &mut layers)
                        .expect("the stack has a base");
                    assert_eq!(
                        quad.above.len(),
                        cards.len() - 1,
                        "{name}: every card above the base is a layer"
                    );
                    let got = full(&mut gpu, &mut stack_win, &input, Some(quad));
                    let (off, max) = differ(&want, &got);
                    presents += 1;
                    bytes = want.len();
                    if off != 0 {
                        table.push_str(&format!(
                            "{} {form:?} {}: {name}: {off} bytes off, max {max}\n",
                            o.name(),
                            if light { "light" } else { "dark" },
                        ));
                    }
                }
            }
        }
    }
    eprintln!(
        "tray stack against the CPU fold: {presents} presents of {bytes} bytes, {} differ\n{table}",
        table.lines().count()
    );
    assert!(
        table.is_empty(),
        "a stacked present is the fold's:\n{table}"
    );
}

/// The serial of the first layer of the stack `deck` under `top` ([`present_tray_quad`]).
fn top_layer_serial(deck: &SettingsCard, top: &SettingsCard) -> u64 {
    let mut layers = Vec::new();
    let quad = present_tray_quad(
        Some([Some(deck), None, Some(top)]),
        None,
        None,
        0,
        0,
        &mut layers,
    )
    .expect("the stack has a base");
    quad.above[0].quad.serial
}

/// A full present of the stack `deck` under `top` on `win`, read back.
fn show_stack(
    gpu: &mut GpuRenderer,
    win: &mut WindowGpu,
    input: &RenderInput,
    deck: &SettingsCard,
    top: &SettingsCard,
) -> Vec<u8> {
    let mut layers = Vec::new();
    let quad = present_tray_quad(
        Some([Some(deck), None, Some(top)]),
        None,
        None,
        0,
        0,
        &mut layers,
    )
    .expect("the stack has a base");
    full(gpu, win, input, Some(quad))
}

/// A stack's card that a native page's motion frame renews (its own serial, with its model,
/// geometry, place and size as they were) carries a new layer serial, and the renderer uploads
/// it again: the window shows the renewed card, not the one it holds at the old serial.
#[test]
fn a_renewed_stack_card_is_uploaded_again() {
    let mut gpu = gpu();
    let input = frame(&gpu, 900, 600);
    let deck = top_card("deck", (880, 160), (0, 0), 7);
    let mut settings = top_card("settings", (420, 300), (300, 40), 2);
    settings.serial = 41;
    // The motion frame's repaint: new pixels under the card's identity, its serial renewed.
    let mut renewed = top_card("settings", (420, 300), (300, 40), 3);
    (renewed.fp, renewed.geom, renewed.serial) = (settings.fp, settings.geom, 42);
    assert_ne!(
        top_layer_serial(&deck, &settings),
        top_layer_serial(&deck, &renewed),
        "a renewed card's layer carries a new serial"
    );
    let mut win = WindowGpu::new();
    let held = show_stack(&mut gpu, &mut win, &input, &deck, &settings);
    let got = show_stack(&mut gpu, &mut win, &input, &deck, &renewed);
    let want = show_stack(&mut gpu, &mut WindowGpu::new(), &input, &deck, &renewed);
    assert_ne!(
        differ(&held, &want).0,
        0,
        "CONTROL: the renewed card's pixels show in the read-back"
    );
    let (off, max) = differ(&want, &got);
    assert_eq!(
        (off, max),
        (0, 0),
        "the window shows the renewed card, not the one it held ({off} bytes off, max {max})"
    );
}

/// An overlay-only present shows what a full redraw of the same state shows: a new reading of
/// the deck, a top card's opacity eased, a top card taken away (whose pixels the frame must get
/// back) and a card moved, in turn; and it encodes no terminal frame to do it.
#[test]
fn an_overlay_only_present_equals_a_full_redraw() {
    use crate::stats_deck::paint_tests::owner_scenes::Owner;
    use crate::stats_plan::DeckForm;
    let mut gpu = gpu();
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let input = frame(&gpu, 2_100, 1_150);
    let (deck0, lens) = deck_and_lens(Owner::ALL[0], DeckForm::Full, false);
    let (deck1, _) = deck_and_lens(Owner::ALL[2], DeckForm::Full, false);
    let lens = lens.expect("a lens");
    let settings = top_card("settings", (760, 900), (1_300, 60), 2);
    let moved = card(
        settings.rgba.clone(),
        (settings.pw, settings.ph),
        (900, 120),
        9,
    );
    fn quad(c: &SettingsCard) -> TrayQuad<'_> {
        tray_quad_below_y(c, 0).expect("a quad")
    }
    fn layer(c: &SettingsCard, opacity: f32) -> TrayLayer<'_> {
        TrayLayer {
            quad: quad(c),
            opacity,
            offset_y: 0,
        }
    }
    fn with<'a>(base: &'a SettingsCard, above: &'a [TrayLayer<'a>]) -> TrayQuad<'a> {
        TrayQuad {
            above,
            ..quad(base)
        }
    }
    let opaque = [layer(&lens, 1.0), layer(&settings, 1.0)];
    let eased = [layer(&lens, 1.0), layer(&settings, 0.55)];
    let lens_only = [layer(&lens, 1.0)];
    let moved_up = [layer(&lens, 1.0), layer(&moved, 1.0)];
    // The full frame, then each change overlay-only on one window and in full on another.
    let steps: [(&str, &SettingsCard, &[TrayLayer<'_>]); 6] = [
        ("a new reading", &deck1, &[]),
        ("a stack over it", &deck1, &opaque),
        ("the top card's opacity eased", &deck1, &eased),
        ("the top card taken away", &deck1, &lens_only),
        ("a card moved", &deck1, &moved_up),
        ("the stack taken away", &deck0, &[]),
    ];
    let mut win = WindowGpu::new();
    full(&mut gpu, &mut win, &input, Some(quad(&deck0)));
    for (what, base, above) in steps {
        let encoded = gpu.scissor_taken() + gpu.full_repaints();
        let got = overlay(&mut gpu, &mut win, &input, Some(with(base, above)))
            .unwrap_or_else(|why| panic!("{what}: refused ({why:?})"));
        assert_eq!(
            gpu.scissor_taken() + gpu.full_repaints(),
            encoded,
            "{what}: an overlay-only present encodes no terminal frame"
        );
        let want = full(
            &mut gpu,
            &mut WindowGpu::new(),
            &input,
            Some(with(base, above)),
        );
        let (off, max) = differ(&want, &got);
        eprintln!("overlay-only against a full redraw, {what}: {off} bytes off, max {max}");
        assert_eq!(
            (off, max),
            (0, 0),
            "{what}: the overlay-only present is the full redraw's"
        );
    }
    // CONTROL: the read-back sees the tray at all.
    let without = full(&mut gpu, &mut WindowGpu::new(), &input, None);
    let with_deck = full(&mut gpu, &mut WindowGpu::new(), &input, Some(quad(&deck0)));
    assert_ne!(
        differ(&without, &with_deck).0,
        0,
        "CONTROL: the tray shows in the read-back"
    );
}

/// The renderer refuses an overlay-only present where the last frame's band shift lies under
/// the tray's union, where the window has no frame to show again, and where the present's shape
/// is not the last frame's; and admits one whose tray lies clear of the band.
#[test]
fn overlay_only_refuses_when_an_effect_overlaps_the_union() {
    let mut gpu = gpu();
    let mut input = frame(&gpu, 900, 600);
    let (_, ch) = gpu.cell_size();
    // A fractional scroll moves rows 10 to 20's band.
    input.grid_top_row = 10;
    input.grid_bot_row = 20;
    input.scroll_frac_px = 5;
    let (fw, _) = gpu.frame_size(input.rows, input.cols);
    let paint = |pw: u32, ph: u32| vec![200u8; (pw * ph * 4) as usize];
    let strip = paint(fw as u32 - 40, (ch * 2) as u32);
    let at = |row: usize| TrayQuad {
        rgba: &strip,
        pw: fw as u32 - 40,
        ph: (ch * 2) as u32,
        dx: 20,
        dy: (row * ch) as u32,
        scrim: None,
        serial: 0,
        delta: None,
        above: &[],
    };

    let mut win = WindowGpu::new();
    assert_eq!(
        overlay(&mut gpu, &mut win, &input, Some(at(2))).err(),
        Some(OverlayRefusal::NotMetal),
        "a renderer that has drawn no frame on Metal shows none again"
    );
    full(&mut gpu, &mut win, &input, Some(at(2)));
    assert_eq!(
        overlay(&mut gpu, &mut WindowGpu::new(), &input, Some(at(2))).err(),
        Some(OverlayRefusal::NoResidentFrame),
        "a window with no frame has nothing to show again"
    );
    assert_eq!(
        overlay(&mut gpu, &mut win, &input, Some(at(14))).err(),
        Some(OverlayRefusal::EffectOverlaps),
        "a tray moved into the shifted band is refused"
    );
    assert!(
        overlay(&mut gpu, &mut win, &input, Some(at(3))).is_ok(),
        "CONTROL: a tray clear of the band is admitted"
    );
    let (fw, fh) = gpu.frame_size(input.rows, input.cols);
    assert_eq!(
        gpu.present_overlay_virtual(
            &mut win,
            true,
            None,
            Some(at(3)),
            Some(aterm_gpu::PresentCrop {
                source_y: 0,
                height: fh as u32,
            }),
            (fw as u32, fh as u32),
        )
        .err(),
        Some(OverlayRefusal::ShapeChanged),
        "an inverted present is not the last frame's shape"
    );

    // CONTROL: with no fractional scroll the same move is admitted.
    input.scroll_frac_px = 0;
    let mut still = WindowGpu::new();
    full(&mut gpu, &mut still, &input, Some(at(2)));
    assert!(
        overlay(&mut gpu, &mut still, &input, Some(at(14))).is_ok(),
        "CONTROL: without the band shift the move is admitted"
    );
}

/// A recording of the window sees every overlay-only present as it sees a full one: a frame
/// for each, holding what a full redraw of the same state holds.
#[test]
fn a_recording_sees_every_overlay_frame() {
    use crate::stats_deck::paint_tests::owner_scenes::Owner;
    use crate::stats_plan::DeckForm;
    let mut gpu = gpu();
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let input = frame(&gpu, 2_100, 700);
    let decks: Vec<SettingsCard> = Owner::ALL
        .iter()
        .take(4)
        .map(|&o| deck_and_lens(o, DeckForm::Folded, false).0)
        .collect();
    let (fw, fh) = gpu.frame_size(input.rows, input.cols);
    let opts = aterm_gpu::video_tap::CaptureOpts {
        half_res: false,
        budget_bytes: aterm_gpu::video_tap::DEFAULT_BUDGET,
        fps_cap: None,
        requested_ms: 0,
    };
    let mut win = WindowGpu::new();
    gpu.virtual_begin(&mut win, fw as u32, fh as u32, opts)
        .expect("a virtual recording");
    let mut want = Vec::new();
    for (i, deck) in decks.iter().enumerate() {
        let tray = tray_quad_below_y(deck, 0);
        if i == 0 {
            full(&mut gpu, &mut win, &input, tray);
        } else {
            overlay(&mut gpu, &mut win, &input, tray)
                .unwrap_or_else(|why| panic!("frame {i}: refused ({why:?})"));
        }
        gpu.video_after_present(&mut win, i as u64 + 1);
        want.push(full(&mut gpu, &mut WindowGpu::new(), &input, tray));
    }
    let take = gpu.video_finish(&mut win).expect("the take").settle();
    assert_eq!(take.dropped, 0, "the recording lost frames");
    assert_eq!(
        take.frames.len(),
        decks.len(),
        "a frame for every present, overlay-only or full"
    );
    for (i, (frame, want)) in take.frames.iter().zip(&want).enumerate() {
        assert!(
            frame.rgba == *want,
            "frame {i} is what a full redraw shows ({:?} bytes off)",
            differ(&frame.rgba, want)
        );
    }
    assert_ne!(
        take.frames[0].rgba, take.frames[1].rgba,
        "CONTROL: the recorded frames show the deck's change"
    );
}

/// A deck app on a real Metal renderer presenting into its virtual target at `scale`, its deck
/// laid in the head the window reserves for it.
#[cfg(unix)]
fn metal_deck_app(scale: f64) -> (crate::App, crate::WindowId) {
    let (mut app, wid) = crate::stats_live::tests::live_app(std::time::Instant::now());
    app.vitals.landed_period = Some(std::time::Duration::from_secs(3_600));
    // Every decorative effect off, so only what a test changes moves the frame.
    app.config.serious_mode = Some(true);
    app.config.cursor_trail = Some(false);
    app.config.cursor_trail_style = Some("off".into());
    app.config.output_streak = Some(crate::app_config::OutputStreakConfig {
        enabled: Some(false),
        ..Default::default()
    });
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    let (size, pad, pad_top) = (
        app.font_size,
        app.config.window_padding_or_default(),
        app.config.window_padding_top_or_default(),
    );
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.scale = scale;
    ws.metrics = crate::MetricsView::for_scale_padded(size, scale, pad, pad_top);
    let (_, cell) = app.win_cell_size(wid);
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.metrics.head = crate::stats_deck::reserved_deck_px(
        crate::stats_deck::StatsBars::Deck,
        ws.deck_form,
        ws.scale,
        cell,
    );
    let font_px = ws.metrics.font_px;
    let mut renderer = match GpuRenderer::new(font_px, app.theme) {
        Ok(gpu) => gpu,
        Err(error) => {
            aterm_spec::host::could_not_run(format_args!("no GPU renderer on this host: {error}"))
        }
    };
    renderer.set_tray_prim_law(crate::tray_raster::gpu_prims::tray_prim_law());
    if !(renderer.tray_prims_supported() && renderer.tray_stack_supported()) {
        aterm_spec::host::could_not_run(format_args!("this GPU folds no tray stack"));
    }
    app.backend = crate::BackendSlot::Ready(crate::Backend::Gpu(crate::GpuBackend::new(renderer)));
    app.bind_window_renderer_state(wid);
    app.windows.get_mut(&wid).expect("the window").present = Some(crate::PresentTarget::Virtual {
        window_gpu: WindowGpu::new(),
    });
    (app, wid)
}

/// What the app's window last presented, and how many terminal frames its renderer has encoded.
#[cfg(unix)]
fn presented(app: &mut crate::App, wid: crate::WindowId) -> (Vec<u8>, u64) {
    let crate::App {
        backend, windows, ..
    } = app;
    let gpu = backend.gpu_mut().expect("the Metal renderer");
    let Some(crate::PresentTarget::Virtual { window_gpu }) = windows[&wid].present.as_ref() else {
        panic!("the window presents into its virtual target");
    };
    (
        read_back(gpu, window_gpu),
        gpu.scissor_taken() + gpu.full_repaints(),
    )
}

/// A redraw whose only change is the deck's is an overlay-only present: the app composes the
/// deck alone, the renderer encodes no terminal frame, and the window shows what a full redraw
/// of the same state shows. The Metal window leaves the deck's cards to the renderer as a stack
/// where the CPU folds them.
#[cfg(unix)]
#[test]
fn a_deck_only_redraw_is_an_overlay_only_present() {
    let _attached = crate::stats_deck::tests::attach_stats();
    let (mut app, wid) = metal_deck_app(1.0);
    // The first frames settle what the window's first redraw starts (its decorations' scan).
    let mut encoded = u64::MAX;
    for _ in 0..4 {
        app.redraw_window(wid);
        let (_, now) = presented(&mut app, wid);
        if now == encoded {
            break;
        }
        encoded = now;
    }
    let (first, settled) = presented(&mut app, wid);
    assert_eq!(
        settled, encoded,
        "CONTROL: an unchanged frame is not presented again"
    );
    assert!(
        app.windows[&wid].deck_card.is_some(),
        "CONTROL: the window draws a deck"
    );
    // The reading's landing forgotten: the deck goes stale (its grey), and nothing else moves.
    app.vitals.landed_at = None;
    app.redraw_window(wid);
    let (overlaid, after) = presented(&mut app, wid);
    assert_eq!(
        after, encoded,
        "the deck-only redraw encoded no terminal frame (an overlay-only present)"
    );
    assert_ne!(overlaid, first, "CONTROL: the deck's change shows");
    // The same state, redrawn in full.
    app.windows.get_mut(&wid).expect("the window").last_present = None;
    app.redraw_window(wid);
    let (redrawn, full_after) = presented(&mut app, wid);
    assert!(
        full_after > after,
        "CONTROL: the forced redraw encoded its frame"
    );
    let (off, max) = differ(&redrawn, &overlaid);
    assert_eq!(
        (off, max),
        (0, 0),
        "the overlay-only present is the full redraw's ({off} bytes off, max {max})"
    );

    // A lens over the deck: the Metal window stacks the cards where the CPU folds them.
    let lens = top_card("lens", (300, 120), (40, 40), 5);
    app.windows.get_mut(&wid).expect("the window").lens_card = Some(lens);
    app.stack_or_fold_stats_deck(wid);
    let ws = &app.windows[&wid];
    assert!(
        ws.tray_stacked && ws.deck_fold.is_none() && ws.tray_stack().is_some(),
        "a Metal window leaves the deck and the lens to the renderer"
    );
    // CONTROL: a window on the CPU renderer folds them.
    let (mut cpu_app, cpu_wid) = crate::stats_live::tests::live_app(std::time::Instant::now());
    let _ = cpu_app.stats_deck_fp(cpu_wid);
    cpu_app.splice_stats_deck(cpu_wid);
    cpu_app
        .windows
        .get_mut(&cpu_wid)
        .expect("the window")
        .lens_card = Some(top_card("lens", (300, 120), (40, 40), 5));
    cpu_app.stack_or_fold_stats_deck(cpu_wid);
    let ws = &cpu_app.windows[&cpu_wid];
    assert!(
        !ws.tray_stacked && ws.deck_fold.is_some() && ws.tray_stack().is_none(),
        "CONTROL: the CPU renderer's window folds the deck and the lens"
    );
}

/// What an overlay-only present costs the main thread against a full redraw of the same change
/// (design §3.3, S4's measure), p50 and p95 over a run of each. In the renderer: the deck under
/// the lens and the Settings panel at 2238 device px, the panel's opacity eased between two values
/// each frame (the synthetic change), timed as the present's encode on the main thread
/// (`WindowGpu::last_present_encode_ns`: a full present encodes the terminal frame and then the
/// tray, effects and blit; an overlay-only present the latter alone). In the app: a deck app on
/// the same renderer whose deck alone changes each frame (its reading going stale and fresh again), timed
/// as the whole redraw on the main thread (its wall time less the wait for the GPU a virtual
/// present makes), overlay-only and with the frame forced to redraw in full. It times; it asserts
/// nothing about time. Run it optimized and alone:
/// `targo --unverified test -p aterm-gui --profile opt-test --lib -- --ignored
/// overlay_only_present_cost --nocapture --test-threads 1`.
#[cfg(unix)]
#[test]
#[ignore = "measures main-thread time for the design's record: run it optimized and alone"]
fn overlay_only_present_cost() {
    use crate::stats_deck::paint_tests::owner_scenes::Owner;
    use crate::stats_plan::DeckForm;
    use std::time::{Duration, Instant};
    const RUNS: usize = 300;
    let pct = |v: &mut Vec<f64>, q: f64| {
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * q).round() as usize]
    };
    let ms = |ns: u64| ns as f64 / 1e6;
    let mut table = String::from("path\tp50_ms\tp95_ms\n");
    let mut row = |name: &str, v: &mut Vec<f64>| {
        let (p50, p95) = (pct(v, 0.5), pct(v, 0.95));
        table.push_str(&format!("{name}\t{p50:.3}\t{p95:.3}\n"));
    };

    let mut gpu = gpu();
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let input = frame(&gpu, 2_238, 1_150);
    let (deck, lens) = deck_and_lens(Owner::ALL[1], DeckForm::Folded, false);
    let lens = lens.expect("a lens");
    let settings = top_card("settings", (760, 900), (1_300, 60), 2);
    let layers = |opacity: f32| {
        [
            TrayLayer {
                quad: TrayQuad {
                    serial: 2,
                    ..tray_quad_below_y(&lens, 0).expect("a quad")
                },
                opacity: 1.0,
                offset_y: 0,
            },
            TrayLayer {
                quad: TrayQuad {
                    serial: 3,
                    ..tray_quad_below_y(&settings, 0).expect("a quad")
                },
                opacity,
                offset_y: 0,
            },
        ]
    };
    let (one, eased) = (layers(1.0), layers(0.55));
    // The deck's quad carries its serial, as the app stamps it; the layers theirs.
    let tray = |i: usize| TrayQuad {
        above: if i.is_multiple_of(2) { &one } else { &eased },
        serial: 1,
        ..tray_quad_below_y(&deck, 0).expect("a quad")
    };
    let (fw, fh) = gpu.frame_size(input.rows, input.cols);
    let crop = aterm_gpu::PresentCrop {
        source_y: 0,
        height: fh as u32,
    };
    let dest = (fw as u32, fh as u32);
    let (mut full_ms, mut overlay_ms) = (Vec::new(), Vec::new());
    let compared = gpu.tray_compare_bytes();
    let mut win = WindowGpu::new();
    for i in 0..RUNS {
        assert!(gpu.present_virtual_cropped_with_effects_transport(
            &mut win,
            &input,
            false,
            None,
            Some(tray(i)),
            crop,
            0,
            dest
        ));
        full_ms.push(ms(win.last_present_encode_ns()));
    }
    let mut win = WindowGpu::new();
    assert!(gpu.present_virtual_cropped_with_effects_transport(
        &mut win,
        &input,
        false,
        None,
        Some(tray(0)),
        crop,
        0,
        dest
    ));
    for i in 1..=RUNS {
        assert_eq!(
            gpu.present_overlay_virtual(&mut win, false, None, Some(tray(i)), Some(crop), dest),
            Ok(true)
        );
        overlay_ms.push(ms(win.last_present_encode_ns()));
    }
    let compared = gpu.tray_compare_bytes() - compared;
    row(
        "renderer: full present, top card opacity eased (encode)",
        &mut full_ms,
    );
    row(
        "renderer: overlay-only present, same change (encode)",
        &mut overlay_ms,
    );

    let _attached = crate::stats_deck::tests::attach_stats();
    let (mut app, wid) = metal_deck_app(2.0);
    // The harness's window (design §6.1): 110×32 cells at 2×.
    let ws = app.windows.get_mut(&wid).expect("the window");
    (ws.cols, ws.rows) = (110, 32);
    for _ in 0..4 {
        app.redraw_window(wid);
    }
    let ws = &app.windows[&wid];
    let deck_px = ws.deck_card.as_ref().map(|c| (c.pw, c.ph));
    let scale = ws.scale;
    let redraw = |app: &mut crate::App, i: usize, forced: bool| {
        app.vitals.landed_at = i.is_multiple_of(2).then(Instant::now);
        if forced {
            app.windows.get_mut(&wid).expect("the window").last_present = None;
        }
        let (_, before) = presented(app, wid);
        let started = Instant::now();
        app.redraw_window(wid);
        let wall = started.elapsed();
        let Some(crate::PresentTarget::Virtual { window_gpu }) = app.windows[&wid].present.as_ref()
        else {
            panic!("the window presents into its virtual target");
        };
        let gpu_wait = Duration::from_nanos(
            window_gpu
                .last_present_work_ns()
                .saturating_sub(window_gpu.last_present_encode_ns()),
        );
        let (_, after) = presented(app, wid);
        assert_eq!(after > before, forced, "frame {i}: forced {forced}");
        wall.saturating_sub(gpu_wait).as_secs_f64() * 1e3
    };
    let (mut app_full, mut app_overlay) = (Vec::new(), Vec::new());
    for i in 0..RUNS {
        app_full.push(redraw(&mut app, i, true));
    }
    for i in 0..RUNS {
        app_overlay.push(redraw(&mut app, i, false));
    }
    row(
        "app: full redraw, deck-only change (main thread)",
        &mut app_full,
    );
    row(
        "app: overlay-only present, same change (main thread)",
        &mut app_overlay,
    );
    // The deck's own share of either: its fingerprint's layout and its splice's raster.
    let mut deck_ms = Vec::new();
    for i in 0..RUNS {
        app.vitals.landed_at = i.is_multiple_of(2).then(Instant::now);
        let started = Instant::now();
        let _ = app.stats_deck_fp(wid);
        app.splice_stats_deck(wid);
        deck_ms.push(started.elapsed().as_secs_f64() * 1e3);
    }
    row(
        "app: the deck's fingerprint and splice alone, same change",
        &mut deck_ms,
    );
    eprintln!(
        "{RUNS} frames each; renderer {fw}x{fh} px, compared {compared} tray bytes over its {} \
         presents; app 110x32 at {scale}x, deck card {deck_px:?}\n{table}",
        2 * RUNS + 1
    );
}
