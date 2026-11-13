// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::terminal::Terminal;

fn episode(shared: bool) -> ((u64, u64, u64), Vec<usize>) {
    let mut gpu = gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    gpu.set_fx_palette_shared_for_test(shared);
    let mut win = WindowGpu::new();
    let (rows, cols) = (8usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let art = Arc::new(
        aterm_core::render::FxArt::new(
            4,
            2,
            aterm_core::render::FxTexels::Index8,
            vec![1u8, 2, 1, 2, 2, 1, 2, 1],
        )
        .expect("sized"),
    );
    let mut before = None;
    let mut bound = Vec::new();
    for n in 0..186u32 {
        let palette = vec![
            aterm_core::render::FxInk::add(0x0010_0000 | (n & 0xff) << 8),
            aterm_core::render::FxInk::add(0x0000_2000 | (n * 3) & 0xff),
        ];
        let mut input = term.cell_frame(rows, cols);
        input.fx_layers = vec![aterm_core::render::FxLayer::new(
            1,
            Arc::new(aterm_core::render::FxContent::new(
                vec![
                    aterm_core::render::FxItem::Sprite(aterm_core::render::FxSprite {
                        x: 8,
                        y: 8,
                        dw: 40,
                        dh: 20,
                        flip_x: false,
                        art: Arc::clone(&art),
                        ink: 0,
                        clip: aterm_core::render::PxRect::EVERYWHERE,
                    }),
                    aterm_core::render::FxItem::Rect(aterm_core::render::FxRect {
                        x: 60,
                        y: 30,
                        w: 90,
                        h: 50,
                        ink: 0,
                    }),
                ],
                palette,
            )),
            aterm_core::render::FxSlot::Glow,
            aterm_core::render::FxSpace::Window,
            aterm_core::render::FxBlend::Exact,
        )];
        gpu.encode_present_frame(&mut win, &input);
        assert!(gpu.last_frame_arm_metal, "frame {n} encoded on Metal");
        assert!(gpu.frame_shape().fx_instances > 0, "frame {n} draws fx");
        bound.push(gpu.metal_fx_hazard().palette);
        if n == 0 {
            before = Some(gpu.metal_fx_hazard());
        }
    }
    let (a, b) = (before.expect("frame 0 ran"), gpu.metal_fx_hazard());
    (
        (
            b.settles - a.settles,
            b.settle_ns - a.settle_ns,
            b.park_ns - a.park_ns,
        ),
        bound,
    )
}

fn assert_ring_palettes(bound: &[usize], what: &str) {
    assert!(
        bound.iter().all(|&p| p != 0),
        "{what}: every frame bound one"
    );
    for (n, &p) in bound.iter().enumerate() {
        for back in 1..ARM_FRAME_SLOTS.min(n + 1) {
            assert_ne!(
                p,
                bound[n - back],
                "{what}: frame {n} binds frame {}'s palette buffer",
                n - back
            );
        }
    }
}

#[test]
fn animating_fx_frames_settle_no_reader() {
    if crate::metal::ffi::Device::preferred_listed().is_none() {
        aterm_spec::host::could_not_run("no Metal device");
    }
    let ((settles, settle_ns, park_ns), bound) = episode(false);
    let ((c_settles, c_settle_ns, c_park_ns), c_bound) = episode(true);
    assert_ring_palettes(&bound, "the ring");
    assert_ring_palettes(&c_bound, "the control's ring");
    eprintln!(
        "fx palette over 185 pipelined frames: ring {settles} settles, {settle_ns} ns \
         settled, {park_ns} ns parked in all; shared texture {c_settles} settles, \
         {c_settle_ns} ns settled, {c_park_ns} ns parked in all"
    );
    assert_eq!(settles, 0, "the per-slot palette settles no reader");
    assert_eq!(
        c_settles, 185,
        "the control: a shared palette texture settles its readers every frame"
    );
}
