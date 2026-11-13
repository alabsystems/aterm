// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::widget::{ImageArt, TextFace, TextWeight};

const CANVAS: DeviceRect = (0, 0, 200, 100);

fn panel(x: f32, fill: [u8; 4]) -> DrawPrim {
    DrawPrim::Panel {
        x,
        y: 10.0,
        w: 40.0,
        h: 20.0,
        radius: 6.0,
        fill,
    }
}

#[test]
fn gpu_prims_refuses_text_and_images() {
    let text = DrawPrim::Text {
        x: 4.0,
        baseline: 20.0,
        s: "42%".into(),
        px: 13.0,
        color: [255; 4],
        weight: TextWeight::Regular,
        face: TextFace::Ui,
    };
    let image = DrawPrim::Image {
        x: 0.0,
        y: 0.0,
        w: 20.0,
        h: 20.0,
        art: ImageArt::Kitty {
            pose: ImageArt::BRAND_POSE,
            dark: true,
        },
        alpha: 255,
    };
    let icon = DrawPrim::BandIcon {
        x: 0.0,
        y: 0.0,
        size: 12.0,
        icon: aterm_render::BandIcon::Info,
        color: [255; 4],
    };
    let disk = DrawPrim::HsvDisk {
        cx: 50.0,
        cy: 50.0,
        r: 20.0,
        value: 1.0,
    };
    let fade = DrawPrim::FadePush {
        y: 0.0,
        h: 10.0,
        from: 1.0,
        to: 0.0,
    };
    for refused in [text, image, icon, disk, fade, DrawPrim::FadePop] {
        let list = [panel(4.0, [9, 9, 9, 255]), refused.clone()];
        assert_eq!(
            gpu_prims(&list, 1.0, CANVAS),
            None,
            "{refused:?} has no GPU form"
        );
    }
    let vector = [panel(4.0, [9, 9, 9, 255]), panel(60.0, [9, 9, 9, 128])];
    assert_eq!(
        gpu_prims(&vector, 1.0, CANVAS).map(|p| p.len()),
        Some(2),
        "CONTROL: a vector list maps prim for prim"
    );
}

#[test]
fn a_prim_the_cpu_skips_maps_to_nothing() {
    let hidden_text = [
        DrawPrim::ClipPush {
            x: 500.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        },
        DrawPrim::Text {
            x: 500.0,
            baseline: 8.0,
            s: "x".into(),
            px: 8.0,
            color: [255; 4],
            weight: TextWeight::Bold,
            face: TextFace::Ui,
        },
        DrawPrim::ClipPop,
        panel(4.0, [0, 0, 0, 0]),
    ];
    assert_eq!(
        gpu_prims(&hidden_text, 1.0, CANVAS),
        Some(Vec::new()),
        "text a clip off the canvas hides is skipped, as the CPU skips it, and a clear ink \
         composites to nothing"
    );
    let ring = DrawPrim::Ring {
        cx: 50.0,
        cy: 50.0,
        r_outer: 20.0,
        thickness: 4.0,
        track: [0, 0, 0, 0],
        start: 0.25,
        frac: 0.4,
        fill: [200, 100, 50, 255],
    };
    let mapped = gpu_prims(&[ring], 2.0, (0, 0, 400, 200)).expect("a ring maps");
    assert_eq!(mapped.len(), 1, "a ring's clear track inks nothing");
    assert_eq!(mapped[0].kind, kind::RING_ARC);
    assert_eq!(mapped[0].geom[..6], [100.0, 100.0, 36.0, 8.0, 0.4, 0.25]);
}

#[test]
fn a_prims_pixels_are_the_cpu_rasters_bounds_within_the_clip() {
    let list = [
        DrawPrim::ClipPush {
            x: 0.0,
            y: 0.0,
            w: 30.5,
            h: 100.0,
        },
        panel(4.25, [1, 2, 3, 255]),
        DrawPrim::ClipPop,
    ];
    let mapped = gpu_prims(&list, 1.0, CANVAS).expect("a panel maps");
    // Snapped to the grid (4..44 × 10..30), its bounds a pixel past, the clip's right edge
    // rounded out.
    assert_eq!(mapped[0].rect, [3, 9, 31, 31]);
    assert_eq!(mapped[0].geom[..5], [4.0, 10.0, 40.0, 20.0, 6.0]);
}

#[test]
fn the_law_is_the_cpu_quantizer() {
    let law = tray_prim_law();
    assert_eq!(law.thresholds[255], f32::INFINITY);
    for b in 0..=255u8 {
        let l = law.to_linear[usize::from(b)];
        assert_eq!(
            super::super::linear_to_srgb_u8(l),
            b,
            "byte {b} round-trips"
        );
        let count = law.thresholds.partition_point(|&t| l >= t);
        assert_eq!(count, usize::from(b), "the thresholds count byte {b}");
    }
}
