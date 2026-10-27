// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! OpenGL present composition, post damage tracking, and window surface attachment.

use super::*;
use crate::device_layer::TexelFormat;
use crate::gl::encoder::GlFrame;
use crate::gl::resources::{GlBuffer, GlBufferRole, GlTexture};
use gl_arm::gl_arm_note;
use gl_arm::{GlArmLive, offscreen_format};

/// The present's resident uniform blocks and glow streams on one device, rewritten every present.
pub(crate) struct GlPresentBufs {
    blit: GlBuffer,
    bloom_cell: GlBuffer,
    bloom: GlBuffer,
    shimmer: GlBuffer,
    tray: GlBuffer,
    crown: GlBuffer,
    /// The bloom extract's instances (`bloom_glow_scratch[extract_first..]`).
    extract: Option<(GlBuffer, usize)>,
    /// The crown's instances (`bloom_glow_scratch[..count]`).
    crown_stream: Option<(GlBuffer, usize)>,
}

impl GlPresentBufs {
    fn new(live: &GlArmLive) -> Result<Self, String> {
        let u = |size: usize| live.dev.buffer(GlBufferRole::Uniform, size);
        Ok(Self {
            blit: u(std::mem::size_of::<BlitUniform>())?,
            bloom_cell: u(std::mem::size_of::<Uniforms>())?,
            bloom: u(std::mem::size_of::<BloomUniform>())?,
            shimmer: u(std::mem::size_of::<ShimmerUniform>())?,
            tray: u(std::mem::size_of::<TrayUniform>())?,
            crown: u(std::mem::size_of::<HdrGlowUniform>())?,
            extract: None,
            crown_stream: None,
        })
    }
}

/// What a window's post composed beyond the frame's own texels: the blit that placed the frame on
/// the stand-in, the sizes, and what drew over the frame.
#[derive(Clone, Copy)]
pub(crate) struct GlPosted {
    /// The blit's uniform: placement (`content_off`), crop, invert, overlay,
    /// bands.
    blit: BlitUniform,
    /// The frame's size, and the destination's.
    frame: (u32, u32),
    dest: (u32, u32),
    /// What the effects other than the bloom and the tray card drew over the
    /// present copy the blit sampled, frame pixels: `Some([0; 4])` nothing
    /// (the blit sampled the clean offscreen), `None` unbounded.
    fx: Option<[u32; 4]>,
    /// The bloom's scissor, frame pixels, as `fx`: `Some([0; 4])` where no
    /// bloom drew.
    bloom: Option<[u32; 4]>,
    /// The tray card drawn: its rect, frame pixels, and its upload
    /// (`GlTrayCard::serial`).
    tray: Option<([u32; 4], u64)>,
    /// The crown's scissor in the blit pass, destination pixels; `[0; 4]`
    /// where no crown drew.
    crown: [u32; 4],
}

/// The stand-in's damage for a post that composed `now` after one that
/// composed `last`, the offscreen written meanwhile over `dirty`
/// (`WindowGpu::offscreen_dirty_since_post`): `[x0, y0, x1, y1]` in
/// destination pixels, or `None` for all of it. All of it where nothing was
/// posted or the blit or a size moved; otherwise what changed in the frame
/// (`dirty`, with the effects over the copy at the last post and at this
/// one) carried through the blit, with both crowns. A bloom held (drawn from
/// the same inputs at both posts, `bloom_held`) adds nothing: it blends the
/// same glow over each texel, so only the texels written show anew under it.
fn post_damage(
    last: Option<&GlPosted>,
    now: &GlPosted,
    dirty: Option<[u32; 4]>,
    bloom_held: bool,
) -> Option<[u32; 4]> {
    let last = last?;
    let same_blit = aterm_bits::bytes_of(&last.blit) == aterm_bits::bytes_of(&now.blit);
    if !same_blit || last.frame != now.frame || last.dest != now.dest {
        return None;
    }
    let both = |held: bool, a: Option<[u32; 4]>, b: Option<[u32; 4]>| {
        if held {
            Some([0, 0, 0, 0])
        } else {
            union_rect_opt(a, b, now.frame)
        }
    };
    let card = |t: Option<([u32; 4], u64)>| Some(t.map_or([0, 0, 0, 0], |(rect, _)| rect));
    let bloom = both(bloom_held, last.bloom, now.bloom);
    let tray = both(last.tray == now.tray, card(last.tray), card(now.tray));
    let changed = [last.fx, now.fx, bloom, tray]
        .into_iter()
        .try_fold(dirty?, |acc, r| union_rect_opt(Some(acc), r, now.frame))?;
    let placed = blit_footprint(&now.blit, changed, now.dest);
    union_rect_opt(
        union_rect_opt(Some(placed), Some(last.crown), now.dest),
        Some(now.crown),
        now.dest,
    )
}

/// Where a change to `rect` of the frame (frame pixels, half-open) shows on
/// a `dw` x `dh` destination through `blit` (`fs_blit`): frame texel `p` is
/// the destination pixel whose centre less `content_off` floors to it, inside
/// `floor` and `ceil` of `rect + content_off`; beside a host-chrome row the
/// bands repeat the frame's edge texels, so with chrome the change reaches
/// the whole width. A placement that is not a number reaches everything.
fn blit_footprint(
    blit: &BlitUniform,
    [x0, y0, x1, y1]: [u32; 4],
    (dw, dh): (u32, u32),
) -> [u32; 4] {
    let [ox, oy] = blit.content_off;
    if !ox.is_finite() || !oy.is_finite() {
        return [0, 0, dw, dh];
    }
    if x0 >= x1 || y0 >= y1 {
        return [0, 0, 0, 0];
    }
    let place = |v: u32, o: f32, max: u32, round: fn(f64) -> f64| {
        round(f64::from(v) + f64::from(o)).clamp(0.0, f64::from(max)) as u32
    };
    let (dx0, dx1) = if blit.chrome_y1 > 0.0 {
        (0, dw)
    } else {
        (place(x0, ox, dw, f64::floor), place(x1, ox, dw, f64::ceil))
    };
    [
        dx0,
        place(y0, oy, dh, f64::floor),
        dx1,
        place(y1, oy, dh, f64::ceil),
    ]
}

/// The union of `quads`' rects that draw, offscreen px.
fn glow_footprint(quads: &[GlowInstance]) -> Option<[u32; 4]> {
    quads
        .iter()
        .map(|q| q.rect.map(u32::from))
        .filter(|[_, _, qw, qh]| *qw > 0 && *qh > 0)
        .map(|[qx, qy, qw, qh]| [qx, qy, qx + qw, qy + qh])
        .reduce(|a, b| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        })
}

/// Bind an extract's or a crown's lights for `row`: the item list at the
/// row's vertex unit where it reads one, else the glow instances `stream`.
fn bind_glow_lights(
    pass: &mut crate::gl::encoder::GlPass<'_, '_>,
    live: &GlArmLive,
    row: Pipeline,
    stream: Option<&(GlBuffer, usize)>,
) {
    match row.spec().binds.vertex_items {
        Some(unit) => {
            let items = live
                .glow_items
                .as_ref()
                .expect("written with the frame's list");
            pass.bind_texture(items, live.nearest(), unit, true);
        }
        None => pass.set_stream(&stream.expect("uploaded with the frame's lights").0),
    }
}

/// The crown's drawn footprint in destination px: `lit` (offscreen px, the
/// union of its lights) placed at `[ox, oy]` and clipped to the scissor.
fn crown_footprint(
    lit: Option<[u32; 4]>,
    [ox, oy]: [f32; 2],
    (x, y, w, h): (u32, u32, u32, u32),
) -> [u32; 4] {
    let scissor = [x, y, x.saturating_add(w), y.saturating_add(h)];
    if !ox.is_finite() || !oy.is_finite() {
        return scissor;
    }
    let Some([x0, y0, x1, y1]) = lit else {
        return [0, 0, 0, 0];
    };
    let place =
        |v: u32, o: f32, round: fn(f64) -> f64| round(f64::from(v) + f64::from(o)).max(0.0) as u32;
    let r = [
        place(x0, ox, f64::floor).max(scissor[0]),
        place(y0, oy, f64::floor).max(scissor[1]),
        place(x1, ox, f64::ceil).min(scissor[2]),
        place(y1, oy, f64::ceil).min(scissor[3]),
    ];
    if r[0] < r[2] && r[1] < r[3] {
        r
    } else {
        [0, 0, 0, 0]
    }
}

fn upload_glow(
    live: &GlArmLive,
    slot: &mut Option<(GlBuffer, usize)>,
    bytes: &[u8],
) -> Result<(), String> {
    if slot.as_ref().is_none_or(|(_, cap)| *cap < bytes.len()) {
        let cap = bytes.len().next_power_of_two().max(256);
        *slot = Some((live.dev.buffer(GlBufferRole::Instance, cap)?, cap));
    }
    let (buf, _) = slot.as_ref().expect("sized above");
    live.dev.write_buffer(buf, bytes)
}

fn ensure_offscreen_family(
    live: &GlArmLive,
    slot: &mut Option<GlTexture>,
    (w, h): (u32, u32),
) -> Result<bool, String> {
    let fits = slot
        .as_ref()
        .is_some_and(|t| t.on(&live.dev) && (t.width, t.height) == (w, h));
    if fits {
        return Ok(false);
    }
    if let Some(old) = slot.take() {
        old.retire();
    }
    let (format, alias) = offscreen_format(&live.dev);
    *slot = Some(live.dev.texture_2d(format, w, h, alias)?);
    Ok(true)
}

/// Where a GL present draws.
#[derive(Clone, Copy)]
pub(crate) enum GlDest<'a> {
    /// A caller-held texture (the window stand-in, a throwaway).
    Texture(&'a GlTexture),
    /// The window's resident virtual target (`WindowGpu::gl_virtual_off`).
    Virtual,
}

impl GpuRenderer {
    /// The GL present body after the frame's encode: compose, effects, card, blit and crown into
    /// `dest` at its size, then the taps' copies.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the present tuple every arm carries; one transcript of the shared order"
    )]
    pub(crate) fn gl_compose_present(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        (fw, fh): (u32, u32),
        dest: GlDest<'_>,
        translucent: bool,
    ) -> Result<(bool, GlPosted), String> {
        let virtual_dims = win.gl_virtual_off.as_ref().map(|t| (t.width, t.height));
        let (dw, dh) = match &dest {
            GlDest::Texture(t) => (t.width, t.height),
            GlDest::Virtual => virtual_dims.ok_or("the virtual target is absent")?,
        };
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
            false,
            translucent,
            false,
        );
        let live = self.gl_arm.as_ref().ok_or("the GL arm is not minted")?;
        let raw = live.dev.caps().srgb_views;
        want.encode_srgb = if raw { 0.0 } else { 1.0 };
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
        let apron_strip = match band_shift {
            Some(_) => self.apron_strip_plan(win, input, fh as usize),
            None => None,
        };
        let use_present_off = bloom_state.is_some()
            || shimmer_region.is_some()
            || tray.is_some()
            || band_shift.is_some();

        let live = self.gl_arm.as_mut().ok_or("the GL arm is not minted")?;
        for row in [Pipeline::Blit, Pipeline::SdrGlow] {
            live.ensure_pipeline(row)?;
        }
        let (extract_row, crown_row) = if self.bloom_items_live {
            (Pipeline::GlowItems, Pipeline::SdrGlowItems)
        } else {
            (Pipeline::GlowAdd, Pipeline::SdrGlow)
        };
        if bloom_state.is_some() {
            live.ensure_pipeline(extract_row)?;
            live.ensure_pipeline(Pipeline::Bloom)?;
        }
        if run_sdr_glow && crown_count > 0 {
            live.ensure_pipeline(crown_row)?;
        }
        if shimmer_region.is_some() {
            live.ensure_pipeline(Pipeline::Shimmer)?;
        }
        if tray.is_some() {
            live.ensure_pipeline(Pipeline::Tray)?;
        }
        if run_hdr_glow {
            return Err("an EDR crown on a GL destination".to_owned());
        }
        if live.present.is_none() {
            live.present = Some(GlPresentBufs::new(live)?);
        }
        let off_dims = win
            .gl_offscreen
            .as_ref()
            .filter(|o| o.tex.on(&live.dev))
            .map(|o| (o.w, o.h))
            .ok_or("the armed frame's GL offscreen is absent")?;
        debug_assert_eq!(off_dims, (fw, fh), "the present composes the encoded frame");
        let (format, alias) = offscreen_format(&live.dev);
        if use_present_off
            && live
                .dev
                .texture_2d_resizing(&mut win.gl_present_off, format, fw, fh, alias)?
        {
            win.offscreen_dirty_since_sync = None;
            win.present_offscreen_fx = None;
        }
        let (bw, bh) = ((fw / BLOOM_DOWNSCALE).max(1), (fh / BLOOM_DOWNSCALE).max(1));
        if bloom_state.is_some() {
            // Roomy, so a resize keeps its storage: minting and freeing one each step of a drag
            // cost milliseconds before the window manager saw the frame (`bloom.glsl` reads the
            // texels in use through `BloomUniform::extent`).
            live.dev
                .texture_2d_resizing(&mut win.gl_bloom_half, format, bw, bh, alias)?;
        }
        if shimmer_region.is_some() {
            ensure_offscreen_family(live, &mut win.gl_shimmer_scratch, (fw, fh))?;
        }
        if band_shift.is_some() {
            ensure_offscreen_family(live, &mut win.gl_shift_scratch, (fw, fh))?;
        }
        let apron = match apron_strip {
            Some(strip) => {
                let fresh =
                    ensure_offscreen_family(live, &mut win.gl_apron_tex, (strip.w, strip.cell_h))?;
                if fresh || win.gl_apron_gen != win.apron_rgba_gen {
                    let tex = win.gl_apron_tex.as_ref().expect("ensured above");
                    live.dev.upload_rows(
                        tex,
                        0,
                        strip.cell_h,
                        &win.apron_rgba,
                        strip.w * 4,
                        strip.w,
                    )?;
                    win.gl_apron_gen = win.apron_rgba_gen;
                }
                Some(strip)
            }
            None => None,
        };
        if let Some(t) = tray.as_ref() {
            debug_assert!(
                t.scrim.is_none(),
                "the GL arm draws no tray scrim (`TrayQuad::scrim`): Linux shows none"
            );
            let unchanged = win.gl_tray_card.as_ref().is_some_and(|c| {
                c.tex.on(&live.dev) && c.w == t.pw && c.h == t.ph && c.pixels.as_slice() == t.rgba
            });
            if !unchanged {
                let tex = live
                    .dev
                    .texture_2d(TexelFormat::Rgba8Unorm, t.pw, t.ph, None)?;
                live.dev
                    .upload_rows(&tex, 0, t.ph, t.rgba, t.pw * 4, t.pw)?;
                self.tray_uploads += 1;
                let mut pixels = win.gl_tray_card.take().map_or_else(Vec::new, |c| c.pixels);
                pixels.clear();
                pixels.extend_from_slice(t.rgba);
                win.gl_tray_card = Some(gl_arm::GlTrayCard {
                    tex,
                    w: t.pw,
                    h: t.ph,
                    pixels,
                    serial: self.tray_uploads,
                });
            }
        }
        let bloom_uniforms = bloom_state.map(|_| {
            (
                Uniforms {
                    screen: [fw as f32, fh as f32],
                    text_blend: text_blend as f32,
                    _pad: 0.0,
                },
                BloomUniform {
                    texel: [1.0 / bw as f32, 1.0 / bh as f32],
                    strength: page_bloom_strength(self.bloom_strength, input, self.theme.bg),
                    radius: self.bloom_radius,
                    chroma_radius: bloom_chroma_radius(self.bloom_radius),
                    extent: win
                        .gl_bloom_half
                        .as_ref()
                        .map_or([1.0; 2], crate::gl::resources::GlTexture::extent),
                    _pad: 0.0,
                },
            )
        });
        let mut bufs = live.present.take().expect("minted above");
        let staged = (|| -> Result<(), String> {
            live.dev
                .write_buffer(&bufs.blit, aterm_bits::bytes_of(&want))?;
            if self.bloom_items_live && (bloom_state.is_some() || crown_count > 0) {
                gl_arm::write_fx_palette(
                    &live.dev,
                    &mut live.glow_items,
                    self.bloom_items.words(),
                )?;
            }
            if let (Some((count, _, extract_first)), Some((cell, bloom))) =
                (bloom_state, bloom_uniforms.as_ref())
            {
                if !self.bloom_items_live {
                    let insts =
                        &self.bloom_glow_scratch[extract_first as usize..][..count as usize];
                    upload_glow(live, &mut bufs.extract, aterm_bits::cast_slice(insts))?;
                }
                live.dev
                    .write_buffer(&bufs.bloom_cell, aterm_bits::bytes_of(cell))?;
                live.dev
                    .write_buffer(&bufs.bloom, aterm_bits::bytes_of(bloom))?;
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
        win.gl_bloom_inputs.clear();
        if let (Some((count, _, extract_first)), Some((cell, bloom))) =
            (bloom_state, bloom_uniforms.as_ref())
        {
            let inputs = &mut win.gl_bloom_inputs;
            // The extract's lights: its item list (tagged) or its glow instances.
            if self.bloom_items_live {
                inputs.push(1);
                inputs.extend_from_slice(aterm_bits::cast_slice(self.bloom_items.words()));
            } else {
                let insts = &self.bloom_glow_scratch[extract_first as usize..][..count as usize];
                inputs.push(0);
                inputs.extend_from_slice(aterm_bits::cast_slice(insts));
            }
            inputs.extend_from_slice(aterm_bits::bytes_of(cell));
            inputs.extend_from_slice(aterm_bits::bytes_of(bloom));
            for v in bloom_scissor.unwrap_or([u32::MAX; 4]) {
                inputs.extend_from_slice(&v.to_le_bytes());
            }
        }
        let copy_rect = present_copy_rect(
            win.offscreen_dirty_since_sync,
            win.present_offscreen_fx,
            (fw, fh),
        );

        let live = self.gl_arm.as_ref().expect("checked above");
        let bufs = live.present.as_ref().expect("restored above");
        let pipe = |row: Pipeline| {
            live.pipeline(row)
                .unwrap_or_else(|| panic!("gl present: {row:?} ensured above"))
        };
        let nearest = live.nearest();
        let linear = live.linear();
        let off = &win.gl_offscreen.as_ref().expect("checked above").tex;
        let present_off = use_present_off.then(|| win.gl_present_off.as_ref().expect("ensured"));
        let composed = present_off.unwrap_or(off);
        let dest_tex = match dest {
            GlDest::Texture(t) => t,
            GlDest::Virtual => win.gl_virtual_off.as_ref().expect("dims read above"),
        };
        let mut frame = GlFrame::begin(&live.dev)?;
        if let Some(copy) = present_off {
            let [x0, y0, x1, y1] = copy_rect;
            if x1 > x0 && y1 > y0 {
                frame.copy_rect(off, (x0, y0), copy, (x0, y0), x1 - x0, y1 - y0)?;
            }
        }
        if let Some((count, _, _)) = bloom_state {
            let half = win.gl_bloom_half.as_ref().expect("ensured above");
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
            pass.bind_texture(half, linear, binds.fragment_textures[0], raw);
            pass.draw(3, 1)?;
        }
        if let Some(region) = shimmer_region.as_ref() {
            let scratch = win.gl_shimmer_scratch.as_ref().expect("ensured above");
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
            pass.bind_texture(scratch, linear, binds.fragment_textures[0], raw);
            pass.draw(3, 1)?;
        }
        if let Some(shift) = band_shift {
            let copy = present_off.ok_or("the band shift needs the present copy")?;
            let scratch = win.gl_shift_scratch.as_ref().expect("ensured above");
            frame.copy_rect(copy, (0, shift.src_y), scratch, (0, 0), fw, shift.moved)?;
            frame.copy_rect(scratch, (0, 0), copy, (0, shift.dst_y), fw, shift.moved)?;
        }
        if let Some(strip) = apron {
            let copy = present_off.ok_or("the apron strip needs the present copy")?;
            let tex = win.gl_apron_tex.as_ref().expect("ensured above");
            let w = fw.min(tex.width);
            let rows = strip
                .rows
                .min(tex.height)
                .min(fh.saturating_sub(strip.dst_y));
            if w > 0 && rows > 0 {
                frame.copy_rect(tex, (0, 0), copy, (0, strip.dst_y), w, rows)?;
            }
        }
        if tray.is_some() {
            let card = win.gl_tray_card.as_ref().expect("ensured above");
            let binds = Pipeline::Tray.spec().binds;
            let mut pass = frame.begin_pass(
                composed,
                false,
                crate::device_layer::FrameLoad::Load,
                None,
                (&bufs.tray, binds.vertex_uniform.expect("row 17 binds at 2")),
            )?;
            pass.set_pipeline(pipe(Pipeline::Tray));
            pass.bind_texture(&card.tex, linear, binds.fragment_textures[0], false);
            pass.draw(4, 1)?;
        }
        let mut crown_drawn = [0, 0, 0, 0];
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
            pass.bind_texture(composed, nearest, binds.fragment_textures[0], raw);
            pass.draw(3, 1)?;
            if run_sdr_glow
                && crown_count > 0
                && let Some(scissor) = crown_scissor
                && (self.bloom_items_live || bufs.crown_stream.is_some())
            {
                let lit = if self.bloom_items_live {
                    self.bloom_items.footprint()
                } else {
                    glow_footprint(&self.bloom_glow_scratch[..crown_count as usize])
                };
                crown_drawn = crown_footprint(lit, want.content_off, scissor);
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
        let mut fx: Option<[u32; 4]> = Some([0, 0, 0, 0]);
        let bloom = if bloom_state.is_some() {
            bloom_scissor
        } else {
            Some([0, 0, 0, 0])
        };
        if use_present_off {
            if let Some(region) = shimmer_region.as_ref() {
                fx = union_rect_opt(
                    fx,
                    Some([region.x0, region.y0, region.x1, region.y1]),
                    (fw, fh),
                );
            }
            if let Some(footprint) = band_fx {
                fx = union_rect_opt(fx, Some(footprint), (fw, fh));
            }
            win.offscreen_dirty_since_sync = Some([0, 0, 0, 0]);
            win.present_offscreen_fx = union_rect_opt(
                union_rect_opt(fx, bloom, (fw, fh)),
                Some(tray_fx.unwrap_or([0, 0, 0, 0])),
                (fw, fh),
            );
        }
        let tray = tray_fx.map(|rect| {
            let card = win.gl_tray_card.as_ref().expect("uploaded above");
            (rect, card.serial)
        });
        let posted = GlPosted {
            blit: want,
            frame: (fw, fh),
            dest: (dw, dh),
            fx,
            bloom,
            tray,
            crown: crown_drawn,
        };
        if win.video.is_some() || win.presented_snapshot.is_some() {
            let capture_color_space = win.capture_color_space();
            let capture_sdr_white_scale = win.sdr_white_scale();
            let dest_tex = match dest {
                GlDest::Texture(t) => t,
                GlDest::Virtual => win.gl_virtual_off.as_ref().expect("dims read above"),
            };
            let pixels =
                live.dev
                    .read_rgba8(dest_tex, dw, dh, crate::padded_bytes_per_row(dw as usize))?;
            if let Some(tap) = win.video.as_mut() {
                tap.gl_enqueue_frame(
                    (dw, dh),
                    crate::device_layer::TexelFormat::Rgba8Unorm,
                    &pixels,
                    capture_color_space,
                    capture_sdr_white_scale,
                );
            }
            if let Some(tap) = win.presented_snapshot.as_mut() {
                tap.gl_enqueue_frame(
                    (dw, dh),
                    crate::device_layer::TexelFormat::Rgba8Unorm,
                    &pixels,
                    capture_color_space,
                    capture_sdr_white_scale,
                );
            }
        }
        Ok((use_present_off, posted))
    }

    /// The armed headless present: encode on GL, compose into the resident virtual target at the
    /// destination size (opaque, as every arm's virtual present is).
    #[allow(
        clippy::too_many_arguments,
        reason = "the shared virtual-present seam's tuple"
    )]
    pub(crate) fn gl_present_virtual_with_crop(
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
        let device = Arc::clone(
            self.gl_arm
                .as_ref()
                .ok_or("the GL arm is not live")?
                .dev
                .shared(),
        );
        let _bound = device.enter().ok();
        let work_started = aterm_time::Instant::now();
        let (fw, fh) = self.encode_present_frame(win, input);
        if !self.last_frame_arm_gl {
            return Err("the frame did not draw on the GL arm".to_owned());
        }
        win.resident_input_effects_transport_shift_y = effects_transport_shift_y;
        if tray.is_none() {
            win.gl_tray_card = None;
        }
        let live = self.gl_arm.as_ref().ok_or("the GL arm is not live")?;
        let fits = win
            .gl_virtual_off
            .as_ref()
            .is_some_and(|t| t.on(&live.dev) && (t.width, t.height) == (w, h));
        if !fits {
            win.gl_virtual_off = None;
            win.gl_virtual_off = Some(live.dev.texture_2d(TexelFormat::Rgba8Unorm, w, h, None)?);
            win.virtual_presented_input_epoch = 0;
        }
        self.gl_compose_present(
            win,
            input,
            invert,
            overlay,
            tray,
            source_crop,
            (fw, fh),
            GlDest::Virtual,
            false,
        )?;
        win.virtual_presented_input_epoch = win.resident_input_epoch;
        win.last_present_work_ns =
            u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        win.last_present_encode_ns = win.last_present_work_ns;
        self.gl_maybe_inject_loss();
        Ok(())
    }

    /// The injected-loss drill's fire site (`gl_inject_loss_after`): counts successful GL presents,
    /// and at zero arms the device so it loses its context as a real loss does
    /// (`GlShared::inject_context_loss`: on a GLX device the next present's reset check reads a
    /// reset, after its GLX binds; on an EGL device the next `eglMakeCurrent` fails as a lost
    /// context's does).
    pub(crate) fn gl_maybe_inject_loss(&mut self) {
        let Some(n) = self.gl_inject_loss_after.as_mut() else {
            return;
        };
        if *n > 1 {
            *n -= 1;
            return;
        }
        self.gl_inject_loss_after = None;
        let Some(live) = self.gl_arm.as_ref() else {
            return;
        };
        let what = live.dev.shared().inject_context_loss();
        crate::stderr_line!("aterm-gpu: ATERM_GL_INJECT_LOSS fired; {what} (drill lever)");
    }

    /// Fire the injected-loss drill after `n` more successful GL presents, as
    /// `ATERM_GL_INJECT_LOSS=<n>` does at construct.
    #[cfg(test)]
    pub(crate) fn gl_inject_loss_after_for_test(&mut self, n: u64) {
        self.gl_inject_loss_after = Some(n).filter(|n| *n > 0);
    }

    /// The armed present helper's readback (`present_input_readback`): a plain frame reads the
    /// offscreen; an effect frame composes into a throwaway destination and reads the present copy
    /// the blit sampled.
    pub(crate) fn gl_present_readback(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        (w, h): (u32, u32),
    ) -> Result<Frame, String> {
        let effect_frame =
            (self.enable_bloom && fx_gpu::bloom_source_live(input)) || self.shimmer_live(input);
        if !effect_frame {
            return self.gl_read_back_offscreen(win, w, h);
        }
        let live = self.gl_arm.as_ref().ok_or("the GL arm is not live")?;
        let scratch = live.dev.texture_2d(TexelFormat::Rgba8Unorm, w, h, None)?;
        let composed = self.gl_compose_present(
            win,
            input,
            false,
            None,
            None,
            None,
            (w, h),
            GlDest::Texture(&scratch),
            false,
        )?;
        let live = self.gl_arm.as_ref().ok_or("the GL arm is not live")?;
        let tex = if composed.0 {
            win.gl_present_off.as_ref().ok_or("no present copy")?
        } else {
            &win.gl_offscreen.as_ref().ok_or("no GL offscreen")?.tex
        };
        crate::device_layer::gl_try_read_back(&live.dev, tex, w, h)
    }

    /// The resident virtual destination's bytes, as read back, for
    /// `virtual_presented_snapshot_current` (the caller checked the epoch).
    pub(crate) fn gl_virtual_snapshot(
        &self,
        win: &WindowGpu,
        t_us: u64,
    ) -> Result<crate::video_tap::RawCapture, String> {
        let live = self.gl_arm.as_ref().ok_or("the GL arm is not live")?;
        let tex = win
            .gl_virtual_off
            .as_ref()
            .filter(|t| t.on(&live.dev))
            .ok_or("presented snapshot: no virtual destination has been presented")?;
        let (w, h) = (tex.width, tex.height);
        let rgba = live.dev.read_rgba8(tex, w, h, w as usize * 4)?;
        crate::video_tap::RawCapture::rgba8_srgb(rgba, w, h, t_us)
    }
}

/// The shipped Linux build's window presents: the GL arm owns the window's
/// surface, a `GLXWindow` on X11 and an EGL surface on Wayland.
impl GpuRenderer {
    /// Attach `target`'s window: the arm on the window system's display (minted there, or minted
    /// again there if a headless frame came first), and a window surface at `width` x `height`
    /// (clamped like every framebuffer): a `GLXWindow` on X11, an EGL surface on Wayland (where
    /// X11's GLX cannot serve there is no device, `gl::glx`).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn gl_create_window_surface<W: WindowTarget>(
        &mut self,
        target: W,
        width: u32,
        height: u32,
        sink: Option<Box<dyn crate::FrameSink>>,
    ) -> Result<GpuSurface, String> {
        use crate::gl::device::WindowSystem;
        use crate::gl::surface::{GlWindowSurface, NativeWindow};
        use aterm_types::native_window::NativeWindow as Handles;
        if !self.gl_armed {
            return Err(self.gl_start_error());
        }
        let (ws, native) = match target.native_window() {
            Some(Handles::Xlib {
                display,
                screen,
                window,
                visual,
            }) => (
                WindowSystem::X11 {
                    display: display.as_ptr() as usize,
                    screen,
                },
                NativeWindow::X11 {
                    window: u64::from(window),
                    visual: (visual != 0).then_some(visual),
                },
            ),
            Some(Handles::Wayland { display, surface }) => (
                WindowSystem::Wayland {
                    display: display.as_ptr() as usize,
                },
                NativeWindow::Wayland {
                    surface: surface.as_ptr(),
                },
            ),
            None => return Err("the window has no X11 or Wayland handles".to_owned()),
        };
        if self.gl_window_system.is_some_and(|have| have != ws) {
            return Err("a second window system in one process".to_owned());
        }
        self.gl_window_system = Some(ws);
        if !self.gl_arm_ready() {
            return Err(self.gl_start_error());
        }
        let width = self.clamp_fb_dim(width.max(1));
        let height = self.clamp_fb_dim(height.max(1));
        let live = self.gl_arm.as_ref().expect("gl_arm_ready above");
        let sink = sink.filter(|_| crate::gl::surface::readback_wanted_on(&live.dev));
        let gs = GlWindowSurface::create(&live.dev, native, width, height, Box::new(target), sink)?;
        if crate::verbose() {
            crate::stderr_line!(
                "aterm-gpu: GL window surface, swap interval {}",
                u32::from(gs.synced())
            );
        }
        Ok(GpuSurface {
            acquire_id: next_surface_acquire_id(),
            neutral: NeutralSurfaceConfig {
                width,
                height,
                format: TexelFormat::Rgba8Unorm,
            },
            supports_f16: false,
            post_mult: false,
            copyable: true,
            gl: Some(gs),
        })
    }

    /// The window present: encode on GL, compose into the surface's stand-in, post it
    /// (`GlWindowSurface::present`: the flipped blit and the swap, timed as the acquire wait, since
    /// a synchronized swap is where a GL present waits for the display).
    #[allow(
        clippy::too_many_arguments,
        reason = "the shared swapchain seam's established tuple"
    )]
    pub(crate) fn gl_present_input_with_crop(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        effects_transport_shift_y: i32,
    ) -> Result<(), SurfacePresentFailure> {
        let source_crop = self
            .resident_present_crop(input, source_crop)
            .map_err(|()| SurfacePresentFailure::Validation)?;
        let Some(gs) = surf.gl.as_mut() else {
            gl_arm_note("armed present refused: the window has no GL surface (re-attach it)");
            return Err(SurfacePresentFailure::Validation);
        };
        if !self.gl_arm_ready() {
            gl_arm_note("armed present refused: the GL arm is not live");
            return Err(SurfacePresentFailure::Validation);
        }
        let device = Arc::clone(
            self.gl_arm
                .as_ref()
                .expect("gl_arm_ready above")
                .dev
                .shared(),
        );
        let _bound = device.enter().ok();
        gs.resize(surf.neutral.width, surf.neutral.height);
        let work_started = aterm_time::Instant::now();
        let (fw, fh) = self.encode_present_frame(win, input);
        if !self.last_frame_arm_gl {
            gl_arm_note("armed present dropped: the frame did not draw on the GL arm");
            return Err(SurfacePresentFailure::Validation);
        }
        win.resident_input_effects_transport_shift_y = effects_transport_shift_y;
        if tray.is_none() {
            win.gl_tray_card = None;
        }
        let translucent = self.cpu.background_opacity() < 1.0 && surf.post_mult;
        let composed = (|| -> Result<GlPosted, String> {
            let live = self.gl_arm.as_ref().ok_or("the GL arm is not live")?;
            let standin = gs.standin(&live.dev)?;
            self.gl_compose_present(
                win,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                (fw, fh),
                GlDest::Texture(standin),
                translucent,
            )
            .map(|(_, posted)| posted)
        })();
        let composed = match composed {
            Ok(posted) => posted,
            Err(e) => {
                gl_arm_note(&format!("armed present dropped: the compose failed ({e})"));
                return Err(SurfacePresentFailure::Validation);
            }
        };
        win.last_present_work_ns =
            u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        win.last_present_encode_ns = win.last_present_work_ns;
        let live = self.gl_arm.as_ref().expect("checked above");
        let fill = frame_live_bg(input, self.theme.bg);
        let bloom_held =
            !win.gl_bloom_inputs.is_empty() && win.gl_bloom_inputs == win.gl_posted_bloom;
        let damage = post_damage(
            win.gl_posted.as_ref(),
            &composed,
            win.offscreen_dirty_since_post,
            bloom_held,
        );
        let posted = win.measure_surface_acquire(|| gs.present(&live.dev, fill, damage));
        live.dev.free_retired();
        win.gl_posted = posted.is_ok().then_some(composed);
        if posted.is_ok() {
            win.offscreen_dirty_since_post = Some([0, 0, 0, 0]);
            std::mem::swap(&mut win.gl_posted_bloom, &mut win.gl_bloom_inputs);
        }
        posted.map_err(|e| {
            gl_arm_note(&format!("armed present: the post failed ({e})"));
            e.kind
        })?;
        self.gl_maybe_inject_loss();
        Ok(())
    }

    /// The error a caller returns when the arm cannot start: the mint failure's cause when there
    /// was one.
    pub(crate) fn gl_start_error(&self) -> String {
        match &self.gl_mint_error {
            Some(e) => format!("the GL renderer could not start: {e}"),
            None => "the GL renderer could not start".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posted(content_off: [f32; 2], dest: (u32, u32)) -> GlPosted {
        let mut blit = BlitUniform::bell(false);
        blit.dims = [800.0, 600.0];
        blit.content_off = content_off;
        GlPosted {
            blit,
            frame: (800, 600),
            dest,
            fx: Some([0, 0, 0, 0]),
            bloom: Some([0, 0, 0, 0]),
            tray: None,
            crown: [0, 0, 0, 0],
        }
    }

    #[test]
    fn the_crown_covers_its_quads_not_its_scissor() {
        let quad = |x, y, w, h| GlowInstance {
            rect: [x, y, w, h],
            color: [255; 4],
            color2: [255; 4],
        };
        let scissor = (0, 0, 800, 600);
        let quads = [
            quad(100, 40, 20, 30),
            quad(300, 50, 10, 0),
            quad(90, 60, 5, 40),
        ];
        let lit = glow_footprint(&quads);
        assert_eq!(
            crown_footprint(lit, [0.0, 0.0], scissor),
            [90, 40, 120, 100],
            "the box of the quads that draw (an empty one draws nothing)"
        );
        assert_eq!(
            crown_footprint(lit, [3.0, -50.0], scissor),
            [93, 0, 123, 50],
            "placed by content_off, clipped to the scissor"
        );
        assert_eq!(
            crown_footprint(lit, [0.5, 0.0], (0, 0, 100, 600)),
            [90, 40, 100, 100],
            "a fractional placement covers both pixels, inside the scissor"
        );
        assert_eq!(
            crown_footprint(glow_footprint(&[]), [0.0, 0.0], scissor),
            [0, 0, 0, 0]
        );
        assert_eq!(
            crown_footprint(lit, [f32::NAN, 0.0], scissor),
            [0, 0, 800, 600],
            "an unplaceable crown covers its scissor"
        );
    }

    #[test]
    fn a_post_s_damage_is_what_changed_since_the_last() {
        let row = Some([0, 40, 800, 70]);
        let fit = posted([0.0, 0.0], (800, 600));
        assert_eq!(
            post_damage(None, &fit, row, false),
            None,
            "the first post is whole"
        );
        assert_eq!(
            post_damage(Some(&fit), &fit, row, false),
            Some([0, 40, 800, 70])
        );
        assert_eq!(
            post_damage(Some(&fit), &fit, Some([0, 0, 0, 0]), false),
            Some([0, 0, 0, 0]),
            "nothing written, nothing posted"
        );
        assert_eq!(
            post_damage(Some(&fit), &fit, None, false),
            None,
            "unknown is whole"
        );

        let placed = posted([3.0, 5.0], (806, 610));
        assert_eq!(
            post_damage(Some(&placed), &placed, row, false),
            Some([3, 45, 803, 75]),
            "frame pixels land at the blit's placement"
        );
        let half = posted([2.5, 0.5], (806, 610));
        assert_eq!(
            post_damage(Some(&half), &half, row, false),
            Some([2, 40, 803, 71]),
            "a fractional placement covers both pixels a texel can reach"
        );
        let mut chrome = placed;
        chrome.blit.chrome_y1 = 30.0;
        assert_eq!(
            post_damage(Some(&chrome), &chrome, row, false),
            Some([0, 45, 806, 75]),
            "beside chrome the bands repeat the frame's edge: the whole width"
        );

        let mut flash = fit;
        flash.blit.flag = 1;
        assert_eq!(
            post_damage(Some(&fit), &flash, row, false),
            None,
            "a new blit is whole"
        );
        assert_eq!(
            post_damage(Some(&fit), &posted([0.0, 0.0], (801, 600)), row, false),
            None,
            "a new size is whole"
        );

        let mut halo = fit;
        halo.fx = Some([100, 300, 200, 400]);
        assert_eq!(
            post_damage(Some(&halo), &fit, row, false),
            Some([0, 40, 800, 400]),
            "an effect gone since the last post is damage"
        );
        assert_eq!(
            post_damage(Some(&fit), &halo, row, false),
            Some([0, 40, 800, 400]),
            "an effect drawn now is damage"
        );
        let mut unbounded = fit;
        unbounded.fx = None;
        assert_eq!(post_damage(Some(&unbounded), &fit, row, false), None);
        let mut crowned = fit;
        crowned.crown = [10, 500, 20, 520];
        assert_eq!(
            post_damage(Some(&crowned), &fit, row, false),
            Some([0, 40, 800, 520]),
            "a crown gone since the last post is damage"
        );

        let mut bloomed = fit;
        bloomed.bloom = Some([0, 0, 800, 600]);
        assert_eq!(
            post_damage(Some(&bloomed), &bloomed, row, true),
            Some([0, 40, 800, 70]),
            "a bloom held since the last post shows only the texels written under it"
        );
        assert_eq!(
            post_damage(Some(&bloomed), &bloomed, row, false),
            Some([0, 0, 800, 600]),
            "a bloom drawn from new inputs is damage over its scissor"
        );
        assert_eq!(
            post_damage(Some(&bloomed), &fit, row, false),
            Some([0, 0, 800, 600]),
            "a bloom gone since the last post is damage"
        );
        let mut spread = fit;
        spread.bloom = None;
        assert_eq!(post_damage(Some(&fit), &spread, row, false), None);
        assert_eq!(
            post_damage(Some(&spread), &spread, row, true),
            Some([0, 40, 800, 70]),
            "an unscissored bloom held is no damage of its own either"
        );
        let mut both = halo;
        both.bloom = Some([0, 0, 800, 600]);
        assert_eq!(
            post_damage(Some(&both), &both, row, true),
            Some([0, 40, 800, 400]),
            "a held bloom leaves the other effects' damage"
        );

        let mut card = fit;
        card.tray = Some(([0, 0, 800, 30], 7));
        assert_eq!(
            post_damage(Some(&card), &card, row, false),
            Some([0, 40, 800, 70]),
            "the same card at the same place shows nothing new"
        );
        let mut redrawn = card;
        redrawn.tray = Some(([0, 0, 800, 30], 8));
        assert_eq!(
            post_damage(Some(&card), &redrawn, row, false),
            Some([0, 0, 800, 70]),
            "a new card is damage"
        );
        let mut moved = card;
        moved.tray = Some(([0, 500, 800, 530], 7));
        assert_eq!(
            post_damage(Some(&card), &moved, row, false),
            Some([0, 0, 800, 530]),
            "a card moved is damage where it was and where it is"
        );
        assert_eq!(
            post_damage(Some(&card), &fit, row, false),
            Some([0, 0, 800, 70]),
            "a card gone is damage"
        );
    }
}
