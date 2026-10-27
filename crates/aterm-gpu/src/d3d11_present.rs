// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The D3D11 arm's presents (Windows, docs/AUDIT-third-party-2026-10-15.md
//! GW.5), the twin of `gl_present.rs`: after the frame's encode, compose the
//! present copy, the bloom halo, the heat shimmer, the sub-row band shift with
//! its apron, the settings card over its scrim, then the letterbox blit and
//! the crown (the SDR one, or the EDR aurora on an f16 destination) into a
//! destination texture, in the order and with the uniforms, gates and
//! scissors the other arms use.
//!
//! The destination is the headless window's virtual target
//! (`WindowGpu::d3d11_virtual_off`), a window swap chain's buffer (RGBA8, or
//! f16 scRGB for the EDR present), or a throwaway for the readback helpers.
//! The immediate context runs commands in issue order and orders a buffer or
//! texture write after the draws already issued that read it, so every
//! resource here is resident and rewritten in place: no ring, no completion
//! probe. The taps read the destination back as soon as it is drawn.

use super::*;
use crate::d3d11::encoder::D3dFrame;
use crate::d3d11::resources::{D3dBuffer, D3dBufferRole, D3dTexture};
use crate::device_layer::TexelFormat;
use d3d11_arm::{D3dArmLive, offscreen_format};

/// The present's resident uniform blocks and glow streams on one device,
/// rewritten every present.
pub(crate) struct D3dPresentBufs {
    blit: D3dBuffer,
    bloom_cell: D3dBuffer,
    bloom: D3dBuffer,
    shimmer: D3dBuffer,
    tray: D3dBuffer,
    scrim: D3dBuffer,
    crown: D3dBuffer,
    /// The bloom extract's instances (`bloom_glow_scratch[extract_first..]`),
    /// when the frame's lights are not an item list.
    extract: Option<(D3dBuffer, usize)>,
    /// The crown's instances (`bloom_glow_scratch[..count]`), likewise.
    crown_stream: Option<(D3dBuffer, usize)>,
}

impl D3dPresentBufs {
    fn new(live: &D3dArmLive) -> Result<Self, String> {
        let u = |size: usize| live.dev.buffer(D3dBufferRole::Uniform, size);
        Ok(Self {
            blit: u(std::mem::size_of::<BlitUniform>())?,
            bloom_cell: u(std::mem::size_of::<Uniforms>())?,
            bloom: u(std::mem::size_of::<BloomUniform>())?,
            shimmer: u(std::mem::size_of::<ShimmerUniform>())?,
            tray: u(std::mem::size_of::<TrayUniform>())?,
            scrim: u(std::mem::size_of::<TrayUniform>())?,
            crown: u(std::mem::size_of::<HdrGlowUniform>())?,
            extract: None,
            crown_stream: None,
        })
    }
}

/// Write `bytes` into an instance stream, growing it geometrically.
fn upload_glow(
    live: &D3dArmLive,
    slot: &mut Option<(D3dBuffer, usize)>,
    bytes: &[u8],
) -> Result<(), String> {
    if slot.as_ref().is_none_or(|(_, cap)| *cap < bytes.len()) {
        let cap = bytes.len().next_power_of_two().max(256);
        *slot = Some((live.dev.buffer(D3dBufferRole::Instance, cap)?, cap));
    }
    let (buf, _) = slot.as_ref().expect("sized above");
    live.dev.write_buffer(buf, bytes)
}

/// Bind an extract's or a crown's lights for `row`: the item list at the
/// row's vertex slot where it reads one, else the glow instances `stream`
/// (`gl_present.rs`'s twin).
fn bind_glow_lights(
    pass: &mut crate::d3d11::encoder::D3dPass<'_, '_>,
    live: &D3dArmLive,
    row: Pipeline,
    stream: Option<&(D3dBuffer, usize)>,
) {
    match row.spec().binds.vertex_items {
        Some(unit) => {
            let items = live
                .glow_items
                .as_ref()
                .expect("written with the frame's list");
            pass.bind_vertex_texture(items, unit);
        }
        None => pass.set_stream(&stream.expect("uploaded with the frame's lights").0),
    }
}

/// A resident window texture in the offscreen's format at `(w, h)` on the
/// arm's device, re-minted when absent, resized, or from another device.
fn ensure_offscreen_family(
    live: &D3dArmLive,
    slot: &mut Option<D3dTexture>,
    (w, h): (u32, u32),
) -> Result<bool, String> {
    let fits = slot
        .as_ref()
        .is_some_and(|t| t.on(&live.dev) && (t.width, t.height) == (w, h));
    if fits {
        return Ok(false);
    }
    *slot = None;
    let (format, alias) = offscreen_format();
    *slot = Some(live.dev.texture_2d(format, w, h, alias)?);
    Ok(true)
}

/// The tray scrim's 1×1 fill on the arm's device, re-uploaded only when its
/// colour changes.
pub(crate) struct D3dScrimFill {
    tex: D3dTexture,
    rgba: [u8; 4],
}

/// Where a D3D11 present draws.
#[derive(Clone, Copy)]
pub(crate) enum D3dDest<'a> {
    /// A caller-held texture (a throwaway, a window's back buffer); an f16 one
    /// takes the EDR present (`format_plan::hdr_present_plan`).
    Texture(&'a D3dTexture),
    /// The window's resident virtual target (`WindowGpu::d3d11_virtual_off`).
    Virtual,
}

impl GpuRenderer {
    /// The D3D11 present body after the frame's encode: compose, effects,
    /// scrim and card, blit and crown into `dest` at its size, then the taps'
    /// reads. `translucent` emits the blit's alpha, premultiplied when
    /// `premult` (a DirectComposition destination). Returns whether the
    /// present copy was composed (the readback helper samples it). `Err` names
    /// the failure; the trackers stay conservative.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the present tuple every arm carries; one transcript of the shared order"
    )]
    pub(crate) fn d3d11_compose_present(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        (fw, fh): (u32, u32),
        dest: D3dDest<'_>,
        translucent: bool,
        premult: bool,
    ) -> Result<bool, String> {
        let virtual_dims = win.d3d11_virtual_off.as_ref().map(|t| (t.width, t.height));
        let (dw, dh) = match &dest {
            D3dDest::Texture(t) => (t.width, t.height),
            D3dDest::Virtual => virtual_dims.ok_or("the virtual target is absent")?,
        };
        // The present gate keys on the destination's actual format.
        let f16_dest = match &dest {
            D3dDest::Texture(t) => t.texel_format() == TexelFormat::Rgba16Float,
            D3dDest::Virtual => false,
        };
        // ---- Phase A: derivations off `self` and `win` --------------------
        let PresentUniforms {
            live_bg,
            mut want,
            run_hdr_glow,
            run_sdr_glow,
            crown_scissor,
            crown,
        } = self.present_uniforms(
            win,
            input,
            invert,
            overlay,
            source_crop,
            (fw, fh),
            (dw, dh),
            f16_dest,
            translucent,
            premult,
        );
        // The blit samples the frame through its Unorm view: the arm always
        // runs the native plan, whatever the context reported.
        want.encode_srgb = 0.0;
        let want_bloom = self.enable_bloom && fx_gpu::bloom_source_live(input);
        let want_crown = run_hdr_glow || run_sdr_glow;
        let bloom_glow = (want_bloom || want_crown).then(|| self.build_bloom_glow(input, true));
        let bloom_state = bloom_glow.filter(|(count, _, _)| want_bloom && *count > 0);
        let crown_count = if want_crown {
            bloom_glow.map_or(0, |(n, _, _)| n)
        } else {
            0
        };
        let text_blend =
            u32::from(self.cpu.text_blending() == aterm_render::TextBlending::LinearCorrected);
        let shimmer_region = if self.shimmer_live(input) {
            self.shimmer_region(input, fw, fh)
        } else {
            None
        };
        let shimmer_phase = self.shimmer_phase();
        let (cell_w, cell_h) = self.cpu.cell_size();
        let (band_y0, band_y1) = aterm_render::scroll_translate::grid_band_px(
            self.cpu.grid_top(),
            cell_h,
            input.grid_top_row,
            input.grid_bot_row,
            fh as usize,
        );
        let band_shift = band_shift_ops(band_y0, band_y1, i64::from(input.scroll_frac_px));
        let band_fx: Option<[u32; 4]> = band_shift.map(|_| [0, band_y0 as u32, fw, band_y1 as u32]);
        let tray_fx: Option<[u32; 4]> = tray.as_ref().map(|t| {
            [
                t.dx.min(fw),
                t.dy.min(fh),
                t.dx.saturating_add(t.pw).min(fw),
                t.dy.saturating_add(t.ph).min(fh),
            ]
        });
        // The scrim covers every pixel from its top row down; `None` when it
        // covers nothing.
        let scrim = tray
            .as_ref()
            .and_then(|t| t.scrim)
            .filter(|s| s.top.min(fh) < fh && fw > 0);
        let scrim_fx: Option<[u32; 4]> = scrim.map(|s| [0, s.top.min(fh), fw, fh]);
        let apron_strip = match band_shift {
            Some(_) => self.apron_strip_plan(win, input, fh as usize),
            None => None,
        };
        let use_present_off = bloom_state.is_some()
            || shimmer_region.is_some()
            || tray.is_some()
            || band_shift.is_some();

        // ---- Phase B: residency ------------------------------------------
        let live = self
            .d3d11_arm
            .as_mut()
            .ok_or("the D3D11 arm is not minted")?;
        live.ensure_pipeline(Pipeline::Blit)?;
        // The bloom's lights as its item list where the frame laid one,
        // else as glow instances; the crown reads the same.
        let (extract_row, crown_row) = match (self.bloom_items_live, run_hdr_glow) {
            (true, true) => (Pipeline::GlowItems, Pipeline::HdrGlowItems),
            (true, false) => (Pipeline::GlowItems, Pipeline::SdrGlowItems),
            (false, true) => (Pipeline::GlowAdd, Pipeline::HdrGlow),
            (false, false) => (Pipeline::GlowAdd, Pipeline::SdrGlow),
        };
        if bloom_state.is_some() {
            live.ensure_pipeline(extract_row)?;
            live.ensure_pipeline(Pipeline::Bloom)?;
        }
        if want_crown && crown_count > 0 {
            live.ensure_pipeline(crown_row)?;
        }
        if shimmer_region.is_some() {
            live.ensure_pipeline(Pipeline::Shimmer)?;
        }
        if tray.is_some() {
            live.ensure_pipeline(Pipeline::Tray)?;
        }
        if live.present.is_none() {
            live.present = Some(D3dPresentBufs::new(live)?);
        }
        let off_dims = win
            .d3d11_offscreen
            .as_ref()
            .filter(|o| o.tex.on(&live.dev))
            .map(|o| (o.w, o.h))
            .ok_or("the armed frame's D3D11 offscreen is absent")?;
        debug_assert_eq!(off_dims, (fw, fh), "the present composes the encoded frame");
        if use_present_off && ensure_offscreen_family(live, &mut win.d3d11_present_off, (fw, fh))? {
            // A fresh copy holds undefined texels: copy the whole frame.
            win.offscreen_dirty_since_sync = None;
            win.present_offscreen_fx = None;
        }
        let (bw, bh) = ((fw / BLOOM_DOWNSCALE).max(1), (fh / BLOOM_DOWNSCALE).max(1));
        if bloom_state.is_some() {
            ensure_offscreen_family(live, &mut win.d3d11_bloom_half, (bw, bh))?;
        }
        if shimmer_region.is_some() {
            ensure_offscreen_family(live, &mut win.d3d11_shimmer_scratch, (fw, fh))?;
        }
        if band_shift.is_some() {
            ensure_offscreen_family(live, &mut win.d3d11_shift_scratch, (fw, fh))?;
        }
        let apron = match apron_strip {
            Some(strip) => {
                let fresh = ensure_offscreen_family(
                    live,
                    &mut win.d3d11_apron_tex,
                    (strip.w, strip.cell_h),
                )?;
                if fresh || win.d3d11_apron_gen != win.apron_rgba_gen {
                    let tex = win.d3d11_apron_tex.as_ref().expect("ensured above");
                    live.dev.upload_rows(
                        tex,
                        0,
                        strip.cell_h,
                        &win.apron_rgba,
                        strip.w * 4,
                        strip.w,
                    )?;
                    win.d3d11_apron_gen = win.apron_rgba_gen;
                }
                Some(strip)
            }
            None => None,
        };
        if let Some(t) = tray.as_ref() {
            let unchanged = win.d3d11_tray_card.as_ref().is_some_and(|c| {
                c.tex.on(&live.dev) && c.w == t.pw && c.h == t.ph && c.pixels.as_slice() == t.rgba
            });
            if !unchanged {
                let tex = live
                    .dev
                    .texture_2d(TexelFormat::Rgba8Unorm, t.pw, t.ph, None)?;
                live.dev
                    .upload_rows(&tex, 0, t.ph, t.rgba, t.pw * 4, t.pw)?;
                self.tray_uploads += 1;
                let mut pixels = win
                    .d3d11_tray_card
                    .take()
                    .map_or_else(Vec::new, |c| c.pixels);
                pixels.clear();
                pixels.extend_from_slice(t.rgba);
                win.d3d11_tray_card = Some(d3d11_arm::D3dTrayCard {
                    tex,
                    w: t.pw,
                    h: t.ph,
                    pixels,
                });
            }
        }
        if let Some(s) = scrim {
            let fresh = live.scrim.as_ref().is_none_or(|f| !f.tex.on(&live.dev));
            if fresh {
                live.scrim = Some(D3dScrimFill {
                    tex: live.dev.texture_2d(TexelFormat::Rgba8Unorm, 1, 1, None)?,
                    rgba: s.rgba,
                });
            }
            let fill = live.scrim.as_mut().expect("minted above");
            if fresh || fill.rgba != s.rgba {
                live.dev.upload_rows(&fill.tex, 0, 1, &s.rgba, 4, 1)?;
                fill.rgba = s.rgba;
            }
        }
        // Uniforms and streams.
        let mut bufs = live.present.take().expect("minted above");
        let staged = (|| -> Result<(), String> {
            live.dev
                .write_buffer(&bufs.blit, aterm_bits::bytes_of(&want))?;
            if self.bloom_items_live && (bloom_state.is_some() || crown_count > 0) {
                d3d11_arm::write_fx_palette(
                    &live.dev,
                    &mut live.glow_items,
                    self.bloom_items.words(),
                )?;
            }
            if let Some((count, _, extract_first)) = bloom_state {
                if !self.bloom_items_live {
                    let insts =
                        &self.bloom_glow_scratch[extract_first as usize..][..count as usize];
                    upload_glow(live, &mut bufs.extract, aterm_bits::cast_slice(insts))?;
                }
                live.dev.write_buffer(
                    &bufs.bloom_cell,
                    aterm_bits::bytes_of(&Uniforms {
                        screen: [fw as f32, fh as f32],
                        text_blend: text_blend as f32,
                        _pad: 0.0,
                    }),
                )?;
                live.dev.write_buffer(
                    &bufs.bloom,
                    aterm_bits::bytes_of(&BloomUniform {
                        texel: [1.0 / bw as f32, 1.0 / bh as f32],
                        strength: page_bloom_strength(self.bloom_strength, input, self.theme.bg),
                        radius: self.bloom_radius,
                        chroma_radius: bloom_chroma_radius(self.bloom_radius),
                        extent: [1.0; 2],
                        _pad: 0.0,
                    }),
                )?;
            }
            if let Some(region) = shimmer_region.as_ref() {
                let mut heat = [[0f32; 4]; SHIMMER_BANDS / 4];
                for (i, v) in region.heat.iter().enumerate() {
                    heat[i / 4][i % 4] = *v;
                }
                live.dev.write_buffer(
                    &bufs.shimmer,
                    aterm_bits::bytes_of(&ShimmerUniform {
                        frame: [fw as f32, fh as f32],
                        region_min: [region.x0 as f32, region.y0 as f32],
                        region_max: [region.x1 as f32, region.y1 as f32],
                        hot_top: region.hot_top,
                        rise: region.rise,
                        amp: (cell_h as f32 / 18.0).clamp(0.75, SHIMMER_AMP_PX),
                        period: (cell_h as f32).max(4.0),
                        phase: shimmer_phase,
                        band_x0: region.band_x0,
                        band_w: region.band_w,
                        rolloff: (cell_w as f32).max(1.0),
                        _pad: [0.0; 2],
                        heat,
                    }),
                )?;
            }
            if let Some(t) = tray.as_ref() {
                live.dev.write_buffer(
                    &bufs.tray,
                    aterm_bits::bytes_of(&TrayUniform {
                        rect: [t.dx as f32, t.dy as f32, t.pw as f32, t.ph as f32],
                        fb: [fw as f32, fh as f32],
                        _pad: [0.0, 0.0],
                    }),
                )?;
            }
            if let Some([_, top, _, _]) = scrim_fx {
                live.dev.write_buffer(
                    &bufs.scrim,
                    aterm_bits::bytes_of(&TrayUniform {
                        rect: [0.0, top as f32, fw as f32, (fh - top) as f32],
                        fb: [fw as f32, fh as f32],
                        _pad: [0.0, 0.0],
                    }),
                )?;
            }
            if let Some(hu) = crown.filter(|_| crown_count > 0) {
                live.dev
                    .write_buffer(&bufs.crown, aterm_bits::bytes_of(&hu))?;
                if !self.bloom_items_live {
                    let insts = &self.bloom_glow_scratch[..crown_count as usize];
                    upload_glow(live, &mut bufs.crown_stream, aterm_bits::cast_slice(insts))?;
                }
            }
            Ok(())
        })();
        live.present = Some(bufs);
        staged?;

        // The bloom composite scissor (`encode_bloom_halo`'s spelling).
        let bloom_scissor = bloom_state.and_then(|(_, bbox, _)| {
            let clip = input.fx_clip.map(|(cx0, cy0, cx1, cy1)| {
                [
                    u32::from(cx0),
                    u32::from(cy0),
                    u32::from(cx1),
                    u32::from(cy1),
                ]
            });
            let scissor = if let Some([x0, y0, x1, y1]) = bbox {
                let d =
                    ((2.0 * self.bloom_radius + 1.0) * BLOOM_DOWNSCALE as f32).ceil() as u32 + 1;
                Some([
                    x0.saturating_sub(d),
                    y0.saturating_sub(d),
                    (x1 + d).min(fw),
                    (y1 + d).min(fh),
                ])
            } else {
                clip.map(|_| [0, 0, fw, fh])
            };
            match (scissor, clip) {
                (Some([sx0, sy0, sx1, sy1]), Some([cx0, cy0, cx1, cy1])) => Some([
                    sx0.max(cx0),
                    sy0.max(cy0),
                    sx1.min(cx1).min(fw),
                    sy1.min(cy1).min(fh),
                ]),
                (s, None) => s,
                (None, Some(_)) => unreachable!("clip-only case handled above"),
            }
            .filter(|[sx0, sy0, sx1, sy1]| sx1 > sx0 && sy1 > sy0)
        });
        let copy_rect = present_copy_rect(
            win.offscreen_dirty_since_sync,
            win.present_offscreen_fx,
            (fw, fh),
        );

        // ---- Phase C: the encode -----------------------------------------
        let live = self.d3d11_arm.as_ref().expect("checked above");
        let bufs = live.present.as_ref().expect("restored above");
        let pipe = |row: Pipeline| {
            live.pipeline(row)
                .unwrap_or_else(|| panic!("d3d11 present: {row:?} ensured above"))
        };
        let nearest = live.nearest();
        let linear = live.linear();
        let off = &win.d3d11_offscreen.as_ref().expect("checked above").tex;
        let present_off = use_present_off.then(|| win.d3d11_present_off.as_ref().expect("ensured"));
        let composed = present_off.unwrap_or(off);
        let dest_tex = match dest {
            D3dDest::Texture(t) => t,
            D3dDest::Virtual => win.d3d11_virtual_off.as_ref().expect("dims read above"),
        };
        let mut frame = D3dFrame::begin(&live.dev)?;
        // 1. Compose: the clean offscreen's stale rect onto the present copy.
        if let Some(copy) = present_off {
            let [x0, y0, x1, y1] = copy_rect;
            if x1 > x0 && y1 > y0 {
                frame.copy_rect(off, (x0, y0), copy, (x0, y0), x1 - x0, y1 - y0)?;
            }
        }
        // 2. Bloom: extract into the cleared half target, then the screen
        //    composite over the copy.
        if let Some((count, _, _)) = bloom_state {
            let half = win.d3d11_bloom_half.as_ref().expect("ensured above");
            {
                let mut pass = frame.begin_pass(
                    half,
                    false,
                    crate::device_layer::FrameLoad::Clear(crate::device_layer::ClearColor4 {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    }),
                    None,
                    (
                        &bufs.bloom_cell,
                        extract_row
                            .spec()
                            .binds
                            .vertex_uniform
                            .expect("the extract rows have a vertex uniform"),
                    ),
                )?;
                pass.set_pipeline(pipe(extract_row));
                bind_glow_lights(&mut pass, live, extract_row, bufs.extract.as_ref());
                pass.draw(6, count)?;
            }
            let binds = Pipeline::Bloom.spec().binds;
            let mut pass = frame.begin_pass(
                composed,
                false,
                crate::device_layer::FrameLoad::Load,
                bloom_scissor.map(|[x0, y0, x1, y1]| (x0, y0, x1 - x0, y1 - y0)),
                (&bufs.bloom, binds.fragment_buffers[0]),
            )?;
            pass.set_pipeline(pipe(Pipeline::Bloom));
            pass.bind_texture(half, linear, binds.fragment_textures[0], true);
            pass.draw(3, 1)?;
        }
        // 3. Shimmer: stage the sample rect, then the scissored refract.
        if let Some(region) = shimmer_region.as_ref() {
            let scratch = win.d3d11_shimmer_scratch.as_ref().expect("ensured above");
            let m = SHIMMER_COPY_MARGIN;
            let [sx0, sy0, sx1, sy1] = [
                region.x0.saturating_sub(m),
                region.y0.saturating_sub(m),
                region.x1.saturating_add(m).min(fw),
                region.y1.saturating_add(m).min(fh),
            ];
            frame.copy_rect(
                composed,
                (sx0, sy0),
                scratch,
                (sx0, sy0),
                sx1 - sx0,
                sy1 - sy0,
            )?;
            let binds = Pipeline::Shimmer.spec().binds;
            let mut pass = frame.begin_pass(
                composed,
                false,
                crate::device_layer::FrameLoad::Load,
                Some((
                    region.x0,
                    region.y0,
                    region.x1 - region.x0,
                    region.y1 - region.y0,
                )),
                (&bufs.shimmer, binds.fragment_buffers[0]),
            )?;
            pass.set_pipeline(pipe(Pipeline::Shimmer));
            pass.bind_texture(scratch, linear, binds.fragment_textures[0], true);
            pass.draw(3, 1)?;
        }
        // 3b. The sub-row band shift over the copy, never the offscreen.
        if let Some(shift) = band_shift {
            let copy = present_off.ok_or("the band shift needs the present copy")?;
            let scratch = win.d3d11_shift_scratch.as_ref().expect("ensured above");
            frame.copy_rect(copy, (0, shift.src_y), scratch, (0, 0), fw, shift.moved)?;
            frame.copy_rect(scratch, (0, 0), copy, (0, shift.dst_y), fw, shift.moved)?;
        }
        // 3c. The incoming row over the exposed strip.
        if let Some(strip) = apron {
            let copy = present_off.ok_or("the apron strip needs the present copy")?;
            let tex = win.d3d11_apron_tex.as_ref().expect("ensured above");
            let w = fw.min(tex.width);
            let rows = strip
                .rows
                .min(tex.height)
                .min(fh.saturating_sub(strip.dst_y));
            if w > 0 && rows > 0 {
                frame.copy_rect(tex, (0, 0), copy, (0, strip.dst_y), w, rows)?;
            }
        }
        // 4. The scrim, then the card over it, on the finished copy.
        if tray.is_some() {
            let card = win.d3d11_tray_card.as_ref().expect("ensured above");
            let binds = Pipeline::Tray.spec().binds;
            let binding = binds.vertex_uniform.expect("row 17 binds at 2");
            let fill = scrim.and(live.scrim.as_ref());
            let mut pass = frame.begin_pass(
                composed,
                false,
                crate::device_layer::FrameLoad::Load,
                None,
                (
                    if fill.is_some() {
                        &bufs.scrim
                    } else {
                        &bufs.tray
                    },
                    binding,
                ),
            )?;
            pass.set_pipeline(pipe(Pipeline::Tray));
            if let Some(fill) = fill {
                pass.bind_texture(&fill.tex, linear, binds.fragment_textures[0], false);
                pass.draw(4, 1)?;
                pass.bind_uniform(&bufs.tray, binding);
            }
            pass.bind_texture(&card.tex, linear, binds.fragment_textures[0], false);
            pass.draw(4, 1)?;
        }
        // 5. The blit pass: clear to the live background, the letterbox blit
        //    over the whole destination, then the crown in the same pass.
        {
            let binds = Pipeline::Blit.spec().binds;
            let clear = crate::device_layer::ClearColor4 {
                r: f64::from((live_bg >> 16) & 0xff) / 255.0,
                g: f64::from((live_bg >> 8) & 0xff) / 255.0,
                b: f64::from(live_bg & 0xff) / 255.0,
                a: 1.0,
            };
            let mut pass = frame.begin_pass(
                dest_tex,
                false,
                crate::device_layer::FrameLoad::Clear(clear),
                None,
                (&bufs.blit, binds.fragment_buffers[0]),
            )?;
            pass.set_pipeline(pipe(Pipeline::Blit));
            pass.bind_texture(composed, nearest, binds.fragment_textures[0], true);
            pass.draw(3, 1)?;
            // The crown: the EDR aurora above reference white on an f16
            // destination, else the SDR one (the gate makes them exclusive),
            // over the bloom's item list or its glow instances.
            if want_crown
                && crown_count > 0
                && let Some(scissor) = crown_scissor
                && (self.bloom_items_live || bufs.crown_stream.is_some())
            {
                let crown_binds = crown_row.spec().binds;
                pass.bind_uniform(
                    &bufs.crown,
                    crown_binds
                        .vertex_uniform
                        .expect("the crown rows bind a vertex uniform"),
                );
                pass.set_scissor(Some(scissor));
                pass.set_pipeline(pipe(crown_row));
                bind_glow_lights(&mut pass, live, crown_row, bufs.crown_stream.as_ref());
                pass.draw(6, crown_count)?;
            }
        }
        frame.submit()?;
        // Bookkeeping after the encode: the copy now differs from the
        // offscreen only where this present wrote over it.
        if use_present_off {
            let mut fx: Option<[u32; 4]> = Some([0, 0, 0, 0]);
            fx = union_rect_opt(fx, bloom_scissor, (fw, fh));
            if let Some(region) = shimmer_region.as_ref() {
                fx = union_rect_opt(
                    fx,
                    Some([region.x0, region.y0, region.x1, region.y1]),
                    (fw, fh),
                );
            }
            for footprint in [band_fx, tray_fx, scrim_fx].into_iter().flatten() {
                fx = union_rect_opt(fx, Some(footprint), (fw, fh));
            }
            win.offscreen_dirty_since_sync = Some([0, 0, 0, 0]);
            win.present_offscreen_fx = fx;
        }
        // The taps read the drawn destination back now.
        if win.video.is_some() || win.presented_snapshot.is_some() {
            let capture_color_space = win.capture_color_space();
            let capture_sdr_white_scale = win.sdr_white_scale();
            let dest_tex = match dest {
                D3dDest::Texture(t) => t,
                D3dDest::Virtual => win.d3d11_virtual_off.as_ref().expect("dims read above"),
            };
            // RGBA8, or an EDR destination's f16, rows padded as the taps'.
            let format = dest_tex.texel_format();
            let padded = (dw as usize * dest_tex.format.bytes as usize).div_ceil(256) * 256;
            let pixels = live.dev.read_texels(dest_tex, dw, dh, padded)?;
            if let Some(tap) = win.video.as_mut() {
                tap.gl_enqueue_frame(
                    (dw, dh),
                    format,
                    &pixels,
                    capture_color_space,
                    capture_sdr_white_scale,
                );
            }
            if let Some(tap) = win.presented_snapshot.as_mut() {
                tap.gl_enqueue_frame(
                    (dw, dh),
                    format,
                    &pixels,
                    capture_color_space,
                    capture_sdr_white_scale,
                );
            }
        }
        Ok(use_present_off)
    }

    /// The armed headless present: encode on D3D11, compose into the resident
    /// virtual target at the destination size (opaque, as every arm's virtual
    /// present is). Its bytes are what the tap ring and
    /// `virtual_presented_snapshot_current` read.
    #[allow(
        clippy::too_many_arguments,
        reason = "the shared virtual-present seam's tuple"
    )]
    pub(crate) fn d3d11_present_virtual_with_crop(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        effects_transport_shift_y: i32,
        destination: (u32, u32),
    ) -> Result<(), String> {
        let source_crop = self
            .resident_present_crop(input, source_crop)
            .map_err(|()| "virtual present crop outside the resident frame")?;
        let w = self.clamp_fb_dim(destination.0.max(1));
        let h = self.clamp_fb_dim(destination.1.max(1));
        let work_started = aterm_time::Instant::now();
        let (fw, fh) = self.encode_present_frame(win, input);
        if !self.last_frame_arm_d3d11 {
            return Err("the frame did not draw on the D3D11 arm".to_owned());
        }
        win.resident_input_effects_transport_shift_y = effects_transport_shift_y;
        if tray.is_none() {
            win.d3d11_tray_card = None;
        }
        let live = self.d3d11_arm.as_ref().ok_or("the D3D11 arm is not live")?;
        let fits = win
            .d3d11_virtual_off
            .as_ref()
            .is_some_and(|t| t.on(&live.dev) && (t.width, t.height) == (w, h));
        if !fits {
            win.d3d11_virtual_off = None;
            win.d3d11_virtual_off =
                Some(live.dev.texture_2d(TexelFormat::Rgba8Unorm, w, h, None)?);
            win.virtual_presented_input_epoch = 0;
        }
        self.d3d11_compose_present(
            win,
            input,
            invert,
            overlay,
            tray,
            source_crop,
            (fw, fh),
            D3dDest::Virtual,
            false,
            false,
        )?;
        win.virtual_presented_input_epoch = win.resident_input_epoch;
        win.last_present_work_ns =
            u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        win.last_present_encode_ns = win.last_present_work_ns;
        self.d3d11_maybe_inject_loss();
        Ok(())
    }

    /// The injected-loss drill's fire site (`d3d11_inject_loss_after`):
    /// counts successful D3D11 presents, and at zero arms the device so its
    /// next frame answers as a removed device does. Announces itself on
    /// stderr.
    pub(crate) fn d3d11_maybe_inject_loss(&mut self) {
        let Some(n) = self.d3d11_inject_loss_after.as_mut() else {
            return;
        };
        if *n > 1 {
            *n -= 1;
            return;
        }
        self.d3d11_inject_loss_after = None;
        let Some(live) = self.d3d11_arm.as_ref() else {
            return;
        };
        live.dev.shared().inject_device_loss();
        crate::stderr_line!(
            "aterm-gpu: ATERM_D3D11_INJECT_LOSS fired; the D3D11 device's next frame reports \
             it removed (drill lever)"
        );
    }

    /// Fire the injected-loss drill after `n` more successful D3D11 presents,
    /// as `ATERM_D3D11_INJECT_LOSS=<n>` does at construct.
    #[cfg(test)]
    pub(crate) fn d3d11_inject_loss_after_for_test(&mut self, n: u64) {
        self.d3d11_inject_loss_after = Some(n).filter(|n| *n > 0);
    }

    /// The armed present helper's readback (`present_input_readback`): a plain
    /// frame reads the offscreen; an effect frame composes into a throwaway
    /// destination and reads the present copy the blit sampled.
    pub(crate) fn d3d11_present_readback(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        (w, h): (u32, u32),
    ) -> Result<Frame, String> {
        let effect_frame =
            (self.enable_bloom && fx_gpu::bloom_source_live(input)) || self.shimmer_live(input);
        if !effect_frame {
            return self.d3d11_read_back_offscreen(win, w, h);
        }
        let live = self.d3d11_arm.as_ref().ok_or("the D3D11 arm is not live")?;
        let scratch = live.dev.texture_2d(TexelFormat::Rgba8Unorm, w, h, None)?;
        let composed = self.d3d11_compose_present(
            win,
            input,
            false,
            None,
            None,
            None,
            (w, h),
            D3dDest::Texture(&scratch),
            false,
            false,
        )?;
        let live = self.d3d11_arm.as_ref().ok_or("the D3D11 arm is not live")?;
        let tex = if composed {
            win.d3d11_present_off.as_ref().ok_or("no present copy")?
        } else {
            &win.d3d11_offscreen
                .as_ref()
                .ok_or("no D3D11 offscreen")?
                .tex
        };
        crate::device_layer::d3d11_try_read_back(&live.dev, tex, w, h)
    }

    /// The EDR present's test seam on the D3D11 arm
    /// ([`GpuRenderer::present_hdr_sized_for_test`]): encode, then the real
    /// compose into an `Rgba16Float` destination `extra_w` x `extra_h` px
    /// larger than the frame, read back and decoded to linear f32. Without
    /// `boost_pass` the window's panel is taken as SDR for this present
    /// (`edr_max` 1.0, no headroom), which the gate answers with no aurora
    /// pass.
    pub(crate) fn d3d11_present_hdr_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        boost_pass: bool,
        (extra_w, extra_h): (u32, u32),
    ) -> Result<(Vec<f32>, u32, u32), String> {
        let (fw, fh) = self.encode_present_frame(win, input);
        if !self.last_frame_arm_d3d11 {
            return Err("the frame did not draw on the D3D11 arm".to_owned());
        }
        let (w, h) = (fw + extra_w, fh + extra_h);
        let live = self.d3d11_arm.as_ref().ok_or("the D3D11 arm is not live")?;
        let dst = live.dev.texture_2d(TexelFormat::Rgba16Float, w, h, None)?;
        let edr_max = win.edr_max;
        if !boost_pass {
            win.edr_max = 1.0;
        }
        let composed = self.d3d11_compose_present(
            win,
            input,
            false,
            None,
            None,
            None,
            (fw, fh),
            D3dDest::Texture(&dst),
            false,
            false,
        );
        win.edr_max = edr_max;
        composed?;
        let live = self.d3d11_arm.as_ref().ok_or("the D3D11 arm is not live")?;
        let row = w as usize * 8;
        let padded = row.div_ceil(256) * 256;
        let bytes = live.dev.read_texels(&dst, w, h, padded)?;
        let mut out = Vec::with_capacity(w as usize * h as usize * 4);
        for line in bytes.chunks_exact(padded) {
            out.extend(
                line[..row]
                    .chunks_exact(2)
                    .map(|b| f16_bits_to_f32(u16::from_le_bytes([b[0], b[1]]))),
            );
        }
        Ok((out, w, h))
    }

    /// The resident virtual destination's bytes, as read back, for
    /// `virtual_presented_snapshot_current` (the caller checked the epoch).
    pub(crate) fn d3d11_virtual_snapshot(
        &self,
        win: &WindowGpu,
        t_us: u64,
    ) -> Result<crate::video_tap::RawCapture, String> {
        let live = self.d3d11_arm.as_ref().ok_or("the D3D11 arm is not live")?;
        let tex = win
            .d3d11_virtual_off
            .as_ref()
            .filter(|t| t.on(&live.dev))
            .ok_or("presented snapshot: no virtual destination has been presented")?;
        let (w, h) = (tex.width, tex.height);
        let rgba = live.dev.read_rgba8(tex, w, h, w as usize * 4)?;
        crate::video_tap::RawCapture::rgba8_srgb(rgba, w, h, t_us)
    }
}
