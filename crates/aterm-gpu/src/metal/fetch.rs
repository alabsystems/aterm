// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Framebuffer fetch on Apple GPUs (design §5, lane B6): whether a device may draw a frame's Unorm-law groups inside its sRGB passes, decided once per device by an exhaustive 256-value round trip through its sRGB store and fetch.
//!
//! A frame's additive and glow light blends by the Unorm byte law, so on every arm it draws on the offscreen's Unorm view, and a frame that holds it opens a pass per switch of view: five for a card or a cursor glow over text. On a device with framebuffer fetch (`[[color(0)]]`, an Apple GPU), each Unorm-law row has a twin (`*_fetch` in `cell.metal`) that reads the stored byte through the sRGB view, applies its row's blend in exact integers and stores the result through the same view, so the frame stays in one pass. That is byte-exact only where the device's sRGB fetch and store return every byte: `encode(decode(b)) == b` for all 256 values, through the very conversions the twins call, and where a twin fetching what an earlier fixed-function sRGB blend of its own pass left reads the byte the pass would store. This module probes exactly that, once per device, and caches the answer; until it is [`FetchProbe::Passed`] the split stays.

use std::sync::{Mutex, PoisonError};

use super::encoder::EncodeSession;
use super::ffi::{Device, Library, PixelFormat};
use super::resources::MetalResourceDevice;
use crate::device_layer::{
    DeviceHandle, FrameEncoder, FrameLoad, FramePipeline, FrameStream, FrameUniforms, FrameView,
    SubmittedFrame, TexUsage, TexelFormat,
};
use crate::pipeline_table::{Pipeline, PipelineSpec, WriteMask};
use crate::{FetchProbe, FetchStage};

/// `MTLGPUFamilyApple1`: every Apple GPU answers it, and every Apple GPU has framebuffer fetch. An Intel or AMD Mac's GPU answers the `Mac2` family alone.
const GPU_FAMILY_APPLE1: isize = 1001;

/// The probe's fragment stages, by entry point: [`SHIPPED`] in the process, mutants in the tests.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stages {
    /// Row 0: every byte's linear value, stored through the sRGB view.
    pub(crate) store: &'static str,
    /// Row 1, and row 3 in a pass of its own: every stored byte, fetched and stored again.
    pub(crate) trip: &'static str,
    /// Rows 2 and 3: a colour over the uploaded bytes at a fractional alpha, through a fixed-function sRGB blend, which leaves values off the byte lattice in the tile until the pass stores them.
    pub(crate) blend: &'static str,
    /// Row 2: the trip in its blend's own pass, so it fetches what the blend left in the tile rather than the byte a load reads back.
    pub(crate) in_pass: &'static str,
}

/// The stages `cell.metal` compiles under `ATERM_FETCH`, which the process probes with.
pub(crate) const SHIPPED: Stages = Stages {
    store: "fs_fetch_probe_store",
    trip: "fs_fetch_probe_trip",
    blend: "fs_fetch_probe_blend",
    in_pass: "fs_fetch_probe_trip",
};

/// The probe target's width: one pixel per byte value.
const ROW: u32 = 256;
/// The probe target's rows ([`Stages`]).
const ROWS: u32 = 4;
/// One row's bytes.
const ROW_LEN: usize = (ROW * 4) as usize;

/// Whether `dev` has framebuffer fetch, so `cell.metal` compiles its twins and probe for it ([`super::shaders::source_for`]).
pub(crate) fn capable(dev: &Device) -> bool {
    dev.supports_family(GPU_FAMILY_APPLE1)
}

/// Whether `lib` is a `cell.metal` compiled with its twins and probe for a device with framebuffer fetch: without them (an Intel or AMD Mac, or a compiler that refused them, [`super::pipelines::compile_library`]) the device reads as [`FetchProbe::Unsupported`].
pub(crate) fn compiled_with_twins(dev: &Device, lib: &Library) -> bool {
    capable(dev) && lib.function(SHIPPED.store).is_some()
}

/// Every verdict this process reached, by the device's `registryID`.
static VERDICTS: Mutex<Vec<(u64, FetchProbe)>> = Mutex::new(Vec::new());

/// `id`'s cached verdict.
fn cached(id: u64) -> Option<FetchProbe> {
    VERDICTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(d, _)| *d == id)
        .map(|&(_, v)| v)
}

/// What the process knows of `dev`'s verdict without probing: [`FetchProbe::Unsupported`] where `lib` (its `cell.metal`) holds no twins, else the cached verdict, else `None`.
pub(crate) fn known(dev: &Device, lib: &Library) -> Option<FetchProbe> {
    if !compiled_with_twins(dev, lib) {
        return Some(FetchProbe::Unsupported);
    }
    cached(dev.registry_id())
}

/// The device's verdict: [`FetchProbe::Unsupported`] where `lib` (the device's `cell.metal`, [`super::pipelines::compile_library`]) holds no twins, else [`settle`] of [`probe`] with [`SHIPPED`]. The first-frame warm asks on its cell lane, off the first window's thread; an arm asks itself only for the first frame that draws Unorm-law light while no warm still owes the probe ([`super::warm::WarmLatch::probe_pending`]). A device that fails is reported once, on stderr, as its verdict is cached.
pub(crate) fn verdict(
    mint: &MetalResourceDevice,
    session: &EncodeSession,
    lib: &Library,
) -> FetchProbe {
    let dev = mint.device();
    if !compiled_with_twins(dev, lib) {
        return FetchProbe::Unsupported;
    }
    let (v, fresh) = settle(dev.registry_id(), || probe(mint, session, lib, SHIPPED));
    if fresh
        && let FetchProbe::Failed {
            stage,
            channel,
            want,
            got,
        } = v
    {
        crate::stderr_line!(
            "aterm-gpu: {} failed the framebuffer-fetch probe ({stage:?} stage, channel \
             {channel}: byte {want} came back {got}); Unorm-law light keeps its own passes",
            dev.name()
        );
    }
    v
}

/// `id`'s cached verdict, else `probe`'s, cached unless it was [`FetchProbe::Refused`] (the probe could not run, so the next ask probes again), with whether this call cached it. The cache is not held across `probe`, so two first asks may both probe; the first verdict cached stands.
fn settle(id: u64, probe: impl FnOnce() -> FetchProbe) -> (FetchProbe, bool) {
    if let Some(v) = cached(id) {
        return (v, false);
    }
    let v = probe();
    if v == FetchProbe::Refused {
        return (v, false);
    }
    let mut verdicts = VERDICTS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&(_, first)) = verdicts.iter().find(|(d, _)| *d == id) {
        return (first, false);
    }
    verdicts.push((id, v));
    (v, true)
}

/// The probe itself, through `stages` ([`Stages`]): one 256 × 4 Rgba8Unorm target drawn through its sRGB view in two passes. The first draws row 0 by `store` over zeroed colour, row 1 by `trip` over the bytes the store stage should give, row 2 by `blend` and then `in_pass`, and row 3 by `blend`; the second draws row 3 by `trip`, over the bytes the first stored. Pixel `x` carries `x`, `255 − x` and `x ^ 0x5A` (`fetch_probe_bytes`), so each channel meets every byte. Row 0 must read back those bytes, row 1 itself, and row 2 row 3's bytes: a fetch inside the blend's pass sees the byte the pass stores. Every row's alpha is uploaded and must come back untouched, as the twins' Color write mask keeps it.
pub(crate) fn probe(
    mint: &MetalResourceDevice,
    session: &EncodeSession,
    lib: &Library,
    stages: Stages,
) -> FetchProbe {
    match run(mint, session, lib, stages) {
        Ok(got) => judge(&got, &expected_rows(&got)),
        Err(why) => {
            crate::stderr_line!("aterm-gpu: the framebuffer-fetch probe could not run ({why})");
            FetchProbe::Refused
        }
    }
}

/// The bytes the probe rows carry at pixel `x`, as `fetch_probe_bytes` spells them.
const fn row_bytes(x: u32) -> [u8; 3] {
    let x = x as u8;
    [x, 255 - x, x ^ 0x5A]
}

/// The rows as uploaded: row 0 zero colour under alpha `x`, the others the bytes under alphas of their own.
fn upload_rows() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(ROW_LEN * ROWS as usize);
    for x in 0..ROW {
        bytes.extend_from_slice(&[0, 0, 0, x as u8]);
    }
    let alphas: [fn(u8) -> u8; 3] = [|x| 255 - x, |x| x ^ 0xA5, |x| x.rotate_left(3)];
    for alpha in alphas {
        for x in 0..ROW {
            let [r, g, b] = row_bytes(x);
            bytes.extend_from_slice(&[r, g, b, alpha(x as u8)]);
        }
    }
    bytes
}

/// What each row must read back, given what the probe read (`got`): row 0 the bytes, row 1 itself, rows 2 and 3 the colour the separate pass stored in row 3, each under its uploaded alpha.
fn expected_rows(got: &[u8]) -> Vec<u8> {
    let mut want = upload_rows();
    for x in 0..ROW as usize {
        want[x * 4..x * 4 + 3].copy_from_slice(&row_bytes(x as u32));
        let stored = &got[3 * ROW_LEN + x * 4..][..3];
        for row in [2, 3] {
            want[row * ROW_LEN + x * 4..][..3].copy_from_slice(stored);
        }
    }
    want
}

/// The first byte `got` disagrees with `want` on, as a verdict.
fn judge(got: &[u8], want: &[u8]) -> FetchProbe {
    match got.iter().zip(want).position(|(g, w)| g != w) {
        None => FetchProbe::Passed,
        Some(i) => FetchProbe::Failed {
            stage: match i / ROW_LEN {
                0 => FetchStage::Store,
                1 => FetchStage::RoundTrip,
                _ => FetchStage::InPass,
            },
            channel: (i % 4) as u8,
            want: want[i],
            got: got[i],
        },
    }
}

/// The spec of a probe stage: `vs_bg` and its layout, the stage's fragment, the sRGB view, the Color write mask, and no blend but the blend stage's straight-alpha over (the cursor blend's).
fn stage_spec(fs: &'static str, blended: bool) -> PipelineSpec {
    let base = if blended {
        Pipeline::CursorBlend
    } else {
        Pipeline::Bg
    };
    let base = base.spec();
    PipelineSpec {
        label: "aterm-gpu framebuffer-fetch probe",
        fs,
        blend: blended.then_some(base.blend).flatten(),
        write_mask: WriteMask::Color,
        ..*base
    }
}

/// One `BgInstance` (rect `u16 × 4`, colour `u8 × 4`) covering probe row `y`.
fn row_instance(y: u16) -> [u8; 12] {
    let mut b = [0u8; 12];
    for (i, v) in [0u16, y, ROW as u16, 1].into_iter().enumerate() {
        b[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }
    b
}

/// Draw the probe and read its rows back.
fn run(
    mint: &MetalResourceDevice,
    session: &EncodeSession,
    lib: &Library,
    stages: Stages,
) -> Result<Vec<u8>, String> {
    let dev = mint.device();
    let build = |fs, blended| {
        super::pipelines::build(dev, lib, &stage_spec(fs, blended), PixelFormat::Bgra8Unorm)
    };
    let store = build(stages.store, false)?;
    let trip = build(stages.trip, false)?;
    let blend = build(stages.blend, true)?;
    let in_pass = if stages.in_pass == stages.trip {
        trip.clone_retained()
    } else {
        build(stages.in_pass, false)?
    };
    let layer = DeviceHandle::Metal(mint);
    let tex = layer.try_create_texture_2d(
        "aterm-gpu fetch probe target",
        TexelFormat::Rgba8Unorm,
        ROW,
        ROWS,
        TexUsage::OFFSCREEN,
        Some(TexelFormat::Rgba8UnormSrgb),
    )?;
    layer.upload_texture_full(&tex, &upload_rows(), ROW * 4, ROW, ROWS);
    let tex = tex.into_metal();
    let srgb = tex
        .alias_view(PixelFormat::Rgba8UnormSrgb)
        .ok_or_else(|| "the probe target's sRGB view failed to vend".to_owned())?;

    let uniform = layer.try_create_uniform_buffer("aterm-gpu fetch probe uniforms", 16)?;
    let mut rows = Vec::with_capacity(ROWS as usize);
    for y in 0..ROWS as u16 {
        let row = layer.try_create_instance_buffer("aterm-gpu fetch probe row", 12)?;
        // SAFETY: the buffer is freshly minted shared storage no GPU work has touched, written once before the one command buffer that reads it.
        unsafe { layer.write_buffer_from_zero(&row, &row_instance(y)) };
        rows.push(row);
    }
    let screen: [f32; 4] = [ROW as f32, ROWS as f32, 0.0, 0.0];
    let screen: Vec<u8> = screen.iter().flat_map(|v| v.to_le_bytes()).collect();
    // SAFETY: as each row's buffer above.
    unsafe { layer.write_buffer_from_zero(&uniform, &screen) };

    let passes = [
        vec![
            (&store, 0),
            (&trip, 1),
            (&blend, 2),
            (&in_pass, 2),
            (&blend, 3),
        ],
        vec![(&trip, 3)],
    ];
    let mut enc = FrameEncoder::metal(session)?;
    for draws in &passes {
        let mut pass = enc.begin_pass(
            "aterm-gpu fetch probe",
            FrameView::Metal(&srgb),
            FrameLoad::Load,
            None,
            FrameUniforms::Metal {
                buf: uniform.metal(),
                vertex_slot: 0,
                fragment_slot: None,
            },
        )?;
        for &(pso, row) in draws {
            pass.set_pipeline(FramePipeline::Metal(pso));
            pass.set_stream(FrameStream::Metal(rows[row].metal()));
            pass.draw_quads(1)?;
        }
    }
    let outcome = match enc.submit() {
        SubmittedFrame::Metal(submitted) => submitted.wait_outcome(),
        #[cfg(multi_arm)]
        _ => unreachable!("a Metal encoder commits a Metal frame"),
    };
    if outcome != super::loss::CbOutcome::Completed {
        return Err(format!("the probe's command buffer ended {outcome:?}"));
    }
    let (readback, stride) = crate::device_layer::metal_stage_readback(
        session,
        mint,
        &tex,
        ROW as usize,
        ROWS as usize,
    )?;
    // SAFETY: `readback` is shared storage of `stride * ROWS` bytes, and `metal_stage_readback` returned after the copy that wrote it completed.
    let bytes = unsafe { super::ffi::buffer_bytes(&readback, stride * ROWS as usize) };
    let mut got = Vec::with_capacity(ROW_LEN * ROWS as usize);
    for y in 0..ROWS as usize {
        got.extend_from_slice(&bytes[y * stride..y * stride + ROW_LEN]);
    }
    Ok(got)
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
