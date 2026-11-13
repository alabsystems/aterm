// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(target_os = "macos")]
use super::decode_native_chrome_png;
use super::{
    BitmapDesc, CapturePixels, CapturePng, RgbaOverlay, WindowClientRect, bgrx_to_opaque_rgba8,
    composite_straight_rgba_overlay, multiply_platform_outer_shape_alpha, normalize_bitmap,
    stitch_presented_client_into_window_rgba, unpremultiply_rgba8,
};
use aterm_render::Frame;

#[test]
fn strip_caption_line_reports_the_frame_and_its_buttons() {
    use super::strip_caption_chrome_line;
    use crate::client_frame::ClientFrame;
    use crate::tab_bar::{CaptionButton, CaptionSlot, VersionSlotCols};
    let frame = ClientFrame {
        minimize: true,
        maximize: true,
        window_menu: true,
        maximized: false,
        edges: true,
        header: true,
        layout: crate::client_frame::ButtonLayout::FALLBACK,
    };
    let slot = CaptionSlot::measure(Some(frame), 9, 1.0);
    let segs = crate::tab_bar::layout_segments_with_caption(
        80,
        1,
        &[],
        0,
        false,
        VersionSlotCols::NONE,
        slot,
    );
    assert_eq!(
        strip_caption_chrome_line(&segs, Some(frame), Some(CaptionButton::Close)),
        "strip-caption frame=client buttons=minimize:68..72,maximize:72..76,close:76..80 \
         hovered=close maximized=false edges=true menu=true"
    );
    let maximized = ClientFrame {
        maximized: true,
        edges: false,
        ..frame
    };
    let slot = CaptionSlot::measure(Some(maximized), 9, 1.0);
    let segs = crate::tab_bar::layout_segments_with_caption(
        80,
        1,
        &[],
        0,
        false,
        VersionSlotCols::NONE,
        slot,
    );
    assert_eq!(
        strip_caption_chrome_line(&segs, Some(maximized), None),
        "strip-caption frame=client buttons=minimize:68..72,restore:72..76,close:76..80 \
         hovered=none maximized=true edges=false menu=true"
    );
    assert_eq!(
        strip_caption_chrome_line(&[], Some(frame), None),
        "strip-caption frame=client buttons=none hovered=none maximized=false edges=true \
         menu=true",
        "no strip laid out"
    );
    assert_eq!(
        strip_caption_chrome_line(&segs, None, None),
        "strip-caption frame=system"
    );
}

#[test]
fn ime_line_reports_the_composition_and_its_spot() {
    use super::ime_chrome_line;
    assert_eq!(
        ime_chrome_line("", None, None),
        "ime preedit=\"\" caret=- spot=-"
    );
    assert_eq!(
        ime_chrome_line("\u{4f60}\u{597d}", Some(6), Some((66, 36, 9, 20))),
        "ime preedit=\"\u{4f60}\u{597d}\" caret=6 spot=66,36,9x20"
    );
    assert_eq!(
        ime_chrome_line("ni hao\n", Some(0), Some((-4, 0, 1, 1))),
        "ime preedit=\"ni hao\\n\" caret=0 spot=-4,0,1x1"
    );
}

#[test]
fn strip_version_line_reports_what_was_laid_out() {
    use super::{StripVersionAbsence, strip_version_chrome_line};
    use crate::menu::{VersionRung, strip_version_label};
    use crate::tab_bar::{TabHit, TabSegment};
    let slot = |rung: VersionRung, start_col: u16, end_col: u16| TabSegment {
        start_col,
        end_col,
        close_col: None,
        kind: TabHit::Version {
            rung,
            attention: false,
        },
        solo: false,
    };
    let chip = TabSegment {
        start_col: 0,
        end_col: 28,
        close_col: Some(26),
        kind: TabHit::Select(0),
        solo: false,
    };
    let full = strip_version_label(VersionRung::Full, false).expect("a full rung");
    assert_eq!(
        strip_version_chrome_line(
            &[chip, slot(VersionRung::Full, 110, 132)],
            false,
            StripVersionAbsence::StripFull,
            false
        ),
        format!(
            "strip-version shown=yes rung=full cols=110..132 text={full:?} \
             attention=false hovered=false action=Version"
        )
    );
    if let Some(short) = strip_version_label(VersionRung::Short, false) {
        assert_eq!(
            strip_version_chrome_line(
                &[chip, slot(VersionRung::Short, 119, 132)],
                true,
                StripVersionAbsence::StripFull,
                false
            ),
            format!(
                "strip-version shown=yes rung=short cols=119..132 text={short:?} \
                 attention=false hovered=true action=Version"
            )
        );
    }
    let dev_facts = crate::menu::VersionFacts {
        release: false,
        version: "1.2.0",
        dev_commits: "7",
        commit: "abcdef1",
    };
    assert_eq!(
        crate::menu::strip_version_text(dev_facts, VersionRung::Full).as_deref(),
        Some("v1.2.7")
    );
    assert_eq!(
        crate::menu::strip_version_text(dev_facts, VersionRung::Short).as_deref(),
        Some("v1.2")
    );
    let rel_facts = crate::menu::VersionFacts {
        release: true,
        ..dev_facts
    };
    assert_eq!(
        crate::menu::strip_version_text(rel_facts, VersionRung::Full).as_deref(),
        Some("v1.2")
    );
    assert_eq!(
        crate::menu::strip_version_text(rel_facts, VersionRung::Short),
        None
    );
    for (absent, reason) in [
        (StripVersionAbsence::NoStrip, "no-strip"),
        (StripVersionAbsence::NotLaidOut, "not-laid-out"),
        (StripVersionAbsence::NoUiFace, "no-ui-face"),
        (StripVersionAbsence::StripFull, "strip-full"),
    ] {
        assert_eq!(
            strip_version_chrome_line(&[chip], false, absent, false),
            format!(
                "strip-version shown=no reason={reason} text={full:?} attention=false \
                 action=Version"
            )
        );
    }
    let marked = strip_version_label(VersionRung::Full, true).expect("a full rung");
    let action = crate::menu::version_slot_click(true);
    assert_eq!(
        strip_version_chrome_line(&[], false, StripVersionAbsence::StripFull, true),
        format!(
            "strip-version shown=no reason=strip-full text={marked:?} attention=true \
             action={action:?}"
        )
    );
}

#[cfg(windows)]
#[test]
fn the_strip_version_reason_reads_the_layout_it_reports() {
    let line = |app: &crate::App| {
        app.read_native_chrome()
            .into_iter()
            .find(|line| line.starts_with("strip-version "))
            .expect("Windows reports the strip's version label")
    };
    crate::tray_raster::clear_ui_fonts_for_test();
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 0;
    assert!(
        line(&app).contains("shown=no reason=no-strip"),
        "{}",
        line(&app)
    );
    app.tab_strip_rows = 1;
    assert!(
        line(&app).contains("shown=no reason=not-laid-out"),
        "nothing has laid the strip out yet: {}",
        line(&app)
    );
    app.splice_tab_strip_with(wid, 1);
    assert!(
        line(&app).contains("shown=no reason=no-ui-face"),
        "laid out before the UI face: {}",
        line(&app)
    );
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    assert!(
        line(&app).contains("shown=no reason=no-ui-face"),
        "the face landing re-lays nothing by itself — the layout reported is \
         still the faceless one: {}",
        line(&app)
    );
    app.splice_tab_strip_with(wid, 1);
    assert!(
        line(&app).contains("shown=yes rung=full"),
        "the next frame lays the label out in the face: {}",
        line(&app)
    );
    app.windows.get_mut(&wid).expect("window").last_strip_fp = None;
    assert!(line(&app).contains("shown=yes rung=full"), "{}", line(&app));
    app.windows.get_mut(&wid).expect("window").cols = 32;
    app.splice_tab_strip_with(wid, 1);
    assert!(
        line(&app).contains("shown=no reason=strip-full"),
        "{}",
        line(&app)
    );
    app.windows
        .get_mut(&wid)
        .expect("window")
        .tab_segments
        .clear();
    assert!(
        line(&app).contains("shown=no reason=not-laid-out"),
        "{}",
        line(&app)
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn mac_window_stitch_keeps_fresh_client_and_outer_shape_alpha_only() {
    let (width, height) = (5_u32, 5_u32);
    let mut platform_alpha = vec![255_u8; (width * height) as usize];
    for (x, y) in [(0, 0), (4, 0), (0, 4), (4, 4)] {
        platform_alpha[(y * width + x) as usize] = 0;
    }
    for (x, y) in [
        (1, 0),
        (0, 1),
        (3, 0),
        (4, 1),
        (0, 3),
        (1, 4),
        (4, 3),
        (3, 4),
    ] {
        platform_alpha[(y * width + x) as usize] = 128;
    }
    platform_alpha[(2 * width + 2) as usize] = 31;

    let mut window = vec![0_u8; (width * height * 4) as usize];
    let mut fresh = Vec::with_capacity(window.len());
    for i in 0..(width * height) {
        fresh.extend_from_slice(&[(100 + i) as u8, 70, 40, 200]);
    }
    let client = CapturePixels::rgba8(width, height, fresh.clone()).expect("a 5x5 client");
    stitch_presented_client_into_window_rgba(
        &mut window,
        width,
        height,
        WindowClientRect {
            x: 0,
            y: 0,
            width,
            height,
        },
        &client,
    )
    .unwrap();
    multiply_platform_outer_shape_alpha(&mut window, width, height, &platform_alpha).unwrap();

    let at = |x: u32, y: u32| ((y * width + x) * 4) as usize;
    for (x, y) in [(0, 0), (4, 0), (0, 4), (4, 4)] {
        let index = at(x, y);
        assert_eq!(
            &window[index..index + 3],
            &fresh[index..index + 3],
            "shape retention never restores stale platform RGB"
        );
        assert_eq!(window[index + 3], 0);
    }
    assert_eq!(window[at(1, 0) + 3], 100);
    let center = at(2, 2);
    assert_eq!(&window[center..center + 3], &fresh[center..center + 3]);
    assert_eq!(
        window[center + 3],
        200,
        "isolated platform alpha in an ordinary row is stale and ignored"
    );
}

#[test]
fn mac_window_stitch_keeps_what_the_platform_did_not_photograph() {
    let (width, height) = (8_u32, 4_u32);
    let mut platform_alpha = vec![255_u8; (width * height) as usize];
    for y in 0..height {
        for x in 5..width {
            platform_alpha[(y * width + x) as usize] = 0;
        }
    }
    for y in [0, height - 1] {
        platform_alpha[(y * width) as usize] = 0;
        platform_alpha[(y * width + 1) as usize] = 128;
    }
    let mut window = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..(width * height) {
        window.extend_from_slice(&[10, 20, 30, 200]);
    }
    multiply_platform_outer_shape_alpha(&mut window, width, height, &platform_alpha).unwrap();
    let alpha = |x: u32, y: u32| window[((y * width + x) * 4 + 3) as usize];
    assert_eq!((alpha(6, 1), alpha(7, 2), alpha(5, 1)), (200, 200, 200));
    assert_eq!((alpha(7, 0), alpha(7, 3)), (0, 0));
    assert_eq!((alpha(6, 0), alpha(6, 3)), (100, 100));
    assert_eq!((alpha(0, 0), alpha(1, 0), alpha(2, 1)), (0, 100, 200));
}

#[test]
fn sigusr_windowed_snapshot_roundtrips_exact_non_cell_destination_and_present_passes() {
    let expected = vec![
        11, 12, 13, 255, // semantic base
        90, 80, 70, 255, // present-only crown transformed pixel
        3, 4, 5, 255, // right remainder band
        21, 31, 41, 255, // semantic base
        210, 160, 120, 192, // present-only glow + alpha
        6, 7, 8, 255, // right remainder band
    ];
    let stride = 256;
    let mut mapped = vec![0xA5_u8; stride * 2];
    for (index, &[r, g, b, a]) in expected.as_chunks::<4>().0.iter().enumerate() {
        let at = (index / 3) * stride + (index % 3) * 4;
        mapped[at..at + 4].copy_from_slice(&[b, g, r, a]);
    }
    let raw = aterm_gpu::video_tap::RawCapture::bgra8_srgb_for_test(mapped.clone(), 3, 2, stride)
        .expect("a whole destination");
    let pixels = CapturePixels::raw(raw);
    assert_eq!(pixels.dims(), (3, 2));

    let encoded = CapturePng::encode(&pixels).expect("encodes");
    assert_eq!((encoded.width, encoded.height), (3, 2));
    let (decoded, w, h) = aterm_render::decode_png_rgba8(&encoded.png).expect("decodes");
    assert_eq!((w, h), (3, 2));
    assert_eq!(
        decoded, expected,
        "the snapshot cannot rerender, crop, or recolor the exact destination"
    );

    let short = mapped[..stride + 3 * 4 - 1].to_vec();
    assert!(aterm_gpu::video_tap::RawCapture::bgra8_srgb_for_test(short, 3, 2, stride).is_err());
}

#[test]
fn cpu_softbuffer_capture_is_its_rgb_and_opaque() {
    let pixels = CapturePixels::softbuffer(vec![0x7f11_2233, 0xff44_5566], 2, 1)
        .expect("a well-shaped softbuffer");
    let mut row = Vec::new();
    pixels.rgba_row(0, &mut row);
    assert_eq!(
        row,
        [0x11, 0x22, 0x33, 255, 0x44, 0x55, 0x66, 255],
        "softbuffer ignores renderer transmittance and presents opaque RGB"
    );
    assert!(CapturePixels::softbuffer(vec![0], 2, 1).is_err());
    assert!(CapturePixels::softbuffer(vec![0; 3], 2, 1).is_err());
}

fn packed_fixture(width: usize, height: usize) -> Frame {
    Frame {
        width,
        height,
        pixels: (0..width * height)
            .map(|i| {
                let i = i as u32;
                (((i * 37) % 256) << 24)
                    | (((i * 11) % 256) << 16)
                    | (((i * 5 + 3) % 256) << 8)
                    | ((i * 3 + 1) % 256)
            })
            .collect(),
    }
}

fn every_form(frame: &Frame) -> Vec<(&'static str, CapturePixels)> {
    let (width, height) = (frame.width, frame.height);
    let rgba = frame.rgba_bytes();
    let stride = (width * 4).div_ceil(256) * 256;
    let mut mapped = vec![0x5A_u8; stride * height];
    for (index, &[r, g, b, a]) in rgba.as_chunks::<4>().0.iter().enumerate() {
        let at = (index / width) * stride + (index % width) * 4;
        mapped[at..at + 4].copy_from_slice(&[b, g, r, a]);
    }
    let raw = aterm_gpu::video_tap::RawCapture::bgra8_srgb_for_test(
        mapped,
        width as u32,
        height as u32,
        stride,
    )
    .expect("a whole mapping");
    let mut forms = vec![
        (
            "rgba8",
            CapturePixels::rgba8(width as u32, height as u32, rgba).expect("straight"),
        ),
        (
            "packed",
            CapturePixels::packed(frame.clone()).expect("packed"),
        ),
        ("raw", CapturePixels::raw(raw)),
    ];
    if frame.pixels.iter().all(|pixel| pixel >> 24 == 0) {
        forms.push((
            "softbuffer",
            CapturePixels::softbuffer(frame.pixels.clone(), width, height).expect("softbuffer"),
        ));
    }
    forms
}

#[test]
fn packed_frame_encodes_like_its_rgba_bytes() {
    for (width, height) in [(1usize, 1usize), (3, 2), (37, 29)] {
        let frame = packed_fixture(width, height);
        let rgba = frame.rgba_bytes();
        let forms = every_form(&frame);
        let packed = CapturePng::encode(&forms[1].1).expect("packed encodes");
        for (form, pixels) in &forms {
            let png = CapturePng::encode(pixels).expect("encodes");
            assert_eq!(png.png, packed.png, "{width}x{height} {form}");
        }
        for pixels in [width * height + 1, width * height - 1] {
            let mut wrong = frame.clone();
            wrong.pixels.resize(pixels, 0);
            let mut bytes = rgba.clone();
            bytes.resize(pixels * 4, 0);
            assert!(
                CapturePixels::packed(wrong).is_err()
                    && CapturePixels::rgba8(width as u32, height as u32, bytes).is_err(),
                "{width}x{height}: {pixels} pixels are refused in both forms"
            );
        }
        let (decoded, w, h) = aterm_render::decode_png_rgba8(&packed.png).expect("decodes");
        assert_eq!((w, h, decoded), (width, height, rgba), "{width}x{height}");

        let mut nudged = frame;
        nudged.pixels[width * height / 2] ^= 1;
        let other = CapturePixels::packed(nudged)
            .and_then(|pixels| CapturePng::encode(&pixels))
            .expect("encodes");
        assert_ne!(
            other.png, packed.png,
            "{width}x{height}: one channel differs"
        );
    }
}

#[test]
fn rgba8_and_packed_pixels_share_one_fingerprint() {
    for (width, height) in [(1usize, 1usize), (32, 32), (37, 29), (1031, 3)] {
        for opaque in [false, true] {
            let mut frame = packed_fixture(width, height);
            if opaque {
                frame
                    .pixels
                    .iter_mut()
                    .for_each(|pixel| *pixel &= 0x00FF_FFFF);
            }
            let expected = crate::App::image_pixel_fingerprint(width, height, &frame.pixels);
            for (form, pixels) in every_form(&frame) {
                assert_eq!(
                    pixels.pixel_fingerprint(),
                    expected,
                    "{width}x{height} {form}"
                );
            }
            let mut rgba = frame.rgba_bytes();
            rgba[3] ^= 1;
            let nudged = CapturePixels::rgba8(width as u32, height as u32, rgba)
                .expect("straight")
                .pixel_fingerprint();
            assert_ne!(nudged, expected, "{width}x{height}: one alpha step");
        }
    }
}

#[test]
fn a_cut_capture_is_its_rect() {
    let frame = packed_fixture(9, 7);
    let rgba = frame.rgba_bytes();
    let rect = aterm_render::PixelRect {
        x: 2,
        y: 1,
        width: 5,
        height: 4,
    };
    let own = |rect: aterm_render::PixelRect| {
        let bytes: Vec<u8> = (rect.y..rect.y + rect.height)
            .flat_map(|y| {
                let start = (y * 9 + rect.x) * 4;
                rgba[start..start + rect.width * 4].to_vec()
            })
            .collect();
        CapturePixels::rgba8(rect.width as u32, rect.height as u32, bytes).expect("own")
    };
    let expected = own(rect);
    let expected_png = CapturePng::encode(&expected).expect("encodes").png;
    for (form, pixels) in every_form(&frame) {
        let cut = pixels
            .cut_to(aterm_render::PixelRect {
                x: 1,
                y: 0,
                width: 7,
                height: 6,
            })
            .and_then(|pixels| {
                pixels.cut_to(aterm_render::PixelRect {
                    x: 1,
                    y: 1,
                    width: 5,
                    height: 4,
                })
            })
            .expect("inside");
        assert_eq!(cut.dims(), (5, 4), "{form}");
        assert_eq!(
            CapturePng::encode(&cut).expect("encodes").png,
            expected_png,
            "{form}"
        );
        assert_eq!(
            cut.pixel_fingerprint(),
            expected.pixel_fingerprint(),
            "{form}"
        );
        let past = cut.cut_to(aterm_render::PixelRect {
            x: 1,
            y: 0,
            width: 5,
            height: 4,
        });
        assert!(past.is_err(), "{form}: a rect past the pixels");
    }
    let shifted = own(aterm_render::PixelRect { x: 3, ..rect });
    assert_ne!(
        shifted.pixel_fingerprint(),
        expected.pixel_fingerprint(),
        "one pixel right is other pixels"
    );
}

#[test]
fn window_stitch_uses_explicit_asymmetric_client_origin() {
    let mut window = vec![9_u8; 5 * 4 * 4];
    let rgba: Vec<u8> = (0_u8..16).collect();
    let client = CapturePixels::rgba8(2, 2, rgba.clone()).expect("a 2x2 client");
    stitch_presented_client_into_window_rgba(
        &mut window,
        5,
        4,
        WindowClientRect {
            x: 2,
            y: 1,
            width: 2,
            height: 2,
        },
        &client,
    )
    .unwrap();
    let at = |row: usize, col: usize| (row * 5 + col) * 4;
    assert_eq!(&window[at(1, 2)..at(1, 4)], &rgba[..8]);
    assert_eq!(&window[at(2, 2)..at(2, 4)], &rgba[8..]);
    assert!(window[..at(1, 2)].iter().all(|&byte| byte == 9));
    assert!(window[at(3, 0)..].iter().all(|&byte| byte == 9));
}

#[test]
fn window_stitch_and_unpremultiply_fail_closed_and_cover_alpha_endpoints() {
    let honest = CapturePixels::rgba8(2, 1, vec![0; 8]).expect("a 2x1 client");
    let rect = WindowClientRect {
        x: 0,
        y: 0,
        width: 2,
        height: 1,
    };
    assert!(stitch_presented_client_into_window_rgba(&mut [0; 7], 2, 1, rect, &honest).is_err());
    let wrong_shape = CapturePixels::rgba8(1, 1, vec![0; 4]).expect("a 1x1 client");
    assert!(
        stitch_presented_client_into_window_rgba(&mut [0; 8], 2, 1, rect, &wrong_shape).is_err()
    );
    let outside = WindowClientRect { x: 1, ..rect };
    assert!(stitch_presented_client_into_window_rgba(&mut [0; 8], 2, 1, outside, &honest).is_err());

    let mut premultiplied = vec![
        9, 8, 7, 0, // transparent normalizes colour
        64, 32, 16, 128, // half-alpha unassociates to approximately 128/64/32
        4, 5, 6, 255, // opaque is byte-identical
    ];
    unpremultiply_rgba8(&mut premultiplied);
    assert_eq!(premultiplied, [0, 0, 0, 0, 128, 64, 32, 128, 4, 5, 6, 255]);
}

#[test]
fn appkit_chrome_uses_straight_alpha_source_over_and_exact_placement() {
    let mut destination = vec![
        9, 8, 7, 255, // untouched left
        0, 0, 255, 128, // translucent blue beneath chrome
        4, 5, 6, 255, // transparent source leaves this exact
        1, 2, 3, 255, // second row untouched
        1, 2, 3, 255, //
        1, 2, 3, 255, //
    ];
    let chrome = RgbaOverlay {
        x: 1,
        y: 0,
        width: 2,
        height: 1,
        rgba: vec![
            255, 0, 0, 128, // half-red over half-blue
            200, 100, 50, 0, // transparent payload must not alter destination
        ],
    };
    let clip = WindowClientRect {
        x: 0,
        y: 0,
        width: 3,
        height: 2,
    };
    composite_straight_rgba_overlay(&mut destination, 3, 2, &chrome, clip).unwrap();
    assert_eq!(&destination[..4], &[9, 8, 7, 255]);
    assert_eq!(
        &destination[4..8],
        &[170, 0, 85, 192],
        "straight-alpha source-over must weight both source and destination alpha"
    );
    assert_eq!(&destination[8..12], &[4, 5, 6, 255]);
    let (second_row, _) = destination[12..].as_chunks::<4>();
    assert!(second_row.iter().all(|px| *px == [1, 2, 3, 255]));
}

#[test]
fn appkit_chrome_is_clipped_to_replaced_client_and_fails_closed_on_bad_shape() {
    let chrome = RgbaOverlay {
        x: 0,
        y: 0,
        width: 3,
        height: 1,
        rgba: vec![11, 12, 13, 255, 21, 22, 23, 255, 31, 32, 33, 255],
    };
    let clip = WindowClientRect {
        x: 1,
        y: 0,
        width: 1,
        height: 1,
    };
    let mut destination = vec![0_u8; 3 * 2 * 4];
    composite_straight_rgba_overlay(&mut destination, 3, 2, &chrome, clip).unwrap();
    assert_eq!(&destination[..4], &[0, 0, 0, 0]);
    assert_eq!(&destination[4..8], &[21, 22, 23, 255]);
    assert_eq!(&destination[8..], &[0; 16]);

    let wrong_buffer = RgbaOverlay {
        rgba: vec![0; 11],
        ..chrome.clone()
    };
    assert!(composite_straight_rgba_overlay(&mut destination, 3, 2, &wrong_buffer, clip).is_err());
    let outside = RgbaOverlay {
        x: 2,
        width: 2,
        rgba: vec![0; 8],
        ..chrome
    };
    assert!(composite_straight_rgba_overlay(&mut destination, 3, 2, &outside, clip).is_err());
    let invalid_clip = WindowClientRect {
        x: 3,
        width: 1,
        ..clip
    };
    let inside = RgbaOverlay {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        rgba: vec![0; 4],
    };
    assert!(
        composite_straight_rgba_overlay(&mut destination, 3, 2, &inside, invalid_clip).is_err()
    );
}

fn bitmap_of(pixels: &[[u8; 4]; 6], offsets: [usize; 4], bytes_per_row: usize) -> Vec<u8> {
    let mut bytes = vec![0xC3_u8; bytes_per_row * 2];
    for (index, pixel) in pixels.iter().enumerate() {
        let at = (index / 3) * bytes_per_row + (index % 3) * 4;
        for (channel, offset) in offsets.iter().enumerate() {
            bytes[at + offset] = pixel[channel];
        }
    }
    bytes
}

#[test]
fn normalize_bitmap_covers_every_supported_layout() {
    let premultiplied = [
        [0x40, 0x20, 0x10, 0x80],
        [1, 2, 3, 255],
        [9, 8, 7, 0],
        [0, 0, 0, 255],
        [255, 255, 255, 255],
        [0x10, 0x08, 0x04, 0x20],
    ];
    let unassociated = [
        128, 64, 32, 128, 1, 2, 3, 255, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 128, 64, 32,
        32,
    ];
    let straight: Vec<u8> = premultiplied.iter().flatten().copied().collect();
    let layouts = [
        (1, 0, [0, 1, 2, 3]),       // premultiplied last: R G B A
        (1, 4 << 12, [0, 1, 2, 3]), // the same, 32-bit big-endian
        (1, 2 << 12, [3, 2, 1, 0]), // little-endian: A B G R
        (2, 0, [1, 2, 3, 0]),       // premultiplied first: A R G B
        (2, 2 << 12, [2, 1, 0, 3]), // little-endian: B G R A
        (3, 0, [0, 1, 2, 3]),       // straight last
        (3, 2 << 12, [3, 2, 1, 0]),
        (4, 0, [1, 2, 3, 0]), // straight first
        (4, 2 << 12, [2, 1, 0, 3]),
    ];
    let desc = |bitmap_info: u32| BitmapDesc {
        width: 3,
        height: 2,
        bytes_per_row: 16,
        bits_per_component: 8,
        bits_per_pixel: 32,
        bitmap_info,
        rgb: true,
    };
    for (alpha, order, offsets) in layouts {
        let bytes = bitmap_of(&premultiplied, offsets, 16);
        let expected = if alpha <= 2 {
            unassociated.to_vec()
        } else {
            straight.clone()
        };
        assert_eq!(
            normalize_bitmap(&bytes, desc(alpha | order)),
            Ok(expected),
            "alpha info {alpha}, byte order {order:#x}"
        );
    }

    let bytes = bitmap_of(&premultiplied, [0, 1, 2, 3], 16);
    let sixteen = BitmapDesc {
        bits_per_component: 16,
        bits_per_pixel: 64,
        bytes_per_row: 32,
        ..desc(1)
    };
    for (label, refused) in [
        ("16-bit", sixteen),
        ("float", desc(1 | 1 << 8)),
        ("no alpha", desc(5)),
        ("16-bit byte order", desc(1 | 1 << 12)),
        (
            "not RGB",
            BitmapDesc {
                rgb: false,
                ..desc(1)
            },
        ),
        (
            "row narrower than its pixels",
            BitmapDesc {
                bytes_per_row: 8,
                ..desc(1)
            },
        ),
    ] {
        assert!(refused.direct_layout().is_err(), "{label}");
        assert!(normalize_bitmap(&bytes, refused).is_err(), "{label}");
    }
    assert!(
        normalize_bitmap(&bytes[..16 + 11], desc(1)).is_err(),
        "short of its last row"
    );
}

#[cfg(target_os = "macos")]
fn srgb_chrome_rep(
    (width, height): (isize, isize),
    bits: isize,
    straight: bool,
    tagged_srgb: bool,
    fill: impl FnOnce(&mut [u8], usize),
) -> (aterm_objc::Obj, aterm_objc::Id) {
    use aterm_macos_ui::appkit::class;
    use aterm_objc::{Bool, Id, Obj, Sel, sel};

    use crate::appkit;
    use crate::appkit::consts::NS_COLOR_RENDERING_INTENT_PERCEPTUAL;

    const ALPHA_NONPREMULTIPLIED: usize = 1 << 1;
    type Init = unsafe extern "C-unwind" fn(
        Id,
        Sel,
        *mut *mut u8,
        isize,
        isize,
        isize,
        isize,
        Bool,
        Bool,
        Id,
        usize,
        isize,
        isize,
    ) -> Id;
    let space = appkit::nsstring("NSDeviceRGBColorSpace").expect("a colour space name");
    // SAFETY: `+alloc` and designated `NSBitmapImageRep` initializer with NULL planes allocate an owned buffer.
    let rep = unsafe {
        let init: Init = aterm_objc::checked_msg();
        let alloc = appkit::send_id(class(c"NSBitmapImageRep").as_id(), sel!(alloc));
        Obj::from_owned(init(
            alloc,
            sel!(initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bitmapFormat:bytesPerRow:bitsPerPixel:),
            std::ptr::null_mut(),
            width,
            height,
            bits,
            4,
            Bool::YES,
            Bool::NO,
            space.id(),
            if straight { ALPHA_NONPREMULTIPLIED } else { 0 },
            0,
            0,
        ))
    }
    .expect("an NSBitmapImageRep");
    // SAFETY: `-bytesPerRow` and `-bitmapData` on the live `rep` owning `bytesPerRow * height` bytes.
    let (plane, row) = unsafe {
        let data: unsafe extern "C-unwind" fn(Id, Sel) -> *mut u8 = aterm_objc::checked_msg();
        let row =
            usize::try_from(appkit::send_isize(rep.id(), sel!(bytesPerRow))).expect("a row length");
        let rows = usize::try_from(height).expect("a height");
        (
            std::slice::from_raw_parts_mut(data(rep.id(), sel!(bitmapData)), row * rows),
            row,
        )
    };
    fill(plane, row);
    // SAFETY: `+sRGBColorSpace` and color-space conversion sends on the live `NSBitmapImageRep`.
    let converted = unsafe {
        let srgb = appkit::send_id(class(c"NSColorSpace").as_id(), sel!(sRGBColorSpace));
        let source = if tagged_srgb {
            appkit::send_id_id(
                rep.id(),
                sel!(bitmapImageRepByRetaggingWithColorSpace:),
                srgb,
            )
        } else {
            rep.id()
        };
        appkit::send_id_id_isize(
            source,
            sel!(bitmapImageRepByConvertingToColorSpace:renderingIntent:),
            srgb,
            NS_COLOR_RENDERING_INTENT_PERCEPTUAL,
        )
    };
    assert!(!converted.is_null(), "AppKit converts the rep to sRGB");
    (rep, converted)
}

#[cfg(target_os = "macos")]
#[test]
fn an_appkit_chrome_bitmap_is_copied_as_its_png_would_read() {
    use super::{ChromeLayer, ChromePixels, chrome_pixels_of, chrome_png_of};

    aterm_objc::autoreleasepool(|_| {
        let (_rep, converted) = srgb_chrome_rep((256, 256), 8, false, false, |plane, row| {
            for alpha in 0..256 {
                for x in 0..256 {
                    let colour = x * alpha / 255;
                    let pixel = &mut plane[alpha * row + x * 4..][..4];
                    pixel.copy_from_slice(&[
                        colour as u8,
                        (colour * 3 % (alpha + 1)) as u8,
                        (alpha - colour) as u8,
                        alpha as u8,
                    ]);
                }
            }
        });
        let (pixels, width, height) = chrome_pixels_of(converted).expect("the chrome bytes");
        assert_eq!((width, height), (256, 256));
        let ChromePixels::Bitmap { bytes, desc } = pixels else {
            panic!("an 8-bit sRGB rep is copied directly, not re-encoded");
        };
        let png = chrome_png_of(converted).expect("AppKit's PNG of the rep");
        let (expected, png_width, png_height) =
            decode_native_chrome_png(&png).expect("the PNG decodes");
        assert_eq!((png_width, png_height), (width, height));
        assert!(
            expected
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| (1..255).contains(&pixel[3])),
            "the rep has translucent pixels"
        );
        let as_straight = BitmapDesc {
            bitmap_info: (desc.bitmap_info & !0x1F) | 3,
            ..desc
        };
        assert_ne!(
            normalize_bitmap(&bytes, as_straight).expect("a straight reading"),
            expected,
            "negative control: the copy's premultiplication matters"
        );
        let overlay = ChromeLayer {
            x: 0,
            y: 0,
            width,
            height,
            pixels: ChromePixels::Bitmap { bytes, desc },
        }
        .overlay()
        .expect("the overlay");
        assert_eq!(overlay.rgba.len(), expected.len());
        let first_difference = overlay
            .rgba
            .iter()
            .zip(&expected)
            .position(|(copied, png)| copied != png);
        assert_eq!(
            first_difference, None,
            "the direct copy reads as AppKit's PNG of the rep"
        );
    });
}

#[cfg(target_os = "macos")]
#[test]
fn a_sixteen_bit_appkit_chrome_bitmap_goes_through_its_png() {
    use super::{ChromeLayer, ChromePixels, chrome_pixels_of};

    let samples: Vec<u16> = (0..3 * 2 * 4).map(|i| 0x1234 + i * 0x0A0B).collect();
    aterm_objc::autoreleasepool(|_| {
        let (_rep, converted) = srgb_chrome_rep((3, 2), 16, false, false, |_, _| {});
        let (pixels, ..) = chrome_pixels_of(converted).expect("the chrome bytes");
        assert!(
            matches!(pixels, ChromePixels::Png(_)),
            "a converted 16-bit rep stays 16-bit and is not read directly"
        );
        let (_rep, converted) = srgb_chrome_rep((3, 2), 16, true, true, |plane, row| {
            for (pixel, rgba) in samples.chunks(4).enumerate() {
                let at = pixel / 3 * row + pixel % 3 * 8;
                for (channel, sample) in rgba.iter().enumerate() {
                    plane[at + channel * 2..][..2].copy_from_slice(&sample.to_ne_bytes());
                }
            }
        });
        let (pixels, width, height) = chrome_pixels_of(converted).expect("the chrome bytes");
        assert_eq!((width, height), (3, 2));
        assert!(
            matches!(pixels, ChromePixels::Png(_)),
            "a 16-bit rep is not read directly"
        );
        let overlay = ChromeLayer {
            x: 0,
            y: 0,
            width,
            height,
            pixels,
        }
        .overlay()
        .expect("the PNG decodes");
        let high: Vec<u8> = samples
            .iter()
            .map(|sample| sample.to_be_bytes()[0])
            .collect();
        assert_eq!(overlay.rgba, high);
    });
    assert!(decode_native_chrome_png(&[0; 16]).is_err());
}

#[test]
fn bgrx_photograph_becomes_opaque_rgba() {
    let mut pixels = vec![1, 2, 3, 99, 4, 5, 6, 0];
    bgrx_to_opaque_rgba8(&mut pixels);
    assert_eq!(pixels, [3, 2, 1, 255, 6, 5, 4, 255]);
}

#[cfg(target_os = "macos")]
#[test]
fn a_window_is_stitched_on_the_encode_worker() {
    use super::{ChromeLayer, ChromePixels, EncodeJob, PlatformShot, ShotEncoding, WindowStitch};

    let (width, height) = (4_u32, 3_u32);
    let stitch = || {
        let mut photograph = Vec::new();
        for index in 0..width * height {
            let corner = [0, 3, 8, 11].contains(&index);
            photograph.extend(if corner { [0; 4] } else { [9, 9, 9, 255] });
        }
        let client: Vec<u32> = (0..width * height).map(|i| 0x0010_2030 + i).collect();
        let mut chrome = vec![0x80, 0, 0x80, 0x80, 3, 2, 1, 255];
        chrome.extend([0xEE; 4]);
        WindowStitch {
            shot: PlatformShot {
                pixels: photograph,
                width,
                height,
                encoding: ShotEncoding::PremultipliedRgba,
                chrome: Some(ChromeLayer {
                    x: 1,
                    y: 0,
                    width: 2,
                    height: 1,
                    pixels: ChromePixels::Bitmap {
                        bytes: chrome,
                        desc: BitmapDesc {
                            width: 2,
                            height: 1,
                            bytes_per_row: 12,
                            bits_per_component: 8,
                            bits_per_pixel: 32,
                            bitmap_info: 2 | 2 << 12,
                            rgb: true,
                        },
                    },
                }),
            },
            client: CapturePixels::softbuffer(client, 4, 3).expect("a 4x3 client"),
            rect: WindowClientRect {
                x: 0,
                y: 0,
                width,
                height,
            },
        }
    };
    let dir = std::env::temp_dir().join(format!(
        "aterm-window-stitch-worker-{}-{}",
        std::process::id(),
        aterm_time::since_epoch().expect("clock").as_nanos()
    ));
    crate::control_auth::ensure_private_dir(&dir).expect("capture dir");
    let mut app = crate::App::headless_for_test();
    let (reply, rx) = std::sync::mpsc::channel();
    app.submit_encode_job(EncodeJob::Window {
        stitch: stitch(),
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target: crate::control_auth::ConfinedImage::for_test(&dir, "window.png"),
        cancel: crate::control::CaptureCancellation::new(),
        reply,
    });
    let answer = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the window job answers")
        .expect("the window job succeeds");
    assert_eq!(answer.value, (width, height));
    let png = std::fs::read(dir.join("window.png")).expect("the written PNG");
    drop(answer);
    let here = std::thread::current().id();
    assert!(!super::ran_pixel_work(here), "the stitch ran on the worker");

    let (rgba, w, h) = aterm_render::decode_png_rgba8(&png).expect("a PNG");
    assert_eq!((w, h), (4, 3));
    let at = |x: usize, y: usize| &rgba[(y * 4 + x) * 4..][..4];
    assert_eq!(
        at(0, 0)[3],
        0,
        "the photograph's corner is transparent again"
    );
    assert_eq!(at(1, 1), [0x10, 0x20, 0x35, 255], "a client pixel");
    assert_eq!(at(2, 0), [1, 2, 3, 255], "the opaque chrome pixel");
    assert_eq!(
        at(1, 0)[3],
        255,
        "the translucent chrome is over an opaque client"
    );
    assert_ne!(at(1, 0), [0x10, 0x20, 0x31, 255], "and blended into it");

    let here_pixels = stitch().finish().expect("finishes");
    assert!(super::ran_pixel_work(here));
    let here_png = CapturePng::encode(&here_pixels).expect("encodes");
    assert_eq!(here_png.png, png);
    let _ = std::fs::remove_dir_all(&dir);
}
