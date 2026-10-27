// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The frames every arm differential draws: the replay fixtures (one per
//! frame-plan family, each with the streams it claims), the image plane, the
//! fx engine's layers, the heat-shimmer scene and the cursor frame, with the
//! present helper and the byte comparison they share. Every arm's frame
//! record (`frame_record_tests`) draws these inputs.

use super::*;

pub(crate) const ROWS: usize = 6;

pub(crate) const COLS: usize = 24;

pub(crate) fn fixtures(
    gpu: &GpuRenderer,
    glow_add: bool,
) -> Vec<(&'static str, RenderInput, Vec<StreamId>)> {
    use StreamId as S;
    use aterm_core::render::{
        FireHaloCell, FireMode, FirePatch, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer,
        FxSlot, FxSpace, FxSprite, FxTexels, HaloMode, PxRect, RainHalo,
    };
    use aterm_core::terminal::Terminal;
    use aterm_render::{CharFg, GlowQuad, premul_rgb};

    let (cw, ch) = gpu.cell_size();
    let (cw16, ch16) = (cw as u16, ch as u16);
    let base_input = || -> RenderInput {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(b"$ gl differential >_\r\n");
        term.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[4munder\x1b[0m\r\n");
        term.process(b"\x1b[1mbold\x1b[0m plain 0123456789 \xf0\x9f\x99\x82\r\n");
        term.cell_frame(ROWS, COLS)
    };
    // An 8×8 straight-RGBA art, and one Linear Grid layer of it at `(x, y)`
    // (the rain's rows and the companions both reach the GPU this way).
    let scene_art = |seed: usize| {
        let rgba: Vec<u8> = (0..8 * 8 * 4)
            .map(|i| ((i * 13 + seed) % 256) as u8)
            .collect();
        Arc::new(FxArt::new(8, 8, FxTexels::Rgba8Straight, rgba).expect("sized"))
    };
    let sprite_layer =
        |id: u64, slot: FxSlot, knock: bool, (x, y): (i32, i32), art: &Arc<FxArt>, flip_x| {
            let mut l = FxLayer::new(
                id,
                Arc::new(FxContent::new(
                    vec![FxItem::Sprite(FxSprite {
                        x,
                        y,
                        dw: 8,
                        dh: 8,
                        flip_x,
                        art: Arc::clone(art),
                        ink: 0,
                        clip: PxRect::EVERYWHERE,
                    })],
                    vec![FxInk::over(0x00ff_ffff, 255)],
                )),
                slot,
                FxSpace::Grid,
                FxBlend::Linear,
            );
            l.knock = knock;
            l
        };
    let glow = |row: usize, x: usize, w: usize, color: u32| GlowQuad {
        row: row as u16,
        x: x as u16,
        y: (row * ch) as u16,
        w: w as u16,
        h: ch16,
        color,
        alpha: 0,
        color2: color,
        alpha2: 0,
    };
    let halo = |row: usize, col: usize, color: u32, rx: usize, mode: HaloMode| RainHalo {
        row: row as u16,
        x: (col * cw) as u16,
        y: (row * ch) as u16,
        w: cw16,
        h: ch16,
        color,
        cx: (col * cw + cw / 2) as u16,
        cy: (row * ch + ch / 2) as u16,
        rx: rx as u16,
        ry: ch16,
        mode,
    };

    let mut out: Vec<(&'static str, RenderInput, Vec<StreamId>)> = vec![(
        "text_cursor",
        base_input(),
        vec![S::Bg, S::Glyph, S::Deco, S::CursorBlock],
    )];

    if colour_face_installed() {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process("\u{1F642} \u{1F680} \u{1F44D}\u{1F3FD} ok\r\n".as_bytes());
        term.process(
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{1F1FA}\u{1F1F8} \u{31}\u{FE0F}\u{20E3}"
                .as_bytes(),
        );
        out.push((
            "colour_emoji",
            term.cell_frame(ROWS, COLS),
            vec![S::Bg, S::Glyph, S::Color],
        ));
    }

    {
        let mut input = base_input();
        let (fw, fh) = gpu.frame_size(ROWS, COLS);
        input.wallpaper = Some(Arc::new(SceneAtlas {
            width: fw as u32,
            height: fh as u32,
            rgba: (0..fw * fh)
                .flat_map(|i| {
                    let (x, y) = (i % fw, i / fw);
                    [
                        (x * 3 % 256) as u8,
                        (y * 5 % 256) as u8,
                        ((x + y) % 97) as u8,
                        255,
                    ]
                })
                .collect(),
            version: 1,
        }));
        out.push(("wallpaper", input, vec![S::Wallpaper, S::Glyph]));
    }

    {
        let mut input = base_input();
        input.glow_halo.push(halo(
            2,
            6,
            premul_rgb(0x00ff_c060, 200),
            cw * 2,
            HaloMode::Add,
        ));
        input
            .glow_halo
            .push(halo(2, 9, 0x0040_30a0, cw * 2, HaloMode::Over));
        input
            .glow_halo
            .push(halo(3, 3, premul_rgb(0x00ff_3020, 150), cw, HaloMode::Add));
        input
            .glow_halo
            .push(halo(3, 7, 0x0040_1030, cw * 2, HaloMode::Over));
        let mut drive = vec![S::GlowHalo, S::GlowHaloOver];
        if glow_add {
            for (i, a) in [40u8, 160, 255].iter().enumerate() {
                input
                    .cursor_glow_add
                    .push(glow(1, (i + 1) * cw, cw, premul_rgb(0x0050_fa7b, *a)));
            }
            input
                .nova_add
                .push(glow(4, 2 * cw, 2 * cw, premul_rgb(0x00ff_79c6, 220)));
            drive.push(S::GlowAdd);
            drive.push(S::NovaAdd);
        }
        out.push(("glow_halo", input, drive));
    }

    {
        let mut input = base_input();
        let tile = scene_art(5);
        for i in 0..3u16 {
            let at = (i32::from(i * 5 * cw16), i32::from(4 * ch16));
            let l = sprite_layer(
                1 + u64::from(i),
                FxSlot::Under,
                false,
                at,
                &tile,
                i % 2 == 1,
            );
            input.fx_layers.push(l);
        }
        input.rain_add.push(halo(
            4,
            2,
            premul_rgb(0x0060_ff60, 180),
            cw * 2,
            HaloMode::Add,
        ));
        input
            .rain_add
            .push(halo(4, 6, 0x0020_2040, cw, HaloMode::Over));
        input
            .rain_add
            .push(halo(5, 3, premul_rgb(0x00ff_3020, 150), cw, HaloMode::Add));
        input
            .rain_add
            .push(halo(5, 7, 0x0040_1030, cw * 2, HaloMode::Over));
        out.push((
            "rain",
            input,
            vec![S::RainAdd, S::RainAddOver, S::FxLinearUnder],
        ));
    }

    {
        let mut input = base_input();
        for (i, mode) in [FireMode::Add, FireMode::Over].into_iter().enumerate() {
            for (row, col, wcells, phase, temp, strength, lean, cov_cap, base, peak) in [
                (
                    2usize,
                    1 + i * 4,
                    3usize,
                    512u32,
                    200u8,
                    220u8,
                    -2i8,
                    200u8,
                    3usize,
                    2usize,
                ),
                (4, 2 + i * 4, 2, 233, 160, 180, 1, 170, 5, 1),
            ] {
                input.fire_patch.push(FirePatch {
                    row: row as u16,
                    x: (col * cw) as u16,
                    y: (row * ch) as u16,
                    w: (wcells * cw) as u16,
                    h: ch16,
                    base_y: (base * ch) as u16,
                    peak_h: (peak * ch) as u16,
                    phase,
                    temp,
                    strength,
                    lean,
                    cov_cap,
                    cell_h: ch16,
                    mode,
                });
            }
        }
        input.fire_halo.push(FireHaloCell {
            row: 2,
            col: 2,
            strength: 200,
        });
        input.char_fg.push(CharFg {
            row: 2,
            col: 2,
            fg: 0x0020_1008,
        });
        let mut drive = vec![S::FireAdd, S::FireOver, S::GlyphHalo];
        if glow_add {
            input
                .glow_under
                .push(glow(2, cw, 6 * cw, premul_rgb(0x00ff_6020, 140)));
            drive.push(S::GlowUnder);
        }
        out.push(("fire", input, drive));
    }

    {
        let mut input = base_input();
        let (body, cat) = (scene_art(11), scene_art(23));
        let y = (ch / 2) as i32;
        input.fx_layers = vec![
            sprite_layer(1, FxSlot::Under, true, (2 * cw as i32, y), &body, false),
            sprite_layer(2, FxSlot::Over, false, (3 * cw as i32, y), &body, false),
            sprite_layer(
                3,
                FxSlot::Under,
                true,
                (10 * cw as i32, 5 * ch as i32),
                &cat,
                false,
            ),
        ];
        out.push(("sprites", input, vec![S::FxKnockUnder, S::FxLinearOver]));
    }

    for (name, frac) in [("scroll", 1i32), ("scroll_down", -1)] {
        let mut input = base_input();
        input.scroll_frac_px = frac * (ch as i32 / 2).max(1);
        input.grid_top_row = 0;
        input.grid_bot_row = ROWS;
        out.push((name, input, vec![S::Bg, S::Glyph]));
    }
    out
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) const COLOUR_FACE: &str = "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf";

#[cfg(target_os = "macos")]
pub(crate) const COLOUR_FACE: &str = "/System/Library/Fonts/Apple Color Emoji.ttc";

#[cfg(windows)]
pub(crate) const COLOUR_FACE: &str = r"C:\Windows\Fonts\seguiemj.ttf";

pub(crate) fn colour_face_installed() -> bool {
    std::path::Path::new(COLOUR_FACE).is_file()
}

pub(crate) fn end_unless_the_colour_row_ran() {
    if !colour_face_installed() {
        aterm_spec::host::could_not_run(format!(
            "{COLOUR_FACE} is not installed here: the colour row was not compared"
        ));
    }
}

pub(crate) fn image_fixture(gpu: &GpuRenderer) -> (&'static str, RenderInput, Vec<StreamId>) {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
    use aterm_core::terminal::Terminal;
    let (cw, ch) = gpu.cell_size();
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(b"$ images over and under >_\r\nunder the picture 0123456789\r\n");
    let mut input = term.cell_frame(ROWS, COLS);
    for (z, row0, col0, seed) in [(0, 0usize, 2usize, 7usize), (-1, 1, 12, 41)] {
        let (w, h) = (17u16, 11u16);
        let image = Arc::new(ImageData {
            bytes: (0..usize::from(w) * usize::from(h) * 4)
                .map(|i| ((i * 29 + seed) % 256) as u8)
                .collect(),
            format: ImageFormat::RawRgba8 {
                width: w,
                height: h,
            },
            cols: 4,
            rows: 2,
            z_index: z,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        });
        for r in 0..2u16 {
            for c in 0..4u16 {
                input.images[row0 + usize::from(r)].push((
                    col0 + usize::from(c),
                    ImageRef {
                        image: image.clone(),
                        cell_row: r,
                        cell_col: c,
                        kitty: None,
                    },
                ));
            }
        }
    }
    (
        "images",
        input,
        vec![StreamId::Image, StreamId::ImageUnder, StreamId::Glyph],
    )
}

pub(crate) fn fx_fixture(gpu: &GpuRenderer) -> (&'static str, RenderInput, Vec<StreamId>) {
    use aterm_core::render::{
        FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRamps, FxRect, FxSlot, FxSpace,
        FxSprite, FxTexels, PxRect,
    };
    use aterm_core::terminal::Terminal;
    let (cw, ch) = gpu.cell_size();
    let (cw, ch) = (cw as i32, ch as i32);
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ fx over and under the text >_\r\n0123456789 abcdef\r\n");
    let mut input = term.cell_frame(ROWS, COLS);
    let layer = |id, items, palette, slot, blend| {
        FxLayer::new(
            id,
            Arc::new(FxContent::new(items, palette)),
            slot,
            FxSpace::Window,
            blend,
        )
    };
    let index = Arc::new(
        FxArt::new(
            5,
            3,
            FxTexels::Index8,
            vec![1, 0, 2, 3, 1, 2, 2, 0, 3, 1, 0, 1, 3, 2, 1],
        )
        .expect("sized"),
    );
    let premul: Vec<u8> = (0..12u32)
        .flat_map(|i| match i % 4 {
            0 => [0x30, 0x18, 0x40, 0],
            1 => [0, 0, 0, 0],
            2 => [(i * 9) as u8, 0x20, 0x50, 0x80],
            _ => [0xd0, (i * 17) as u8, 0x30, 0xff],
        })
        .collect();
    let premul = Arc::new(FxArt::new(4, 3, FxTexels::Rgba8Premul, premul).expect("sized"));
    let straight: Vec<u8> = (0..9u32)
        .flat_map(|i| {
            [
                (i * 29) as u8,
                (200 - i * 11) as u8,
                (i * 53) as u8,
                [0u8, 255, 140][(i % 3) as usize],
            ]
        })
        .collect();
    let straight = Arc::new(FxArt::new(3, 3, FxTexels::Rgba8Straight, straight).expect("sized"));
    let sprite = |(x, y): (i32, i32), (dw, dh): (i32, i32), flip_x, art: &Arc<FxArt>| {
        FxItem::Sprite(FxSprite {
            x,
            y,
            dw: dw as u16,
            dh: dh as u16,
            flip_x,
            art: Arc::clone(art),
            ink: 0,
            clip: PxRect::EVERYWHERE,
        })
    };
    input.fx_layers = vec![
        layer(
            1,
            vec![
                FxItem::Rect(FxRect {
                    x: cw,
                    y: ch / 2,
                    w: (10 * cw) as u16,
                    h: (2 * ch) as u16,
                    ink: 0,
                }),
                FxItem::Ramps(FxRamps {
                    x: 12 * cw,
                    y: ch,
                    w: (6 * cw) as u16,
                    h: 3,
                    ink: 2,
                }),
            ],
            [
                0x0018_2a30,
                0x0030_1008,
                0x0020_2020,
                0x0008_3018,
                0x0010_0820,
                0x0028_2810,
                0x0004_1830,
                0x0030_3030,
            ]
            .map(FxInk::add)
            .to_vec(),
            FxSlot::Under,
            FxBlend::Exact,
        ),
        layer(
            2,
            vec![sprite((3 * cw + 1, ch + 3), (37, 2 * ch + 5), true, &index)],
            vec![
                FxInk::add(0x0040_2010),
                FxInk::over(0x0030_4020, 0xa0),
                FxInk::over(0x0010_2030, 0xff),
            ],
            FxSlot::Glow,
            FxBlend::Exact,
        ),
        layer(
            3,
            vec![
                FxItem::Rect(FxRect {
                    x: 2 * cw,
                    y: 3 * ch,
                    w: (7 * cw) as u16,
                    h: (ch + 3) as u16,
                    ink: 0,
                }),
                sprite((14 * cw, 2 * ch + 2), (29, 23), false, &premul),
            ],
            vec![
                FxInk::over(0x0020_1030, 0x90),
                FxInk::over(0x0050_2010, 0xe0),
            ],
            FxSlot::Over,
            FxBlend::Exact,
        ),
        layer(
            4,
            vec![sprite((cw / 2, 4 * ch), (31, 19), false, &straight)],
            vec![FxInk::over(0x00ff_ffff, 0xff)],
            FxSlot::Under,
            FxBlend::Linear,
        ),
        layer(
            5,
            vec![sprite((18 * cw, 3 * ch + 1), (23, 29), true, &straight)],
            vec![FxInk::over(0x00c0_e0ff, 0xc0)],
            FxSlot::Over,
            FxBlend::Linear,
        ),
    ];
    (
        "fx",
        input,
        vec![
            StreamId::FxUnder,
            StreamId::FxGlow,
            StreamId::FxOver,
            StreamId::FxLinearUnder,
            StreamId::FxLinearOver,
            StreamId::Glyph,
        ],
    )
}

#[cfg(d3d11_arm)]
pub(crate) fn assert_frames_identical(name: &str, expected: &Frame, actual: &Frame) {
    if let Some(why) = frames_differ(name, expected, actual) {
        panic!("{why}");
    }
}

/// `None` when every pixel is equal, else the finding: how many texels
/// differ, the largest byte delta and the first differing texel.
#[cfg(d3d11_arm)]
pub(crate) fn frames_differ(name: &str, expected: &Frame, actual: &Frame) -> Option<String> {
    assert_eq!(
        (expected.width, expected.height),
        (actual.width, actual.height),
        "{name}: frame geometry diverged"
    );
    if expected.pixels == actual.pixels {
        return None;
    }
    let (e, a) = (expected.rgba_bytes(), actual.rgba_bytes());
    let (e, a) = (e.as_chunks::<4>().0, a.as_chunks::<4>().0);
    let mut diffs = 0usize;
    let mut first = None;
    let mut max_delta = 0u8;
    for (i, (pe, pa)) in e.iter().zip(a).enumerate() {
        if pe != pa {
            diffs += 1;
            for (x, y) in pe.iter().zip(pa) {
                max_delta = max_delta.max(x.abs_diff(*y));
            }
            first.get_or_insert((i, *pe, *pa));
        }
    }
    let (i, pe, pa) = first.expect("pixels differ, so a texel differs");
    Some(format!(
        "{name}: THE FRAMES ARE NOT BYTE-IDENTICAL: {diffs} of {} texels differ, max byte \
         delta {max_delta}; first at ({},{}): expected {pe:02x?} != actual {pa:02x?}. Measure \
         the cause (twin, blend, view, clear, sampler, copy); never loosen the comparison.",
        e.len(),
        i % expected.width,
        i / expected.width,
    ))
}

pub(crate) fn fixtures_base_for_cursor() -> RenderInput {
    let mut term = aterm_core::terminal::Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ cursor blend >_\r\n");
    term.cell_frame(ROWS, COLS)
}

pub(crate) fn presented(
    gpu: &mut GpuRenderer,
    win: &mut WindowGpu,
    input: &RenderInput,
    invert: bool,
    overlay: Option<DropOverlay>,
    tray: Option<TrayQuad<'_>>,
    crop: Option<(PresentCrop, (u32, u32))>,
) -> Frame {
    gpu.reset_glow_ease_for_test(win);
    let ok = match crop {
        None => gpu.present_virtual(win, input, invert, overlay, tray),
        Some((crop, dest)) => gpu.present_virtual_cropped_with_effects_transport(
            win, input, invert, overlay, tray, crop, 0, dest,
        ),
    };
    assert!(ok, "the virtual present refused (see the arm's note)");
    let raw = gpu
        .virtual_presented_snapshot_current(win, win.resident_input_epoch(), 1)
        .expect("the presented destination reads back");
    assert_eq!(win.virtual_snapshot_bytes(), Some(raw.allocation()));
    let shot = raw.convert().expect("the presented destination converts");
    Frame {
        width: shot.w as usize,
        height: shot.h as usize,
        pixels: shot
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[r, g, b, a]| {
                ((255 - u32::from(a)) << 24)
                    | (u32::from(r) << 16)
                    | (u32::from(g) << 8)
                    | u32::from(b)
            })
            .collect(),
    }
}

/// The heat scene: a comet's glow on row 2 over an additive fire patch on row 3. The haze rises
/// only through blank rows (`lettered_foot`), so row 1 is blank cells in two-column background
/// blocks: the shimmer has edges to bend there, and row 0's letters stop it.
pub(crate) fn heat_input(gpu: &GpuRenderer) -> RenderInput {
    use aterm_core::render::{FireMode, FirePatch};
    use aterm_render::{GlowQuad, premul_rgb};
    let (cw, ch) = gpu.cell_size();
    let mut term = aterm_core::terminal::Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ heat over the comet >_\r\n");
    for _ in 0..COLS / 4 {
        term.process(b"\x1b[47m  \x1b[0m  ");
    }
    term.process(b"\r\n");
    let mut input = term.cell_frame(ROWS, COLS);
    for (i, a) in [80u8, 200, 255].iter().enumerate() {
        input.cursor_glow_add.push(GlowQuad {
            row: 2,
            x: ((i + 2) * cw) as u16,
            y: (2 * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(0x00ff_ffff, *a),
            alpha: 0,
            color2: premul_rgb(0x0050_fa7b, *a),
            alpha2: 0,
        });
    }
    input.fire_patch.push(FirePatch {
        row: 3,
        x: (2 * cw) as u16,
        y: (3 * ch) as u16,
        w: (4 * cw) as u16,
        h: ch as u16,
        base_y: (4 * ch) as u16,
        peak_h: (2 * ch) as u16,
        phase: 300,
        temp: 220,
        strength: 230,
        lean: 1,
        cov_cap: 220,
        cell_h: ch as u16,
        mode: FireMode::Add,
    });
    input
}
