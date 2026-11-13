// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ArmAtlas, DrawAtlas, MetalArmLive};
use crate::device_layer::TexelFormat;
use crate::metal::ffi::{AutoreleasePool, PixelFormat, TEXTURE_USAGE_SHADER_READ};
use crate::metal::loss::LossLatch;

fn reader_count(live: &MetalArmLive) -> i64 {
    live.frame_slots
        .iter()
        .filter(|s| s.inflight.is_some())
        .count() as i64
}

fn atlas_state(live: &MetalArmLive, uploaded_h: i64, presented: i64) -> aterm_spec::interp::State {
    assert_eq!(super::ARM_FRAME_SLOTS, 3, "the model's MaxReaders");
    let busy = |i: usize| i64::from(live.frame_slots[i].inflight.is_some());
    [
        ("epoch", 1),
        ("tex_h", 3),
        ("occupied_h", 3),
        ("uploaded_h", uploaded_h),
        ("presented", presented),
        ("readers", reader_count(live)),
        ("upload_conflict", 0),
        ("fx_gen", 1),
        ("fx_regions", 1),
        ("fx_uploaded", 1),
        ("fx_rewrite", 0),
        ("region_race", 0),
        ("pal_conflict", 0),
        ("busy0", busy(0)),
        ("busy1", busy(1)),
        ("busy2", busy(2)),
        ("cur", ((live.cur_slot + 1) % super::ARM_FRAME_SLOTS) as i64),
    ]
    .into_iter()
    .collect()
}

fn fx_state(
    live: &MetalArmLive,
    fx_regions: i64,
    fx_uploaded: i64,
    presented: i64,
) -> aterm_spec::interp::State {
    let mut s = atlas_state(live, 3, presented);
    s.insert("fx_regions", fx_regions);
    s.insert("fx_uploaded", fx_uploaded);
    s
}

fn left_in_flight(mut state: aterm_spec::interp::State) -> aterm_spec::interp::State {
    state.insert("readers", 2);
    state.insert("busy0", 1);
    state.insert("busy2", 1);
    state
}

fn live_or_skip() -> MetalArmLive {
    match MetalArmLive::new(
        crate::DevicePolicy::Headless,
        std::sync::Arc::new(LossLatch::new()),
        &crate::metal::warm::HEADLESS_FIRST_FRAME,
    ) {
        Ok(live) => live,
        Err(error) => aterm_spec::host::could_not_run(format!("no Metal arm: {error}")),
    }
}

fn readers_in(
    live: &mut MetalArmLive,
    tex: &crate::metal::resources::SealedTexture,
    slots: &[usize],
) {
    readers_in_as(live, tex, PixelFormat::R8Unorm, slots);
}

fn readers_in_as(
    live: &mut MetalArmLive,
    tex: &crate::metal::resources::SealedTexture,
    format: PixelFormat,
    slots: &[usize],
) {
    let dest = live
        .mint
        .texture_2d(format, tex.width(), tex.height(), TEXTURE_USAGE_SHADER_READ)
        .expect("reader destination");
    for &index in slots {
        let mut cb = live.session.begin().expect("reader command buffer");
        cb.copy_texture_sub_rect(tex, (0, 0), &dest, (0, 0), 1, 1)
            .expect("reader");
        live.frame_slots[index].inflight = Some(cb.commit());
    }
}

fn two_readers(live: &mut MetalArmLive, tex: &crate::metal::resources::SealedTexture) {
    readers_in(live, tex, &[0, 2]);
    assert_eq!(reader_count(live), 2);
}

fn index_art(w: u16, h: u16, fill: u8) -> std::sync::Arc<aterm_core::render::FxArt> {
    std::sync::Arc::new(
        aterm_core::render::FxArt::new(
            w,
            h,
            aterm_core::render::FxTexels::Index8,
            vec![fill; usize::from(w) * usize::from(h)],
        )
        .expect("sized"),
    )
}

/// An fx region rewritten after an LRU eviction first settles both frames in flight that
/// sample it; the texture is kept, only the new tile's texels upload, and the region reads back
/// as the new tile.
#[test]
fn an_fx_region_rewrite_settles_every_frame_reader() {
    let _pool = AutoreleasePool::new();
    let mut live = live_or_skip();
    let mut fx = super::fx_gpu::FxGpu::default();
    let tile = |rgb: [u8; 3]| {
        let texel = [rgb[0], rgb[1], rgb[2], 0xff];
        std::sync::Arc::new(
            aterm_core::render::FxArt::new(
                512,
                512,
                aterm_core::render::FxTexels::Rgba8Straight,
                texel.repeat(512 * 512),
            )
            .expect("sized"),
        )
    };
    let cap = super::fx_gpu::FX_ATLAS_MIN;
    fx.admit(&[], &[tile([0x11, 0x22, 0x33])], cap);
    live.sync_fx(&fx.atlas, false).expect("first mint");
    let rgba = super::fx_gpu::FxAtlasKind::Rgba as usize;
    let tex_id = live.fx_tex[rgba].as_ref().unwrap().tex.metal().obj().id();
    let tex = live.fx_tex[rgba]
        .as_ref()
        .unwrap()
        .tex
        .metal()
        .clone_handle();
    readers_in_as(&mut live, &tex, PixelFormat::Rgba8Unorm, &[0, 2]);
    assert_eq!(reader_count(&live), 2);
    fx.admit(&[], &[tile([0x44, 0x55, 0x66])], cap);
    let side = fx.atlas.side(super::fx_gpu::FxAtlasKind::Rgba);
    assert_eq!(
        (side.generation(), side.evictions(), side.len()),
        (1, 1, 1),
        "the new tile evicts the old one into the same generation"
    );
    let mut before = fx_state(&live, 1, 1, 0);
    before.insert("fx_rewrite", 1);
    let bytes = live.sync_fx(&fx.atlas, false).expect("rewrite");
    assert_eq!(bytes, 512 * 512 * 4, "only the new tile's texels upload");
    assert_eq!(reader_count(&live), 0, "the rewrite settled both readers");
    assert_eq!(live.fx_hazard.settles, 1);
    assert_eq!(
        live.fx_reader_wait.0, 2,
        "the frame shape's reader waits count both readers"
    );
    let kept = live.fx_tex[rgba].as_ref().unwrap();
    assert_eq!(
        kept.tex.metal().obj().id(),
        tex_id,
        "a rewrite keeps the texture"
    );
    let frame = crate::device_layer::metal_try_read_back(
        &live.session,
        &live.mint,
        kept.tex.metal(),
        512,
        512,
    )
    .expect("rewritten atlas readback");
    assert!(
        frame.pixels.iter().all(|&p| p == 0x0044_5566),
        "the new tile"
    );
    let after = fx_state(&live, 1, 1, 1);
    let model = aterm_models_render::metal_atlas_residency_model();
    assert_eq!(
        aterm_spec::interp::admits(&model, &before, &after),
        Some("SyncAndPresent")
    );
    assert_eq!(
        aterm_spec::interp::admits(&model, &before, &left_in_flight(after.clone())),
        None,
        "negative control: a rewrite under readers in flight is refused"
    );
    let mut raced = after;
    raced.insert("region_race", 1);
    assert_eq!(
        aterm_spec::interp::admits(&model, &before, &raced),
        None,
        "negative control: the region race is refused"
    );
}

#[test]
fn an_fx_append_settles_every_frame_reader() {
    let _pool = AutoreleasePool::new();
    let mut live = live_or_skip();
    let mut fx = super::fx_gpu::FxGpu::default();
    let first = vec![index_art(5, 3, 1)];
    fx.admit(&[], &first, super::fx_gpu::FX_ATLAS_CAP);
    live.sync_fx(&fx.atlas, false).expect("first mint");
    let r8 = super::fx_gpu::FxAtlasKind::R8 as usize;
    let tex_id = live.fx_tex[r8].as_ref().unwrap().tex.metal().obj().id();
    assert_eq!(live.fx_hazard.settles, 0, "a fresh texture has no reader");
    let tex = live.fx_tex[r8].as_ref().unwrap().tex.metal().clone_handle();
    two_readers(&mut live, &tex);
    assert_eq!(live.sync_fx(&fx.atlas, false).expect("unchanged"), 0);
    assert_eq!(reader_count(&live), 2);
    let second = vec![index_art(5, 3, 1), index_art(7, 2, 1)];
    fx.admit(&[], &second, super::fx_gpu::FX_ATLAS_CAP);
    let before = fx_state(&live, 2, 1, 0);
    let bytes = live.sync_fx(&fx.atlas, false).expect("resident append");
    assert_eq!(bytes, 14, "only the appended art's texels upload");
    assert_eq!(reader_count(&live), 0, "the append settled both readers");
    assert_eq!(live.fx_hazard.settles, 1);
    assert_eq!(
        live.fx_tex[r8].as_ref().unwrap().tex.metal().obj().id(),
        tex_id,
        "an append keeps the texture"
    );
    let after = fx_state(&live, 2, 2, 1);
    let model = aterm_models_render::metal_atlas_residency_model();
    assert_eq!(
        aterm_spec::interp::admits(&model, &before, &after),
        Some("SyncAndPresent")
    );
    let unsettled = left_in_flight(after.clone());
    assert_eq!(
        aterm_spec::interp::admits(&model, &before, &unsettled),
        None,
        "negative control: an append under readers in flight is refused"
    );
    let mut raced = after;
    raced.insert("region_race", 1);
    assert_eq!(
        aterm_spec::interp::admits(&model, &before, &raced),
        None,
        "negative control: the region race is refused"
    );
}

#[test]
fn a_frame_palette_never_lands_under_a_reader() {
    let _pool = AutoreleasePool::new();
    let mut live = live_or_skip();
    let mut fx = super::fx_gpu::FxGpu::default();
    fx.admit(&[], &[index_art(2, 2, 1)], super::fx_gpu::FX_ATLAS_CAP);
    live.sync_fx(&fx.atlas, false).expect("mint");
    let tex = live.fx_tex[0].as_ref().unwrap().tex.metal().clone_handle();
    two_readers(&mut live, &tex);
    live.cur_slot = 0;
    let before = atlas_state(&live, 3, 1);
    live.cur_slot = 1;
    let words = [0x0102_0304u32, 0x0506_0708];
    let buf = live.upload_fx_palette(&words).expect("staged");
    assert_eq!(
        reader_count(&live),
        2,
        "staging the palette settles nothing"
    );
    assert_eq!(live.fx_hazard.settles, 0);
    assert_eq!(
        buf.id(),
        live.frame_slots[1]
            .fx_palette
            .as_ref()
            .unwrap()
            .buf
            .metal()
            .id(),
        "the palette is slot 1's own stream"
    );
    assert!(
        live.frame_slots[0].fx_palette.is_none() && live.frame_slots[2].fx_palette.is_none(),
        "no other slot's palette is touched"
    );
    readers_in(&mut live, &tex, &[1]);
    let after = atlas_state(&live, 3, 1);
    assert_eq!(after["readers"], 3);
    let model = aterm_models_render::metal_atlas_residency_model();
    assert_eq!(
        aterm_spec::interp::admits(&model, &before, &after),
        Some("SubmitFrame")
    );
    let mut conflict = after.clone();
    conflict.insert("pal_conflict", 1);
    assert_eq!(
        aterm_spec::interp::admits(&model, &before, &conflict),
        None,
        "negative control: a palette written under a reader is refused"
    );
    let mut taken = before.clone();
    taken.insert("busy1", 1);
    taken.insert("busy2", 0);
    let mut staged = taken.clone();
    staged.insert("readers", 3);
    staged.insert("cur", 2);
    staged.insert("pal_conflict", 1);
    assert_eq!(
        aterm_spec::interp::admits(&aterm_spec::interp::with_buggy(&model, 4), &taken, &staged),
        Some("SubmitFrame"),
        "the mutant stages into the busy slot"
    );
    for pal_conflict in [0, 1] {
        staged.insert("pal_conflict", pal_conflict);
        assert_eq!(
            aterm_spec::interp::admits(&model, &taken, &staged),
            None,
            "negative control: no frame stages into a slot in flight"
        );
    }
    live.write_shared_palette(&words).expect("mint");
    readers_in(&mut live, &tex, &[0, 2]);
    assert_eq!(reader_count(&live), 3);
    live.write_shared_palette(&words).expect("rewrite");
    assert_eq!(
        reader_count(&live),
        0,
        "a shared palette settles every reader"
    );
    assert_eq!(live.fx_hazard.settles, 1);
}

#[test]
fn an_in_place_atlas_upload_settles_every_frame_reader() {
    let _pool = AutoreleasePool::new();
    let mut live = match MetalArmLive::new(
        crate::DevicePolicy::Headless,
        std::sync::Arc::new(LossLatch::new()),
        &crate::metal::warm::HEADLESS_FIRST_FRAME,
    ) {
        Ok(live) => live,
        Err(error) => {
            aterm_spec::host::could_not_run(format!("no Metal arm: {error}"));
        }
    };
    let mut atlas: Option<ArmAtlas> = None;
    let key = (4, 3, 1);
    let first = [0x11, 0x22, 0x33, 0xff].repeat(8);
    MetalArmLive::ensure_atlas(
        crate::device_layer::DeviceHandle::Metal(&live.mint),
        &mut atlas,
        DrawAtlas::Color,
        key,
        0,
        0,
        TexelFormat::Rgba8Unorm,
        &first,
        &mut live.frame_slots,
    )
    .expect("initial atlas upload");
    let texture_id = atlas.as_ref().unwrap().tex.metal().obj().id();

    let dest = live
        .mint
        .texture_2d(PixelFormat::Rgba8Unorm, 4, 3, TEXTURE_USAGE_SHADER_READ)
        .expect("reader destination");
    for index in [0, 2] {
        let mut cb = live.session.begin().expect("reader command buffer");
        cb.copy_texture_sub_rect(
            atlas.as_ref().unwrap().tex.metal(),
            (0, 0),
            &dest,
            (0, 0),
            4,
            2,
        )
        .expect("atlas reader");
        live.frame_slots[index].inflight = Some(cb.commit());
    }
    assert_eq!(reader_count(&live), 2);

    assert_eq!(
        MetalArmLive::ensure_atlas(
            crate::device_layer::DeviceHandle::Metal(&live.mint),
            &mut atlas,
            DrawAtlas::Color,
            key,
            0,
            0,
            TexelFormat::Rgba8Unorm,
            &first,
            &mut live.frame_slots,
        )
        .expect("unchanged atlas"),
        (0, 0)
    );
    assert_eq!(reader_count(&live), 2);

    let mut appended = first;
    appended.extend_from_slice(&[0x44, 0x55, 0x66, 0xff].repeat(4));
    let before = atlas_state(&live, 2, 0);
    let (consumed, _) = MetalArmLive::ensure_atlas(
        crate::device_layer::DeviceHandle::Metal(&live.mint),
        &mut atlas,
        DrawAtlas::Color,
        key,
        1,
        0,
        TexelFormat::Rgba8Unorm,
        &appended,
        &mut live.frame_slots,
    )
    .expect("resident append");
    assert_eq!(consumed, 2);
    let resident = atlas.as_ref().unwrap();
    assert_eq!(
        resident.tex.metal().obj().id(),
        texture_id,
        "append keeps the texture"
    );
    assert_eq!(resident.write_gen, 1);
    let after = atlas_state(&live, 3, 1);
    let model = aterm_models_render::metal_atlas_residency_model();
    assert_eq!(
        aterm_spec::interp::admits(&model, &before, &after),
        Some("SyncAndPresent")
    );
    let overlapping = left_in_flight(after);
    assert_eq!(
        aterm_spec::interp::admits(&model, &before, &overlapping),
        None,
        "negative control: uploading while another frame owns the atlas is rejected"
    );

    let frame = crate::device_layer::metal_try_read_back(
        &live.session,
        &live.mint,
        resident.tex.metal(),
        4,
        3,
    )
    .expect("uploaded atlas readback");
    assert_eq!(&frame.pixels[..8], &[0x112233; 8]);
    assert_eq!(&frame.pixels[8..], &[0x445566; 4]);
}
