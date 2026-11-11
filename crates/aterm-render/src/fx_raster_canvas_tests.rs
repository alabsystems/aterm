// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use aterm_core::render::FxContent;

use super::*;

struct WindowPx<'a> {
    frame: FramePx<'a>,
    bounds: FxSlice,
    calls: [usize; 3],
}

impl WindowPx<'_> {
    fn contains(&self, y: usize, x0: usize, x1: usize) {
        assert!(self.bounds.y0 <= y && y < self.bounds.y1);
        assert!(self.bounds.x0 <= x0 && x0 < x1 && x1 <= self.bounds.x1);
    }
}

impl FxCanvas for WindowPx<'_> {
    fn bounds(&self) -> FxSlice {
        self.bounds
    }
    fn flat(&mut self, y: usize, x0: usize, x1: usize, c: u32, a: u8) {
        self.contains(y, x0, x1);
        self.calls[0] += 1;
        self.frame.flat(y, x0, x1, c, a);
    }
    fn ramp(&mut self, y: usize, x0: usize, x1: usize, i0: u32, qw: u32, c0: FxInk, c1: FxInk) {
        self.contains(y, x0, x1);
        self.calls[1] += 1;
        self.frame.ramp(y, x0, x1, i0, qw, c0, c1);
    }
    fn linear(&mut self, y: usize, x0: usize, x1: usize, c: u32, a: u8) {
        self.contains(y, x0, x1);
        self.calls[2] += 1;
        self.frame.linear(y, x0, x1, c, a);
    }
}

#[test]
fn a_partial_canvas_gets_the_frames_pixels_and_leaves_its_surroundings() {
    let f = FxFrame {
        w: 12,
        h: 10,
        pad: 1,
        grid_top: 1,
        cell_h: 3,
        rows: 3,
    };
    let bounds = FxSlice {
        x0: 3,
        x1: 10,
        y0: 2,
        y1: 9,
    };
    let sprite = |texels: FxTexels, data: Vec<u8>| {
        FxItem::Sprite(FxSprite {
            x: -1,
            y: -1,
            dw: 12,
            dh: 10,
            flip_x: true,
            ink: 0,
            clip: PxRect::new(1, 0, 11, 10),
            art: Arc::new(FxArt::new(2, 2, texels, data).expect("four texels")),
        })
    };
    let layers = [
        FxLayer::new(
            1,
            Arc::new(FxContent::new(
                vec![
                    FxItem::Rect(FxRect {
                        x: -2,
                        y: 0,
                        w: 15,
                        h: 10,
                        ink: 0,
                    }),
                    FxItem::Ramps(FxRamps {
                        x: 0,
                        y: 4,
                        w: 12,
                        h: 2,
                        ink: 0,
                    }),
                    sprite(
                        FxTexels::Rgba8Premul,
                        vec![3, 5, 7, 0, 32, 64, 96, 128, 0, 0, 0, 0, 80, 40, 20, 255],
                    ),
                ],
                vec![
                    FxInk::over(0x030507, 64),
                    FxInk::over(0x204060, 128),
                    FxInk::over(0x334455, 255),
                    FxInk::over(0x090301, 64),
                ],
            )),
            FxSlot::Over,
            FxSpace::Window,
            FxBlend::Exact,
        ),
        FxLayer::new(
            2,
            Arc::new(FxContent::new(
                vec![sprite(
                    FxTexels::Rgba8Straight,
                    vec![
                        255, 0, 0, 180, 0, 255, 0, 90, 0, 0, 255, 0, 100, 50, 200, 255,
                    ],
                )],
                vec![FxInk::over(0xffffff, 255)],
            )),
            FxSlot::Over,
            FxSpace::Window,
            FxBlend::Linear,
        ),
    ];
    let occluders = [PxRect::new(5, 3, 7, 6)];
    let gate = [false, true, false];
    let ground = 0x88112233;
    for dirty in [None, Some(gate.as_slice())] {
        let mut full = vec![ground; f.w * f.h];
        let mut partial = full.clone();
        let mut scratch = FxScratch::default();
        let mut window = WindowPx {
            frame: FramePx::new(&mut partial, f.w),
            bounds,
            calls: [0; 3],
        };
        for blend in [FxBlend::Exact, FxBlend::Linear] {
            draw_layers(
                &mut full,
                &f,
                &layers,
                &occluders,
                FxSlot::Over,
                blend,
                dirty,
                &mut scratch,
            );
            draw_picked_layers_on(
                &mut window,
                &f,
                &layers,
                &occluders,
                (FxSlot::Over, blend, Knock::Any),
                dirty,
                &mut scratch,
            );
        }
        assert!(
            window.calls.into_iter().all(|n| n > 0),
            "all three paints reached"
        );
        let mut painted = false;
        for y in 0..f.h {
            for x in 0..f.w {
                let inside = bounds.x0 <= x && x < bounds.x1 && bounds.y0 <= y && y < bounds.y1;
                let want = if inside { full[y * f.w + x] } else { ground };
                assert_eq!(partial[y * f.w + x], want, "pixel ({x}, {y})");
                painted |= inside && want != ground;
            }
        }
        assert!(painted, "the restricted target actually painted");
        assert_eq!(partial[4 * f.w + 5], ground, "the occluder stays clear");
        if dirty.is_some() {
            assert_eq!(partial[2 * f.w + 3], ground, "closed band stays clear");
        }
    }
}
