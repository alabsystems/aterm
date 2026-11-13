// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::metal::ffi::Device;
use crate::metal::loss::LossLatch;
use std::sync::Arc;

/// Every route outside `metal/` by which a texture minted in loss domain 2
/// could reach a domain-1 encode or present (alias view, copy destination,
/// a second swapchain's drawable and alias, cross present) refuses by name.
#[test]
fn no_texture_launders_across_loss_domains() {
    use crate::metal::encoder::{EncodeSession, RenderPassDesc, StoreAction};
    use crate::metal::ffi::{
        ClearColor, LoadAction, PixelFormat, TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
        TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ,
    };
    use crate::metal::swapchain::{Swapchain, SwapchainConfig};

    let Some(dev) = Device::preferred_listed() else {
        aterm_spec::host::could_not_run("no Metal device");
    };
    let _test_pool = crate::metal::ffi::AutoreleasePool::new();
    let latch1 = Arc::new(LossLatch::new());
    let session1 = EncodeSession::new(&dev, Arc::clone(&latch1)).expect("session 1");
    let mint1 = MetalResourceDevice::new(&dev, Arc::clone(&latch1));
    let latch2 = Arc::new(LossLatch::new());
    let session2 = EncodeSession::new(&dev, Arc::clone(&latch2)).expect("session 2");
    let mint2 = MetalResourceDevice::new(&dev, Arc::clone(&latch2));

    let usage_rt =
        TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ | TEXTURE_USAGE_PIXEL_FORMAT_VIEW;
    let rogue = mint2
        .texture_2d(PixelFormat::Rgba8Unorm, 8, 8, usage_rt)
        .expect("rogue texture");
    let native1 = mint1
        .texture_2d(PixelFormat::Rgba8Unorm, 8, 8, usage_rt)
        .expect("domain-1 texture");

    let pass_desc = |t| RenderPassDesc {
        target: t,
        load: LoadAction::Clear(ClearColor {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }),
        store: StoreAction::Store,
        viewport: None,
        scissor: None,
    };

    let rogue_alias = rogue
        .alias_view(PixelFormat::Rgba8UnormSrgb)
        .expect("alias view");
    let mut cb1 = session1.begin().expect("cb1");
    let err = cb1
        .render_pass(&pass_desc(&rogue_alias))
        .expect_err("an aliased foreign texture must not become a domain-1 target");
    assert!(err.contains("loss domain"), "route A names the seal: {err}");

    let err = cb1
        .copy_texture_to_texture(&native1, &rogue_alias)
        .expect_err("an aliased foreign texture must not become a copy dst");
    assert!(err.contains("loss domain"), "route B names the seal: {err}");

    let mut sc2 = Swapchain::standalone(
        &dev,
        &SwapchainConfig {
            format: PixelFormat::Bgra8Unorm,
            width: 8,
            height: 8,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 2,
            opaque: true,
        },
        Arc::clone(&latch2),
    )
    .expect("swapchain 2");
    let frame2 = sc2.acquire().expect("acquire on sc2");
    let rt2 = frame2.render_target();
    let err = cb1
        .render_pass(&pass_desc(&rt2))
        .expect_err("a domain-2 drawable must not render on session 1");
    assert!(err.contains("loss domain"), "route C render: {err}");
    let rt2_alias = rt2.alias_view(PixelFormat::Bgra8UnormSrgb);
    if let Some(rt2_alias) = rt2_alias.as_ref() {
        let err = cb1
            .render_pass(&pass_desc(rt2_alias))
            .expect_err("a domain-2 drawable ALIAS must not render on session 1");
        assert!(err.contains("loss domain"), "route C alias: {err}");
    }
    let err = frame2
        .present(&session1)
        .expect_err("a domain-2 frame must not present via session 1");
    assert!(
        err.contains("loss domain") || err.contains("latch"),
        "route C present: {err}"
    );

    let handle1 = DeviceHandle::Metal(&mint1);
    let wrapped = LayerTexture::Metal(rogue);
    handle1.upload_texture_rows(&wrapped, 2, 2, &[7u8; 64], 32, 8);

    cb1.render_pass(&pass_desc(&native1))
        .expect("same-domain control");
    assert_eq!(
        cb1.commit().wait_outcome(),
        crate::metal::loss::CbOutcome::Completed,
        "cb1 completes"
    );
    let cb2 = session2.begin().expect("cb2");
    assert_eq!(
        cb2.commit().wait_outcome(),
        crate::metal::loss::CbOutcome::Completed,
        "the untouched domain-2 cb completes"
    );
    assert!(
        !latch1.is_lost() && !latch2.is_lost(),
        "laundering attempts are wiring errors, not device losses"
    );
}

fn mint() -> Option<MetalResourceDevice> {
    let dev = Device::preferred_listed()?;
    Some(MetalResourceDevice::new(&dev, Arc::new(LossLatch::new())))
}

/// `replaceRegion:`'s contract, judged without a device: the region inside the texture
/// (no wrapping origin), each row at least `w` whole texels, and `h` such rows of bytes.
#[test]
fn a_metal_region_upload_is_refused_unless_it_fits_its_rows_and_texture() {
    let refusal =
        |tex, bpt, region, len, stride| super::metal_region_refusal(tex, bpt, region, len, stride);
    assert_eq!(refusal((8, 8), Some(4), (0, 0, 8, 8), 256, 32), None);
    assert_eq!(refusal((8, 8), Some(4), (7, 7, 1, 1), 4, 4), None);
    assert_eq!(refusal((8, 8), Some(4), (0, 0, 2, 3), 3 * 64, 64), None);
    assert_eq!(refusal((8, 8), Some(1), (0, 0, 8, 1), 8, 8), None);
    for (tex, bpt, region, len, stride) in [
        ((8, 8), Some(4), (u32::MAX, 0, 1, 1), 4, 4),
        ((8, 8), Some(4), (0, u32::MAX, 1, 1), 4, 4),
        ((8, 8), Some(4), (1, 0, 8, 1), 32, 32),
        ((8, 8), Some(4), (0, 0, 8, 1), 32, 0),
        ((8, 8), Some(4), (0, 0, 8, 1), 32, 31),
        ((8, 8), Some(4), (0, 0, 2, 1), 10, 10),
        ((8, 8), Some(4), (0, 0, 8, 2), 63, 32),
        ((8, 8), Some(1), (0, 0, 8, 1), 0, 0),
        ((8, 8), None, (0, 0, 1, 1), 64, 8),
    ] {
        let why = refusal(tex, bpt, region, len, stride);
        assert!(
            why.as_deref().is_some_and(|w| w.contains("region upload")),
            "{tex:?} {bpt:?} {region:?} len={len} stride={stride}: {why:?}"
        );
    }
}

/// The Metal arm's length guards: a short row band, a zero stride, a wrapping origin and an
/// overlong buffer write each fail the layer's assert before `replaceRegion:` / `memcpy`,
/// while a full upload and a one-texel corner region at an exact fit go through.
#[test]
fn the_metal_arm_refuses_short_uploads_and_oversized_writes() {
    let Some(mint) = mint() else {
        aterm_spec::host::could_not_run("no Metal device");
    };
    let _test_pool = crate::metal::ffi::AutoreleasePool::new();
    let handle = DeviceHandle::Metal(&mint);
    let tex = handle
        .try_create_texture_2d(
            "device-layer short-upload probe",
            TexelFormat::R8Unorm,
            8,
            8,
            TexUsage::UPLOADED_SAMPLED,
            None,
        )
        .expect("an 8x8 R8 texture on a live Metal device");
    let short = [0u8; 8];
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle.upload_texture_rows(&tex, 0, 2, &short, 8, 8);
    }))
    .expect_err("a short row band must refuse");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|m| (*m).to_owned()))
        .unwrap_or_default();
    assert!(msg.contains("region upload"), "got: {msg}");

    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle.upload_texture_rows(&tex, 0, 1, &[], 0, 8);
    }))
    .expect_err("a zero row stride for a non-empty width must refuse");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|m| (*m).to_owned()))
        .unwrap_or_default();
    assert!(msg.contains("region upload"), "got: {msg}");

    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle.upload_texture_region(&tex, u32::MAX, 0, 1, 1, &[0], 1);
    }))
    .expect_err("a wrapping x + w origin must refuse");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|m| (*m).to_owned()))
        .unwrap_or_default();
    assert!(msg.contains("region upload"), "got: {msg}");

    handle.upload_texture_full(&tex, &[0u8; 64], 8, 8, 8);
    handle.upload_texture_region(&tex, 7, 7, 1, 1, &[0], 1);

    let buf = handle
        .try_create_instance_buffer("device-layer short-buffer probe", 4)
        .expect("a 4-byte MTLBuffer on a live Metal device");
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: no GPU work exists on this fresh buffer; the call must
        // refuse on length before any copy.
        unsafe { handle.write_buffer_from_zero(&buf, &[0u8; 16]) };
    }))
    .expect_err("an oversized buffer write must refuse");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|m| (*m).to_owned()))
        .unwrap_or_default();
    assert!(msg.contains("MTLBuffer"), "got: {msg}");
}
