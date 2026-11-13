// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::metal::ffi::{
    self, ClearColor, TEXTURE_USAGE_PIXEL_FORMAT_VIEW, TEXTURE_USAGE_RENDER_TARGET,
    TEXTURE_USAGE_SHADER_READ,
};
use crate::metal::pipelines;
use crate::metal::test_support::device;

/// An `Rgba8Unorm` texture holding `bytes` (tight stride), sealed in
/// `mint`'s loss domain.
fn uploaded_texture(
    mint: &super::super::resources::MetalResourceDevice,
    w: usize,
    h: usize,
    bytes: &[u8],
) -> SealedTexture {
    let tex = mint
        .texture_2d(PixelFormat::Rgba8Unorm, w, h, TEXTURE_USAGE_SHADER_READ)
        .expect("texture");
    // SAFETY: managed-storage (non-Private) texture, exact extent and stride.
    unsafe { tex.upload(ffi::MtlRegion::full_2d(w, h), bytes, w * 4) };
    tex
}

/// Read a texture back through the session into tight-stride bytes.
fn read_texture(
    session: &EncodeSession,
    dev: &Device,
    tex: &SealedTexture,
    w: usize,
    h: usize,
) -> Vec<u8> {
    let row = w * 4;
    let rb = dev.new_buffer(row * h).expect("readback");
    let mut cb = session.begin().expect("cb");
    cb.copy_texture_to_buffer(tex, w, h, &rb, row)
        .expect("copy");
    assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
    // SAFETY: shared storage; the command buffer completed above.
    unsafe { ffi::buffer_bytes(&rb, row * h) }
}

/// Full, sub-rect, and Unorm-to-sRGB-view texture copies move exact texel bytes.
#[test]
fn texture_copies_move_exactly_the_named_bytes() {
    let dev = device();
    let _test_pool = AutoreleasePool::new();
    let latch = Arc::new(loss::LossLatch::new());
    let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
    let mint = super::super::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));

    const W: usize = 16;
    const H: usize = 16;
    let unique: Vec<u8> = (0..W * H)
        .flat_map(|i| {
            let (x, y) = (i % W, i / W);
            [(x * 16) as u8, (y * 16) as u8, (x + y) as u8, 255]
        })
        .collect();
    const SENTINEL: [u8; 4] = [7, 9, 11, 13];
    let sentinel_frame: Vec<u8> = SENTINEL.iter().copied().cycle().take(W * H * 4).collect();

    let src = uploaded_texture(&mint, W, H, &unique);

    let dst_full = uploaded_texture(&mint, W, H, &sentinel_frame);
    let mut cb = session.begin().expect("cb");
    cb.copy_texture_to_texture(&src, &dst_full)
        .expect("full copy");
    assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
    assert_eq!(
        read_texture(&session, &dev, &dst_full, W, H),
        unique,
        "the full copy must reproduce the source byte-for-byte"
    );

    const CW: usize = 5;
    const CH: usize = 6;
    const SO: (usize, usize) = (3, 2);
    const DO: (usize, usize) = (8, 7);
    let dst = uploaded_texture(&mint, W, H, &sentinel_frame);
    let mut cb = session.begin().expect("cb");
    cb.copy_texture_sub_rect(&src, SO, &dst, DO, CW, CH)
        .expect("sub-rect copy");
    assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
    let got = read_texture(&session, &dev, &dst, W, H);
    for (i, texel) in got.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (i % W, i / W);
        let inside = x >= DO.0 && x < DO.0 + CW && y >= DO.1 && y < DO.1 + CH;
        let want: [u8; 4] = if inside {
            let (sx, sy) = (x - DO.0 + SO.0, y - DO.1 + SO.1);
            let j = (sy * W + sx) * 4;
            unique[j..j + 4].try_into().expect("4 bytes")
        } else {
            SENTINEL
        };
        assert_eq!(
            *texel, want,
            "({x},{y}): a transposed origin pair, a dropped source origin or a \
             leaked write outside the destination rect lands exactly here"
        );
    }
    assert!(!latch.is_lost());

    let dst_pair = mint
        .texture_2d(
            PixelFormat::Rgba8Unorm,
            W,
            H,
            TEXTURE_USAGE_SHADER_READ
                | TEXTURE_USAGE_RENDER_TARGET
                | TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
        )
        .expect("pair dst");
    let dst_view = dst_pair
        .alias_view(PixelFormat::Rgba8UnormSrgb)
        .expect("view");
    let mut cb = session.begin().expect("cb");
    cb.copy_texture_to_texture(&src, &dst_view)
        .expect("pair copy");
    assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
    assert_eq!(
        read_texture(&session, &dev, &dst_pair, W, H),
        unique,
        "the Unorm->sRGB alias pair must copy raw bytes, not re-encode them"
    );
    crate::stderr_line!(
        "copy proof on {}: full, 5x6 sub-rect (3,2)->(8,7) and alias-pair copies \
         all byte-exact over {} texels",
        dev.name(),
        W * H
    );
}

/// Copy validators refuse out-of-bounds, empty, overlapping, incompatible, or undersized copies before opening an encoder.
#[test]
fn the_copy_validators_refuse_what_metal_would_corrupt() {
    let dev = device();
    let _test_pool = AutoreleasePool::new();
    let latch = Arc::new(loss::LossLatch::new());
    let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
    let mint = super::super::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));

    let t16 = mint
        .texture_2d(PixelFormat::Rgba8Unorm, 16, 16, TEXTURE_USAGE_SHADER_READ)
        .expect("16x16");
    let t8 = mint
        .texture_2d(PixelFormat::Rgba8Unorm, 8, 8, TEXTURE_USAGE_SHADER_READ)
        .expect("8x8");
    let f16 = mint
        .texture_2d(PixelFormat::Rgba16Float, 16, 16, TEXTURE_USAGE_SHADER_READ)
        .expect("f16");
    let r8 = mint
        .texture_2d(PixelFormat::R8Unorm, 16, 16, TEXTURE_USAGE_SHADER_READ)
        .expect("r8");

    let mut cb = session.begin().expect("cb");

    let err = cb
        .copy_texture_sub_rect(&t16, (12, 0), &t16, (0, 0), 5, 5)
        .expect_err("source rect out of bounds");
    assert!(err.contains("source rect"), "got: {err}");
    let err = cb
        .copy_texture_sub_rect(&t16, (0, 0), &t8, (4, 4), 5, 5)
        .expect_err("destination rect out of bounds");
    assert!(err.contains("destination rect"), "got: {err}");
    let err = cb
        .copy_texture_sub_rect(&t16, (0, 0), &t16, (0, 0), 0, 4)
        .expect_err("empty region");
    assert!(err.contains("empty"), "got: {err}");
    let err = cb
        .copy_texture_sub_rect(&t16, (0, 3), &t16, (0, 0), 16, 8)
        .expect_err("overlapping same-texture band");
    assert!(err.contains("overlapping"), "got: {err}");
    let err = cb
        .copy_texture_to_texture(&t16, &f16)
        .expect_err("4-byte -> 8-byte texels");
    assert!(err.contains("copy-compatible"), "got: {err}");
    let err = cb
        .copy_texture_to_texture(&t16, &r8)
        .expect_err("4-byte -> 1-byte texels");
    assert!(err.contains("copy-compatible"), "got: {err}");

    let big = dev.new_buffer(128 * 16).expect("big");
    let err = cb
        .copy_texture_to_buffer(&f16, 16, 16, &big, 16 * 4)
        .expect_err("understated bytes_per_row");
    assert!(err.contains("destinationBytesPerRow"), "got: {err}");
    let short = dev.new_buffer(64).expect("short");
    let err = cb
        .copy_texture_to_buffer(&t16, 16, 16, &short, 16 * 4)
        .expect_err("short readback buffer");
    assert!(err.contains("holds"), "got: {err}");
    let err = cb
        .copy_texture_to_buffer(&t16, 0, 16, &big, 16 * 4)
        .expect_err("empty readback region");
    assert!(err.contains("empty"), "got: {err}");
    let err = cb
        .copy_texture_to_buffer(&t16, 17, 16, &big, 17 * 4)
        .expect_err("oversized readback region");
    assert!(err.contains("exceeds"), "got: {err}");

    assert_eq!(cb.commit().wait_outcome(), loss::CbOutcome::Completed);
    assert!(
        !latch.is_lost(),
        "refused copies are caller errors, not losses"
    );
}

/// An injected `PageFault` through [`Submitted::settle`] latches, and every
/// later [`EncodeSession::begin`] refuses naming the first loss, with no
/// command-buffer mint ([`queue_ffi_census`]).
#[test]
fn an_injected_loss_refuses_further_encodes_fast_and_named() {
    let dev = device();
    let _test_pool = AutoreleasePool::new();
    let latch = Arc::new(loss::LossLatch::new());
    let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");

    let minted = queue_ffi_census::calls();
    let s0 = session.begin().expect("healthy begin").commit();
    assert_eq!(
        queue_ffi_census::calls(),
        minted + 1,
        "a healthy begin mints exactly one command buffer"
    );
    assert_eq!(s0.wait_outcome(), loss::CbOutcome::Completed);
    assert!(!latch.is_lost());

    let retry = s0.settle(loss::classify(
        loss::STATUS_ERROR,
        Some(loss::ERROR_TIMEOUT),
    ));
    assert!(matches!(retry, loss::CbOutcome::Retryable { .. }));
    drop(
        session
            .begin()
            .expect("a retryable failure must not stop encoding")
            .commit(),
    );

    let lost = s0.settle(loss::classify(
        loss::STATUS_ERROR,
        Some(loss::ERROR_PAGE_FAULT),
    ));
    assert!(matches!(lost, loss::CbOutcome::Lost { .. }));
    assert!(latch.is_lost());

    for attempt in 0..2 {
        let minted = queue_ffi_census::calls();
        let err = session
            .begin()
            .expect_err("begin after the loss must refuse");
        assert!(
            err.contains("encode refused") && err.contains("PageFault"),
            "attempt {attempt}: the refusal names the first loss: {err}"
        );
        assert_eq!(
            queue_ffi_census::calls(),
            minted,
            "attempt {attempt}: the refusal must precede any FFI — it \
             minted a command buffer on the dead queue"
        );
    }
}

/// One latch, one session: a second session on the same latch is refused,
/// and dropping the session frees the slot for the loss rebuild.
#[test]
fn one_latch_one_session_and_drop_frees_the_slot() {
    let dev = device();
    let _test_pool = AutoreleasePool::new();
    let latch = Arc::new(loss::LossLatch::new());
    let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("first session");

    let err = match EncodeSession::new(&dev, Arc::clone(&latch)) {
        Ok(_) => panic!(
            "a second session on the SAME latch must refuse — a rogue \
             twin presents on a queue the rendering never used"
        ),
        Err(e) => e,
    };
    assert!(
        err.contains("one latch means ONE queue"),
        "the refusal names the seal: {err}"
    );

    assert_eq!(
        session.begin().expect("cb").commit().wait_outcome(),
        loss::CbOutcome::Completed
    );

    drop(session);
    let rebuilt = EncodeSession::new(&dev, Arc::clone(&latch))
        .expect("after the first session drops, the latch is claimable again");
    assert_eq!(
        rebuilt.begin().expect("cb").commit().wait_outcome(),
        loss::CbOutcome::Completed
    );
}
/// One command buffer feeds the latch exactly once through the status paths,
/// however `wait_outcome` and `try_outcome` are mixed.
#[test]
fn terminal_outcomes_feed_the_latch_exactly_once_across_wait_and_poll() {
    let dev = device();
    let _test_pool = AutoreleasePool::new();
    let latch = Arc::new(loss::LossLatch::new());
    let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");

    let s1 = session.begin().expect("cb 1").commit();
    #[cfg(debug_assertions)]
    let before = latch.fed_count();
    assert_eq!(s1.wait_outcome(), loss::CbOutcome::Completed);
    #[cfg(debug_assertions)]
    assert_eq!(latch.fed_count(), before + 1, "the wait feeds once");
    assert_eq!(s1.try_outcome(), Some(loss::CbOutcome::Completed));
    assert_eq!(s1.wait_outcome(), loss::CbOutcome::Completed);
    assert_eq!(s1.try_outcome(), Some(loss::CbOutcome::Completed));
    #[cfg(debug_assertions)]
    assert_eq!(
        latch.fed_count(),
        before + 1,
        "polls and waits AFTER the terminal answer feed nothing — the \
         once-feed cache is what keeps per-submit feed counts truthful"
    );

    let s2 = session.begin().expect("cb 2").commit();
    let polled = loop {
        if let Some(o) = s2.try_outcome() {
            break o;
        }
        std::hint::spin_loop();
    };
    assert_eq!(polled, loss::CbOutcome::Completed);
    #[cfg(debug_assertions)]
    let after_poll = latch.fed_count();
    #[cfg(debug_assertions)]
    assert_eq!(after_poll, before + 2, "the one terminal poll fed once");
    assert_eq!(s2.wait_outcome(), loss::CbOutcome::Completed);
    #[cfg(debug_assertions)]
    assert_eq!(
        latch.fed_count(),
        after_poll,
        "a wait after the terminal poll answers from the cache"
    );

    #[cfg(debug_assertions)]
    {
        let seam_before = latch.fed_count();
        let _ = s2.settle(loss::CbOutcome::Completed);
        let _ = s2.settle(loss::CbOutcome::Completed);
        assert_eq!(
            latch.fed_count(),
            seam_before + 2,
            "settle is the injection seam and must feed every call"
        );
    }
    assert!(!latch.is_lost());
}

/// The draw-state checks: a draw with no pipeline, an instanced draw with no
/// stream, and a pass onto a non-render-target texture are named errors.
#[test]
fn the_draw_state_machine_refuses_what_the_driver_punishes() {
    let dev = device();
    let _test_pool = AutoreleasePool::new();
    let latch = Arc::new(loss::LossLatch::new());
    let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");

    let mint = super::super::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));
    let target = mint
        .texture_2d(
            PixelFormat::Rgba8Unorm,
            8,
            8,
            TEXTURE_USAGE_RENDER_TARGET
                | TEXTURE_USAGE_SHADER_READ
                | TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
        )
        .expect("target");
    let target_srgb = target
        .alias_view(PixelFormat::Rgba8UnormSrgb)
        .expect("sRGB render-target view");
    let sampled_only = mint
        .texture_2d(PixelFormat::Rgba8Unorm, 8, 8, TEXTURE_USAGE_SHADER_READ)
        .expect("sampled-only");

    let mut cb = session.begin().expect("cb");
    let err = cb
        .render_pass(&RenderPassDesc {
            target: &sampled_only,
            load: LoadAction::Clear(ClearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            store: StoreAction::Store,
            viewport: None,
            scissor: None,
        })
        .expect_err("non-render-target pass must refuse");
    assert!(err.contains("TEXTURE_USAGE_RENDER_TARGET"), "got: {err}");

    {
        let pass = cb
            .render_pass(&RenderPassDesc {
                target: &target,
                load: LoadAction::Clear(ClearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                store: StoreAction::Store,
                viewport: None,
                scissor: None,
            })
            .expect("pass");
        let err = pass
            .draw_fullscreen_triangle()
            .expect_err("no-pipeline draw must refuse");
        assert!(err.contains("no pipeline"), "got: {err}");
        let err = pass
            .draw_instanced(PrimitiveType::Triangle, 6, 1)
            .expect_err("no-pipeline instanced draw must refuse");
        assert!(
            err.contains("no instance stream") || err.contains("no pipeline"),
            "got: {err}"
        );
    }

    {
        let cell = pipelines::compile_library(&dev, crate::pipeline_table::ShaderLibrary::Cell)
            .expect("cell library");
        let bg = pipelines::build(
            &dev,
            &cell,
            crate::pipeline_table::Pipeline::Bg.spec(),
            PixelFormat::Rgba8Unorm,
        )
        .expect("bg pipeline");
        let pass = cb
            .render_pass(&RenderPassDesc {
                target: &target_srgb,
                load: LoadAction::Load,
                store: StoreAction::Store,
                viewport: None,
                scissor: None,
            })
            .expect("pass");
        pass.set_pipeline(&bg);
        let err = pass
            .draw_instanced(PrimitiveType::Triangle, 6, 1)
            .expect_err("stream-less instanced draw must refuse");
        assert!(err.contains("no instance stream"), "got: {err}");
    }
    drop(cb);

    let uniform = dev.new_buffer(64).expect("uniform");
    let mut cb = session.begin().expect("cb2");
    {
        let pass = cb
            .render_pass(&RenderPassDesc {
                target: &target,
                load: LoadAction::Load,
                store: StoreAction::Store,
                viewport: None,
                scissor: None,
            })
            .expect("pass");
        let err = pass
            .set_vertex_buffer(&uniform, super::super::ffi::INSTANCE_STREAM_SLOT)
            .expect_err("binding a vertex uniform at the stream slot must refuse");
        assert!(
            err.contains("INSTANCE_STREAM_SLOT") || err.contains("30"),
            "got: {err}"
        );
    }
    drop(cb);

    let mut cb = session.begin().expect("cb3");
    let err = cb
        .copy_texture_sub_rect(&target, (0, 0), &target, (0, 2), 8, 6)
        .expect_err("overlapping self-copy must refuse");
    assert!(err.contains("overlapping same-texture"), "got: {err}");
    cb.copy_texture_sub_rect(&target, (0, 0), &target, (0, 4), 8, 4)
        .expect("disjoint same-texture copy is legal");
    let outcome = cb.commit().wait_outcome();
    assert_eq!(outcome, loss::CbOutcome::Completed);
    assert!(!latch.is_lost());
}
