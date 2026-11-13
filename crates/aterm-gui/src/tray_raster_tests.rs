// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::widget::DrawPrim;
use aterm_render::Theme;

fn assert_audited_text_ink(s: &str, px: f32, weight: TextWeight, face: TextFace, baseline: f32) {
    let (top, bottom) = text_ink_band(s, px, weight, face, baseline)
        .expect("the painted run has complete ink geometry");
    for scale in [1.0, 2.0] {
        let mut canvas = Canvas::new(512, 192, [0, 0, 0, 0]);
        with_fonts(|fonts| {
            canvas.text(
                fonts,
                4.25 * scale,
                baseline * scale,
                s,
                px * scale,
                weight,
                face,
                [255, 255, 255, 255],
            )
        });
        let mut ink = 0;
        for (index, pixel) in canvas.px.as_chunks::<4>().0.iter().enumerate() {
            if pixel[3] == 0 {
                continue;
            }
            ink += 1;
            let y = (index / 512) as f32 / scale;
            assert!(
                y + 0.1 >= top && y + 1.0 / scale <= bottom + 0.1,
                "{s:?} {face:?} {weight:?} {px}px at {scale}x: ink {y} outside {top}..{bottom}"
            );
        }
        assert!(ink > 0, "{s:?} {face:?} has actual ink at {scale}x");
    }
}

#[test]
fn audited_text_bands_contain_the_painters_actual_ink_at_native_scales() {
    let mut renderer = Renderer::from_bytes(aterm_render::embedded_font(), 14.0, Theme::default())
        .expect("embedded renderer");
    renderer.set_runtime_font_discovery(false);
    install_settled_chrome_fonts_for_test(renderer);
    prepare_ui_fonts_for_direct_view_test();
    for face in [TextFace::Mono, TextFace::Ui, TextFace::UiBold] {
        for weight in [TextWeight::Regular, TextWeight::Bold] {
            for px in [11.0, 13.5, 26.0] {
                assert_audited_text_ink("ÅgjÉQp", px, weight, face, 40.25);
            }
        }
    }
    assert!(
        text_ink_band(
            "H\u{10FFFF}",
            13.0,
            TextWeight::Regular,
            TextFace::Mono,
            40.25
        )
        .is_none(),
        "a measured prefix must not hide an unmeasured glyph"
    );
}

/// An arc covers each pixel once: a translucent white arc over black lightens no pixel past
/// what one blend of it at full cover does, across its band, its round ends and the run
/// where a row crosses the ring's hole, and an opaque one covers about its area (the band's
/// length times its width, and the two half-discs of its ends), within 2 %.
#[test]
fn an_arc_covers_each_pixel_once_and_its_area() {
    let (w, h) = (120_u32, 120_u32);
    let raster = |prims: &[DrawPrim]| rasterize_tray(prims, w, h, 1.0, [0, 0, 0, 255]).0;
    let one = raster(&[DrawPrim::Dot {
        cx: 0.5,
        cy: 0.5,
        r: 3.0,
        color: [255, 255, 255, 128],
    }])[0];
    for (r, width, from, to) in [
        (40.0, 10.6, -0.375, 0.1),
        (40.0, 10.6, -0.375, 0.6),
        (12.0, 30.0, 0.0, 0.3),
        (30.0, 4.0, 0.2, 0.2),
    ] {
        let arc = |color: [u8; 4]| DrawPrim::Arc {
            cx: 60.3,
            cy: 59.6,
            r,
            width,
            from,
            to,
            color,
        };
        let px = raster(&[arc([255, 255, 255, 128])]);
        let brightest = px.chunks(4).map(|p| p[0]).max().unwrap_or(0);
        assert!(
            brightest <= one,
            "r {r} {from}..{to}: {brightest} past one blend's {one}"
        );
        let solid = raster(&[arc([255; 4])]);
        // The raster blends in linear light: a pixel's cover is its byte decoded.
        let linear = srgb_to_linear_lut();
        let area = solid
            .chunks(4)
            .map(|p| f64::from(linear[usize::from(p[0])]))
            .sum::<f64>();
        let span = f64::from((to - from).clamp(0.0, 1.0));
        let (r, half) = (f64::from(r), f64::from(width) / 2.0);
        let want =
            std::f64::consts::TAU * r * span * 2.0 * half + std::f64::consts::PI * half * half;
        assert!(
            (area - want).abs() < 0.02 * want,
            "r {r} {from}..{to}: covers {area:.1} px, not {want:.1}"
        );
    }
}

#[test]
fn a_span_blends_byte_for_byte_as_each_of_its_pixels() {
    let ground = |w: u32, h: u32| {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for i in 0..w * h {
            let run = (i / 7) % 5;
            px.extend_from_slice(&match run {
                0 => [0, 0, 0, 0],
                1 => [30, 40, 50, 255],
                2 => [200, 10, 90, 128],
                3 => [(i % 251) as u8, (i % 13) as u8 * 19, 7, (i % 256) as u8],
                _ => [255, 255, 255, 64],
            });
        }
        px
    };
    let colours = [
        [10, 20, 30, 255],
        [250, 250, 250, 40],
        [30, 40, 50, 128],
        [0, 0, 0, 1],
        [123, 45, 67, 0],
    ];
    let (w, h) = (64_u32, 6_u32);
    for origin in [(0, 0), (-5, -2), (17, 3)] {
        for clip in [
            None,
            Some((origin.0 + 9, origin.1 + 1, origin.0 + 50, origin.1 + 5)),
        ] {
            for (c, coverage) in colours
                .into_iter()
                .flat_map(|c| [1.0_f32, 0.999, 0.4, 0.0].map(|coverage| (c, coverage)))
            {
                for (x0, x1) in [(0, 64), (-10, 30), (12, 13), (40, 90), (30, 30)] {
                    let fresh = || {
                        let mut cv = Canvas::with_origin(origin.0, origin.1, w, h, [0; 4]);
                        cv.px = ground(w, h);
                        if let Some(clip) = clip {
                            cv.clip.push(clip);
                        }
                        cv
                    };
                    let (mut span, mut each) = (fresh(), fresh());
                    for y in origin.1 - 1..origin.1 + h as i32 + 1 {
                        span.blend_span(
                            origin.0 + x0,
                            origin.0 + x1,
                            y,
                            c,
                            coverage,
                            &mut RimMemo::default(),
                        );
                        for x in origin.0 + x0..origin.0 + x1 {
                            each.blend(x, y, c, coverage);
                        }
                    }
                    assert!(
                        span.px == each.px,
                        "origin {origin:?} clip {clip:?} colour {c:?} at {coverage} span {x0}..{x1}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_surface_composites_byte_for_byte_as_each_of_its_pixels() {
    let surface = |w: u32, h: u32, seed: u32| {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for i in 0..w * h {
            let k = i.wrapping_mul(2_654_435_761).wrapping_add(seed);
            px.extend_from_slice(&match (i / 5 + seed) % 6 {
                0 => [0, 0, 0, 0],
                1 | 2 => [30, 40, 50, 255],
                3 => [(k % 251) as u8, (k >> 8) as u8, (k >> 16) as u8, 255],
                4 => [(k % 251) as u8, 9, (k >> 3) as u8, (k >> 11) as u8],
                _ => [255, 255, 255, 64],
            });
        }
        px
    };
    let (dw, dh, sw, sh) = (40_u32, 9_u32, 23_u32, 6_u32);
    for (d_origin, s_origin) in [
        ((0, 0), (0, 0)),
        ((10, 4), (3, 1)),
        ((0, 0), (30, 6)),
        ((5, 5), (44, 2)),
        ((100, 100), (0, 0)),
    ] {
        let mut composed = surface(dw, dh, 1);
        assert!(composite_rgba_surface(
            &mut composed,
            (dw, dh),
            d_origin,
            &surface(sw, sh, 2),
            (sw, sh),
            s_origin,
        ));
        let mut each = Canvas::with_origin(d_origin.0 as i32, d_origin.1 as i32, dw, dh, [0; 4]);
        each.px = surface(dw, dh, 1);
        let source = surface(sw, sh, 2);
        for row in 0..sh {
            for col in 0..sw {
                let i = ((row * sw + col) * 4) as usize;
                let p = [source[i], source[i + 1], source[i + 2], source[i + 3]];
                each.blend((s_origin.0 + col) as i32, (s_origin.1 + row) as i32, p, 1.0);
            }
        }
        assert!(composed == each.px, "{d_origin:?} under {s_origin:?}");
    }
}

#[test]
fn a_rounded_box_draws_the_bytes_its_pixels_drew_one_by_one() {
    fn reference(cv: &mut Canvas, x: f32, y: f32, w: f32, h: f32, r: f32, c: [u8; 4]) {
        let (cx, cy, hx, hy) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let r = r.min(hx).min(hy);
        let (x0, y0, x1, y1) = cv.clipped_bounds(
            (x - 1.0).floor() as i32,
            (y - 1.0).floor() as i32,
            (x + w + 1.0).ceil() as i32,
            (y + h + 1.0).ceil() as i32,
        );
        let inset = 1.0_f32;
        let (inner_w, inner_h) = (w - inset * 2.0, h - inset * 2.0);
        let inner_r = (r - inset).max(0.0).min(inner_w / 2.0).min(inner_h / 2.0);
        let (inner_x, inner_y) = (x + inset, y + inset);
        for py in y0..y1 {
            let pyc = py as f32 + 0.5;
            let span =
                (inner_w > 0.0 && inner_h > 0.0 && pyc >= inner_y && pyc <= inner_y + inner_h)
                    .then(|| {
                        let (left, right) = if inner_r <= 0.0
                            || (pyc >= inner_y + inner_r && pyc <= inner_y + inner_h - inner_r)
                        {
                            (inner_x, inner_x + inner_w)
                        } else {
                            let corner = if pyc < inner_y + inner_r {
                                inner_y + inner_r
                            } else {
                                inner_y + inner_h - inner_r
                            };
                            let dy = (pyc - corner).abs();
                            let e = (inner_r * inner_r - dy * dy).max(0.0).sqrt();
                            (inner_x + inner_r - e, inner_x + inner_w - inner_r + e)
                        };
                        let start = ((left - 0.5).ceil() as i32).clamp(x0, x1);
                        let end = (((right - 0.5).floor() as i32).saturating_add(1)).clamp(x0, x1);
                        (start < end).then_some((start, end))
                    })
                    .flatten();
            for px in x0..x1 {
                let inside = span.is_some_and(|(start, end)| px >= start && px < end);
                let coverage = if inside {
                    1.0
                } else {
                    cov(sd_round_box(px as f32 + 0.5, pyc, cx, cy, hx, hy, r))
                };
                cv.blend(px, py, c, coverage);
            }
        }
    }
    let (w, h) = (90_u32, 60_u32);
    let ground = |origin: (i32, i32)| {
        let mut cv = Canvas::with_origin(origin.0, origin.1, w, h, [0; 4]);
        for (n, pixel) in cv.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let band = (n as u32 / w) / 9 + (n as u32 % w) / 23;
            *pixel = match band % 4 {
                0 => [0, 0, 0, 0],
                1 => [24, 28, 36, 255],
                2 => [(n % 256) as u8, 90, (n % 7) as u8 * 30, 200],
                _ => [240, 240, 245, 96],
            };
        }
        cv
    };
    let boxes = [
        (3.3_f32, 2.6_f32, 70.4_f32, 40.2_f32, 9.5_f32),
        (-4.5, 10.0, 50.0, 30.0, 0.0),
        (20.25, -3.75, 30.5, 70.0, 15.0),
        (10.0, 10.0, 2.5, 2.5, 1.0),
        (40.7, 30.1, 60.0, 8.0, 4.0),
        (5.0, 20.3, 80.0, 1.0, 0.0),
        (2.5, 33.6, 70.0, 2.0, 1.0),
        (-3.2, 44.5, 100.0, 0.5, 0.25),
        (30.0, 5.0, 1.0, 50.0, 0.0),
    ];
    let colours = [[250, 250, 250, 153], [12, 14, 16, 255], [90, 160, 255, 30]];
    for origin in [(0, 0), (-6, 4), (11, -3)] {
        for clip in [
            None,
            Some((origin.0 + 7, origin.1 + 5, origin.0 + 70, origin.1 + 52)),
        ] {
            for &(x, y, bw, bh, r) in &boxes {
                for c in colours {
                    let fresh = || {
                        let mut cv = ground(origin);
                        if let Some(clip) = clip {
                            cv.clip.push(clip);
                        }
                        cv
                    };
                    let (mut fast, mut slow) = (fresh(), fresh());
                    let (x, y) = (x + origin.0 as f32, y + origin.1 as f32);
                    fast.round_rect(x, y, bw, bh, r, c);
                    reference(&mut slow, x, y, bw, bh, r, c);
                    assert!(
                        fast.px == slow.px,
                        "origin {origin:?} clip {clip:?} box {:?} colour {c:?}",
                        (x, y, bw, bh, r)
                    );
                }
            }
        }
    }
}

#[test]
fn a_translucent_panel_over_a_flat_one_composites_a_handful_of_times() {
    let panel = |x: f32, y: f32, w: f32, h: f32, fill: [u8; 4]| DrawPrim::Panel {
        x,
        y,
        w,
        h,
        radius: 10.0,
        fill,
    };
    let prims = [
        panel(0.0, 0.0, 640.0, 440.0, [40, 44, 52, 230]),
        panel(20.0, 20.0, 600.0, 400.0, [250, 250, 250, 153]),
    ];
    let before = linear_composites();
    let rgba = rasterize_tray_pixels(&prims, 640, 440, 1.0, [0, 0, 0, 0]);
    let spent = linear_composites() - before;
    assert_eq!(rgba.len(), 640 * 440 * 4);
    assert!(
        spent < 100,
        "{spent} linear composites for two panels of 280,000 and 240,000 pixels"
    );
    let mut cv = Canvas::new(1, 1, [0; 4]);
    cv.blend(0, 0, [40, 44, 52, 230], 1.0);
    cv.blend(0, 0, [250, 250, 250, 153], 1.0);
    let middle = ((220 * 640 + 320) * 4) as usize;
    assert_eq!(rgba[middle..middle + 4], cv.px[..4]);
}

#[test]
fn a_word_the_ui_face_draws_whole_is_measured_once() {
    prepare_ui_fonts_for_direct_view_test();
    assert!(
        ui_font_of(TextFace::Ui).is_some(),
        "a UI face to measure in"
    );
    let (word, px) = ("Searchable scrollback lines", 13.0);
    let first = ui_text_width_for(TextFace::Ui, word, px);
    let walks = measure_walks();
    assert_eq!(ui_text_width_for(TextFace::Ui, word, px), first);
    assert_eq!(measure_walks(), walks, "kept");
    let paragraph = "Find searches this many recent lines; 0 searches only the screen.";
    let lines = ui_text_wrap_ranges(paragraph, px, 120.0);
    let walks = measure_walks();
    assert_eq!(ui_text_wrap_ranges(paragraph, px, 120.0), lines);
    assert_eq!(measure_walks(), walks, "kept");
    let mixed = "\u{8a2d}\u{5b9a} Settings";
    let _ = ui_text_width_for(TextFace::Ui, mixed, px);
    let walks = measure_walks();
    let _ = ui_text_width_for(TextFace::Ui, mixed, px);
    assert_eq!(measure_walks(), walks + 1, "never kept");
    let held = with_fonts(|fonts| fonts.ui_regular.clone());
    let other = ChromeFace::from_bytes(aterm_render::embedded_font(), 0)
        .map(|face| face.font)
        .expect("the embedded face");
    with_fonts(|fonts| fonts.ui_regular = Some(other));
    let walks = measure_walks();
    let _ = ui_text_width_for(TextFace::Ui, word, px);
    assert_eq!(measure_walks(), walks + 1, "a new font measures afresh");
    with_fonts(|fonts| fonts.ui_regular = held);
}

#[test]
fn lines_with_a_char_the_ui_face_lacks_are_kept_while_it_measures_the_same() {
    prepare_ui_fonts_for_direct_view_test();
    let font = ui_font_of(TextFace::Ui).expect("a UI face to measure in");
    let (px, width) = (13.0, 120.0);
    let laid = std::cell::Cell::new(0);
    let lay = |text: &str| {
        kept_lines(LinesRule::Copy, TextFace::Ui, text, px, width, || {
            laid.set(laid.get() + 1);
            vec![text.to_string()]
        })
    };
    let path = "Open System Settings \u{25b8} Privacy & Security to grant it.";
    assert!(
        !font.maps('\u{25b8}'),
        "premise: the UI face lacks the mark"
    );
    lay(path);
    lay(path);
    assert_eq!(laid.get(), 1, "kept");
    with_fonts(|fonts| {
        for (_, kept) in fonts.measures.lines.values_mut() {
            for (_, misses) in kept.values_mut() {
                for (_, advance) in misses.iter_mut() {
                    *advance ^= 1;
                }
            }
        }
    });
    lay(path);
    assert_eq!(
        laid.get(),
        2,
        "the mark measures otherwise now, so laid out again"
    );
    lay(path);
    assert_eq!(laid.get(), 2, "and kept at what it measures now");
    let east_asian = "\u{8a2d}\u{5b9a} \u{25b8} Settings";
    assert!(
        !font.maps('\u{8a2d}'),
        "premise: the UI face lacks the ideograph"
    );
    lay(east_asian);
    lay(east_asian);
    assert_eq!(
        laid.get(),
        4,
        "an East Asian char the face lacks is never kept"
    );
}

#[test]
fn the_settings_preview_resolves_the_bundled_default_family() {
    use aterm_render::bundled;
    let mut families = Vec::new();
    if bundled::ACTIVE {
        families.push((
            bundled::PRIMARY_FAMILY,
            bundled::PRIMARY_ID,
            bundled::PRIMARY_BOLD_ID,
        ));
        families.push((
            bundled::JETBRAINS_FAMILY,
            bundled::JETBRAINS_ID,
            bundled::JETBRAINS_BOLD_ID,
        ));
    }
    if bundled::CASCADIA_ACTIVE {
        families.push((
            bundled::CASCADIA_FAMILY,
            bundled::CASCADIA_ID,
            bundled::CASCADIA_BOLD_ID,
        ));
    }
    if bundled::IOSEVKA_ACTIVE {
        families.push((
            bundled::IOSEVKA_FAMILY,
            bundled::IOSEVKA_ID,
            bundled::IOSEVKA_BOLD_ID,
        ));
    }
    let mut cases = Vec::new();
    for (family, id, bold_id) in families {
        if aterm_render::resolve_font_family(family, aterm_render::FaceTarget::REGULAR).is_none() {
            cases.push((family.to_string(), id));
            cases.push((format!("{family} Bold"), bold_id));
        }
    }
    let mut cache = CandidateFaceCache::new();
    for (family, id) in cases {
        let family = family.as_str();
        let face = resolve_candidate_face(family, aterm_render::FaceTarget::REGULAR, &mut cache)
            .unwrap_or_else(|error| panic!("{family}: {error}"));
        assert_eq!(face.face.path, id);
        assert_eq!(
            &face.bytes[..],
            aterm_render::bundled::face_for_id(id).unwrap()
        );
    }
}

#[test]
fn chrome_font_store_is_cold_until_host_preparation_installs_ui_faces() {
    std::thread::spawn(|| {
        with_fonts(|fonts| {
            assert!(fonts.ui_regular.is_none());
            assert!(fonts.ui_semibold.is_none());
        });

        set_chrome_fonts(None, None, None);
        let expected = prepared_ui_font_assets();
        with_fonts(|fonts| {
            match (&expected.regular, &fonts.ui_regular) {
                (Some(expected), Some(installed)) => assert!(Arc::ptr_eq(expected, installed)),
                (None, None) => {}
                _ => panic!("regular UI face did not match the prepared host asset"),
            }
            match (&expected.semibold, &fonts.ui_semibold) {
                (Some(expected), Some(installed)) => assert!(Arc::ptr_eq(expected, installed)),
                (None, None) => {}
                _ => panic!("semibold UI face did not match the prepared host asset"),
            }
        });
    })
    .join()
    .expect("cold-to-prepared font transition exits cleanly");
}

#[cfg(windows)]
#[test]
fn windows_ui_face_candidates_lead_with_segoe_ui_variable_over_a_static_semibold() {
    let candidates = ui_font_candidates();
    let file = |path: &std::path::Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default()
    };
    assert_eq!(candidates.len(), 2);
    assert_eq!(file(&candidates[0].regular_path), "seguivar.ttf");
    assert_eq!(file(&candidates[0].semibold_path), "seguisb.ttf");
    assert_eq!(file(&candidates[1].regular_path), "segoeui.ttf");
    assert_eq!(file(&candidates[1].semibold_path), "seguisb.ttf");
    for candidate in &candidates {
        assert_ne!(
            candidate.regular_path, candidate.semibold_path,
            "a candidate must never pair a face with itself"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_ui_face_ladder_reaches_noto_on_bold_only_hosts() {
    let candidates = ui_font_candidates();
    let file = |path: &std::path::Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default()
    };
    let noto_pairs: Vec<(String, String)> = candidates
        .iter()
        .filter(|candidate| file(&candidate.regular_path).contains("noto"))
        .map(|candidate| {
            (
                file(&candidate.regular_path),
                file(&candidate.semibold_path),
            )
        })
        .collect();
    assert!(
        noto_pairs
            .iter()
            .any(|(regular, semibold)| regular == "notosans-regular.ttf"
                && semibold == "notosans-bold.ttf"),
        "a Regular+Bold pairing must back up the SemiBold cut Debian never ships: {noto_pairs:?}"
    );
    assert_eq!(
        noto_pairs.first().map(|(_, semibold)| semibold.as_str()),
        Some("notosans-semibold.ttf"),
        "the true SemiBold keeps priority where a distro ships it"
    );
    let first_dejavu = candidates
        .iter()
        .position(|candidate| file(&candidate.regular_path).contains("dejavu"))
        .expect("DejaVu remains the last-resort pair");
    let last_noto = candidates
        .iter()
        .rposition(|candidate| file(&candidate.regular_path).contains("noto"))
        .expect("Noto pairs exist");
    assert!(
        last_noto < first_dejavu,
        "every Noto pairing outranks the wider DejaVu fallback"
    );
    for candidate in &candidates {
        assert_ne!(
            candidate.regular_path, candidate.semibold_path,
            "a candidate must never pair a face with itself"
        );
    }
    let debian_regular =
        std::path::Path::new("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf");
    let debian_bold = std::path::Path::new("/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf");
    if debian_regular.exists() && debian_bold.exists() {
        // Past the count: [`UI_FONT_RESOLVE_ATTEMPTS`] counts the host's resolutions, and
        // a test of this binary that reads it would see this one as its own.
        let _ = prepared_ui_font_assets();
        let host = UI_FONT_RESOLVE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed);
        let assets = resolve_ui_font_assets_with(&mut UiFontFiles::default());
        assert_eq!(
            UI_FONT_RESOLVE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed),
            host,
            "the ladder's resolution counted as the host's"
        );
        assert!(
            assets.regular.is_some(),
            "Noto regular resolves on a host carrying the Debian core cuts"
        );
        assert!(
            assets.semibold.is_some(),
            "a heavier companion resolves beside the regular"
        );
        let first_complete = candidates
            .iter()
            .find(|candidate| candidate.regular_path.exists() && candidate.semibold_path.exists())
            .expect("some pair resolves on a host with fonts installed");
        assert!(
            file(&first_complete.regular_path).contains("noto"),
            "the first complete pair is Noto, never the DejaVu fallback: {:?}",
            first_complete.regular_path
        );
    }
}

#[test]
fn ui_font_assets_admit_each_distinct_file_once() {
    let mut files = UiFontFiles::default();
    let assets = resolve_ui_font_assets_with(&mut files);
    assert_eq!(
        files.reads,
        files.admitted.len(),
        "every distinct path was admitted exactly once"
    );
    let Some(pair) = ui_font_candidates()
        .into_iter()
        .find(|c| c.regular_path.is_file() && c.semibold_path.is_file())
    else {
        aterm_spec::host::could_not_run(
            "no complete UI face pair (regular and semibold) on this host",
        );
    };
    assert!(assets.regular.is_some() && assets.semibold.is_some());
    if pair.regular_path == pair.semibold_path {
        assert_eq!(
            files.reads,
            1,
            "regular and semibold live in ONE file ({}); it must be admitted once, \
             not once per face",
            pair.regular_path.display()
        );
    }
    for (path, admitted) in &files.admitted {
        let Some(bytes) = admitted else { continue };
        assert_eq!(
            bytes.is_mapped(),
            cfg!(any(unix, windows)) && aterm_render::font_file::maps_in_place(path),
            "{}: a UI face is a file MAPPING exactly when it lives on the system font \
             volume, and a copy otherwise",
            path.display()
        );
    }
}

#[test]
fn strip_band_seams_follow_the_installed_ui_face() {
    clear_ui_fonts_for_test();
    assert!(!strip_band_ui_ready());
    assert!(!strip_band_run_coverable("Settings"));
    let cold = strip_band_font_epoch();
    prepare_ui_fonts_for_direct_view_test();
    if !strip_band_ui_ready() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    assert_ne!(
        strip_band_font_epoch(),
        cold,
        "the UI face landing must move the band's font fingerprint"
    );
    assert!(strip_band_run_coverable("Settings · pwsh 7"));
    assert!(
        !strip_band_run_coverable("\u{1F680}"),
        "a colour emoji is covered by neither the UI face nor the embedded cascade"
    );
    clear_ui_fonts_for_test();
}

#[test]
fn ui_font_assets_are_host_prepared_and_compile_raster_without_resolver_io() {
    use crate::native_ui::{
        GroupSpec, Insets, Layout, Length, LogicalRect, SemanticRole, StyleRef, TextSpec,
        UiContent, UiNode, UiTree,
    };

    set_chrome_fonts(None, None, None);
    let attempts = UI_FONT_RESOLVE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(attempts >= 1, "host preparation exercised the resolver");
    let installed = with_fonts(|fonts| fonts.ui_regular.clone());

    let tree = UiTree::new(
        UiNode::new(
            "root",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column().padding(Insets::all(8.0)))
        .children(vec![
            UiNode::new(
                "label",
                UiContent::Text(TextSpec {
                    text: "Settings · Aa 012 ✓".to_string(),
                    role: SemanticRole::Heading,
                    style: StyleRef::Primary,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(32.0))),
        ]),
    );
    let viewport = LogicalRect::new(0.0, 0.0, 260.0, 56.0);
    let compiled = tree.compile(viewport).expect("semantic UI compiles");
    let measured = ui_text_width_for(TextFace::Ui, "Settings · Aa 012 ✓", 13.0);
    assert!(measured > 0.0);
    let prims = compiled.tray(Theme::default(), 13.0).prims;
    let (image, _, _) = rasterize_tray(&prims, 260, 56, 1.0, [0, 0, 0, 0]);
    let (pixels, remainder) = image.as_chunks::<4>();
    assert!(remainder.is_empty(), "tray raster has complete pixels");
    assert!(pixels.iter().any(|pixel| pixel[3] != 0));

    assert_eq!(
        UI_FONT_RESOLVE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed),
        attempts,
        "compile, measure, tray paint, and image raster perform no font resolution",
    );
    let after = with_fonts(|fonts| fonts.ui_regular.clone());
    match (installed, after) {
        (Some(before), Some(after)) => assert!(Arc::ptr_eq(&before, &after)),
        (None, None) => {}
        _ => panic!("immutable UI asset identity changed during compile/raster"),
    }
}

fn demo_prims(cw: f32, ch: f32, theme: Theme) -> Vec<DrawPrim> {
    use crate::type_scale::TypeStep;
    use crate::widget::{TextFace, TextWeight, rgba, text_prim};
    let c = crate::chrome_band::band_colors(theme);
    let label = rgba(c.label, 0xFF);
    let track = rgba(c.label, 0x33);
    let good = rgba(c.value, 0xFF);
    let cap = TypeStep::Caption.px(12.0);
    let slot = (cw - 32.0) / 3.0;
    let mut prims = vec![text_prim(
        16.0,
        24.0,
        "RINGS".to_string(),
        cap,
        TextWeight::Bold,
        TextFace::Mono,
        label,
    )];
    for i in 0..3u32 {
        prims.push(DrawPrim::Ring {
            cx: 16.0 + slot * (i as f32 + 0.5),
            cy: 70.0,
            r_outer: 26.0,
            thickness: 7.0,
            track,
            start: 0.0,
            frac: 0.3 + i as f32 * 0.2,
            fill: good,
        });
    }
    prims.push(DrawPrim::Capsule {
        x: 16.0,
        y: ch - 48.0,
        w: cw - 32.0,
        h: 8.0,
        frac: 0.4,
        fill: good,
        track,
    });
    prims.push(DrawPrim::Dot {
        cx: cw - 20.0,
        cy: 20.0,
        r: 4.0,
        color: good,
    });
    prims
}

#[test]
fn chrome_face_gate_exhaustive() {
    let (mut fallback_seen, mut bold_seen, mut primary_seen) = (false, false, false);
    for bits in 0u32..8 {
        let b = |i: u32| (bits >> i) & 1 == 1;
        let (bold_run, bold_has, primary_has) = (b(0), b(1), b(2));
        let pick = select_chrome_face(bold_run, bold_has, primary_has);
        if pick == ChromeFacePick::Fallback {
            fallback_seen = true;
            assert!(
                !(primary_has || (bold_run && bold_has)),
                "DejaVu chosen although a user face covers the glyph \
                 (bold_run={bold_run}, bold_has={bold_has}, primary_has={primary_has})"
            );
        }
        if bold_run && bold_has {
            bold_seen = true;
            assert_eq!(
                pick,
                ChromeFacePick::Bold,
                "covered bold run lost its weight"
            );
        }
        if pick == ChromeFacePick::Primary {
            primary_seen = true;
            assert!(primary_has, "primary chosen without coverage");
        }
    }
    assert!(
        fallback_seen && bold_seen && primary_seen,
        "gate must reach all three faces (fallback={fallback_seen}, \
         bold={bold_seen}, primary={primary_seen})"
    );
}

#[test]
fn row_baseline_balances_gaps_on_the_shipping_path() {
    let ratio = chrome_cap_ratio();
    assert!((0.4..1.0).contains(&ratio), "sane cap ratio: {ratio}");
    for row_h in [12.0_f32, 17.0, 20.0, 21.5, 34.0, 64.0] {
        for size in [8.0_f32, 9.6, 12.0, 13.8, 14.0, 26.0] {
            let cap = size * ratio;
            if cap > row_h {
                continue;
            }
            for y0 in [0.0_f32, 7.5, 100.0] {
                let b = row_baseline(y0, row_h, size);
                let top_gap = (b - cap) - y0;
                let bottom_gap = (y0 + row_h) - b;
                assert!(
                    (top_gap - bottom_gap).abs() <= 1.0 + 1.0 / 64.0 + 1e-3,
                    "row_h={row_h} size={size} y0={y0}: gaps {top_gap} vs {bottom_gap}"
                );
            }
        }
    }
    let (row_h, size) = (20.0_f32, 14.0_f32);
    let old_b = (row_h - size) * 0.5 + size;
    let cap = size * ratio;
    assert!(
        ((old_b - cap) - (row_h - old_b)).abs() > 1.5,
        "the pre-fix baseline must be detectably unbalanced"
    );
}

#[test]
fn draw_stays_within_measured_width() {
    let s = "MMMMMMMMMMMMMMMMMMMMMMMMMMMMMMMM"; // 32 advances
    let px = 13.0_f32;
    let measured = measure_text(s, px, TextWeight::Regular);
    assert!(measured > 0.0);
    let w = (measured.ceil() as u32) + 4;
    let prims = vec![crate::widget::text_prim(
        1.0,
        14.0,
        s.to_string(),
        crate::type_scale::TypeStep::Body.px(px),
        TextWeight::Regular,
        TextFace::Mono,
        [255, 255, 255, 255],
    )];
    let (buf, pw, ph) = rasterize_tray(&prims, w, 20, 1.0, [0, 0, 0, 255]);
    let mut ink_right = 0u32;
    for yy in 0..ph {
        for xx in 0..pw {
            if buf[((yy * pw + xx) * 4) as usize] > 0 {
                ink_right = ink_right.max(xx + 1);
            }
        }
    }
    assert!(ink_right > 0, "the run painted");
    assert!(
        (ink_right as f32) <= 1.0 + measured + 1.0,
        "ink right edge {ink_right} exceeds measured width {measured} (+1px rounding)"
    );
}

const MONO_GLYPH_BASELINE: f32 = 40.0;
const MONO_GLYPH_CANVAS: (u32, u32) = (64, 56);
const MONO_GLYPH_PX: f32 = 26.0;

fn mono_run_image(primary: &Arc<[u8]>, s: &str) -> Vec<u8> {
    set_chrome_fonts(Some((Arc::clone(primary), 0)), None, None);
    let prims = vec![crate::widget::text_prim(
        4.0,
        MONO_GLYPH_BASELINE,
        s.to_string(),
        crate::type_scale::TypeStep::Body.px(MONO_GLYPH_PX),
        TextWeight::Regular,
        TextFace::Mono,
        [255, 255, 255, 255],
    )];
    let (width, height) = MONO_GLYPH_CANVAS;
    rasterize_tray(&prims, width, height, 1.0, [0, 0, 0, 255]).0
}

fn primary_draws(ch: char) -> bool {
    with_fonts(|fonts| {
        let Some(primary) = fonts.primary.as_ref().map(|face| Arc::clone(&face.font)) else {
            return false;
        };
        fonts
            .face_for(TextWeight::Regular, ch)
            .is_some_and(|face| Arc::ptr_eq(&face.font, &primary))
    })
}

#[test]
fn a_mono_run_draws_latin_1_by_the_unicode_cmap() {
    let shipped: Arc<[u8]> = Arc::from(aterm_render::embedded_font());
    let moved: Arc<[u8]> = aterm_render::font::with_cmap_record_last(&shipped, 1, 0)
        .expect("DejaVu Sans Mono carries a Mac Roman (1,0) subtable")
        .into();
    for (ch, mac) in [
        ('\u{B7}', '\u{2211}'),
        ('\u{D7}', '\u{25CA}'),
        ('\u{B0}', '\u{221E}'),
        ('\u{E9}', '\u{C8}'),
    ] {
        let drawn = mono_run_image(&moved, &ch.to_string());
        assert!(
            primary_draws(ch),
            "the reordered face draws {ch} itself, not the fallback under it"
        );
        let (texels, _) = drawn.as_chunks::<4>();
        assert!(texels.iter().any(|texel| texel[0] > 0), "{ch} inks");
        assert!(
            drawn == mono_run_image(&shipped, &ch.to_string()),
            "{ch} draws as the shipped face draws it"
        );
        assert!(
            drawn != mono_run_image(&shipped, &mac.to_string()),
            "{ch} is not drawn as {mac}, the Mac Roman glyph of its byte"
        );
    }
    set_chrome_fonts(None, None, None);
}

#[cfg(target_os = "macos")]
#[test]
fn a_mono_run_on_monaco_draws_the_middle_dot_as_a_dot() {
    let path = "/System/Library/Fonts/Monaco.ttf";
    let Ok(monaco) = std::fs::read(path) else {
        aterm_spec::host::could_not_run(format!("no {path} on this Mac"));
    };
    let monaco: Arc<[u8]> = monaco.into();
    let (width, height) = MONO_GLYPH_CANVAS;
    let ink = |image: &[u8]| {
        let mut count = 0u32;
        let mut bounds = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..height {
            for x in 0..width {
                if image[((y * width + x) * 4) as usize] > 0 {
                    count += 1;
                    bounds = (
                        bounds.0.min(x),
                        bounds.1.min(y),
                        bounds.2.max(x + 1),
                        bounds.3.max(y + 1),
                    );
                }
            }
        }
        (count, bounds)
    };
    let dot_image = mono_run_image(&monaco, "\u{B7}");
    assert!(
        primary_draws('\u{B7}'),
        "Monaco draws · itself, not the fallback under it"
    );
    let (dot, (x0, y0, x1, y1)) = ink(&dot_image);
    let (sum, _) = ink(&mono_run_image(&monaco, "\u{2211}"));
    assert!(dot > 0 && dot * 4 < sum, "· inks {dot} texels, ∑ {sum}");
    let side = MONO_GLYPH_PX * 0.25;
    assert!(
        ((x1 - x0) as f32) < side && ((y1 - y0) as f32) < side,
        "· inks a small box: {x0}..{x1} × {y0}..{y1}"
    );
    let raised = MONO_GLYPH_BASELINE - (y0 + y1) as f32 / 2.0;
    assert!(
        (MONO_GLYPH_PX * 0.15..MONO_GLYPH_PX * 0.5).contains(&raised),
        "· sits {raised} px above the baseline"
    );
    let dejavu: Arc<[u8]> = Arc::from(aterm_render::embedded_font());
    assert!(
        dot_image != mono_run_image(&dejavu, "\u{B7}"),
        "· is Monaco's dot, not the embedded DejaVu's"
    );
    let times = mono_run_image(&monaco, "\u{D7}");
    assert!(
        primary_draws('\u{D7}'),
        "Monaco draws × itself, not the fallback under it"
    );
    assert!(
        times != mono_run_image(&monaco, "\u{25CA}"),
        "× is not drawn as ◊"
    );
    set_chrome_fonts(None, None, None);
}

fn ink_clusters(buf: &[u8], pw: u32, ph: u32) -> Vec<(u32, u32, u32, u32)> {
    let mut clusters: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut open = false;
    for x in 0..pw {
        let mut column: Option<(u32, u32)> = None;
        for y in 0..ph {
            if buf[((y * pw + x) * 4) as usize] > 0 {
                column = Some(column.map_or((y, y + 1), |(a, b)| (a.min(y), b.max(y + 1))));
            }
        }
        match column {
            Some((y0, y1)) if open => {
                let last = clusters.last_mut().expect("an open cluster");
                last.1 = last.1.min(y0);
                last.2 = x + 1;
                last.3 = last.3.max(y1);
            }
            Some((y0, y1)) => {
                clusters.push((x, y0, x + 1, y1));
                open = true;
            }
            None => open = false,
        }
    }
    clusters
}

#[test]
fn a_chain_face_char_draws_on_the_run_baseline_not_its_own_ascent() {
    if !install_chrome_fonts_with_cjk_chain_for_test() {
        aterm_spec::host::could_not_run("no CJK chain face here to install");
    }
    clear_ui_fonts_for_test();
    let px = 16.0_f32;
    let row_h = 28.0_f32;
    let baseline = row_baseline(0.0, row_h, px);
    with_fonts(|fonts| {
        assert!(
            fonts.ui_font(TextFace::Ui).is_none(),
            "force the coverage fallback rather than a host UI face"
        );
        assert_eq!(
            select_chrome_face(false, false, false),
            ChromeFacePick::Fallback,
            "with no user face the gate lands on DejaVu"
        );
        assert!(
            !fonts.fallback.as_ref().expect("embedded DejaVu").has('日'),
            "negative control: the gate's own pick cannot draw 日"
        );
        assert!(
            fonts.face_for(TextWeight::Regular, '日').is_some(),
            "the coverage rung reaches 日"
        );
    });
    let prims = vec![crate::widget::text_prim(
        2.0,
        baseline,
        "H日".to_string(),
        crate::type_scale::TypeStep::Body.px(px),
        TextWeight::Regular,
        TextFace::Ui,
        [255, 255, 255, 255],
    )];
    let (buf, pw, ph) = rasterize_tray(&prims, 72, row_h as u32, 1.0, [0, 0, 0, 255]);
    let clusters = ink_clusters(&buf, pw, ph);
    assert!(
        clusters.len() >= 2,
        "H and 日 both ink, apart: {clusters:?}"
    );
    let h = clusters[0];
    let ideo = *clusters.last().expect("clusters");
    let baseline_i = q_round_to_px(px_to_q(baseline)) as f32;
    assert!(
        (ideo.3 as f32 - 1.0) <= baseline_i + 0.2 * px,
        "日 hangs below the baseline: ink {ideo:?}, baseline {baseline_i}"
    );
    assert!(
        ideo.1 as f32 >= baseline_i - px,
        "日 reaches above an em over the baseline: ink {ideo:?}, baseline {baseline_i}"
    );
    let centre = |c: (u32, u32, u32, u32)| (c.1 + c.3) as f32 * 0.5;
    assert!(
        (centre(ideo) - centre(h)).abs() <= 2.0,
        "日 centre {} vs H centre {} (ink {ideo:?} / {h:?})",
        centre(ideo),
        centre(h)
    );
    let (top, bottom) = text_ink_band("H日", px, TextWeight::Regular, TextFace::Ui, baseline)
        .expect("both the embedded Latin face and the CJK chain face are measured");
    let (twice, pw, ph) = rasterize_tray(&prims, 72, row_h as u32, 2.0, [0, 0, 0, 255]);
    for (scale, clusters) in [(1.0, clusters), (2.0, ink_clusters(&twice, pw, ph))] {
        assert!(clusters.len() >= 2, "both scripts must ink at {scale}x");
        for (_, first, _, last) in clusters {
            assert!(
                first as f32 / scale + 0.1 >= top && last as f32 / scale <= bottom + 0.1,
                "fallback ink {first}..{last} at {scale}x outside audited band {top}..{bottom}"
            );
        }
    }
    clear_ui_fonts_for_test();
}

#[test]
fn ui_measure_agrees_with_the_pen_for_a_chain_face_char() {
    if !install_chrome_fonts_with_cjk_chain_for_test() || !strip_band_ui_ready() {
        clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let s = "A日B";
    let px = 13.0_f32;
    let measured = ui_text_width_for(TextFace::Ui, s, px);
    let notdef_measure = with_fonts(|fonts| {
        let f = fonts.ui_regular.clone().expect("UI regular");
        let mut w = 0.0;
        let mut prev: Option<char> = None;
        for ch in s.chars() {
            if let Some(p) = prev {
                w += f.char_kern(p, ch, px).unwrap_or(0.0);
            }
            w += f.char_metrics(ch, px).advance_width + UI_TRACKING_EM * px;
            prev = Some(ch);
        }
        w
    });
    assert!(
        measured - notdef_measure > 2.0,
        "the `.notdef` measure {notdef_measure} must under-measure the pen's {measured}"
    );
    let w = (measured.ceil() as u32) + 6;
    let prims = vec![crate::widget::text_prim(
        1.0,
        14.0,
        s.to_string(),
        crate::type_scale::TypeStep::Body.px(px),
        TextWeight::Regular,
        TextFace::Ui,
        [255, 255, 255, 255],
    )];
    let (buf, pw, ph) = rasterize_tray(&prims, w, 20, 1.0, [0, 0, 0, 255]);
    let mut ink_right = 0u32;
    for yy in 0..ph {
        for xx in 0..pw {
            if buf[((yy * pw + xx) * 4) as usize] > 0 {
                ink_right = ink_right.max(xx + 1);
            }
        }
    }
    assert!(ink_right > 0, "the run painted");
    assert!(
        (ink_right as f32) <= 1.0 + measured + 1.0,
        "ink right edge {ink_right} exceeds measured width {measured} (+1px rounding)"
    );
    assert!(
        (ink_right as f32) >= 1.0 + measured - 2.0,
        "ink right edge {ink_right} stops short of measured width {measured}"
    );
    clear_ui_fonts_for_test();
}

#[test]
fn coverage_rung_is_consulted_only_after_the_embedded_fallback_misses() {
    if !install_chrome_fonts_with_cjk_chain_for_test() {
        aterm_spec::host::could_not_run("no CJK chain face here to install");
    }
    with_fonts(|fonts| {
        let dejavu = fonts
            .fallback
            .as_ref()
            .expect("embedded DejaVu parses")
            .font
            .clone();
        fonts.sync_coverage();
        let chain = fonts
            .coverage
            .first()
            .expect("the parsed CJK face is admitted")
            .font
            .clone();
        assert!(chain.maps('日'));
        let shared = ['\u{2713}', 'λ', 'π', '√', 'A']
            .into_iter()
            .find(|&ch| dejavu.maps(ch) && chain.maps(ch))
            .expect("Latin at least is in both");
        let face = fonts
            .face_for(TextWeight::Regular, shared)
            .expect("covered");
        assert!(
            Arc::ptr_eq(&face.font, &dejavu),
            "{shared:?} stays on the embedded fallback"
        );
        for weight in [TextWeight::Regular, TextWeight::Bold] {
            let face = fonts.face_for(weight, '日').expect("the rung covers 日");
            assert!(
                Arc::ptr_eq(&face.font, &chain),
                "日 at {weight:?} draws from the chain face"
            );
        }
        assert!(
            fonts.face_for(TextWeight::Regular, '\u{10FFFD}').is_none(),
            "a char no loaded face carries still resolves to nothing"
        );
    });
    clear_ui_fonts_for_test();
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn strip_band_font_epoch_moves_when_a_chain_face_lands() {
    let Some(path) = cjk_chain_face_path_for_test() else {
        aterm_spec::host::could_not_run("no CJK chain face here");
    };
    let theme = aterm_render::Theme::default();
    let mut live = Renderer::from_bytes(aterm_render::embedded_font(), 14.0, theme)
        .expect("embedded renderer");
    live.set_runtime_font_discovery(false);
    live.set_config_fallback_fonts(&[path.to_string()]);
    let _ = live.seal_admitted_font_sources();
    let seed = live
        .fork_semantic_surface(14.0, theme)
        .expect("a sealed generation forks");
    install_dormant_chrome_fonts_for_test(seed);
    prepare_ui_fonts_for_direct_view_test();
    let before = strip_band_font_epoch();
    with_fonts(|fonts| {
        assert!(
            fonts.face_for(TextWeight::Regular, '日').is_none(),
            "an unparsed chain face is not admitted"
        );
    });
    assert_eq!(
        strip_band_font_epoch(),
        before,
        "a re-read is not a landing"
    );
    live.prepare_semantic_text("日");
    let after = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(5),
        || {
            let now = strip_band_font_epoch();
            (now != before).then_some(now)
        },
    )
    .unwrap_or_else(strip_band_font_epoch);
    assert_ne!(after, before, "the band key witnesses the landing");
    with_fonts(|fonts| {
        assert!(
            fonts.face_for(TextWeight::Regular, '日').is_some(),
            "the rung now reaches 日"
        );
    });
    if strip_band_ui_ready() {
        assert!(strip_band_run_coverable("日本語 shell"));
    }
    clear_ui_fonts_for_test();
}

fn ready_chain_faces() -> usize {
    with_fonts(|fonts| {
        fonts
            .semantic
            .as_ref()
            .map_or(0, |seed| seed.ready_fallback_faces().count())
    })
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_cjk_label_has_its_chain_face_parsed_when_the_band_asks() {
    let Some(_live) = install_unparsed_cjk_chain_for_test() else {
        aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
    };
    set_ui_cjk_faces_for_test(None);
    if !strip_band_ui_ready() {
        clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    assert!(strip_band_run_coverable("alpha"));
    assert_eq!(ready_chain_faces(), 0, "a Latin label parses nothing");
    assert!(
        !chrome_cascade_reaches_for_test('日'),
        "CONTROL: nothing has parsed the CJK face"
    );
    assert!(!strip_band_run_coverable("logs \u{0378}"));
    assert_eq!(ready_chain_faces(), 0, "no face maps U+0378: none parsed");
    assert!(
        strip_band_run_coverable("日本語 notes"),
        "the CJK label is coverable on its first ask"
    );
    assert!(ready_chain_faces() >= 1);
    assert!(chrome_cascade_reaches_for_test('日'));
    clear_ui_fonts_for_test();
}

#[test]
fn the_startup_warm_parses_the_cjk_and_hangul_faces_only() {
    let Some(live) = install_unparsed_cjk_chain_for_test() else {
        aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
    };
    assert_eq!(ready_chain_faces(), 0, "CONTROL: nothing parsed yet");
    assert!(warm_chrome_coverage(Some(&live.fallback_coverage()), None) >= 1);
    assert!(chrome_cascade_reaches_for_test('日'));
    let parsed: Vec<Arc<ChromeFont>> = with_fonts(|fonts| {
        fonts
            .semantic
            .as_ref()
            .map(|seed| seed.ready_fallback_faces().cloned().collect())
            .unwrap_or_default()
    });
    assert!(!parsed.is_empty());
    for face in &parsed {
        assert!(
            CHROME_COVERAGE_WARM.chars().any(|ch| face.maps(ch)),
            "the warm parsed a face that maps none of {CHROME_COVERAGE_WARM:?}"
        );
    }
    assert_eq!(
        warm_chrome_coverage(Some(&live.fallback_coverage()), None),
        0,
        "a second warm parses nothing"
    );
    clear_ui_fonts_for_test();
}

/// A face the coverage warm parses changes what chrome text can ink, the epoch a native
/// view's raster is stamped with, and wakes the event loop to draw it again; a warm that
/// parses nothing changes and wakes nothing. Before, the warm's faces landed silently,
/// so a first open's Japanese stayed blank until something else redrew.
#[test]
fn a_face_the_warm_parses_restamps_and_wakes_the_native_views() {
    let Some(live) = install_unparsed_cjk_chain_for_test() else {
        aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
    };
    let before = chrome_coverage_epoch();
    assert_eq!(
        chrome_coverage_epoch(),
        before,
        "stable while nothing lands"
    );
    let wakes = std::cell::Cell::new(0);
    let coverage = live.fallback_coverage();
    let landed = land_chrome_coverage(
        || warm_chrome_coverage(Some(&coverage), None),
        || wakes.set(wakes.get() + 1),
    );
    assert!(landed >= 1, "CONTROL: the warm parsed the CJK face");
    assert_eq!(wakes.get(), 1, "the landing wakes the event loop");
    assert_ne!(
        chrome_coverage_epoch(),
        before,
        "the native views are stale"
    );
    let after = chrome_coverage_epoch();
    let landed = land_chrome_coverage(
        || warm_chrome_coverage(Some(&coverage), None),
        || wakes.set(wakes.get() + 1),
    );
    assert_eq!((landed, wakes.get()), (0, 1), "nothing new, no wake");
    assert_eq!(chrome_coverage_epoch(), after);
    clear_ui_fonts_for_test();
}

#[test]
fn every_install_warms_the_faces_a_cjk_label_draws_with() {
    let Some(_live) = install_unparsed_cjk_chain_for_test() else {
        aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
    };
    let label = "設定を開く";
    assert!(
        !chrome_cascade_reaches_for_test('設'),
        "CONTROL: nothing has parsed the CJK face"
    );
    let warm = with_fonts(|fonts| chrome_coverage_warm(fonts)).expect("an install warms its faces");
    assert!(warm() >= 1, "the warm parses the chain's CJK face");
    assert!(chrome_cascade_reaches_for_test('設'));
    assert_eq!(
        with_fonts(|fonts| fonts.label_misses(TextFace::Ui, label)),
        Vec::<char>::new(),
        "{label} draws in ink"
    );
    clear_ui_fonts_for_test();
}

#[test]
fn east_asian_text_splits_into_hangul_and_han_and_nothing_else_is_asked() {
    for ch in [
        '日', '中', '简', 'の', 'カ', 'ｶ', '、', '「', 'Ａ', '㈱', '㌀', 'ㄅ', '𠀋',
    ] {
        assert_eq!(ui_cjk_script(ch), Some(UiCjkScript::Han), "{ch:?}");
    }
    for ch in ['한', '국', 'ㄱ', 'ᄀ', '㈀', '㉠', 'ￂ'] {
        assert_eq!(ui_cjk_script(ch), Some(UiCjkScript::Hangul), "{ch:?}");
    }
    for ch in [
        'a',
        'é',
        'λ',
        'Ж',
        '→',
        '★',
        '※',
        '✓',
        'क',
        'ก',
        '①',
        '\u{1F680}',
        ' ',
        '\u{3000}',
    ] {
        let asked = ui_cjk_script(ch);
        assert_eq!(asked.is_some(), ch == '\u{3000}', "{ch:?}: {asked:?}");
    }
}

#[cfg(windows)]
fn install_bold_cjk_chain_and_cold_ui_cjk_for_test() -> Option<Arc<UiCjkFaces>> {
    let root = std::path::PathBuf::from(
        std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into()),
    )
    .join("Fonts");
    let mut renderer = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .ok()?;
    renderer.set_runtime_font_discovery(false);
    for file in ["msyhbd.ttc", "malgunbd.ttf"] {
        let bytes = std::fs::read(root.join(file)).ok()?;
        renderer.add_fallback_bytes(&bytes).ok()?;
    }
    install_settled_chrome_fonts_for_test(renderer);
    prepare_ui_fonts_for_direct_view_test();
    let store = host_ui_cjk_faces_for_test()?;
    set_ui_cjk_faces_for_test(Some(Arc::clone(&store)));
    strip_band_ui_ready().then_some(store)
}

#[cfg(windows)]
fn label_ink(s: &str, px: f32, face: TextFace) -> (f32, usize) {
    let prims = vec![DrawPrim::Text {
        x: 4.0,
        baseline: px * 1.4,
        s: s.to_string(),
        px,
        color: [255, 255, 255, 255],
        weight: TextWeight::Regular,
        face,
    }];
    let (w, h) = ((px * 12.0) as u32, (px * 2.0) as u32);
    let (pixels, _, _) = rasterize_tray(&prims, w, h, 1.0, [0, 0, 0, 255]);
    let lit: Vec<f32> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from(c[0]) / 255.0)
        .filter(|&t| t > 0.05)
        .collect();
    (lit.iter().sum::<f32>() / lit.len().max(1) as f32, lit.len())
}

#[cfg(windows)]
#[test]
fn a_quiet_labels_east_asian_text_inks_like_its_latin_and_the_selected_ones_is_semibold() {
    let Some(store) = install_bold_cjk_chain_and_cold_ui_cjk_for_test() else {
        clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run(
            "no Microsoft YaHei Bold, Malgun Gothic Bold or UI face here to install",
        );
    };
    let px = 26.0;
    for (text, latin) in [("日本語", "notes"), ("한국어", "logs")] {
        assert!(strip_band_run_coverable_in(TextFace::Ui, text));
        assert!(strip_band_run_coverable_in(TextFace::UiBold, text));
        let (cjk, cjk_n) = label_ink(text, px, TextFace::Ui);
        let (semibold, semibold_n) = label_ink(text, px, TextFace::UiBold);
        let (latin, _) = label_ink(latin, px, TextFace::Ui);
        eprintln!(
            "{text}: regular {cjk:.3} ({cjk_n} px), semibold {semibold:.3} ({semibold_n} px), latin {latin:.3}"
        );
        assert!(
            (cjk - latin).abs() <= 0.05,
            "{text} in a quiet label inks {cjk:.3}, its Latin {latin:.3}"
        );
        assert!(
            semibold_n as f32 > cjk_n as f32 * 1.08,
            "{text} in the selected label is heavier: {semibold_n} lit px against {cjk_n}"
        );
        let ch = text.chars().next().unwrap();
        with_fonts(|fonts| {
            for (face, weight) in [
                (TextFace::Ui, UiCjkWeight::Regular),
                (TextFace::UiBold, UiCjkWeight::Semibold),
            ] {
                let drawn = Arc::clone(
                    &fonts
                        .ui_miss_face(face, TextWeight::Regular, ch)
                        .expect("a face")
                        .font,
                );
                assert!(
                    store
                        .rung(weight)
                        .iter()
                        .any(|entry| entry.parsed().is_some_and(|font| Arc::ptr_eq(font, &drawn))),
                    "{ch:?} in {face:?} draws from the {weight:?} rung"
                );
            }
        });
        for face in [TextFace::Ui, TextFace::UiBold] {
            let pen: f32 = with_fonts(|fonts| {
                text.chars()
                    .map(|ch| {
                        fonts
                            .ui_miss_face(face, TextWeight::Regular, ch)
                            .expect("a face")
                            .font
                            .char_metrics(ch, px)
                            .advance_width
                    })
                    .sum()
            });
            assert_eq!(ui_text_width_for(face, text, px), pen, "{text} in {face:?}");
        }
    }
    set_ui_cjk_faces_for_test(None);
    let (bold, _) = label_ink("日本語", px, TextFace::Ui);
    let (latin, _) = label_ink("notes", px, TextFace::Ui);
    eprintln!("control: chain 日本語 {bold:.3}, latin {latin:.3}");
    assert!(
        bold - latin > 0.05,
        "CONTROL: the chain's Bold inks {bold:.3} against the Latin's {latin:.3}"
    );
    clear_ui_fonts_for_test();
}

#[cfg(windows)]
#[test]
fn a_semibold_run_never_borrows_the_regular_east_asian_face() {
    let Some(store) = host_ui_cjk_faces_for_test() else {
        aterm_spec::host::could_not_run("no host UI CJK faces here");
    };
    prepare_ui_fonts_for_direct_view_test();
    set_ui_cjk_faces_for_test(Some(Arc::clone(&store)));
    store.materialise_for(UiCjkWeight::Regular, "日の");
    with_fonts(|fonts| {
        assert!(
            fonts
                .ui_miss_face(TextFace::Ui, TextWeight::Regular, '日')
                .is_some(),
            "CONTROL: the Regular run reaches the Regular face"
        );
        assert!(
            fonts
                .ui_miss_face(TextFace::UiBold, TextWeight::Regular, '日')
                .is_none(),
            "the Semibold run does not borrow it"
        );
        assert_eq!(fonts.label_misses(TextFace::UiBold, "日"), ['日']);
        assert!(fonts.label_misses(TextFace::Ui, "日").is_empty());
    });
    store.materialise_for(UiCjkWeight::Semibold, "の");
    let advance = |weight: UiCjkWeight| {
        store.rung(weight)[0]
            .parsed()
            .expect("Yu Gothic UI")
            .char_metrics('の', 1000.0)
            .advance_width
            .round()
    };
    assert_eq!(
        (
            advance(UiCjkWeight::Regular),
            advance(UiCjkWeight::Semibold)
        ),
        (816.0, 836.0)
    );
    clear_ui_fonts_for_test();
}

#[cfg(windows)]
#[test]
fn a_cjk_label_has_its_weights_own_ui_faces_parsed_when_the_band_asks() {
    let Some(_live) = install_unparsed_cjk_chain_for_test() else {
        aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
    };
    let Some(store) = host_ui_cjk_faces_for_test() else {
        clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run("no host UI CJK faces here");
    };
    set_ui_cjk_faces_for_test(Some(Arc::clone(&store)));
    if !strip_band_ui_ready() {
        clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let parsed = |weight: UiCjkWeight, script: UiCjkScript| {
        store
            .rung(weight)
            .iter()
            .filter(|face| face.script == script && face.parsed().is_some())
            .count()
    };
    assert_eq!(store.parsed_count(), 0, "CONTROL: a cold store");
    assert!(
        with_fonts(|fonts| fonts
            .ui_miss_face(TextFace::Ui, TextWeight::Regular, '日')
            .is_none()),
        "CONTROL: nothing reaches 日"
    );
    assert!(strip_band_run_coverable("alpha"));
    assert_eq!(store.parsed_count(), 0, "a Latin label parses nothing");
    let before = strip_band_font_epoch();
    assert!(strip_band_run_coverable_in(TextFace::Ui, "日本語 notes"));
    assert_eq!(parsed(UiCjkWeight::Regular, UiCjkScript::Han), 1);
    assert_eq!(
        store.parsed_count(),
        1,
        "one face: the one that maps 日本語"
    );
    let quiet = strip_band_font_epoch();
    assert_ne!(quiet, before, "the band key witnesses the landing");
    assert!(strip_band_run_coverable_in(TextFace::UiBold, "한국어"));
    assert_eq!(parsed(UiCjkWeight::Semibold, UiCjkScript::Hangul), 1);
    assert_eq!(parsed(UiCjkWeight::Regular, UiCjkScript::Hangul), 0);
    assert_eq!(store.parsed_count(), 2);
    assert_ne!(strip_band_font_epoch(), quiet);
    assert_eq!(ready_chain_faces(), 0, "the chain is not asked");
    clear_ui_fonts_for_test();
}

#[cfg(windows)]
#[test]
fn the_startup_warm_parses_the_ui_faces_east_asian_faces_at_both_weights() {
    let Some(store) = host_ui_cjk_faces_for_test() else {
        aterm_spec::host::could_not_run("no host UI CJK faces here");
    };
    assert_eq!(store.parsed_count(), 0, "CONTROL: a cold store");
    assert_eq!(warm_chrome_coverage(None, Some(&store)), 4);
    for weight in [UiCjkWeight::Regular, UiCjkWeight::Semibold] {
        let rung = store.rung(weight);
        let han: Vec<bool> = rung
            .iter()
            .filter(|face| face.script == UiCjkScript::Han)
            .map(|face| face.parsed().is_some())
            .collect();
        assert_eq!(
            han,
            [true, false],
            "{weight:?}: the Japanese face, not the Chinese"
        );
        assert!(
            rung.iter()
                .filter(|face| face.script == UiCjkScript::Hangul)
                .all(|face| face.parsed().is_some()),
            "{weight:?}: the Hangul face"
        );
    }
    assert_eq!(warm_chrome_coverage(None, Some(&store)), 0);
}

#[test]
fn a_label_asks_the_chain_for_text_only() {
    for ch in ['日', 'の', '한', 'क', 'ก', 'é', '\u{3000}', '∑'] {
        assert!(chain_may_draw_in_a_label(ch), "{ch:?} is text");
    }
    for ch in [
        '\u{1F680}',
        '\u{2600}',
        '\u{2764}',
        '\u{FE0F}',
        '\u{200D}',
        '\u{E0A0}',
        '\u{F0001}',
        '\u{10FFFD}',
        '\u{7}',
    ] {
        assert!(!chain_may_draw_in_a_label(ch), "{ch:?} is not asked");
    }
}

#[test]
fn set_chrome_fonts_bad_bytes_keep_fallback_rendering() {
    let garbage: std::sync::Arc<[u8]> = std::sync::Arc::from(&b"not a font"[..]);
    set_chrome_fonts(Some((garbage.clone(), 0)), Some((garbage, 3)), None);
    let w = measure_text("abc", 12.0, TextWeight::Regular);
    assert!(w > 0.0, "fallback still measures after a bad injection");
    let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(aterm_render::embedded_font());
    set_chrome_fonts(Some((bytes, 0)), None, None);
    let w2 = measure_text("abc", 12.0, TextWeight::Bold);
    assert!(
        (w2 - w).abs() < 1e-3,
        "bold with no bold face downgrades to primary"
    );
    set_chrome_fonts(None, None, None);
}

#[test]
fn the_chrome_keeps_the_persons_font_under_a_display_layer() {
    let own = aterm_render::embedded_font();
    let mut renderer = Renderer::from_bytes(own, 14.0, aterm_render::Theme::default()).unwrap();
    renderer.set_display_faces(&["engraved"]).unwrap();
    let primary = renderer.chrome_primary_face();
    assert_eq!(
        primary
            .as_ref()
            .map(|(bytes, index)| (bytes.as_ref(), *index)),
        Some((own, 0))
    );
    assert!(chrome_face_for_slot(&primary).is_some());
}

fn specimen_input(source: &str, rows: usize, cols: usize) -> aterm_render::RenderInput {
    let mut terminal = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
    terminal.process(source.as_bytes());
    let mut input = terminal.cell_frame(rows, cols);
    input.cursor_visible = false;
    input
}

fn specimen_spec(
    renderer: Renderer,
    identity: crate::widget::SemanticFontCandidate,
    input: aterm_render::RenderInput,
) -> crate::widget::TerminalSpecimenSpec {
    crate::widget::TerminalSpecimenSpec {
        input: Arc::new(input),
        input_fingerprint: 1,
        motion: None,
        prepared_font: PreparedSemanticFont {
            candidate: identity,
            snapshot: SemanticFontSnapshot {
                status: "test renderer snapshot".to_string(),
                ready_epoch: 1,
                pending: false,
            },
            renderer: Some(Rc::new(renderer)),
        },
        theme: aterm_render::Theme::default(),
        font_px: 14.0,
        line_height: 1.0,
        baseline_adjust: 0,
        ligatures: true,
        merged_ligatures: false,
        cursor_break_ligatures: true,
        synthetic_styles: true,
        underline_position: 0,
        underline_thickness: 0,
        underline_skip_descenders: true,
        text_blending: SpecimenTextBlending::default(),
        font_thicken: false,
        stem_gamma: 1.0,
        variations: Vec::new(),
        minimum_contrast: 1.0,
        selection_foreground: None,
        selection_inactive: false,
        wallpaper: None,
    }
}

#[test]
fn semantic_terminal_text_skips_notdef_without_collapsing_the_grid() {
    let raster = |unsupported: bool| -> Vec<u8> {
        let mut renderer = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        renderer.set_runtime_font_discovery(false);
        let mut input = specimen_input("A B", 1, 8);
        if unsupported {
            input.cells[0][1].ch = '\u{10FFFF}';
        }
        let spec = specimen_spec(
            renderer,
            crate::widget::SemanticFontCandidate::default(),
            input,
        );
        let mut canvas = Canvas::new(96, 28, [0, 0, 0, 255]);
        canvas.terminal_specimen(2.0, 4.0, &spec, 1.0);
        canvas.px
    };

    let (with_unsupported, with_space) = (raster(true), raster(false));
    let cell_w = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer")
    .cell_size()
    .0 as usize;
    let band = (2 + cell_w)..(2 + 2 * cell_w);
    for (index, (probe, space)) in with_unsupported.iter().zip(&with_space).enumerate() {
        let x = (index / 4) % 96;
        if !band.contains(&x) {
            assert_eq!(
                probe, space,
                "unsupported-scalar ink leaked outside its cell at x={x}"
            );
        }
    }
    assert_ne!(
        with_unsupported, with_space,
        "the probe scalar must reach the renderer's miss path"
    );
}

#[test]
fn semantic_renderer_install_is_lazy_until_preview_demand() {
    let mut renderer = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    renderer.set_runtime_font_discovery(false);
    set_chrome_fonts(None, None, Some(renderer));
    with_fonts(|fonts| {
        assert!(fonts.semantic.is_some(), "exact fork is retained dormant");
        assert!(
            fonts.semantic_worker.is_some(),
            "backend startup parks the worker before Settings can request a candidate"
        );
        assert!(fonts.semantic_pending.is_none());
        assert!(fonts.semantic_prewarm_ms.is_none());
    })
}

fn await_semantic_landing(fonts: &mut ChromeFonts) {
    const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
    let started = std::time::Instant::now();
    while fonts.semantic_pending.is_some() && started.elapsed() < HANG {
        fonts.poll_semantic_renderer();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    fonts.poll_semantic_renderer();
    assert!(
        fonts.semantic_pending.is_none(),
        "the semantic worker never landed its candidate ({:?})",
        started.elapsed()
    );
    eprintln!("semantic candidate landed after {:?}", started.elapsed());
}

#[test]
fn a_landed_semantic_font_wakes_the_host() {
    static WAKES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    set_semantic_ready_wake(|| {
        WAKES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
    let mut fonts = default_chrome_fonts();
    fonts.semantic_worker =
        SemanticPrewarmWorker::spawn_serialized_by(gate, crate::qos::Role::Responsive);
    fonts.semantic_generation = 1;
    let mut renderer = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    renderer.set_runtime_font_discovery(false);
    fonts.semantic = Some(renderer);
    fonts.semantic_identity = Some(crate::widget::SemanticFontCandidate::default());
    assert!(fonts.ensure_semantic_candidate(&crate::widget::SemanticFontCandidate::default()));
    await_semantic_landing(&mut fonts);
    let started = std::time::Instant::now();
    while WAKES.load(std::sync::atomic::Ordering::SeqCst) == 0
        && started.elapsed() < std::time::Duration::from_secs(60)
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(WAKES.load(std::sync::atomic::Ordering::SeqCst) > 0);
}

#[test]
fn a_resolving_candidate_still_draws_the_committed_specimen() {
    let mut fonts = default_chrome_fonts();
    let mut renderer = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    renderer.set_runtime_font_discovery(false);
    fonts.semantic_generation = 1;
    fonts.semantic = Some(renderer);
    fonts.semantic_identity = Some(crate::widget::SemanticFontCandidate::default());
    fonts.semantic_prewarm_ms = Some(0);
    let (_results_tx, results) = std::sync::mpsc::channel();
    fonts.semantic_worker = Some(SemanticPrewarmWorker {
        queue: Arc::new(SemanticPrewarmQueue::new()),
        results,
    });
    fonts.semantic_worker_generation = Some(1);
    let candidate = crate::widget::SemanticFontCandidate {
        regular: Some("Iosevka Term".to_string()),
        ..crate::widget::SemanticFontCandidate::default()
    };
    assert!(fonts.ensure_semantic_candidate(&candidate), "pending");
    assert!(
        fonts
            .semantic_renderer_fork_for(&candidate, 14.0, aterm_render::Theme::default())
            .is_some(),
        "the committed cascade draws while the candidate resolves"
    );
}

#[test]
fn a_resolving_candidate_still_draws_the_named_face_it_replaced() {
    let mut fonts = default_chrome_fonts();
    let mut renderer = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    renderer.set_runtime_font_discovery(false);
    let saved = crate::widget::SemanticFontCandidate {
        regular: Some("Menlo".to_string()),
        ..crate::widget::SemanticFontCandidate::default()
    };
    fonts.semantic_generation = 1;
    fonts.semantic = Some(renderer);
    fonts.semantic_identity = Some(saved);
    fonts.semantic_prewarm_ms = Some(0);
    let (_results_tx, results) = std::sync::mpsc::channel();
    fonts.semantic_worker = Some(SemanticPrewarmWorker {
        queue: Arc::new(SemanticPrewarmQueue::new()),
        results,
    });
    fonts.semantic_worker_generation = Some(1);
    let typed = crate::widget::SemanticFontCandidate {
        regular: Some("NoSuchFontXYZ".to_string()),
        ..crate::widget::SemanticFontCandidate::default()
    };
    assert!(fonts.ensure_semantic_candidate(&typed), "pending");
    assert!(
        fonts
            .semantic_renderer_fork_for(&typed, 14.0, aterm_render::Theme::default())
            .is_some(),
        "Menlo draws while the typed name resolves"
    );
}

#[test]
fn semantic_prewarm_coalesces_and_installs_only_latest_generation() {
    let renderer = || {
        let mut renderer = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        renderer.set_runtime_font_discovery(false);
        renderer
    };
    let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
    let serial = gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut fonts = default_chrome_fonts();
    fonts.semantic_worker =
        SemanticPrewarmWorker::spawn_serialized_by(gate, crate::qos::Role::Responsive);
    fonts.semantic_generation = 1;
    fonts.semantic = Some(renderer());
    fonts.semantic_identity = Some(crate::widget::SemanticFontCandidate::default());
    let candidate = crate::widget::SemanticFontCandidate::default();
    assert!(fonts.ensure_semantic_candidate(&candidate));
    let worker_ptr = fonts
        .semantic_worker
        .as_ref()
        .map(|worker| Arc::as_ptr(&worker.queue))
        .expect("the parked worker serves the request");

    fonts.semantic_generation = 2;
    fonts.semantic_pending = None;
    fonts.semantic_worker_generation = None;
    fonts.semantic = Some(renderer());
    fonts.semantic_identity = Some(candidate.clone());
    fonts.semantic_prewarm_ms = None;
    assert!(fonts.ensure_semantic_candidate(&candidate));
    assert_eq!(
        fonts
            .semantic_worker
            .as_ref()
            .map(|worker| Arc::as_ptr(&worker.queue)),
        Some(worker_ptr),
        "reload reuses the same worker"
    );
    drop(serial);

    await_semantic_landing(&mut fonts);
    assert_eq!(fonts.semantic_generation, 2);
    assert!(fonts.semantic_pending.is_none(), "land disarms");
    assert!(fonts.semantic.is_some(), "latest exact fork installs");
    assert!(fonts.semantic_prewarm_ms.is_some());
}

#[test]
fn chrome_font_reload_conforms_to_the_generation_queue_reset() {
    let model = aterm_models_misc::semantic_prewarm_generation_model();
    let mut fonts = default_chrome_fonts();
    fonts.semantic_generation = 1;
    let queue = Arc::new(SemanticPrewarmQueue::with_job(SemanticPrewarmJob {
        generation: 1,
        request: 1,
        candidate: crate::widget::SemanticFontCandidate::default(),
        base: None,
    }));
    let (_results_tx, results) = std::sync::mpsc::channel();
    fonts.semantic_worker = Some(SemanticPrewarmWorker {
        queue: Arc::clone(&queue),
        results,
    });
    let observe = |fonts: &ChromeFonts| {
        let mut state = model.init_state();
        state.insert(
            "current",
            i64::try_from(fonts.semantic_generation).expect("small generation"),
        );
        let queued = queue
            .semantic_job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .job
            .as_ref()
            .map_or(0, |job| job.generation);
        state.insert("queued", i64::try_from(queued).expect("small generation"));
        state
    };
    let before = observe(&fonts);
    let mut requested = model.init_state();
    assert!(model.fire("Request", &mut requested));
    assert_eq!(before, requested, "one job queued for the live generation");

    reload_chrome_fonts_locked(&mut fonts, None, None, None);
    let after = observe(&fonts);
    assert_eq!(
        model.successors("Reload", &before),
        vec![after.clone()],
        "the shipping reload is the model's Reload"
    );
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &after),
            "{}",
            invariant.name
        );
    }

    let mut kept = after;
    kept.insert("queued", before["queued"]);
    assert!(!model.successors("Reload", &before).contains(&kept));
    assert!(!model.check_invariant("QueueContainsOnlyCurrent", &kept));
}

#[test]
fn semantic_prewarm_poll_conforms_to_the_handshake_slot_effects() {
    let model = aterm_models_misc::semantic_prewarm_handshake_model();
    let renderer = || {
        let mut renderer = Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded renderer");
        renderer.set_runtime_font_discovery(false);
        renderer
    };
    let requested = crate::widget::SemanticFontCandidate {
        regular: Some("Requested Candidate 51A7".to_string()),
        ..crate::widget::SemanticFontCandidate::default()
    };
    let older = crate::widget::SemanticFontCandidate {
        regular: Some("Older Candidate 0B3E".to_string()),
        ..crate::widget::SemanticFontCandidate::default()
    };
    let mut rows = 0;
    let mut failed_closed_over_active = 0;
    for generation_matches in [false, true] {
        for request_matches in [false, true] {
            for candidate_matches in [false, true] {
                for renderer_ready in [false, true] {
                    for (active_before, active_before_latest) in
                        [(false, false), (true, false), (true, true)]
                    {
                        let mut fonts = default_chrome_fonts();
                        fonts.semantic_generation = 2;
                        fonts.semantic_request = 2;
                        fonts.semantic_requested = requested.clone();
                        fonts.semantic_prewarm_ms = Some(1);
                        if active_before {
                            fonts.semantic = Some(renderer());
                            fonts.semantic_identity = Some(if active_before_latest {
                                requested.clone()
                            } else {
                                older.clone()
                            });
                        }
                        let (results_tx, results) = std::sync::mpsc::channel();
                        fonts.semantic_worker = Some(SemanticPrewarmWorker {
                            queue: Arc::new(SemanticPrewarmQueue::new()),
                            results,
                        });
                        results_tx
                            .send(SemanticPrewarmResult {
                                generation: if generation_matches { 2 } else { 1 },
                                request: if request_matches { 2 } else { 1 },
                                candidate: if candidate_matches {
                                    requested.clone()
                                } else {
                                    older.clone()
                                },
                                renderer: renderer_ready.then(renderer),
                                resolution: SemanticFontResolution::Ready,
                                elapsed_ms: 0,
                            })
                            .expect("stub worker channel");
                        fonts.poll_semantic_renderer();

                        let mut before = model.init_state();
                        for (on, action) in [
                            (generation_matches, "MarkGenerationCurrent"),
                            (request_matches, "MarkRequestCurrent"),
                            (candidate_matches, "MarkCandidateCurrent"),
                            (renderer_ready, "MarkRendererReady"),
                            (active_before && !active_before_latest, "MarkActiveBefore"),
                            (active_before_latest, "MarkActiveBeforeLatest"),
                        ] {
                            if on {
                                assert!(model.fire(action, &mut before), "{action}");
                            }
                        }
                        let modeled = model.successors("DecideResult", &before)[0].clone();
                        let active_after = fonts.semantic.is_some();
                        let latest =
                            active_after && fonts.semantic_identity.as_ref() == Some(&requested);
                        assert_eq!(
                            (modeled["active_after"], modeled["active_after_latest"]),
                            (i64::from(active_after), i64::from(latest)),
                            "the poll's active slot diverged from DecideResult at \
                             generation={generation_matches} request={request_matches} \
                             candidate={candidate_matches} ready={renderer_ready} \
                             active={active_before} latest={active_before_latest}"
                        );
                        if modeled["decision"] == 3 && active_before {
                            failed_closed_over_active += 1;
                        }
                        rows += 1;
                    }
                }
            }
        }
    }
    assert_eq!(rows, 48);
    assert_eq!(
        failed_closed_over_active, 2,
        "the fail-closed arm is judged over a live renderer, not only an empty slot"
    );
}

#[test]
fn rapid_candidate_replacement_carries_base_and_cached_views_do_not_cross_contaminate() {
    let mut renderer = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    renderer.set_runtime_font_discovery(false);
    let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
    let mut fonts = default_chrome_fonts();
    fonts.semantic_worker =
        SemanticPrewarmWorker::spawn_serialized_by(gate, crate::qos::Role::Responsive);
    fonts.semantic_generation = 7;
    fonts.semantic = Some(renderer);
    fonts.semantic_identity = Some(crate::widget::SemanticFontCandidate::default());
    let first = crate::widget::SemanticFontCandidate {
        regular: Some("No Such Candidate A 7F31".to_string()),
        ..crate::widget::SemanticFontCandidate::default()
    };
    let second = crate::widget::SemanticFontCandidate {
        regular: Some("No Such Candidate B 9C42".to_string()),
        ..crate::widget::SemanticFontCandidate::default()
    };

    let serial = gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let initial_ready_epoch = fonts.semantic_ready_epoch;
    assert!(fonts.ensure_semantic_candidate(&first));
    assert!(fonts.ensure_semantic_candidate(&second));
    drop(serial);

    await_semantic_landing(&mut fonts);
    assert_eq!(fonts.semantic_identity.as_ref(), Some(&second));
    assert_ne!(
        fonts.semantic_ready_epoch, initial_ready_epoch,
        "worker landing invalidates retained preview fingerprints"
    );
    assert!(
        fonts.semantic.is_some(),
        "replacement retained the only base"
    );
    assert!(matches!(
        fonts.semantic_resolution,
        SemanticFontResolution::Fallback(_)
    ));

    fonts.semantic_cache.remove(&first);
    assert!(fonts.ensure_semantic_candidate(&first));
    assert!(
        fonts.semantic.is_none(),
        "B cannot paint A while A resolves"
    );
    await_semantic_landing(&mut fonts);
    assert_eq!(fonts.semantic_identity.as_ref(), Some(&first));
    assert!(
        !fonts.ensure_semantic_candidate(&second),
        "returning to B is a bounded renderer-cache swap"
    );
    assert_eq!(fonts.semantic_identity.as_ref(), Some(&second));
}

#[test]
fn exiting_a_test_thread_releases_its_font_store_and_prewarm_worker() {
    let child = std::thread::spawn(|| {
        let font = Arc::new(
            ChromeFont::from_bytes(
                aterm_render::embedded_font(),
                aterm_render::font::FontSettings::default(),
            )
            .expect("embedded font parses"),
        );
        let face = Arc::downgrade(&font);
        with_fonts(|fonts| {
            fonts.ui_regular = Some(font);
            fonts.semantic_worker = SemanticPrewarmWorker::spawn();
            let queue = Arc::downgrade(
                &fonts
                    .semantic_worker
                    .as_ref()
                    .expect("prewarm worker")
                    .queue,
            );
            (face, queue)
        })
    });
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !child.is_finished() {
        assert!(
            std::time::Instant::now() < until,
            "font-store thread did not exit"
        );
        aterm_uds::deadline::poll_yield();
    }
    let (face, queue) = child.join().expect("font-store thread");
    assert!(
        face.upgrade().is_none(),
        "thread-local font store retained its face"
    );
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while queue.strong_count() != 0 {
        assert!(
            std::time::Instant::now() < until,
            "thread-local prewarm worker did not exit"
        );
        aterm_uds::deadline::poll_yield();
    }
}

#[test]
fn dropping_the_semantic_prewarm_worker_stops_an_idle_thread_and_frees_its_retained_base() {
    let wait_exited = |weak: &std::sync::Weak<SemanticPrewarmQueue>, label: &str| {
        let started = std::time::Instant::now();
        while weak.strong_count() > 0 && started.elapsed() < std::time::Duration::from_secs(60) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            weak.strong_count(),
            0,
            "idle semantic prewarm thread survived after owner drop ({label})"
        );
    };

    let gate: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));

    // 1. Negative control: while `slot.closed` stays false (as before `Drop` closed and woke
    //    the queue), a parked worker thread continues holding its `Arc<SemanticPrewarmQueue>`.
    let mut unclosed = std::mem::ManuallyDrop::new(
        SemanticPrewarmWorker::spawn_serialized_by(gate, crate::qos::Role::Responsive)
            .expect("spawn semantic prewarm worker"),
    );
    let weak_unclosed = Arc::downgrade(&unclosed.queue);
    assert_eq!(
        weak_unclosed.strong_count(),
        2,
        "negative control: without Drop closing the slot, both owner and parked thread hold the queue"
    );
    unclosed.queue.ready.notify_all();
    aterm_uds::deadline::poll_yield();
    assert_eq!(
        weak_unclosed.strong_count(),
        2,
        "negative control: spurious wake with slot.closed == false leaves the worker parked"
    );
    // SAFETY: `unclosed` was initialized once above and is never accessed again after drop.
    unsafe { std::mem::ManuallyDrop::drop(&mut unclosed) };
    wait_exited(&weak_unclosed, "cold parked worker");

    // 2. Warm-parked worker holding a prepared `Renderer` base and face cache exits on drop.
    let mut renderer = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    renderer.set_runtime_font_discovery(false);
    let mut fonts = default_chrome_fonts();
    fonts.semantic_worker =
        SemanticPrewarmWorker::spawn_serialized_by(gate, crate::qos::Role::Responsive);
    let weak_warm = Arc::downgrade(
        &fonts
            .semantic_worker
            .as_ref()
            .expect("spawned warm worker")
            .queue,
    );
    fonts.semantic_generation = 1;
    fonts.semantic = Some(renderer);
    fonts.semantic_identity = Some(crate::widget::SemanticFontCandidate::default());
    let candidate = crate::widget::SemanticFontCandidate {
        regular: Some("No Such Candidate Warm 4D2A".to_string()),
        ..crate::widget::SemanticFontCandidate::default()
    };
    assert!(fonts.ensure_semantic_candidate(&candidate));
    await_semantic_landing(&mut fonts);
    fonts.semantic_worker = None;
    wait_exited(&weak_warm, "warm parked worker with retained base");

    // 3. Worker blocked on `gate` after popping a job exits without building once dropped.
    let serial = gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let worker = SemanticPrewarmWorker::spawn_serialized_by(gate, crate::qos::Role::Responsive)
        .expect("spawn gated worker");
    let weak_gated = Arc::downgrade(&worker.queue);
    worker.submit_latest(SemanticPrewarmJob {
        generation: 3,
        request: 1,
        candidate: crate::widget::SemanticFontCandidate::default(),
        base: None,
    });
    let started = std::time::Instant::now();
    while weak_gated
        .upgrade()
        .is_some_and(|q| q.semantic_job.lock().unwrap().job.is_some())
        && started.elapsed() < std::time::Duration::from_secs(60)
    {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    drop(worker);
    drop(serial);
    wait_exited(&weak_gated, "worker waiting on serialization gate");
}

#[test]
fn four_face_candidate_changes_real_pixels_fingerprint_and_ready_semantics() {
    let fixture = include_bytes!("../../aterm-render/tests/fixtures/jetbrains-mono.ttf");
    let family = "aterm deterministic JetBrains fixture";
    let mut faces = CandidateFaceCache::new();
    for slot in 0..4 {
        faces.insert(
            (
                family.to_ascii_lowercase(),
                aterm_render::FaceTarget::for_slot(slot),
            ),
            Ok(ResolvedCandidateFace {
                face: aterm_render::FaceRef::new("jetbrains-mono.ttf", 0),
                bytes: Arc::new(fixture.to_vec()),
            }),
        );
    }
    let candidate = crate::widget::SemanticFontCandidate {
        regular: Some(family.to_string()),
        bold: Some(family.to_string()),
        italic: Some(family.to_string()),
        bold_italic: Some(family.to_string()),
        fallback: Vec::new(),
        symbol: None,
        emoji: None,
        variations: Vec::new(),
        synthetic_styles: true,
        display: Vec::new(),
    };
    let mut base = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    base.set_runtime_font_discovery(false);
    base.prepare_semantic_typography("Regular Aa Bold Aa Italic Aa Bold Italic Aa != =>");
    let (candidate_renderer, resolution) = build_semantic_candidate(&base, &candidate, &mut faces);
    let candidate_renderer = candidate_renderer.expect("fixture candidate renderer");
    assert_eq!(
        candidate_renderer.debug_styled_face_indices(),
        [Some(0), Some(0), Some(0)],
        "all four authored family slots reach the renderer"
    );
    assert_eq!(resolution, SemanticFontResolution::Ready);
    assert!(
        resolution
            .description(&candidate)
            .contains("candidate renderer ready")
    );

    let raster = |renderer: Renderer, identity: crate::widget::SemanticFontCandidate| -> Vec<u8> {
        let input = specimen_input(
            concat!(
                "Regular Aa 012 != =>\r\n",
                "\u{1b}[1mBold Aa 012 != =>\u{1b}[0m\r\n",
                "\u{1b}[3mItalic Aa 012 != =>\u{1b}[0m\r\n",
                "\u{1b}[1;3mBold Italic Aa 012 != =>\u{1b}[0m",
            ),
            4,
            26,
        );
        let spec = specimen_spec(renderer, identity, input);
        let mut canvas = Canvas::new(420, 96, [18, 20, 26, 255]);
        canvas.terminal_specimen(4.0, 4.0, &spec, 1.0);
        canvas.px
    };
    let host = crate::widget::SemanticFontCandidate::default();
    let host_pixels = raster(base, host.clone());
    let candidate_pixels = raster(candidate_renderer, candidate.clone());
    assert!(
        host_pixels
            .iter()
            .zip(&candidate_pixels)
            .filter(|(left, right)| left != right)
            .count()
            > 500,
        "resolved candidate must alter genuine terminal glyph pixels"
    );
    assert_ne!(
        crate::settings_preview::SettingsPreviewSpec::typography(14.0).paint_fingerprint(),
        crate::settings_preview::SettingsPreviewSpec::typography(14.0)
            .with_font_candidate(candidate)
            .paint_fingerprint(),
        "candidate identity participates in retained paint"
    );
}

#[test]
fn a_font_candidate_keeps_the_compiled_in_symbol_face() {
    const ONLY_BUNDLED: char = '\u{F09B}';
    let fixture = include_bytes!("../../aterm-render/tests/fixtures/jetbrains-mono.ttf");
    let family = "aterm deterministic JetBrains fixture";
    let mut faces = CandidateFaceCache::new();
    faces.insert(
        (
            family.to_ascii_lowercase(),
            aterm_render::FaceTarget::REGULAR,
        ),
        Ok(ResolvedCandidateFace {
            face: aterm_render::FaceRef::new("jetbrains-mono.ttf", 0),
            bytes: Arc::new(fixture.to_vec()),
        }),
    );
    let candidate = crate::widget::SemanticFontCandidate {
        regular: Some(family.to_string()),
        ..crate::widget::SemanticFontCandidate::default()
    };
    let mut base = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    let _ = base.seal_admitted_font_sources();
    let (renderer, resolution) = build_semantic_candidate(&base, &candidate, &mut faces);
    assert_eq!(resolution, SemanticFontResolution::Ready);
    let mut renderer = renderer.expect("candidate renderer");
    assert!(renderer.admitted_font_sources_sealed());
    assert_ne!(
        renderer.glyph_key(ONLY_BUNDLED).source,
        aterm_render::FaceId::Primary,
        "the candidate drew the symbol as .notdef"
    );
    let (closed, _) = build_semantic_candidate(&base, &candidate, &mut faces);
    let mut closed = closed.expect("candidate renderer");
    closed.set_runtime_font_discovery(false);
    assert_eq!(
        closed.glyph_key(ONLY_BUNDLED).source,
        aterm_render::FaceId::Primary
    );
}

#[test]
fn invalid_candidate_falls_back_honestly_without_losing_the_renderer() {
    let mut base = Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("embedded renderer");
    base.set_runtime_font_discovery(false);
    let candidate = crate::widget::SemanticFontCandidate {
        regular: Some("No Such Font Family E18D 77A2".to_string()),
        ..crate::widget::SemanticFontCandidate::default()
    };
    let (renderer, resolution) =
        build_semantic_candidate(&base, &candidate, &mut CandidateFaceCache::new());
    assert!(
        renderer.is_some(),
        "invalid draft retains committed fallback renderer"
    );
    let SemanticFontResolution::Fallback(unresolved) = &resolution else {
        panic!("invalid family must expose fallback semantics: {resolution:?}")
    };
    assert!(unresolved.iter().any(|slot| slot.contains("regular")));
    let status = resolution.description(&candidate);
    assert!(status.contains("candidate fallback active"));
    assert!(status.contains("No Such Font Family"));
}

#[test]
fn ui_text_width_is_monotonic_and_falls_back_cleanly() {
    assert_eq!(ui_text_width("", 16.0), 0.0);
    let mut prev = 0.0;
    let mut s = String::new();
    for ch in "Appearance Wavy 17".chars() {
        s.push(ch);
        let w = ui_text_width(&s, 16.0);
        assert!(w > prev, "appending {ch:?} grew the measure: {w} vs {prev}");
        prev = w;
    }
    assert!(ui_text_width("Cursor", 20.0) > ui_text_width("Cursor", 10.0));

    let (_, narrow) = aterm_render::shared_parsed_face(
        include_bytes!("../../aterm-render/assets/bundled/NotoSansMath-Regular.ttf"),
        0,
    )
    .expect("the committed math face parses");
    let mut fonts = default_chrome_fonts();
    fonts.ui_regular = Some(narrow);
    let px = 10.0;
    let floor = 0.2 * px;
    let measure = |fonts: &mut ChromeFonts, query: &str| {
        ui_text_width_with_floor(fonts, TextFace::Ui, query, px, floor)
    };
    assert!(measure(&mut fonts, "Appearance Wavy 17").1);
    let thin = "\u{200a}".repeat(100);
    assert!(fonts.label_misses(TextFace::Ui, &thin).is_empty());
    let (width, bounded) = measure(&mut fonts, &thin);
    assert!(width > 0.0 && width < thin.chars().count() as f32 * floor);
    assert!(!bounded, "coverage alone cannot bound the scalar count");
    assert!(fonts.label_misses(TextFace::Ui, "\u{200b}").is_empty());
    assert!(
        !measure(&mut fonts, "\u{200b}").1,
        "zero advance refuses too"
    );
    fonts.ui_regular = None;
    assert!(measure(&mut fonts, "ordinary fallback").1);

    let (_, kerned) = aterm_render::shared_parsed_face(
        include_bytes!("../../aterm-render/assets/game/Chewy-Regular.ttf"),
        0,
    )
    .expect("the committed kerned face parses");
    let left = kerned.char_metrics('A', px).advance_width + UI_TRACKING_EM * px;
    let right = kerned.char_metrics('U', px).advance_width + UI_TRACKING_EM * px;
    let kern = kerned.char_kern('A', 'U', px).expect("the pair is kerned");
    let pair_floor = (left + (left + kern)) * 0.5;
    assert!(kern < 0.0 && left >= pair_floor && right + kern >= pair_floor);
    fonts.ui_regular = Some(kerned);
    assert!(ui_text_width_with_floor(&mut fonts, TextFace::Ui, "A", px, pair_floor).1);
    assert!(ui_text_width_with_floor(&mut fonts, TextFace::Ui, "U", px, pair_floor).1);
    assert!(
        !ui_text_width_with_floor(&mut fonts, TextFace::Ui, "AU", px, pair_floor).1,
        "removing the left scalar must not reverse the width ordering"
    );
}

#[test]
fn ui_text_wrap_is_lossless_width_bounded_and_linear_at_update_cap() {
    let source = "x".repeat(256 * 1024);
    let (ranges, scans) = ui_text_wrap_ranges_impl(&source, 13.0, 420.0);
    assert!(!ranges.is_empty());
    assert_eq!(ranges[0].start, 0);
    assert_eq!(ranges.last().map(|range| range.end), Some(source.len()));
    assert!(
        ranges.windows(2).all(|pair| pair[0].end == pair[1].start),
        "responsive wrapping must cover the authored source contiguously"
    );
    assert!(
        scans <= source.len() * 2,
        "near-cap unbroken input must remain linear: {scans} scans for {} graphemes",
        source.len()
    );
    assert!(
        ranges
            .iter()
            .all(|range| { ui_text_width(&source[range.clone()], 13.0) <= 420.5 })
    );
    assert_eq!(
        ranges
            .iter()
            .map(|range| &source[range.clone()])
            .collect::<String>(),
        source
    );
}

#[test]
fn a_lines_trailing_space_never_ends_it_a_word_early() {
    let px = 13.0;
    let text = "alpha beta gamma";
    let space = ui_text_width(" ", px);
    assert!(space > 1.0, "premise: a space has width");
    let width = ui_text_width("alpha beta", px) + space * 0.5;
    let lines: Vec<&str> = ui_text_wrap_ranges(text, px, width)
        .into_iter()
        .map(|range| &text[range])
        .collect();
    assert_eq!(lines.concat(), text, "the ranges cover the text");
    assert_eq!(lines[0].trim_end(), "alpha beta", "{lines:?}");
    assert_eq!(lines[1], "gamma", "{lines:?}");
    let short = ui_text_width("alpha bet", px);
    let lines: Vec<&str> = ui_text_wrap_ranges(text, px, short)
        .into_iter()
        .map(|range| &text[range])
        .collect();
    assert_eq!(lines[0].trim_end(), "alpha", "{lines:?}");
    let ended = "alpha beta ";
    assert_eq!(
        ui_text_wrap_ranges(ended, px, width),
        vec![0..ended.len()],
        "one line, its trailing space with it"
    );
}

#[test]
fn ui_text_wrap_preserves_repeated_and_unicode_whitespace_exactly() {
    let source = "\t  alpha\u{2003}\u{2003}beta  終\t tail";
    let ranges = ui_text_wrap_ranges(source, 13.0, 54.0);
    assert!(ranges.len() > 1, "fixture must exercise a real wrap");
    assert_eq!(
        ranges
            .iter()
            .map(|range| &source[range.clone()])
            .collect::<String>(),
        source
    );
}

#[test]
fn ui_text_wrap_keeps_a_no_break_space_as_glue() {
    for glue in ['\u{a0}', '\u{202f}', '\u{2007}'] {
        let source = format!("aaa 50{glue}% bbb");
        let maximum_width = ui_text_width(&format!("aaa 50{glue}"), 13.0) + 0.1;
        assert!(ui_text_width(&format!("aaa 50{glue}%"), 13.0) > maximum_width);
        let lines: Vec<&str> = ui_text_wrap_ranges(&source, 13.0, maximum_width)
            .into_iter()
            .map(|range| &source[range])
            .collect();
        assert_eq!(lines.concat(), source);
        assert!(
            lines.iter().all(|line| !line.starts_with('%')),
            "{glue:?}: {lines:?}"
        );
        assert_eq!(lines.first().copied(), Some("aaa "), "{glue:?}: {lines:?}");
    }
}

#[test]
fn ui_text_wrap_allows_only_indivisible_oversized_graphemes_to_overflow() {
    let oversized = "👨‍👩‍👧‍👦";
    let source = format!("{oversized}xx");
    assert_eq!(source.graphemes().count(), 3);
    let maximum_width = ui_text_width("x", 13.0) + 0.5;
    assert!(ui_text_width(oversized, 13.0) > maximum_width);

    let ranges = ui_text_wrap_ranges(&source, 13.0, maximum_width);
    assert_eq!(ranges.first().map(|range| range.start), Some(0));
    assert_eq!(ranges.last().map(|range| range.end), Some(source.len()));
    assert!(ranges.windows(2).all(|pair| pair[0].end == pair[1].start));
    assert_eq!(
        ranges
            .iter()
            .map(|range| &source[range.clone()])
            .collect::<String>(),
        source
    );
    let overflowing = ranges
        .iter()
        .filter(|range| ui_text_width(&source[range.start..range.end], 13.0) > maximum_width + 0.5)
        .collect::<Vec<_>>();
    assert_eq!(overflowing.len(), 1);
    let overflow = overflowing[0];
    assert_eq!(&source[overflow.start..overflow.end], oversized);
    assert_eq!(source[overflow.start..overflow.end].graphemes().count(), 1);
}

#[test]
fn ui_faces_rasterize_and_bold_is_no_lighter() {
    set_chrome_fonts(None, None, None);
    let text = |face: TextFace| {
        let prims = vec![crate::widget::text_prim(
            2.0,
            row_baseline(0.0, 28.0, 16.0),
            "Appearance".to_string(),
            crate::type_scale::TypeStep::Body.px(16.0),
            TextWeight::Regular,
            face,
            [255, 255, 255, 255],
        )];
        let (px, _, _) = rasterize_tray(&prims, 120, 28, 1.0, [0, 0, 0, 255]);
        px.as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from(c[0]))
            .sum::<u32>()
    };
    let ui = text(TextFace::Ui);
    let bold = text(TextFace::UiBold);
    assert!(ui > 0, "Ui text rendered");
    assert!(
        bold > ui,
        "UiBold strikes more coverage than Ui: {bold} vs {ui}"
    );
}

#[test]
fn a_terminal_frame_blits_as_an_opaque_blend_would() {
    let frame = aterm_render::Frame {
        width: 9,
        height: 7,
        pixels: (0..63_u32)
            .map(|i| 0x0010_2030 ^ (i * 0x0003_0507))
            .collect(),
    };
    let backdrop = [40, 50, 60, 255];
    let reference = |clip: Option<(f32, f32, f32, f32)>, at: (i32, i32)| {
        let mut canvas = Canvas::with_origin(4, 3, 12, 10, backdrop);
        if let Some((x, y, w, h)) = clip {
            canvas.push_clip(x, y, w, h);
        }
        for (row, pixels) in frame.pixels.chunks(frame.width).enumerate() {
            for (column, pixel) in pixels.iter().enumerate() {
                canvas.blend(
                    at.0 + column as i32,
                    at.1 + row as i32,
                    [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, 255],
                    1.0,
                );
            }
        }
        canvas.px
    };
    let blitted = |clip: Option<(f32, f32, f32, f32)>, at: (i32, i32)| {
        let mut canvas = Canvas::with_origin(4, 3, 12, 10, backdrop);
        if let Some((x, y, w, h)) = clip {
            canvas.push_clip(x, y, w, h);
        }
        canvas.blit_terminal_frame(&frame, at.0, at.1);
        canvas.px
    };
    for clip in [
        None,
        Some((6.0, 5.0, 5.0, 4.0)),
        Some((30.0, 30.0, 2.0, 2.0)),
    ] {
        for at in [(4, 3), (0, 0), (10, 8), (13, 9), (-20, -20)] {
            assert_eq!(blitted(clip, at), reference(clip, at), "{clip:?} at {at:?}");
        }
    }
    let untouched = Canvas::with_origin(4, 3, 12, 10, backdrop).px;
    assert_ne!(blitted(None, (4, 3)), untouched);
}

#[test]
fn a_canvas_starts_as_its_backdrop_in_every_pixel() {
    for (w, h) in [(0, 0), (1, 1), (3, 5), (40, 17), (1_280, 40)] {
        for backdrop in [[0, 0, 0, 0], [40, 50, 60, 255], [1, 2, 3, 4]] {
            let canvas = Canvas::with_origin(4, 3, w, h, backdrop);
            let (pixels, rest) = canvas.px.as_chunks::<4>();
            assert_eq!((pixels.len(), rest.len()), ((w * h) as usize, 0), "{w}x{h}");
            assert!(
                pixels.iter().all(|pixel| *pixel == backdrop),
                "{w}x{h} {backdrop:?}"
            );
        }
    }
}

#[test]
fn blend_composites_in_linear_light() {
    let mut cv = Canvas::new(1, 1, [0, 0, 0, 255]);
    cv.blend(0, 0, [255, 255, 255, 255], 0.5);
    assert!(
        (185..=190).contains(&cv.px[0]),
        "linear-light midpoint expected ~188, got {}",
        cv.px[0]
    );
}

#[test]
fn glyph_texels_blend_like_terminal_text() {
    let ink = [0x1f, 0x23, 0x28, 0xff];
    let ground = [0xf1, 0xf1, 0xf1, 0xff];
    let pack = |p: [u8; 4]| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]);
    for coverage in [1_u8, 64, 128, 200, 254] {
        let mut cv = Canvas::new(1, 1, ground);
        cv.blend_glyph_texel(0, 0, ink, coverage);
        let terminal =
            aterm_render::blend_text(pack(ground), pack(ink), pack(ground), coverage, true);
        let want = [
            (terminal >> 16) as u8,
            (terminal >> 8) as u8,
            terminal as u8,
        ];
        for (k, (&got, &want)) in cv.px.iter().zip(&want).enumerate() {
            assert!(
                got.abs_diff(want) <= 1,
                "coverage {coverage} channel {k}: chrome {got} vs terminal {want}"
            );
        }
    }
    let mut plain = Canvas::new(1, 1, ground);
    plain.blend(0, 0, ink, 128.0 / 255.0);
    let mut glyph = Canvas::new(1, 1, ground);
    glyph.blend_glyph_texel(0, 0, ink, 128);
    assert!(
        plain.px[1] > glyph.px[1] + 30,
        "linear {} vs corrected {}",
        plain.px[1],
        glyph.px[1]
    );
    let mut full = Canvas::new(1, 1, ground);
    full.blend_glyph_texel(0, 0, ink, 255);
    assert_eq!(full.px, ink);
    let mut none = Canvas::new(1, 1, ground);
    none.blend_glyph_texel(0, 0, ink, 0);
    assert_eq!(none.px, ground);
    let mut bare = Canvas::new(1, 1, [0, 0, 0, 0]);
    let mut bare_plain = Canvas::new(1, 1, [0, 0, 0, 0]);
    bare.blend_glyph_texel(0, 0, ink, 128);
    bare_plain.blend(0, 0, ink, 128.0 / 255.0);
    assert_eq!(bare.px, bare_plain.px);
}

#[test]
fn mono_text_is_as_heavy_dark_on_light_as_light_on_dark() {
    set_chrome_fonts(None, None, None);
    let run = |ink: [u8; 4], ground: [u8; 4]| {
        let prims = vec![crate::widget::text_prim(
            2.0,
            row_baseline(0.0, 20.0, 13.0),
            "Linen Dracula".to_string(),
            crate::type_scale::TypeStep::Body.px(13.0),
            TextWeight::Regular,
            TextFace::Mono,
            ink,
        )];
        rasterize_tray(&prims, 140, 20, 1.0, ground).0
    };
    let dark_on_light = run([0, 0, 0, 255], [255, 255, 255, 255]);
    let light_on_dark = run([255, 255, 255, 255], [0, 0, 0, 255]);
    let inked = light_on_dark.iter().step_by(4).filter(|&&v| v > 0).count();
    assert!(inked > 50, "the run inks: {inked} texels");
    let worst = dark_on_light
        .iter()
        .step_by(4)
        .zip(light_on_dark.iter().step_by(4))
        .map(|(&d, &l)| (255 - d).abs_diff(l))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= 2,
        "dark-on-light departs from the mirror by {worst}"
    );
}

#[test]
fn the_ui_pen_places_glyphs_at_quarter_pixels() {
    assert_eq!(pen_phase(10.0), (10, 0));
    assert_eq!(pen_phase(10.2), (10, 1));
    assert_eq!(pen_phase(10.5), (10, 2));
    assert_eq!(pen_phase(10.74), (10, 3));
    assert_eq!(pen_phase(10.9), (11, 0));
    assert_eq!(pen_phase(-0.3), (-1, 3));
    let font = ChromeFont::from_bytes(
        aterm_render::embedded_font(),
        aterm_render::font::FontSettings::default(),
    )
    .expect("embedded font parses");
    let edge = |dx: f32| {
        let (m, cov) = font.char_raster_at('l', 13.0, dx);
        let row = &cov[(m.height / 2) * m.width..][..m.width];
        let first = row.iter().position(|&c| c > 0).expect("the stem is inked");
        f64::from(m.xmin) + first as f64 + 1.0 - f64::from(row[first]) / 255.0
    };
    let at0 = edge(0.0);
    for phase in 1..4u8 {
        let dx = f32::from(phase) / PEN_PHASES;
        let moved = edge(dx) - at0;
        assert!(
            (moved - f64::from(dx)).abs() < 0.01,
            "phase {phase}: the stem moved {moved}, not {dx}"
        );
    }
    let (m0, c0) = font.char_raster('l', 13.0);
    let (m1, c1) = font.char_raster_at('l', 13.0, 1.0);
    assert_eq!((m1.xmin, m1.width, &c1), (m0.xmin + 1, m0.width, &c0));
    assert_eq!(
        font.char_raster_at('l', 13.0, 0.0),
        font.char_raster('l', 13.0)
    );
}

#[test]
fn glyph_rasterization_is_memoized_for_one_retained_surface() {
    let font = Arc::new(
        ChromeFont::from_bytes(
            aterm_render::embedded_font(),
            aterm_render::font::FontSettings::default(),
        )
        .expect("embedded font parses"),
    );
    let mut canvas = Canvas::new(1, 1, [0, 0, 0, 0]);

    let (first_metrics, first) = canvas.raster_glyph(&font, 'A', 13.0);
    let (second_metrics, second) = canvas.raster_glyph(&font, 'A', 13.0);

    assert_eq!(first_metrics, second_metrics);
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    assert_eq!(canvas.glyphs.len(), 1);
}

/// Text in each face the pen draws (the UI face and its semibold, the mono face and its bold,
/// a char only the pen designs), half of it over a panel of the other ground, in `ink` over
/// `ground`: each face draws the same words at one size from one pen, so a key that missed
/// the face would draw one face's glyph for another's, and the same words again at sizes and
/// quarter-pixel pens of their own.
fn memo_prims(ink: [u8; 4], ground: [u8; 4], other: [u8; 4]) -> Vec<DrawPrim> {
    let text = |x: f32, baseline: f32, s: &str, px: f32, weight, face| DrawPrim::Text {
        x,
        baseline,
        s: s.to_string(),
        px,
        color: ink,
        weight,
        face,
    };
    let mut prims = vec![
        DrawPrim::Panel {
            x: 0.0,
            y: 0.0,
            w: 240.0,
            h: 72.0,
            radius: 0.0,
            fill: ground,
        },
        DrawPrim::Panel {
            x: 120.0,
            y: 0.0,
            w: 120.0,
            h: 72.0,
            radius: 6.0,
            fill: other,
        },
    ];
    let faces = [
        (TextWeight::Regular, TextFace::Ui),
        (TextWeight::Bold, TextFace::UiBold),
        (TextWeight::Regular, TextFace::Mono),
        (TextWeight::Bold, TextFace::Mono),
    ];
    for (row, (weight, face)) in faces.into_iter().enumerate() {
        let (baseline, step) = (14.0 + 16.0 * row as f32, row as f32);
        prims.push(text(
            4.25,
            baseline,
            "load 11.3 \u{00b7} \u{2235} mds",
            11.0,
            weight,
            face,
        ));
        prims.push(text(
            130.75 - 0.25 * step,
            baseline,
            "Why now \u{2139} 0.42 \u{23f8}",
            12.0 + step,
            weight,
            face,
        ));
    }
    prims
}

const DARK_INK: [u8; 4] = [0xE6, 0xE8, 0xEB, 0xFF];
const LIGHT_INK: [u8; 4] = [0x24, 0x29, 0x2F, 0xFF];
const DARK_GROUND: [u8; 4] = [0x1E, 0x1E, 0x1E, 0xFF];
const LIGHT_GROUND: [u8; 4] = [0xF5, 0xF5, 0xF5, 0xFF];

fn memo_raster(ink: [u8; 4], ground: [u8; 4], other: [u8; 4], scale: f32) -> Vec<u8> {
    rasterize_tray(
        &memo_prims(ink, ground, other),
        240,
        72,
        scale,
        [0, 0, 0, 0],
    )
    .0
}

/// What [`memo_raster`] draws with no glyph kept from another prim: each prim drawn over the
/// bytes the ones before it drew ([`rasterize_tray_over`]), from no kept glyph.
fn fresh_raster(ink: [u8; 4], ground: [u8; 4], other: [u8; 4], scale: f32) -> Vec<u8> {
    let (w, h) = ((240.0 * scale) as u32, (72.0 * scale) as u32);
    let mut bytes = vec![0; (w * h * 4) as usize];
    for prim in memo_prims(ink, ground, other) {
        forget_kept_glyphs();
        bytes = rasterize_tray_over(&[prim], scale, bytes, w, h);
    }
    forget_kept_glyphs();
    bytes
}

#[test]
fn a_raster_over_kept_glyphs_is_the_raster_a_fresh_canvas_draws() {
    prepare_ui_fonts_for_direct_view_test();
    let fresh_dark = fresh_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0);
    let fresh_light = fresh_raster(LIGHT_INK, LIGHT_GROUND, DARK_GROUND, 2.0);
    let fresh_small = fresh_raster(LIGHT_INK, LIGHT_GROUND, DARK_GROUND, 1.0);
    assert!(fresh_dark != fresh_light, "the two inks draw apart");

    assert!(
        memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0) == fresh_dark,
        "the glyphs a raster keeps for itself draw as fresh ones"
    );
    let kept = kept_glyphs();
    assert!(kept > 40, "the first raster keeps its glyphs ({kept})");
    let rasters = glyph_rasters();
    assert!(
        memo_raster(LIGHT_INK, LIGHT_GROUND, DARK_GROUND, 2.0) == fresh_light,
        "the other ink over the other grounds, every glyph a kept one"
    );
    assert_eq!(glyph_rasters(), rasters, "no glyph was rasterized again");
    assert_eq!(kept_glyphs(), kept);
    assert!(
        memo_raster(LIGHT_INK, LIGHT_GROUND, DARK_GROUND, 1.0) == fresh_small,
        "the scale-2 glyphs kept, at scale 1"
    );
    assert!(glyph_rasters() > rasters, "scale 1 rasterizes its own");
    assert!(
        memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0) == fresh_dark,
        "both scales kept"
    );

    // Negative control: a kept glyph is what a raster draws, so a spoiled one shows.
    spoil_kept_glyphs_for_test();
    assert!(
        memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0) != fresh_dark,
        "a spoiled kept glyph changes the bytes"
    );
    forget_kept_glyphs();
    assert!(memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0) == fresh_dark);
}

#[test]
fn a_font_install_drops_the_kept_glyphs() {
    prepare_ui_fonts_for_direct_view_test();
    let fresh = fresh_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0);
    memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0);
    let kept = kept_glyphs();
    assert!(kept > 0);
    // Negative control: another raster, and another ink, keep them.
    memo_raster(LIGHT_INK, LIGHT_GROUND, DARK_GROUND, 2.0);
    assert_eq!(kept_glyphs(), kept, "a raster keeps what the last kept");

    set_chrome_fonts(None, None, None);
    assert_eq!(kept_glyphs(), 0, "a font install drops them");
    assert!(memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0) == fresh);
}

#[test]
fn the_kept_glyphs_hold_their_faces_and_keep_within_their_bounds() {
    prepare_ui_fonts_for_direct_view_test();
    forget_kept_glyphs();
    memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0);
    let held: Vec<usize> = with_fonts(|fonts| {
        fonts
            .glyphs
            .faces
            .iter()
            .map(|face| Arc::as_ptr(face) as usize)
            .collect()
    });
    let named: std::collections::BTreeSet<usize> = with_fonts(|fonts| {
        fonts
            .glyphs
            .glyphs
            .keys()
            .map(|&(address, ..)| address)
            .filter(|&address| address != 0)
            .collect()
    });
    assert!(named.len() >= 3, "the UI, its semibold and a mono face");
    assert!(
        named.iter().all(|address| held.contains(address)),
        "each face a key names is held"
    );

    // One raster that alone draws past the bounds leaves nothing kept.
    let letters: String = ('a'..='z').collect();
    let sizes = |n: u16| -> Vec<DrawPrim> {
        (0..n)
            .map(|step| DrawPrim::Text {
                x: 0.25 * f32::from(step % 4),
                baseline: 30.0,
                s: letters.clone(),
                px: 8.0 + f32::from(step) / 8.0,
                color: DARK_INK,
                weight: TextWeight::Regular,
                face: TextFace::Ui,
            })
            .collect()
    };
    rasterize_tray(&sizes(200), 600, 40, 1.0, [0, 0, 0, 255]);
    assert_eq!(kept_glyphs(), 0, "5200 glyphs in one raster keep none");

    // Past the bounds, only the last raster's glyphs are kept, and only their faces.
    rasterize_tray(&sizes(157), 600, 40, 1.0, [0, 0, 0, 255]);
    let alone = kept_glyphs();
    assert!(
        alone <= GLYPH_MEMO_ENTRIES && with_fonts(|fonts| fonts.glyphs.bytes) <= GLYPH_MEMO_BYTES
    );
    forget_kept_glyphs();
    memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0);
    let before = kept_glyphs();
    assert!(before + alone > GLYPH_MEMO_ENTRIES, "{before} + {alone}");
    rasterize_tray(&sizes(157), 600, 40, 1.0, [0, 0, 0, 255]);
    assert_eq!(kept_glyphs(), alone, "the last raster's glyphs alone");
    assert_eq!(
        with_fonts(|fonts| fonts.glyphs.faces.len()),
        1,
        "the UI face alone"
    );
    // Negative control: within the bounds a raster keeps what the one before it kept.
    forget_kept_glyphs();
    memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0);
    rasterize_tray(&sizes(10), 600, 40, 1.0, [0, 0, 0, 255]);
    assert_eq!(kept_glyphs(), before + 260);
}

/// The kept glyphs' bitmap bytes by their size in device px and their char, each kept once;
/// the memo's byte count is their sum.
fn kept_bytes_by_glyph() -> HashMap<(u32, char), usize> {
    with_fonts(|fonts| {
        let mut by_glyph = HashMap::new();
        for (&(_, px, ch, _), kept) in fonts.glyphs.glyphs.iter() {
            let once = by_glyph.insert((px, ch), kept.bitmap.len()).is_none();
            assert!(once, "{ch:?} at {} px kept once", f32::from_bits(px));
        }
        assert_eq!(
            fonts.glyphs.bytes,
            by_glyph.values().sum::<usize>(),
            "the memo counts its kept bitmaps' bytes"
        );
        by_glyph
    })
}

#[test]
fn the_kept_glyphs_keep_within_their_byte_bound() {
    prepare_ui_fonts_for_direct_view_test();
    // Display text at 2×: two lines of 52 letters, each line's bitmaps under the byte bound and
    // the two past it, 104 glyphs against the bound's 4096.
    let letters: Vec<char> = ('A'..='Z').chain('a'..='z').collect();
    let line = |px: f32, n: usize| DrawPrim::Text {
        x: 0.0,
        baseline: px,
        s: letters[..n].iter().collect(),
        px,
        color: DARK_INK,
        weight: TextWeight::Regular,
        face: TextFace::Ui,
    };
    let raster = |prims: &[DrawPrim]| {
        rasterize_tray(prims, 64, 64, 2.0, [0, 0, 0, 255]);
    };
    let (first, second) = (200.0_f32, 210.0_f32);
    let second_px = (second * 2.0).to_bits();
    forget_kept_glyphs();
    raster(&[line(first, 52)]);
    let first_bytes: usize = kept_bytes_by_glyph().values().sum();
    assert_eq!(kept_glyphs(), 52);
    forget_kept_glyphs();
    raster(&[line(second, 52)]);
    let second_kept = kept_bytes_by_glyph();
    let second_bytes: usize = second_kept.values().sum();
    assert_eq!(second_kept.len(), 52);
    forget_kept_glyphs();
    assert!(
        first_bytes < GLYPH_MEMO_BYTES
            && second_bytes < GLYPH_MEMO_BYTES
            && first_bytes + second_bytes > GLYPH_MEMO_BYTES,
        "{first_bytes} + {second_bytes}"
    );

    // One raster that alone draws past the byte bound leaves nothing kept.
    raster(&[line(first, 52), line(second, 52)]);
    assert_eq!(
        kept_glyphs(),
        0,
        "104 glyphs past the byte bound in one raster keep none"
    );
    assert!(kept_bytes_by_glyph().is_empty() && with_fonts(|fonts| fonts.glyphs.faces.is_empty()));

    // Negative control: just under the bound a raster keeps what the one before it kept. The
    // second line's longest run of letters that fits beside the first line, then one more.
    let glyph_bytes = |at: usize| second_kept[&(second_px, letters[at])];
    let (mut fit, mut under) = (0, first_bytes);
    while under + glyph_bytes(fit) <= GLYPH_MEMO_BYTES {
        under += glyph_bytes(fit);
        fit += 1;
    }
    assert!(fit > 0, "{first_bytes} leaves room for a glyph");
    raster(&[line(first, 52)]);
    raster(&[line(second, fit)]);
    assert_eq!(
        kept_glyphs(),
        52 + fit,
        "{under} bytes keep both rasters' glyphs"
    );
    assert_eq!(kept_bytes_by_glyph().values().sum::<usize>(), under);

    // One glyph past the bound, only the last raster's glyphs are kept, and only their face.
    raster(&[line(second, fit + 1)]);
    let past = kept_bytes_by_glyph();
    assert_eq!(past.len(), fit + 1, "the last raster's glyphs alone");
    assert!(
        (0..=fit).all(|at| past.get(&(second_px, letters[at])) == Some(&glyph_bytes(at))),
        "every one of them, none of the first raster's"
    );
    assert_eq!(
        with_fonts(|fonts| fonts.glyphs.faces.len()),
        1,
        "the UI face alone"
    );
    forget_kept_glyphs();
}

#[test]
fn a_scale_change_drops_the_kept_glyphs() {
    let mut app = crate::App::headless_for_test();
    let window = crate::WindowId(0);
    if let Some(ws) = app.windows.get_mut(&window) {
        ws.scale = 2.0;
    }
    memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0);
    let kept = kept_glyphs();
    assert!(kept > 0);
    // Negative control: the scale it had is no change.
    app.on_scale_factor_changed(window, 2.0);
    assert_eq!(kept_glyphs(), kept, "the same scale keeps them");

    app.on_scale_factor_changed(window, 1.0);
    assert_eq!(kept_glyphs(), 0, "a new scale drops them");
}

#[test]
fn a_theme_change_drops_the_kept_glyphs() {
    use crate::app_colorscheme::ThemeChange;
    let mut app = crate::App::headless_for_test();
    app.install_resolved_colors(ThemeChange::ConfigReload);
    memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0);
    let kept = kept_glyphs();
    assert!(kept > 0);
    // Negative control: installing the theme in force again is no change.
    app.install_resolved_colors(ThemeChange::ConfigReload);
    assert_eq!(kept_glyphs(), kept, "the theme in force keeps them");

    app.publish_theme_chrome();
    assert_eq!(kept_glyphs(), 0, "the chrome's new theme drops them");
    memo_raster(DARK_INK, DARK_GROUND, LIGHT_GROUND, 2.0);
    assert!(kept_glyphs() > 0);
    app.config = aterm_toml::from_str("theme = \"Nord\"\n").expect("valid toml");
    assert!(
        app.install_resolved_colors(ThemeChange::ConfigReload)
            .theme_changed
    );
    assert_eq!(kept_glyphs(), 0, "a new theme drops them");
}

#[test]
fn threshold_oetf_matches_direct_rounded_srgb_quantization() {
    let direct = |linear: f32| {
        let linear = linear.clamp(0.0, 1.0);
        let encoded = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (encoded * 255.0).round().clamp(0.0, 255.0) as u8
    };

    for sample in 0..=65_536 {
        let linear = sample as f32 / 65_536.0;
        assert_eq!(linear_to_srgb_u8(linear), direct(linear));
    }
    for output in 0..255 {
        let encoded = (output as f32 + 0.5) / 255.0;
        let threshold = if encoded <= 0.040_45 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        };
        for bits in threshold.to_bits().saturating_sub(2)..=threshold.to_bits().saturating_add(2) {
            let linear = f32::from_bits(bits);
            assert_eq!(
                linear_to_srgb_u8(linear),
                direct(linear),
                "output boundary {output}, linear={linear:?}"
            );
        }
    }
}

#[test]
fn scanline_round_rect_is_byte_exact_to_the_analytic_reference() {
    let cases = [
        (2.0, 3.0, 31.0, 22.0, 8.0),
        (1.25, 2.75, 34.5, 25.25, 11.5),
        (-4.0, -3.0, 21.0, 16.0, 5.0),
        (7.0, 5.0, 2.0, 17.0, 9.0),
        (4.0, 8.0, 29.0, 7.0, 0.0),
    ];
    let paints = [
        ([0, 0, 0, 0], [18, 28, 42, 255]),
        ([0, 0, 0, 0], [0, 0, 0, 48]),
        ([31, 47, 63, 255], [92, 176, 255, 73]),
    ];

    for &(backdrop, color) in &paints {
        for &(x, y, width, height, radius) in &cases {
            let mut optimized = Canvas::new(40, 32, backdrop);
            let mut reference = Canvas::new(40, 32, backdrop);
            optimized.push_clip(1.5, 1.25, 35.0, 27.5);
            reference.push_clip(1.5, 1.25, 35.0, 27.5);

            optimized.round_rect(x, y, width, height, radius, color);

            let (cx, cy, hx, hy) = (x + width / 2.0, y + height / 2.0, width / 2.0, height / 2.0);
            let radius = radius.min(hx).min(hy);
            let x0 = (x - 1.0).floor() as i32;
            let y0 = (y - 1.0).floor() as i32;
            let x1 = (x + width + 1.0).ceil() as i32;
            let y1 = (y + height + 1.0).ceil() as i32;
            for py in y0..y1 {
                for px in x0..x1 {
                    let distance =
                        sd_round_box(px as f32 + 0.5, py as f32 + 0.5, cx, cy, hx, hy, radius);
                    reference.blend(px, py, color, cov(distance));
                }
            }

            assert_eq!(
                optimized.px, reference.px,
                "geometry=({x},{y},{width},{height},{radius}) color={color:?}"
            );
        }
    }
}

/// A chip's fill and 1pt rim at fractional x, at 1×, 1.5× and 2×, paint each side's rim
/// in whole pixels of the rim's own colour with the ground beyond it.
#[test]
fn a_rounded_rim_at_a_fractional_x_is_whole_pixels() {
    let ground = [226, 226, 226, 255];
    let fill = [240, 240, 240, 255];
    let rim = [128, 129, 131, 255];
    for scale in [1.0_f32, 1.5, 2.0] {
        for offset in [0.0_f32, 0.25, 0.4, 0.5, 0.6, 0.9] {
            let (x, y, w, h) = (10.0 + offset, 6.0 + offset, 41.3, 30.0);
            let prims = vec![
                DrawPrim::Panel {
                    x,
                    y,
                    w,
                    h,
                    radius: 7.0,
                    fill,
                },
                DrawPrim::Stroke {
                    x: x + 0.5,
                    y: y + 0.5,
                    w: w - 1.0,
                    h: h - 1.0,
                    radius: 7.0,
                    width: 1.0,
                    color: rim,
                },
            ];
            let (px, pw, _) = rasterize_tray(&prims, 64, 44, scale, ground);
            let rgb = |x: i32, y: i32| {
                let i = ((y as u32 * pw + x as u32) * 4) as usize;
                [px[i], px[i + 1], px[i + 2]]
            };
            let at = format!("x={x} at {scale}\u{d7}");
            let mid = ((y + h / 2.0) * scale) as i32;
            let centre = ((x + w / 2.0) * scale) as i32;
            let line = scale.round() as i32;
            let (left, right) = ((x * scale).round() as i32, ((x + w) * scale).round() as i32);
            let top = (y * scale).round() as i32;
            for k in 0..line {
                assert_eq!(rgb(left + k, mid), rim[..3], "{at}: left rim");
                assert_eq!(rgb(right - 1 - k, mid), rim[..3], "{at}: right rim");
                assert_eq!(rgb(centre, top + k), rim[..3], "{at}: top rim");
            }
            assert_eq!(rgb(left - 1, mid), ground[..3], "{at}: beyond the left rim");
            assert_eq!(rgb(right, mid), ground[..3], "{at}: beyond the right rim");
            assert_eq!(
                rgb(left + line, mid),
                fill[..3],
                "{at}: inside the left rim"
            );
        }
    }
}

/// Between [`DrawPrim::SubpixelPush`] and its pop a panel and a rim keep their sub-pixel
/// edges (a glide moves them by fractions of a pixel), and after the pop the grid holds
/// again. CONTROL: the same box outside the marker lands on whole pixels.
#[test]
fn a_subpixel_push_keeps_a_gliding_box_off_the_grid() {
    let ground = [0, 0, 0, 255];
    let ink = [200, 200, 200, 255];
    let boxed = |x: f32| {
        [
            DrawPrim::Panel {
                x,
                y: 2.0,
                w: 20.0,
                h: 20.0,
                radius: 4.0,
                fill: ink,
            },
            DrawPrim::Stroke {
                x: x + 0.5,
                y: 30.5,
                w: 19.0,
                h: 19.0,
                radius: 4.0,
                width: 1.0,
                color: ink,
            },
        ]
    };
    let left_edge = |prims: &[DrawPrim]| {
        let (px, pw, _) = rasterize_tray(prims, 40, 56, 1.0, ground);
        let at = |x: u32, y: u32| px[((y * pw + x) * 4) as usize];
        (at(10, 12), at(10, 40))
    };
    let gliding: Vec<DrawPrim> = std::iter::once(DrawPrim::SubpixelPush)
        .chain(boxed(10.4))
        .chain([DrawPrim::SubpixelPop])
        .collect();
    let (fill, rim) = left_edge(&gliding);
    assert!(
        fill > 0 && fill < 200 && rim > 0 && rim < 200,
        "part strength off the grid: {fill} {rim}"
    );
    let after: Vec<DrawPrim> = gliding.iter().cloned().chain(boxed(10.4)).collect();
    assert_eq!(left_edge(&after), (200, 200), "the pop gives the grid back");
    assert_eq!(left_edge(&boxed(10.4)), (200, 200), "CONTROL: on the grid");
}

/// A rounded stroke inks exactly what the distance field over its whole box inks, though it
/// visits only its rim.
#[test]
fn a_rounded_stroke_skips_only_pixels_it_cannot_ink() {
    let cases = [
        (2.0, 3.0, 61.0, 42.0, 8.0, 1.0),
        (1.25, 2.75, 54.5, 35.25, 11.5, 1.5),
        (-4.0, -3.0, 41.0, 36.0, 5.0, 2.0),
        (7.5, 5.5, 9.0, 37.0, 4.0, 1.0),
        (4.0, 8.0, 59.0, 17.0, 0.75, 3.0),
        (3.3, 4.1, 50.0, 30.0, 14.0, 1.0),
        (0.5, 0.5, 63.0, 47.0, 28.0, 2.0),
        (2.0, 3.0, 61.0, 42.0, 2.5, 1.5),
        (3.0, 3.0, 8.0, 30.0, 3.0, 2.5),
        (1.75, 2.25, 60.0, 44.0, 19.2, 3.0),
        (5.0, 6.0, 52.0, 21.0, 10.0, 4.0),
    ];
    let paints = [
        ([0, 0, 0, 0], [18, 28, 42, 255]),
        ([31, 47, 63, 255], [92, 176, 255, 0xE0]),
    ];
    for clip in [None, Some((1.5, 1.25, 55.0, 40.5))] {
        for &(backdrop, color) in &paints {
            for &(x, y, w, h, r, width) in &cases {
                let (mut rim_only, mut every) =
                    (Canvas::new(66, 52, backdrop), Canvas::new(66, 52, backdrop));
                if let Some((cx, cy, cw, ch)) = clip {
                    rim_only.push_clip(cx, cy, cw, ch);
                    every.push_clip(cx, cy, cw, ch);
                }
                rim_only.stroke_rect(x, y, w, h, r, width, color);

                let sw = f32::max(width, 1.0);
                let (cx, cy, hx, hy) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
                let r = f32::min(r, hx).min(hy);
                let (x0, y0, x1, y1) = every.clipped_bounds(
                    (x - sw - 1.0).floor() as i32,
                    (y - sw - 1.0).floor() as i32,
                    (x + w + sw + 1.0).ceil() as i32,
                    (y + h + sw + 1.0).ceil() as i32,
                );
                for py in y0..y1 {
                    for px in x0..x1 {
                        let d = sd_round_box(px as f32 + 0.5, py as f32 + 0.5, cx, cy, hx, hy, r);
                        every.blend(px, py, color, cov(d.abs() - sw / 2.0));
                    }
                }
                assert!(
                    rim_only.px == every.px,
                    "stroke ({x},{y},{w},{h}) r={r} width={width} colour={color:?} clip={clip:?}"
                );
            }
        }
    }
}

/// A rounded stroke weighs its upright sides' line and its corners, not a band as deep as its
/// radius, nor its top and bottom pixel by pixel: the rim of a 2x card (corners of 28 pixels,
/// a 2-pixel line) and a selection ring weigh at most their height times their line and
/// margins, plus four squares a little bigger than their corners.
#[test]
fn a_rounded_stroke_visits_its_line_and_corners_not_a_band_its_radius_deep() {
    for (w, h, r, width) in [
        (400.0_f32, 300.0_f32, 28.0_f32, 2.0_f32),
        (1520.0, 980.0, 28.0, 2.0),
        (1500.0, 34.0, 10.2, 3.0),
    ] {
        let mut canvas = Canvas::new(w as u32 + 20, h as u32 + 20, [0, 0, 0, 0]);
        let before = stroke_visits();
        canvas.stroke_rect(10.0, 10.0, w, h, r, width, [92, 176, 255, 255]);
        let visits = stroke_visits() - before;
        let corner = r + 2.0 * width + 4.0;
        let budget = 2.0 * h * (2.0 * width + 6.0) + 4.0 * corner * corner;
        assert!(
            visits as f32 <= budget,
            "a {w}x{h} stroke, corners {r}, line {width}: {visits} pixels weighed, past {budget}"
        );
    }
}

/// A ring inks exactly what the distance field over its whole box inks, though it visits only
/// its band: whole and partial, with a hole and without, at fractional centres, clipped or
/// not; and a clear ink inks nothing.
#[test]
fn a_ring_skips_only_pixels_it_cannot_ink() {
    let cases = [
        (32.0, 26.0, 20.0, 3.0, 1.0),
        (31.3, 25.7, 22.25, 6.5, 1.0),
        (30.5, 26.5, 14.0, 1.2, 0.37),
        (33.1, 24.9, 9.0, 18.0, 1.0),
        (29.75, 27.2, 4.0, 2.0, 0.81),
        (31.0, 26.0, 23.5, 0.6, 0.999),
        (-3.4, 6.2, 18.0, 4.0, 0.5),
        // Row 43's centre lies exactly `outer` below the centre, and the pixel under the
        // centre there rounds to a coverage of 1e-6: a walk with no row slack missed it.
        (35.5, 20.7, 15.5, 13.6, 1.0),
    ];
    let paints = [
        ([0, 0, 0, 0], [18, 28, 42, 255]),
        ([31, 47, 63, 255], [92, 176, 255, 0xE0]),
    ];
    for clip in [None, Some((1.5, 1.25, 55.0, 40.5))] {
        for &(backdrop, color) in &paints {
            for &(cx, cy, r_mid, th, frac) in &cases {
                let (mut band, mut every) =
                    (Canvas::new(66, 52, backdrop), Canvas::new(66, 52, backdrop));
                if let Some((x, y, w, h)) = clip {
                    band.push_clip(x, y, w, h);
                    every.push_clip(x, y, w, h);
                }
                band.arc(cx, cy, r_mid, th, 0.0, frac, color);

                let r_out = r_mid + th / 2.0;
                let (x0, y0, x1, y1) = every.clipped_bounds(
                    (cx - r_out - 1.0) as i32,
                    (cy - r_out - 1.0) as i32,
                    (cx + r_out + 1.0) as i32,
                    (cy + r_out + 1.0) as i32,
                );
                for py in y0..y1 {
                    for px in x0..x1 {
                        let (dx, dy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                        let rr = (dx * dx + dy * dy).sqrt();
                        let cov_r = cov((rr - r_mid).abs() - th / 2.0);
                        let a = dx.atan2(-dy).rem_euclid(TAU) / TAU;
                        let d = if frac >= 1.0 {
                            -1.0
                        } else if a <= frac {
                            -(a.min(frac - a))
                        } else {
                            (a - frac).min(1.0 - a)
                        };
                        every.blend(px, py, color, cov_r * cov(d * TAU * rr.max(1.0)));
                    }
                }
                assert!(
                    band.px == every.px,
                    "ring ({cx},{cy}) r={r_mid} th={th} frac={frac} colour={color:?} clip={clip:?}"
                );
                let mut clear = Canvas::new(66, 52, backdrop);
                clear.arc(
                    cx,
                    cy,
                    r_mid,
                    th,
                    0.0,
                    frac,
                    [color[0], color[1], color[2], 0],
                );
                assert!(
                    clear.px == Canvas::new(66, 52, backdrop).px,
                    "a clear ring inks"
                );
            }
        }
    }
}

/// A ring or an arc whose centre, radius or width is not finite, or lies past any column,
/// inks nothing and does not overflow a column, as the whole-box walk before the runs did not.
#[test]
fn a_ring_or_arc_past_any_column_inks_nothing() {
    let (blank, _, _) = rasterize_tray(&[], 32, 16, 2.0, [0, 0, 0, 255]);
    let ink = [200, 120, 40, 255];
    let ring = |cx: f32, r_outer: f32, thickness: f32| DrawPrim::Ring {
        cx,
        cy: 8.0,
        r_outer,
        thickness,
        track: [90, 90, 90, 255],
        start: 0.0,
        frac: 1.0,
        fill: ink,
    };
    let arc = |cx: f32, r: f32, width: f32| DrawPrim::Arc {
        cx,
        cy: 8.0,
        r,
        width,
        from: 0.0,
        to: 0.75,
        color: ink,
    };
    let (inf, nan) = (f32::INFINITY, f32::NAN);
    for prim in [
        ring(inf, 6.0, 1.5),
        ring(1e30, 6.0, 1.5),
        ring(8.0, inf, 1.5),
        ring(8.0, 6.0, inf),
        ring(nan, 6.0, 1.5),
        ring(8.0, nan, nan),
        arc(inf, 6.0, 1.5),
        arc(1e30, 6.0, 1.5),
        arc(8.0, inf, 1.5),
        arc(8.0, 1e30, 1.5),
        arc(nan, 6.0, 1.5),
    ] {
        let (px, _, _) = rasterize_tray(std::slice::from_ref(&prim), 32, 16, 2.0, [0, 0, 0, 255]);
        assert!(px == blank, "{prim:?} inks");
    }
}

/// A translucent panel under an opaque one (a card's drop shadow) inks only what the opaque
/// panel leaves showing, and the raster is the same as when it inks all of itself: on the
/// Overview's and the palette's cards, Day and Night, at 1x and 2x, and on loose panels.
#[test]
fn a_shadow_under_an_opaque_panel_rasterizes_as_if_inked_whole() {
    let day = Theme {
        fg: 0x0010_1010,
        bg: 0x00FA_FAFA,
        ..Theme::default()
    };
    let mut trays = Vec::new();
    for theme in [Theme::default(), day] {
        for k in [1.0, 2.0] {
            let g = crate::settings::SettingsGeom {
                cw: 7.0 * k,
                ch: 16.0 * k,
                font_px: 13.0 * k,
                cols: 144,
                panel_rows: 40,
                px_per_pt: k,
            };
            let (w, h) = (
                (g.cols as f32 * g.cw) as u32,
                (g.panel_rows as f32 * g.ch) as u32,
            );
            let overview = crate::overview::closing_scene();
            let palette = crate::palette::PaletteState::new();
            trays.push((
                crate::overview::overview_tray(&overview, &g, theme).prims,
                w,
                h,
                1.0,
            ));
            trays.push((
                crate::palette::palette_tray(&palette, &g, theme).prims,
                w,
                h,
                1.0,
            ));
        }
    }
    let panel = |x: f32, y: f32, w: f32, h: f32, radius: f32, a: u8| DrawPrim::Panel {
        x,
        y,
        w,
        h,
        radius,
        fill: [40, 120, 200, a],
    };
    for (shadow, card) in [
        (
            (9.5, 12.25, 70.0, 41.0, 13.0),
            (12.5, 10.25, 64.0, 35.0, 10.0),
        ),
        ((3.0, 4.0, 30.0, 20.0, 0.0), (5.3, 6.6, 41.0, 31.0, 0.0)),
        ((20.0, 10.0, 50.0, 40.0, 6.0), (2.0, 2.0, 30.0, 30.0, 15.0)),
    ] {
        for scale in [1.0, 2.0] {
            let mut prims = vec![
                panel(shadow.0, shadow.1, shadow.2, shadow.3, shadow.4, 0x2A),
                panel(card.0, card.1, card.2, card.3, card.4 + 1.0, 0x30),
                panel(card.0, card.1, card.2, card.3, card.4, 0xFF),
            ];
            trays.push((prims.clone(), 90, 60, scale));
            prims.insert(
                0,
                DrawPrim::ClipPush {
                    x: 4.0,
                    y: 3.0,
                    w: 60.0,
                    h: 50.0,
                },
            );
            prims.push(DrawPrim::ClipPop);
            trays.push((prims, 90, 60, scale));
        }
    }
    for (prims, w, h, scale) in trays {
        let whole: Vec<DrawPrim> = prims
            .iter()
            .flat_map(|p| {
                let barrier = matches!(p, DrawPrim::Panel { fill, .. } if fill[3] < u8::MAX);
                let mut run = vec![p.clone()];
                if barrier {
                    // a clip of the whole canvas draws nothing and ends the run of panels
                    run.push(DrawPrim::ClipPush {
                        x: -1e6,
                        y: -1e6,
                        w: 2e6,
                        h: 2e6,
                    });
                    run.push(DrawPrim::ClipPop);
                }
                run
            })
            .collect();
        assert!(
            whole.len() > prims.len(),
            "the tray has a translucent panel"
        );
        assert!(
            rasterize_tray(&prims, w, h, scale, [0; 4])
                == rasterize_tray(&whole, w, h, scale, [0; 4]),
            "a {w}x{h} tray at {scale}x rasterizes the same with its shadows inked whole"
        );
    }
}

#[test]
fn stroke_outline_is_hollow() {
    let prims = vec![DrawPrim::Stroke {
        x: 2.0,
        y: 2.0,
        w: 16.0,
        h: 16.0,
        radius: 0.0,
        width: 1.0,
        color: [255, 0, 0, 255],
    }];
    let (px, pw, _ph) = rasterize_tray(&prims, 20, 20, 1.0, [0, 0, 0, 255]);
    let rgb = |x: u32, y: u32| {
        let i = ((y * pw + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    };
    assert_eq!(rgb(9, 2), [255, 0, 0], "top border drawn");
    assert_eq!(rgb(9, 9), [0, 0, 0], "interior stays hollow");
}

#[test]
fn diagonal_line_segment_is_antialiased_round_capped_and_bounded() {
    let prims = vec![DrawPrim::Line {
        x1: 3.0,
        y1: 3.0,
        x2: 16.0,
        y2: 16.0,
        width: 2.0,
        color: [255, 255, 255, 255],
    }];
    let (px, pw, _ph) = rasterize_tray(&prims, 20, 20, 1.0, [0, 0, 0, 255]);
    let red = |x: u32, y: u32| px[((y * pw + x) * 4) as usize];
    assert_eq!(red(9, 9), 255, "center line is fully covered");
    assert!(
        (1..255).contains(&red(8, 9)),
        "diagonal edge carries fractional coverage"
    );
    assert_eq!(red(0, 19), 0, "pixels outside the capsule stay untouched");
}

#[test]
fn clip_rejects_pixels_outside_the_pushed_rect() {
    let prims = vec![
        DrawPrim::ClipPush {
            x: 4.0,
            y: 4.0,
            w: 4.0,
            h: 4.0,
        },
        DrawPrim::Panel {
            x: 0.0,
            y: 0.0,
            w: 16.0,
            h: 16.0,
            radius: 0.0,
            fill: [0, 255, 0, 255],
        },
        DrawPrim::ClipPop,
    ];
    let (px, pw, _ph) = rasterize_tray(&prims, 16, 16, 1.0, [0, 0, 0, 255]);
    let rgb = |x: u32, y: u32| {
        let i = ((y * pw + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    };
    assert_eq!(rgb(5, 5), [0, 255, 0], "inside the clip is painted");
    assert_eq!(rgb(1, 1), [0, 0, 0], "outside the clip stays backdrop");
    assert_eq!(rgb(12, 12), [0, 0, 0], "outside the clip stays backdrop");
}

#[test]
fn a_fade_eases_what_it_holds_out_over_its_band() {
    let panel = |x: f32| DrawPrim::Panel {
        x,
        y: 0.0,
        w: 4.0,
        h: 40.0,
        radius: 0.0,
        fill: [0, 255, 0, 255],
    };
    let prims = vec![
        DrawPrim::FadePush {
            y: 10.0,
            h: 20.0,
            from: 1.0,
            to: 0.0,
        },
        panel(0.0),
        DrawPrim::FadePop,
        panel(8.0),
    ];
    let (px, pw, _ph) = rasterize_tray(&prims, 12, 40, 1.0, [0, 0, 0, 255]);
    let green = |x: u32, y: u32| px[((y * pw + x) * 4 + 1) as usize];
    assert_eq!(green(1, 5), 255, "above the band, untouched");
    assert!(green(1, 10) >= 240, "no step where the band starts");
    let quarter = green(1, 15);
    let half = green(1, 20);
    assert!(
        quarter < 230 && half + 20 < quarter && half > 40,
        "eased out: {quarter} at a quarter, {half} at half"
    );
    assert!(green(1, 31) <= 2, "nothing at and past the band's end");
    assert_eq!(green(9, 31), 255, "what follows the pop is whole");
}

#[test]
fn every_unresolved_slot_is_read_off_a_font_status() {
    let status = "candidate fallback active: unresolved regular \"No Such\": \"No Such\" \
                  does not resolve to a font file, bold italic \"Q \\\"x\\\"\": missing; \
                  requested regular \"No Such\", emoji \"E\"";
    assert_eq!(
        unresolved_slots(status),
        vec![
            ("regular", "No Such".to_string()),
            ("bold italic", "Q \"x\"".to_string())
        ]
    );
    assert!(unresolved_slots("candidate renderer ready: regular \"A\"").is_empty());
}

#[test]
fn a_fade_fades_what_it_holds_as_one_picture() {
    let ground = [40, 29, 78, 255];
    let surface = [17, 19, 24, 255];
    let frame = aterm_render::Frame {
        width: 6,
        height: 30,
        pixels: vec![0x0011_1318; 180],
    };
    let raster = |fade: bool| {
        let mut cv = Canvas::new(24, 40, ground);
        cv.push_clip(0.0, 0.0, 24.0, 36.0);
        if fade {
            cv.push_fade(10.0, 36.0, 1.0, 0.0, true);
        }
        cv.round_rect(0.0, 0.0, 20.0, 36.0, 0.0, surface);
        cv.round_rect(1.0, 0.0, 12.0, 33.0, 0.0, surface);
        cv.blit_terminal_frame(&frame, 2, 0);
        if fade {
            cv.pop_fade();
        }
        cv.pop_clip();
        cv.px
    };
    let at = |px: &[u8], x: usize, y: usize| {
        let i = (y * 24 + x) * 4;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    let whole = raster(false);
    assert_eq!(at(&whole, 4, 20), surface, "CONTROL: the frame blits whole");
    let faded = raster(true);
    assert_eq!(at(&faded, 4, 5), surface, "above the band, untouched");
    let mut dimmed = 0;
    for y in 10..36 {
        let (frame_px, inner, outer) = (at(&faded, 4, y), at(&faded, 10, y), at(&faded, 16, y));
        assert_eq!(frame_px, outer, "row {y}: the frame is the panel past it");
        assert_eq!(inner, outer, "row {y}: a panel over a panel is one panel");
        dimmed += usize::from(outer != surface);
    }
    assert!(dimmed > 20, "the band fades: {dimmed} rows dimmed");
    assert_eq!(
        at(&faded, 4, 35)[..3],
        at(&faded, 22, 35)[..3],
        "gone at the band's end"
    );
}

#[test]
fn hsv_disk_center_edge_and_outside() {
    let disk = |value: f32| DrawPrim::HsvDisk {
        cx: 10.5,
        cy: 10.5,
        r: 8.0,
        value,
    };
    let raster = |value: f32| rasterize_tray(&[disk(value)], 21, 21, 1.0, [0, 0, 0, 255]).0;
    let rgb = |px: &[u8], x: u32, y: u32| {
        let i = ((y * 21 + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    };
    let full = raster(1.0);
    assert_eq!(
        rgb(&full, 10, 10),
        [255, 255, 255],
        "centre = white at value 1"
    );
    let half = raster(0.5);
    assert_eq!(
        rgb(&half, 10, 10),
        [128, 128, 128],
        "centre = white×0.5 at value 0.5"
    );
    let edge = rgb(&full, 18, 10); // 3 o'clock: hue 0.25 = full-sat green-ish
    assert!(
        edge[1] > 0 && edge[1] < 255,
        "rim is anti-aliased, got {edge:?}"
    );
    assert_eq!(rgb(&full, 0, 0), [0, 0, 0], "far corner untouched");
    assert_eq!(rgb(&full, 20, 10), [0, 0, 0], "just past the rim untouched");
}

#[test]
fn hairline_stroke_snaps_to_one_device_row() {
    let prims = vec![DrawPrim::Stroke {
        x: 0.0,
        y: 4.0,
        w: 10.0,
        h: 1.0,
        radius: 0.0,
        width: 1.0,
        color: [255, 255, 255, 255],
    }];
    let (px, pw, ph) = rasterize_tray(&prims, 10, 10, 1.0, [0, 0, 0, 255]);
    let row_white = |y: u32| (0..pw).all(|x| px[((y * pw + x) * 4) as usize] == 255);
    let covered: Vec<u32> = (0..ph).filter(|&y| row_white(y)).collect();
    assert_eq!(covered, vec![4], "exactly the y=4 row is fully white");
}

/// A white [`DrawPrim::Dashed`], its lengths in frame px.
fn dashed(x: f32, y: f32, w: f32, h: f32, width: f32, on: f32, off: f32) -> DrawPrim {
    DrawPrim::Dashed {
        x,
        y,
        w,
        h,
        width,
        on,
        off,
        color: [255, 255, 255, 255],
    }
}

/// Whether the device pixel at `(x, y)` of a raster over black is inked.
fn inked(px: &[u8], pw: u32) -> impl Fn(u32, u32) -> bool + '_ {
    move |x, y| px[((y * pw + x) * 4) as usize] > 0
}

/// `line`'s runs in order: whether each is inked, and how long it is.
fn runs_of(line: impl Iterator<Item = bool>) -> Vec<(bool, u32)> {
    let mut runs: Vec<(bool, u32)> = Vec::new();
    for ink in line {
        match runs.last_mut() {
            Some((was, n)) if *was == ink => *n += 1,
            _ => runs.push((ink, 1)),
        }
    }
    runs
}

#[test]
fn a_dashed_lines_dashes_and_gaps_run_their_asked_lengths_at_1x_and_2x() {
    for scale in [1.0_f32, 2.0] {
        for (on, off) in [(3.0_f32, 2.0_f32), (1.5, 1.5), (4.0, 1.0), (1.0, 3.0)] {
            for across in [true, false] {
                let (w, h) = if across { (60.0, 0.0) } else { (0.0, 60.0) };
                let prim = dashed(2.0, 2.0, w, h, 1.0, on, off);
                let (px, pw, _) = rasterize_tray(&[prim], 64, 64, scale, [0, 0, 0, 255]);
                let lit = inked(&px, pw);
                let line = |cross: u32| -> Vec<bool> {
                    (0..pw)
                        .map(|i| if across { lit(i, cross) } else { lit(cross, i) })
                        .collect()
                };
                let at = format!("{scale}x {on}/{off} across={across}");
                let (start, end, t) = ((2.0 * scale) as u32, (62.0 * scale) as u32, scale as u32);
                let first = line(start);
                for cross in start..start + t {
                    assert_eq!(line(cross), first, "{at}: the stroke's rows dash alike");
                }
                for cross in [start - 1, start + t] {
                    assert!(!line(cross).contains(&true), "{at}: it is {t} px thick");
                }
                assert!(
                    !first[..start as usize].contains(&true)
                        && !first[end as usize..].contains(&true),
                    "{at}: the line stays between its ends"
                );
                let runs = runs_of(first[start as usize..end as usize].iter().copied());
                assert!(runs[0].0, "{at}: a line starts with a dash: {runs:?}");
                let asked = |ink: bool| if ink { on * scale } else { off * scale };
                let (last, whole) = runs.split_last().expect("a run");
                for &(ink, n) in whole {
                    assert!(
                        (n as f32 - asked(ink)).abs() <= 1.0,
                        "{at}: a run of {n} px for {}: {runs:?}",
                        asked(ink)
                    );
                }
                assert!(last.1 as f32 <= asked(last.0) + 1.0, "{at}: {runs:?}");
            }
        }
    }
}

#[test]
fn a_dashed_rect_dashes_all_four_edges_between_solid_corners() {
    for scale in [1.0_f32, 2.0] {
        let (on, off) = (3.0, 2.0);
        let prim = dashed(4.0, 4.0, 40.0, 30.0, 1.0, on, off);
        let (px, pw, ph) = rasterize_tray(&[prim], 48, 40, scale, [0, 0, 0, 255]);
        let lit = inked(&px, pw);
        let s = scale as u32;
        let (x0, y0, x1, y1, t) = (4 * s, 4 * s, 44 * s, 34 * s, s);
        let edges: [(&str, Vec<bool>); 4] = [
            ("top", (x0..x1).map(|x| lit(x, y0)).collect()),
            ("bottom", (x0..x1).map(|x| lit(x, y1 - 1)).collect()),
            ("left", (y0..y1).map(|y| lit(x0, y)).collect()),
            ("right", (y0..y1).map(|y| lit(x1 - 1, y)).collect()),
        ];
        for (name, edge) in &edges {
            let runs = runs_of(edge.iter().copied());
            let at = format!("{scale}x {name}");
            assert!(
                runs.iter().filter(|(ink, _)| !ink).count() >= 4,
                "{at}: the edge is dashed: {runs:?}"
            );
            let (head, tail) = (runs[0], runs[runs.len() - 1]);
            assert!(
                head.0 && head.1 >= t && tail.0 && tail.1 >= t,
                "{at}: the edge runs between its solid corners: {runs:?}"
            );
            for &(ink, n) in &runs[1..runs.len() - 2] {
                let asked = if ink { on } else { off } * scale;
                assert!(
                    (n as f32 - asked).abs() <= 1.0,
                    "{at}: a run of {n} px for {asked}: {runs:?}"
                );
            }
        }
        for y in 0..ph {
            for x in 0..pw {
                let outside = x < x0 || x >= x1 || y < y0 || y >= y1;
                let inside = x >= x0 + t && x < x1 - t && y >= y0 + t && y < y1 - t;
                let corner = (x < x0 + t || x >= x1 - t) && (y < y0 + t || y >= y1 - t);
                if outside || inside {
                    assert!(!lit(x, y), "{scale}x ({x}, {y}) is off the outline");
                } else if corner {
                    assert!(lit(x, y), "{scale}x the corner ({x}, {y}) is solid");
                }
            }
        }
    }
}

#[test]
fn a_dashed_rect_too_thin_for_an_inside_is_one_dashed_band() {
    for scale in [1.0_f32, 2.0] {
        let at = |prim: DrawPrim| rasterize_tray(&[prim], 24, 24, scale, [0, 0, 0, 255]).0;
        let pw = (24.0 * scale) as u32;
        let s = scale as u32;
        let tall = at(dashed(4.0, 2.0, 2.0, 20.0, 1.0, 2.0, 2.0));
        let lit = inked(&tall, pw);
        let (x0, x1, y0, y1) = (4 * s, 6 * s, 2 * s, 22 * s);
        for y in 0..pw {
            let across: Vec<bool> = (0..pw).map(|x| lit(x, y)).collect();
            let band = &across[x0 as usize..x1 as usize];
            assert!(
                band.iter().all(|&ink| ink == band[0]),
                "{scale}x row {y}: the band's columns dash as one: {across:?}"
            );
            assert!(
                !across[..x0 as usize].contains(&true) && !across[x1 as usize..].contains(&true),
                "{scale}x row {y}: the band is the rect's two widths"
            );
            assert!(
                (y0..y1).contains(&y) || !band[0],
                "{scale}x row {y}: the band stays between its ends"
            );
        }
        let runs = runs_of((y0..y1).map(|y| lit(x0, y)));
        let (last, whole) = runs.split_last().expect("a run");
        assert!(
            runs[0].0 && runs.len() >= 8,
            "{scale}x: dashed from the top: {runs:?}"
        );
        assert!(
            whole.iter().all(|&(_, n)| n == 2 * s) && last.1 <= 2 * s,
            "{scale}x: 2 on, 2 off: {runs:?}"
        );
        assert!(
            at(dashed(6.0, 2.0, -2.0, 20.0, 1.0, 2.0, 2.0)) == tall,
            "{scale}x: a negative width spans the same rect"
        );
        let wide = at(dashed(2.0, 4.0, 20.0, 2.0, 1.0, 2.0, 2.0));
        let wide_lit = inked(&wide, pw);
        assert!(
            (0..pw).all(|x| (0..pw).all(|y| wide_lit(x, y) == lit(y, x))),
            "{scale}x: a wide thin rect is the tall one turned"
        );
        let hollow = at(dashed(4.0, 2.0, 3.0, 20.0, 1.0, 2.0, 2.0));
        let hollow_lit = inked(&hollow, pw);
        let (inside, right) = (5 * s..6 * s, 7 * s - 1);
        for y in y0 + s..y1 - s {
            assert!(
                inside.clone().all(|x| !hollow_lit(x, y)),
                "{scale}x row {y}: one more pixel than two widths has an inside"
            );
        }
        for x in [x0, right] {
            let side = runs_of((y0..y1).map(|y| hollow_lit(x, y)));
            assert!(side.len() >= 8, "{scale}x column {x} is dashed: {side:?}");
        }
    }
}

#[test]
fn a_translucent_dashed_stroke_inks_each_pixel_once() {
    for (scale, width) in [(1.0_f32, 1.0_f32), (2.0, 1.5), (1.5, 2.0)] {
        let ink = [255, 255, 255, 128];
        let prims = [
            DrawPrim::Dashed {
                x: 3.0,
                y: 3.0,
                w: 30.0,
                h: 20.0,
                width,
                on: 2.0,
                off: 1.0,
                color: ink,
            },
            DrawPrim::Dashed {
                x: 3.0,
                y: 28.0,
                w: 30.0,
                h: 0.0,
                width,
                on: 0.5,
                off: 0.0,
                color: ink,
            },
        ];
        let (px, _, _) = rasterize_tray(&prims, 40, 34, scale, [0, 0, 0, 255]);
        let inks: std::collections::BTreeSet<[u8; 4]> = px
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] > 0)
            .copied()
            .collect();
        assert_eq!(inks.len(), 1, "{scale}x: one blend of the ink: {inks:?}");
    }
}

#[test]
fn a_dashed_stroke_lands_on_whole_device_pixels() {
    let prims = [
        dashed(2.3, 3.7, 27.4, 18.6, 0.8, 1.7, 1.2),
        dashed(1.6, 26.2, 30.3, 0.0, 1.3, 2.2, 0.9),
        dashed(33.4, 2.2, 0.0, 25.1, 0.6, 1.1, 1.4),
    ];
    let (px, _, _) = rasterize_tray(&prims, 36, 30, 1.5, [0, 0, 0, 255]);
    let reds: std::collections::BTreeSet<u8> =
        px.as_chunks::<4>().0.iter().map(|pixel| pixel[0]).collect();
    assert_eq!(
        reds,
        std::collections::BTreeSet::from([0, 255]),
        "no pixel is partly inked"
    );
}

#[test]
fn a_clip_cuts_a_dashed_stroke_and_its_pop_frees_the_next() {
    let solid = |y: f32| dashed(0.0, y, 40.0, 0.0, 1.0, 3.0, 0.0);
    let prims = vec![
        DrawPrim::ClipPush {
            x: 10.0,
            y: 0.0,
            w: 12.0,
            h: 30.0,
        },
        solid(4.0),
        dashed(2.0, 8.0, 36.0, 14.0, 1.0, 2.0, 2.0),
        DrawPrim::ClipPop,
        solid(26.0),
    ];
    let (px, pw, ph) = rasterize_tray(&prims, 40, 30, 1.0, [0, 0, 0, 255]);
    let lit = inked(&px, pw);
    let row = |y: u32| (0..pw).filter(|&x| lit(x, y)).collect::<Vec<u32>>();
    assert_eq!(row(4), (10..22).collect::<Vec<u32>>(), "the clip's columns");
    assert!(
        row(8).len() > 2 && row(8).len() < 12,
        "the rect's top edge, dashed"
    );
    for y in (0..ph).filter(|&y| y != 26) {
        for x in (0..10).chain(22..pw) {
            assert!(!lit(x, y), "({x}, {y}) is outside the clip");
        }
    }
    assert_eq!(
        row(26),
        (0..40).collect::<Vec<u32>>(),
        "past the pop, whole"
    );
}

#[test]
fn a_regional_raster_dashes_as_the_full_raster_does() {
    let prims = [
        dashed(3.0, 3.0, 50.0, 30.0, 1.0, 3.0, 2.0),
        dashed(1.0, 40.0, 58.0, 0.0, 1.5, 1.5, 1.5),
        dashed(56.0, 1.0, 0.0, 44.0, 1.0, 2.0, 1.0),
    ];
    let scale = 2.0;
    let (full, full_width, _) = rasterize_tray(&prims, 60, 46, scale, [0, 0, 0, 255]);
    let region = RasterRect {
        x: 17,
        y: 9,
        width: 83,
        height: 77,
    };
    let regional = rasterize_tray_region(&prims, scale, [0, 0, 0, 255], region);
    let mut expected = Vec::with_capacity(regional.len());
    for row in region.y..region.y + region.height {
        let start = ((row * full_width + region.x) * 4) as usize;
        expected.extend_from_slice(&full[start..start + (region.width * 4) as usize]);
    }
    assert!(
        regional == expected,
        "a tile's dashes keep the full raster's phase"
    );
}

#[test]
fn a_zero_or_odd_dash_length_is_safe() {
    let (blank, _, _) = rasterize_tray(&[], 32, 12, 2.0, [0, 0, 0, 255]);
    let line = |on: f32, off: f32| dashed(2.0, 4.0, 28.0, 0.0, 1.0, on, off);
    for on in [0.0, -3.0, f32::NAN, f32::NEG_INFINITY] {
        let (px, _, _) = rasterize_tray(&[line(on, 2.0)], 32, 12, 2.0, [0, 0, 0, 255]);
        assert!(px == blank, "on {on} draws nothing");
    }
    for off in [0.0, -2.0, f32::NAN, f32::NEG_INFINITY] {
        let (px, pw, _) = rasterize_tray(&[line(3.0, off)], 32, 12, 2.0, [0, 0, 0, 255]);
        let lit = inked(&px, pw);
        assert!(
            (4..60).all(|x| lit(x, 8) && lit(x, 9)),
            "off {off} runs solid"
        );
    }
    let (px, pw, _) = rasterize_tray(&[line(0.01, 0.01)], 32, 12, 1.0, [0, 0, 0, 255]);
    let lit = inked(&px, pw);
    let runs = runs_of((2..30).map(|x| lit(x, 4)));
    assert!(
        runs.len() == 28 && runs[0].0,
        "the least dash and gap are a pixel each: {runs:?}"
    );
    for prim in [
        dashed(-1e30, 4.0, f32::INFINITY, 0.0, 1.0, 0.01, 0.01),
        dashed(f32::NEG_INFINITY, 2.0, f32::INFINITY, 0.0, 1.0, 1.0, 1.0),
        dashed(f32::NAN, f32::NAN, f32::NAN, f32::NAN, f32::NAN, 3.0, 2.0),
        dashed(0.0, 0.0, 1e30, 1e30, f32::INFINITY, f32::MAX, f32::MAX),
    ] {
        let (px, _, _) = rasterize_tray(&[prim], 32, 12, 2.0, [0, 0, 0, 255]);
        assert_eq!(px.len(), blank.len(), "the canvas holds whatever it drew");
    }
    let (px, pw, _) = rasterize_tray(
        &[dashed(-1e30, 4.0, f32::INFINITY, 0.0, 1.0, 0.01, 0.01)],
        32,
        12,
        1.0,
        [0, 0, 0, 255],
    );
    let lit = inked(&px, pw);
    assert!(
        (0..32).filter(|&x| lit(x, 4)).count() == 16,
        "a line past the canvas dashes what the canvas shows"
    );
}

#[test]
fn regional_raster_is_byte_exact_slice_of_full_raster() {
    let (w, h, scale) = (180_u32, 96_u32, 1.5_f32);
    let mut prims = vec![DrawPrim::Panel {
        x: 0.0,
        y: 0.0,
        w: w as f32,
        h: h as f32,
        radius: 0.0,
        fill: [21, 24, 31, 255],
    }];
    prims.extend(demo_prims(w as f32, h as f32, Theme::default()));
    prims.extend([
        DrawPrim::ClipPush {
            x: 150.0,
            y: 4.0,
            w: 24.0,
            h: 30.0,
        },
        DrawPrim::Panel {
            x: 150.0,
            y: 4.0,
            w: 24.0,
            h: 30.0,
            radius: 5.0,
            fill: [230, 90, 75, 255],
        },
        DrawPrim::ClipPop,
    ]);
    let (full, full_width, full_height) = rasterize_tray(&prims, w, h, scale, [4, 7, 9, 255]);
    let region = RasterRect {
        x: 37,
        y: 19,
        width: 151,
        height: 83,
    };
    assert!(region.x + region.width <= full_width);
    assert!(region.y + region.height <= full_height);

    let regional = rasterize_tray_region(&prims, scale, [4, 7, 9, 255], region);
    let mut expected = Vec::with_capacity(regional.len());
    for row in region.y..region.y + region.height {
        let start = ((row * full_width + region.x) * 4) as usize;
        expected.extend_from_slice(&full[start..start + (region.width * 4) as usize]);
    }
    assert_eq!(regional, expected);
    assert_eq!(
        regional.len() as u64,
        region.pixels() * 4,
        "instrumentation counts only the device pixels inside the tile"
    );
    assert!(region.pixels() < u64::from(full_width) * u64::from(full_height));
}

#[test]
fn raster_patch_preserves_every_byte_outside_its_region() {
    let (width, height) = (11_u32, 9_u32);
    let mut destination = (0..width * height * 4)
        .map(|byte| byte.wrapping_mul(37) as u8)
        .collect::<Vec<_>>();
    let before = destination.clone();
    let region = RasterRect {
        x: 3,
        y: 2,
        width: 4,
        height: 5,
    };
    let patch = vec![0xA7; region.pixels() as usize * 4];
    assert!(apply_raster_patch(
        &mut destination,
        width,
        height,
        region,
        &patch,
    ));
    for y in 0..height {
        for x in 0..width {
            let index = ((y * width + x) * 4) as usize;
            if x >= region.x
                && x < region.x + region.width
                && y >= region.y
                && y < region.y + region.height
            {
                assert_eq!(&destination[index..index + 4], &[0xA7; 4]);
            } else {
                assert_eq!(&destination[index..index + 4], &before[index..index + 4]);
            }
        }
    }
}

#[test]
fn regional_patch_matches_a_fresh_full_raster_after_a_local_change() {
    let (logical_width, logical_height, scale) = (120_u32, 80_u32, 2.0_f32);
    let scene = |fill| {
        vec![
            DrawPrim::Panel {
                x: 0.0,
                y: 0.0,
                w: logical_width as f32,
                h: logical_height as f32,
                radius: 0.0,
                fill: [17, 21, 29, 255],
            },
            DrawPrim::Panel {
                x: 30.0,
                y: 20.0,
                w: 50.0,
                h: 30.0,
                radius: 6.0,
                fill,
            },
        ]
    };
    let old = scene([80, 120, 210, 255]);
    let new = scene([70, 205, 145, 255]);
    let (mut patched, full_width, full_height) =
        rasterize_tray(&old, logical_width, logical_height, scale, [0, 0, 0, 0]);
    let fresh = rasterize_tray(&new, logical_width, logical_height, scale, [0, 0, 0, 0]).0;
    let region = RasterRect::from_logical(28, 18, 55, 35, scale, full_width, full_height)
        .expect("local change maps to device pixels");
    let tile = rasterize_tray_region(&new, scale, [0, 0, 0, 0], region);
    assert!(apply_raster_patch(
        &mut patched,
        full_width,
        full_height,
        region,
        &tile,
    ));
    assert_eq!(
        patched, fresh,
        "patching the declared local change is pixel-identical to rerasterizing the scene"
    );
}

#[test]
fn every_admitted_message_glyph_inks_in_the_cold_store() {
    let cold = default_chrome_fonts();
    with_fonts(|store| *store = cold);
    let mut fonts = default_chrome_fonts();
    assert!(
        fonts.face_for(TextWeight::Regular, '\u{2139}').is_none(),
        "the pin's premise: no cold face carries \u{2139}"
    );
    for &ch in aterm_messages::Glyph::ALLOWED {
        let faced = fonts.face_for(TextWeight::Regular, ch).is_some();
        let own = synthesized::covers(ch);
        assert!(faced || own, "{ch:?} (U+{:04X}) has no ink", u32::from(ch));
        assert!(
            !(faced && own),
            "{ch:?} is the pen's own only where every face misses"
        );
        if !own {
            continue;
        }
        for px in [11.0, 14.0, 28.0, 44.0] {
            let (m, bitmap) = synthesized::rasterize(ch, px).expect("drawn");
            assert_eq!(bitmap.len(), m.width * m.height, "{ch:?} at {px}px");
            assert!(
                bitmap.contains(&255),
                "{ch:?} at {px}px has solid ink, not only edges"
            );
            assert_eq!(
                m.advance_width,
                synthesized::advance(ch, px).unwrap(),
                "the pen advances by what the measure charges"
            );
            assert_eq!(
                measure_text(&ch.to_string(), px, TextWeight::Regular),
                q_to_px(px_to_q(m.advance_width)),
                "{ch:?} at {px}px measures as it is drawn"
            );
            assert!(m.ymin >= -1, "{ch:?} at {px}px sinks under the baseline");
            assert!(
                (m.ymin + m.height as i32) as f32 <= 0.72 * px + 1.0,
                "{ch:?} at {px}px rises past the cap height"
            );
            assert!(
                (m.xmin + m.width as i32) as f32 <= m.advance_width + 1.0,
                "{ch:?} at {px}px overhangs its advance"
            );
            for face in [TextFace::Ui, TextFace::Mono] {
                assert_audited_text_ink(&ch.to_string(), px, TextWeight::Regular, face, 64.25);
                assert_audited_text_ink(&format!("H {ch} g"), px, TextWeight::Regular, face, 64.25);
            }
        }
    }
    for face in [TextFace::Ui, TextFace::Mono] {
        assert!(
            text_ink_band("Hℹ\u{10FFFF}", 14.0, TextWeight::Regular, face, 64.25).is_none(),
            "measured font and synthesized prefixes must not hide an unmapped glyph"
        );
    }
}

#[test]
fn the_pen_inks_the_info_glyph_no_face_carries() {
    let cold = default_chrome_fonts();
    with_fonts(|store| *store = cold);
    let px = 28.0_f32;
    let (x, baseline) = (4.0_f32, 40.0_f32);
    for face in [TextFace::Ui, TextFace::Mono] {
        let prims = vec![DrawPrim::Text {
            x,
            baseline,
            s: "\u{2139}".to_string(),
            px,
            color: [255, 255, 255, 255],
            weight: TextWeight::Regular,
            face,
        }];
        let (pixels, w, _) = rasterize_tray(&prims, 48, 48, 1.0, [0, 0, 0, 255]);
        let lit: Vec<(usize, usize)> = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter(|(_, c)| c[0] >= 128)
            .map(|(i, _)| (i % w as usize, i / w as usize))
            .collect();
        assert!(!lit.is_empty(), "{face:?}: the pen inked nothing");
        let (left, right) = (x as usize, (x + 0.76 * px).ceil() as usize);
        let (top, bottom) = (
            (baseline - 0.69 * px).floor() as usize,
            baseline as usize + 1,
        );
        for &(lx, ly) in &lit {
            assert!(
                (left..=right).contains(&lx) && (top..=bottom).contains(&ly),
                "{face:?}: ink at ({lx}, {ly}) is outside the glyph's box"
            );
        }
        let (cx, cy) = ((x + 0.38 * px) as usize, (baseline - 0.36 * px) as usize);
        let lit_at = |px_x: usize, px_y: usize| lit.contains(&(px_x, px_y));
        assert!(
            lit_at(cx, (baseline - 0.66 * px) as usize)
                && lit_at(cx, (baseline - 0.06 * px) as usize),
            "{face:?}: the ring's top and bottom are lit"
        );
        assert!(
            !lit_at(cx + (0.15 * px) as usize, cy),
            "{face:?}: the ring is hollow beside the stem"
        );
    }
}

/// A need pictogram drawn through [`DrawPrim::BandIcon`] at 1x and 2x, at an origin off the pixel grid, inks only the prim's square and draws.
#[test]
fn a_pictogram_prim_inks_only_its_square() {
    for scale in [1.0f32, 2.0] {
        for size in [11.0f32, 12.0, 13.0] {
            for icon in aterm_render::BandIcon::ALL
                .into_iter()
                .filter(|i| i.is_pictogram())
            {
                let (x, y) = (3.25, 2.5);
                let prims = [DrawPrim::BandIcon {
                    x,
                    y,
                    size,
                    icon,
                    color: [255, 255, 255, 255],
                }];
                let (buf, w, _) = rasterize_tray(&prims, 20, 20, scale, [0, 0, 0, 255]);
                let (x0, y0) = ((x * scale).round() as usize, (y * scale).round() as usize);
                let side = (size * scale).round() as usize;
                let inked: Vec<(usize, usize)> = buf
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c[0] > 0)
                    .map(|(i, _)| (i % w as usize, i / w as usize))
                    .collect();
                let at = format!("{icon:?} {size} pt at {scale}x");
                assert!(inked.len() > side, "{at}: draws ({} px)", inked.len());
                for &(px, py) in &inked {
                    assert!(
                        (x0..x0 + side).contains(&px) && (y0..y0 + side).contains(&py),
                        "{at}: ink at ({px}, {py}) outside its square at ({x0}, {y0})"
                    );
                }
            }
        }
    }
}
