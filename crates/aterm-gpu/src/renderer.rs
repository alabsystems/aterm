// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GPU terminal renderer (`GpuRenderer`): backend orchestration and the glyph/quad pipelines.

use std::sync::{Arc, Weak};

use aterm_hash::{FxHashMap, FxHashSet};

use aterm_core::terminal::{CursorStyle, UnderlineStyle};
use aterm_render::{
    DirtyDecision, Frame, GlyphImage, GlyphKey, Rasterizer, RenderInput, RenderView, Renderer,
    SceneAtlas, Theme, compute_dirty_rows, is_unchanged_frame,
};

use crate::GpuContext;
#[cfg(target_os = "macos")]
use crate::pipeline_table::ShaderLibrary;
use crate::pipeline_table::{self, Pipeline};

const ATLAS_WIDTH: u32 = 1024;

const ATLAS_GROW_HEADROOM: u32 = 256;

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace GpuRowEncodeProofs

        -- When a row has no selection span (rowHasSel = false), no column on
        -- that row can be selected, so skipping selection_hit is exact.
        def cellSelectedOnRow (rowHasSel colInSpan : Bool) : Bool :=
          if rowHasSel then colInSpan else false

        theorem unselected_row_has_no_selected_cell (colInSpan : Bool) :
            cellSelectedOnRow false colInSpan = false := by
          rfl

        -- Consecutive cells on a row sharing (base_fg, cell.bg, hit) resolve to
        -- identical (glyph_color, bg_under) under the fixed frame
        -- selection_palette and min_contrast.
        def resolveRowFg (key memoKey memoVal freshVal : Nat) : Nat :=
          if Nat.beq key memoKey then memoVal else freshVal

        theorem row_fg_memo_hit_is_exact (val : Nat) :
            resolveRowFg 7 7 val val = val := by
          rfl

        theorem row_fg_memo_miss_recomputes (memoVal freshVal : Nat) :
            resolveRowFg 8 7 memoVal freshVal = freshVal := by
          rfl

        end GpuRowEncodeProofs
    }
}

macro_rules! impl_pod_zeroable {
    ($t:ident { $($f:ident : $ft:ty),+ $(,)? }) => {
        const _: () = assert!(
            ::core::mem::size_of::<$t>() == 0usize $(+ ::core::mem::size_of::<$ft>())+,
            concat!(
                stringify!($t),
                ": size_of is not the sum of its field sizes, so it has padding bytes. \
                 `Pod` forbids them (give the hole an explicit `_pad` field).",
            ),
        );
        const _: fn($t) = |v: $t| {
            const fn is_pod<T: ::aterm_bits::Pod>(_: &T) {}
            let $t { $($f: _),+ } = v;
            $({ let f: $ft = v.$f; is_pod(&f); })+
        };
        // SAFETY: `$t` is `#[repr(C)]`, `Copy + 'static`, and padding-free, so the all-zero
        // pattern is valid.
        unsafe impl ::aterm_bits::Zeroable for $t {}
        // SAFETY: `$t` has no padding and every field is `Pod`, so any `size_of::<$t>()`-byte
        // pattern is valid.
        unsafe impl ::aterm_bits::Pod for $t {}
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Uniforms {
    screen: [f32; 2],
    text_blend: f32,
    _pad: f32,
}
impl_pod_zeroable!(Uniforms {
    screen: [f32; 2],
    text_blend: f32,
    _pad: f32
});

#[repr(C)]
#[derive(Clone, Copy)]
struct BgInstance {
    rect: [u16; 4],
    color: [u8; 4],
}
impl_pod_zeroable!(BgInstance {
    rect: [u16; 4],
    color: [u8; 4]
});

#[repr(C)]
#[derive(Clone, Copy)]
struct GlowInstance {
    rect: [u16; 4],
    color: [u8; 4],
    color2: [u8; 4],
}
impl_pod_zeroable!(GlowInstance {
    rect: [u16; 4],
    color: [u8; 4],
    color2: [u8; 4]
});

impl GlowInstance {
    #[inline]
    const fn of(rect: [u16; 4], q: &aterm_render::GlowQuad) -> Self {
        let [r, g, b, a, r2, g2, b2, a2] = glow8(q);
        Self {
            rect,
            color: [r, g, b, a],
            color2: [r2, g2, b2, a2],
        }
    }
}

/// A window-space glow stream as glow instances, in its draw order: each
/// quad, and each lit row of its ramps as the quad it stands for. Rows
/// tagged past `rows` or not `row_active` are skipped.
#[inline]
fn glow_instances_into(
    out: &mut Vec<GlowInstance>,
    (quads, ramps): (&[aterm_render::GlowQuad], &aterm_render::GlowRamps),
    rows: usize,
    row_active: impl Fn(usize) -> bool,
) {
    if ramps.is_empty() {
        // A stream of quads alone, as the renderer laid them before ramps.
        for q in quads {
            if (q.row as usize) >= rows || !row_active(q.row as usize) {
                continue;
            }
            out.push(GlowInstance::of([q.x, q.y, q.w, q.h], q));
        }
        return;
    }
    let live = |row: u16| usize::from(row) < rows && row_active(usize::from(row));
    use aterm_render::glow_ramps::Lights;
    aterm_render::glow_ramps::each_laid(quads, ramps, |laid| match laid {
        Lights::Quads(run) => out.extend(
            run.iter()
                .filter(|q| live(q.row))
                .map(|q| GlowInstance::of([q.x, q.y, q.w, q.h], q)),
        ),
        Lights::Ramp(r) => {
            if live(r.row) {
                out.extend(
                    r.quads(&ramps.ends)
                        .map(|q| GlowInstance::of([q.x, q.y, q.w, q.h], &q)),
                );
            }
        }
    });
}

type BgRun = (u16, u16, [u8; 4]);

#[inline]
fn push_bg_run(
    bg: &mut Vec<BgInstance>,
    run: &mut Option<BgRun>,
    y: u16,
    h: u16,
    x: u16,
    w: u16,
    color: [u8; 4],
) {
    if let Some((rx, rw, rc)) = run
        && *rc == color
        && u32::from(*rx) + u32::from(*rw) == u32::from(x)
        && u32::from(*rw) + u32::from(w) <= u32::from(u16::MAX)
    {
        *rw += w;
        return;
    }
    flush_bg_run(bg, run, y, h);
    *run = Some((x, w, color));
}

#[inline]
fn flush_bg_run(bg: &mut Vec<BgInstance>, run: &mut Option<BgRun>, y: u16, h: u16) {
    if let Some((x, w, color)) = run.take() {
        bg.push(BgInstance {
            rect: [x, y, w, h],
            color,
        });
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GlyphInstance {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [u8; 4],
    bg: [u8; 4],
}
impl_pod_zeroable!(GlyphInstance {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [u8; 4],
    bg: [u8; 4]
});

const _: () = {
    assert!(std::mem::size_of::<BgInstance>() as u64 == pipeline_table::BG_LAYOUT.stride);
    assert!(std::mem::size_of::<GlowInstance>() as u64 == pipeline_table::GLOW_LAYOUT.stride);
    assert!(std::mem::size_of::<GlyphInstance>() as u64 == pipeline_table::GLYPH_LAYOUT.stride);
};

trait RasterEdit {
    fn dropped_rasters(&self) -> bool;
}

impl RasterEdit for bool {
    #[inline]
    fn dropped_rasters(&self) -> bool {
        *self
    }
}

impl RasterEdit for () {
    #[inline]
    fn dropped_rasters(&self) -> bool {
        true
    }
}

impl RasterEdit for Result<(), String> {
    #[inline]
    fn dropped_rasters(&self) -> bool {
        self.is_ok()
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RainGlowInstance {
    rect: [u16; 4],
    color: [u8; 4],
    falloff: [u16; 4],
}
impl_pod_zeroable!(RainGlowInstance {
    rect: [u16; 4],
    color: [u8; 4],
    falloff: [u16; 4]
});
const _: () = assert!(
    std::mem::size_of::<RainGlowInstance>() as u64 == pipeline_table::RAIN_GLOW_LAYOUT.stride
);

#[repr(C)]
#[derive(Clone, Copy)]
struct FireInstance {
    rect: [u16; 4],
    geo: [u16; 4],
    phase: u32,
    tsl: [u8; 4],
}
impl_pod_zeroable!(FireInstance {
    rect: [u16; 4],
    geo: [u16; 4],
    phase: u32,
    tsl: [u8; 4]
});
const _: () =
    assert!(std::mem::size_of::<FireInstance>() as u64 == pipeline_table::FIRE_LAYOUT.stride);

#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
struct HdrGlowUniform {
    screen: [f32; 2],
    content_off: [f32; 2],
    boost: f32,
    headroom: f32,
    _pad: [f32; 2],
}
impl_pod_zeroable!(HdrGlowUniform {
    screen: [f32; 2],
    content_off: [f32; 2],
    boost: f32,
    headroom: f32,
    _pad: [f32; 2],
});

#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
struct BloomUniform {
    texel: [f32; 2],
    strength: f32,
    radius: f32,
    chroma_radius: f32,
    /// The share of the half texture's storage that its texels in use fill, per axis: 1 for an
    /// exact texture. Only the GL arm's present keeps roomy storage, and only `bloom.glsl` reads
    /// it; the other arms' shaders take it as padding.
    extent: [f32; 2],
    _pad: f32,
}
impl_pod_zeroable!(BloomUniform {
    texel: [f32; 2],
    strength: f32,
    radius: f32,
    chroma_radius: f32,
    extent: [f32; 2],
    _pad: f32
});

#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
struct ShimmerUniform {
    frame: [f32; 2],
    region_min: [f32; 2],
    region_max: [f32; 2],
    hot_top: f32,
    rise: f32,
    amp: f32,
    period: f32,
    phase: f32,
    band_x0: f32,
    band_w: f32,
    rolloff: f32,
    _pad: [f32; 2],
    heat: [[f32; 4]; SHIMMER_BANDS / 4],
}
impl_pod_zeroable!(ShimmerUniform {
    frame: [f32; 2],
    region_min: [f32; 2],
    region_max: [f32; 2],
    hot_top: f32,
    rise: f32,
    amp: f32,
    period: f32,
    phase: f32,
    band_x0: f32,
    band_w: f32,
    rolloff: f32,
    _pad: [f32; 2],
    heat: [[f32; 4]; SHIMMER_BANDS / 4],
});

/// Blit uniform.
#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct BlitUniform {
    flag: u32,
    overlay: u32,
    border_px: f32,
    encode_srgb: f32,
    accent: [f32; 3],
    chrome_y1: f32,
    dims: [f32; 2],
    wash_a: f32,
    border_a: f32,
    band: [f32; 4],
    content_off: [f32; 2],
    hdr: f32,
    translucent: f32,
    sdr_white_scale: f32,
    visible_y: f32,
    visible_h: f32,
    premult: f32,
    chrome_y0: f32,
    chrome_pad: [f32; 3],
}
impl_pod_zeroable!(BlitUniform {
    flag: u32,
    overlay: u32,
    border_px: f32,
    encode_srgb: f32,
    accent: [f32; 3],
    chrome_y1: f32,
    dims: [f32; 2],
    wash_a: f32,
    border_a: f32,
    band: [f32; 4],
    content_off: [f32; 2],
    hdr: f32,
    translucent: f32,
    sdr_white_scale: f32,
    visible_y: f32,
    visible_h: f32,
    premult: f32,
    chrome_y0: f32,
    chrome_pad: [f32; 3],
});

impl BlitUniform {
    #[inline]
    const fn bell(invert: bool) -> Self {
        Self {
            flag: invert as u32,
            overlay: 0,
            border_px: 0.0,
            encode_srgb: 0.0, // set by the caller from ctx.srgb_offscreen
            accent: [0.0; 3],
            chrome_y1: 0.0, // set by the present path from the chrome extent
            dims: [0.0, 0.0],
            wash_a: 0.0,
            border_a: 0.0,
            band: [0.0; 4],
            content_off: [0.0, 0.0],
            hdr: 0.0,             // set by the caller from the HdrPlan (f16 swapchains only)
            translucent: 0.0,     // set by the present path when background_opacity < 1.0
            sdr_white_scale: 1.0, // 1.0 = no scaling (SDR / macOS); set on the Windows EDR present
            visible_y: 0.0,
            visible_h: 0.0,
            premult: 0.0,   // set by the present path on the DComp visual swapchain
            chrome_y0: 0.0, // set by the present path from the chrome extent
            chrome_pad: [0.0; 3],
        }
    }

    #[inline]
    const fn with_translucency(mut self, band_a: f32) -> Self {
        self.translucent = 1.0;
        self.band[3] = band_a;
        self
    }

    #[inline]
    const fn with_premultiplied_output(mut self) -> Self {
        self.premult = 1.0;
        self
    }

    #[inline]
    fn with_bands(mut self, fw: u32, fh: u32, dst_w: u32, dst_h: u32, band_rgb: u32) -> Self {
        let chan = |s: u32| ((band_rgb >> s) & 0xff) as f32 / 255.0;
        self.dims = [fw as f32, fh as f32];
        self.visible_y = 0.0;
        self.visible_h = fh as f32;
        let place =
            aterm_render::frame_placement(dst_w as usize, dst_h as usize, fw as usize, fh as usize);
        self.content_off = [place.x.origin as f32, place.y.origin as f32];
        self.band = [chan(16), chan(8), chan(0), 1.0];
        self
    }

    #[inline]
    fn with_source_crop(mut self, crop: Option<PresentCrop>) -> Self {
        if let Some(crop) = crop {
            let raw_h = self.dims[1].max(0.0) as u32;
            let y = crop.source_y.min(raw_h);
            let h = crop.height.min(raw_h.saturating_sub(y));
            self.visible_y = y as f32;
            self.visible_h = h as f32;
        }
        self
    }
}

/// Tray-overlay uniform.
#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct TrayUniform {
    rect: [f32; 4],
    fb: [f32; 2],
    _pad: [f32; 2],
}
impl_pod_zeroable!(TrayUniform {
    rect: [f32; 4],
    fb: [f32; 2],
    _pad: [f32; 2]
});

/// Drop-target highlight parameters, passed to [`GpuRenderer::present_input`].
#[derive(Clone, Copy)]
pub struct DropOverlay {
    pub accent: u32,
    pub wash_a: u8,
    pub border_a: u8,
    pub border_scale_q4: u8,
}

/// Vertical interval of the raw offscreen that a frontend presents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentCrop {
    pub source_y: u32,
    pub height: u32,
}

/// A CPU-rasterized settings card composited over the swapchain at present time.
#[derive(Clone, Copy)]
pub struct TrayQuad<'a> {
    pub rgba: &'a [u8],
    pub pw: u32,
    pub ph: u32,
    pub dx: u32,
    pub dy: u32,
    pub scrim: Option<TrayScrim>,
}

/// A modal's smoke layer under its tray card ([`TrayQuad::scrim`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayScrim {
    pub rgba: [u8; 4],
    pub top: u32,
}

/// Why an on-screen present did not acquire a drawable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfacePresentFailure {
    AcquirePending,
    Reconfigured,
    Timeout,
    Unavailable,
    Occluded,
    Validation,
}

#[inline]
fn drop_border_px_gpu(w: u32, h: u32) -> u32 {
    (w.min(h) / 200).clamp(2, 6)
}

#[cfg(trust_verify)]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret
            $($clauses)+
        { $($body)* }
    };
}

#[cfg(not(trust_verify))]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret { $($body)* }
    };
}

#[cfg(trust_verify)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

/// `base` scaled by `DropOverlay::border_scale_q4` (1/16 units, 0 keeps `base`), at least one
/// pixel once scaled.
#[inline]
pub(crate) fn scaled_border_px(base: u32, scale_q4: u8) -> u32 {
    if scale_q4 == 0 {
        return base;
    }
    ((base * u32::from(scale_q4)) / 16).max(1)
}

#[cfg(test)]
mod scaled_border_px_tests {
    use super::scaled_border_px;

    #[test]
    fn scaled_border_px_is_sixteenths_never_thinner_than_a_pixel() {
        for (base, q4, want) in [
            (6, 0, 6),
            (6, 16, 6),
            (6, 24, 9),
            (6, 40, 15),
            (6, 44, 16),
            (1, 1, 1),
            (0, 16, 1),
            (0, 0, 0),
        ] {
            assert_eq!(scaled_border_px(base, q4), want, "base {base} × {q4}/16");
        }
    }
}

#[inline]
const fn valid_present_crop(crop: PresentCrop, raw_height: u32) -> bool {
    crop.height > 0
        && crop.source_y < raw_height
        && crop.height <= raw_height.saturating_sub(crop.source_y)
}

#[inline]
fn normalize_present_crop(
    crop: PresentCrop,
    logical_raw_height: u32,
    resident_raw_height: u32,
) -> Option<PresentCrop> {
    if !valid_present_crop(crop, logical_raw_height) {
        return None;
    }
    let height = crop
        .height
        .min(resident_raw_height.saturating_sub(crop.source_y));
    (height > 0).then_some(PresentCrop {
        source_y: crop.source_y,
        height,
    })
}

#[inline]
const fn rect_is_empty(r: [u32; 4]) -> bool {
    r[0] >= r[2] || r[1] >= r[3]
}

#[inline]
fn union_rect_opt(
    a: Option<[u32; 4]>,
    b: Option<[u32; 4]>,
    (w, h): (u32, u32),
) -> Option<[u32; 4]> {
    let (a, b) = (a?, b?);
    let clamp = |r: [u32; 4]| [r[0].min(w), r[1].min(h), r[2].min(w), r[3].min(h)];
    let (a, b) = (clamp(a), clamp(b));
    Some(match (rect_is_empty(a), rect_is_empty(b)) {
        (true, true) => [0, 0, 0, 0],
        (true, false) => b,
        (false, true) => a,
        (false, false) => [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ],
    })
}

#[inline]
fn note_offscreen_written(win: &mut WindowGpu, rect: Option<[u32; 4]>, dims: (u32, u32)) {
    win.offscreen_dirty_since_sync = union_rect_opt(win.offscreen_dirty_since_sync, rect, dims);
    #[cfg(gl_arm)]
    {
        win.offscreen_dirty_since_post = union_rect_opt(win.offscreen_dirty_since_post, rect, dims);
    }
}

#[inline]
fn present_copy_rect(
    dirty: Option<[u32; 4]>,
    fx: Option<[u32; 4]>,
    (w, h): (u32, u32),
) -> [u32; 4] {
    union_rect_opt(dirty, fx, (w, h)).unwrap_or([0, 0, w, h])
}

#[inline]
fn elapsed_ns(started: aterm_time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

const G_BASE_BG: usize = 0;
const G_GLOW_UNDER: usize = 1;
const G_BASE_FG: usize = 2;
const G_GLOW: usize = 3;
const G_WDECO_OVER: usize = 4;
const G_WDECO_ADD: usize = 5;
const G_FREE_OVER: usize = 6;
const G_CURSOR: usize = 7;
const FRAME_GROUPS: usize = 8;

const GROUP_SRGB: [bool; FRAME_GROUPS] = {
    let mut srgb = [true; FRAME_GROUPS];
    srgb[G_GLOW_UNDER] = false; // the glow, Exact fx and fire under the text
    srgb[G_GLOW] = false; // aurora, nova and rain halos, and the glow and over fx
    srgb[G_WDECO_ADD] = false; // sparkle-word additive layer (One/One)
    srgb
};

#[inline]
fn coalesce_frame_passes<const N: usize>(
    enabled: &[bool; N],
    srgb: &[bool; N],
) -> ([usize; N], [bool; N], usize) {
    let mut pass_of = [usize::MAX; N];
    let mut pass_srgb = [true; N];
    let mut passes = 0usize;
    for g in 0..N {
        if !enabled[g] {
            continue;
        }
        if passes == 0 || pass_srgb[passes - 1] != srgb[g] {
            pass_srgb[passes] = srgb[g];
            passes += 1;
        }
        pass_of[g] = passes - 1;
    }
    (pass_of, pass_srgb, passes)
}

/// The staged band move's copy plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BandShift {
    pub(crate) src_y: u32,
    pub(crate) dst_y: u32,
    pub(crate) moved: u32,
}

/// Incoming-row apron strip, resolved once per frame by `GpuRenderer::apron_strip_plan`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApronStrip {
    pub(crate) dst_y: u32,
    pub(crate) rows: u32,
    pub(crate) w: u32,
    pub(crate) cell_h: u32,
}

/// The copy plan under every arm's band shift (`shift_offscreen_band_px`).
#[inline]
pub(crate) const fn band_shift_ops(y0: usize, y1: usize, delta: i64) -> Option<BandShift> {
    if delta == 0 {
        return None;
    }
    let mag = delta.unsigned_abs() as usize;
    let band_h = y1.saturating_sub(y0);
    let moved = band_h.saturating_sub(mag); // destination rows that pull in-band
    if moved == 0 {
        return None;
    }
    let (src_y, dst_y) = if delta > 0 {
        (y0 + mag, y0)
    } else {
        (y0, y0 + mag)
    };
    Some(BandShift {
        src_y: src_y as u32,
        dst_y: dst_y as u32,
        moved: moved as u32,
    })
}

#[cfg(target_os = "macos")]
fn record_band_shift(
    enc: &mut crate::device_layer::FrameEncoder<'_>,
    off: &crate::metal::resources::SealedTexture,
    scratch: &crate::metal::resources::SealedTexture,
    shift: BandShift,
    w: u32,
) -> Result<(), String> {
    use crate::device_layer::FrameCopyTexture;
    enc.copy_texture_rect(
        FrameCopyTexture::Metal(off),
        (0, shift.src_y),
        FrameCopyTexture::Metal(scratch),
        (0, 0),
        w,
        shift.moved,
    )?;
    enc.copy_texture_rect(
        FrameCopyTexture::Metal(scratch),
        (0, 0),
        FrameCopyTexture::Metal(off),
        (0, shift.dst_y),
        w,
        shift.moved,
    )
}

/// The pipeline a [`DrawItem`] binds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrawPipe {
    Wallpaper,
    Bg,
    CursorBlend,
    Glyph,
    Color,
    GlowAdd,
    RainGlow,
    RainGlowOver,
    FireAdd,
    FireOver,
    DecoOver,
    DecoAdd,
    Fx,
    FxLinear,
    FxStore,
}

impl DrawPipe {
    /// The table row this pipe draws through.
    #[inline]
    pub(crate) const fn row(self) -> Pipeline {
        match self {
            Self::Bg => Pipeline::Bg,
            Self::Glyph => Pipeline::Glyph,
            Self::Color => Pipeline::ColorGlyph,
            Self::CursorBlend => Pipeline::CursorBlend,
            Self::GlowAdd => Pipeline::GlowAdd,
            Self::RainGlow => Pipeline::RainGlow,
            Self::RainGlowOver => Pipeline::RainGlowOver,
            Self::FireAdd => Pipeline::FireAdd,
            Self::FireOver => Pipeline::FireOver,
            Self::DecoOver => Pipeline::DecoOver,
            Self::DecoAdd => Pipeline::DecoAdd,
            Self::Wallpaper => Pipeline::SpriteOver,
            Self::Fx => Pipeline::Fx,
            Self::FxLinear => Pipeline::FxLinear,
            Self::FxStore => Pipeline::FxStore,
        }
    }
}

/// The atlas a [`DrawItem`] samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrawAtlas {
    Mono = 0,
    Color,
    Image,
    Deco,
    Wallpaper,
    Fx,
}

/// How many [`DrawAtlas`] slots exist.
pub(crate) const DRAW_ATLAS_COUNT: usize = DrawAtlas::Fx as usize + 1;

/// Every per-frame instance stream, in draw order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamId {
    Wallpaper = 0,
    Bg,
    ImageBelowBg,
    ImageBgCover,
    FxLinearUnder,
    Trail,
    GlowUnder,
    FxUnder,
    FireAdd,
    FireOver,
    FxKnockUnder,
    ImageUnder,
    GlyphHalo,
    Glyph,
    Color,
    Image,
    Deco,
    Curl,
    GlowAdd,
    FxGlow,
    GlowHalo,
    GlowHaloOver,
    NovaAdd,
    FxOver,
    RainAdd,
    RainAddOver,
    WdecoOver,
    WdecoAdd,
    FxLinearOver,
    CursorBlock,
    CursorGlyph,
    CursorColor,
    Cursor,
}

/// How many [`StreamId`]s exist (the per-arm stream array length).
pub(crate) const STREAM_COUNT: usize = StreamId::Cursor as usize + 1;

impl StreamId {
    /// Every stream in declaration (draw) order: `ALL[i] as usize == i`.
    pub(crate) const ALL: [Self; STREAM_COUNT] = [
        Self::Wallpaper,
        Self::Bg,
        Self::ImageBelowBg,
        Self::ImageBgCover,
        Self::FxLinearUnder,
        Self::Trail,
        Self::GlowUnder,
        Self::FxUnder,
        Self::FireAdd,
        Self::FireOver,
        Self::FxKnockUnder,
        Self::ImageUnder,
        Self::GlyphHalo,
        Self::Glyph,
        Self::Color,
        Self::Image,
        Self::Deco,
        Self::Curl,
        Self::GlowAdd,
        Self::FxGlow,
        Self::GlowHalo,
        Self::GlowHaloOver,
        Self::NovaAdd,
        Self::FxOver,
        Self::RainAdd,
        Self::RainAddOver,
        Self::WdecoOver,
        Self::WdecoAdd,
        Self::FxLinearOver,
        Self::CursorBlock,
        Self::CursorGlyph,
        Self::CursorColor,
        Self::Cursor,
    ];

    /// Whether this is one of the fx streams.
    #[inline]
    pub(crate) const fn is_fx(self) -> bool {
        matches!(
            self,
            Self::FxLinearUnder
                | Self::FxUnder
                | Self::FxKnockUnder
                | Self::FxGlow
                | Self::FxOver
                | Self::FxLinearOver
        )
    }

    /// The stable wire label `metrics` prints for this stream (`instances_by_stream=`).
    #[inline]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Wallpaper => "wallpaper",
            Self::Bg => "bg",
            Self::ImageBelowBg => "image_below_bg",
            Self::ImageBgCover => "image_bg_cover",
            Self::FxLinearUnder => "fx_linear_under",
            Self::FxKnockUnder => "fx_knock_under",
            Self::Trail => "trail",
            Self::GlowUnder => "glow_under",
            Self::FxUnder => "fx_under",
            Self::FireAdd => "fire_add",
            Self::FireOver => "fire_over",
            Self::ImageUnder => "image_under",
            Self::GlyphHalo => "glyph_halo",
            Self::Glyph => "glyph",
            Self::Color => "color",
            Self::Image => "image",
            Self::Deco => "deco",
            Self::Curl => "curl",
            Self::GlowAdd => "glow_add",
            Self::FxGlow => "fx_glow",
            Self::GlowHalo => "glow_halo",
            Self::GlowHaloOver => "glow_halo_over",
            Self::NovaAdd => "nova_add",
            Self::FxOver => "fx_over",
            Self::RainAdd => "rain_add",
            Self::RainAddOver => "rain_add_over",
            Self::WdecoOver => "wdeco_over",
            Self::WdecoAdd => "wdeco_add",
            Self::FxLinearOver => "fx_linear_over",
            Self::CursorBlock => "cursor_block",
            Self::CursorGlyph => "cursor_glyph",
            Self::CursorColor => "cursor_color",
            Self::Cursor => "cursor",
        }
    }
}

/// A test's route for a frame's glow ramps ([`GpuRenderer::set_ramps_fx_for_test`]).
#[doc(hidden)]
#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RampsFx {
    /// In the fx stream whenever the frame carries any.
    Always,
    /// As the glow instances their lit rows stand for.
    Never,
}

/// The latest encoded frame's shape ([`GpuRenderer::frame_shape`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameShape {
    pub passes: u32,
    pub instances: [u32; STREAM_COUNT],
    pub instance_bytes: u64,
    pub build_ns: u64,
    pub stage_ns: u64,
    pub ablation: UnormAblation,
    pub unorm_draws: u32,
    pub unorm_instances: u64,
    /// Where the Unorm-law draws went: their own passes, or the sRGB passes by framebuffer fetch.
    pub route: UnormRoute,
    /// Whether the Exact Under fx stored at the end of the base pass through the sRGB view
    /// (route (1): every Exact Under layer drew opaque and no cursor glow drew under the text).
    pub under_store: bool,
    /// The framebuffer-fetch verdict as of the frame ([`GpuRenderer::fetch_probe`]): `None` until
    /// a frame that drew Unorm-law light settled it.
    pub fetch_probe: Option<FetchProbe>,
    pub fx_instances: u32,
    /// The fx bytes the frame uploaded: its instances, its palette and [`Self::fx_atlas_bytes`].
    pub fx_upload_bytes: u64,
    /// Texel bytes of the fx atlas regions the frame wrote: 0 once every art it draws is resident.
    pub fx_atlas_bytes: u64,
    pub fx_refused: u64,
    /// Metal: the frames in flight the fx atlas sync waited for before rewriting a region they may
    /// read, and the nanoseconds it parked; 0 on the other arms, whose queues order the writes.
    pub fx_reader_waits: u64,
    pub fx_reader_wait_ns: u64,
}

/// The pass-economy ablation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnormAblation {
    #[default]
    Off,
    Drop,
    Thin,
}

impl UnormAblation {
    /// The seam's spelling, and `metrics`' `unorm_ablation=` word.
    #[must_use]
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Drop => "drop",
            Self::Thin => "thin",
        }
    }

    #[inline]
    fn from_seam(value: &str) -> Self {
        match value {
            "drop" | "1" => Self::Drop,
            "thin" => Self::Thin,
            _ => Self::Off,
        }
    }
}

/// How a frame's Unorm-law draws (the additive and glow groups) reached the offscreen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnormRoute {
    /// Own passes on the Unorm view: every arm, and Metal until its device passed the fetch probe.
    #[default]
    Split,
    /// Inside the sRGB passes, by framebuffer fetch (Metal on an Apple GPU whose probe passed).
    Fetch,
}

impl UnormRoute {
    /// The seam's spelling, and `metrics`' `gpu_unorm_route=` word.
    #[must_use]
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Fetch => "fetch",
        }
    }

    #[inline]
    fn from_seam(value: &str) -> Self {
        match value {
            "split" => Self::Split,
            _ => Self::Fetch,
        }
    }

    /// The route a renderer starts on: fetch wherever the device allows it, unless the
    /// `ATERM_GPU_UNORM_ROUTE` dev seam (`seam`, `None` without dev seams) names the split.
    #[inline]
    fn initial(seam: Option<&std::ffi::OsStr>) -> Self {
        seam.and_then(std::ffi::OsStr::to_str)
            .map_or(Self::Fetch, Self::from_seam)
    }
}

/// Which stage of the framebuffer-fetch probe a byte failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchStage {
    /// A byte's linear value did not store as that byte.
    Store,
    /// A stored byte, fetched and stored again, came back another byte.
    RoundTrip,
    /// A fetch after a fixed-function sRGB blend in the pass read another byte than it stores.
    InPass,
}

/// What a device's framebuffer-fetch probe found: whether `encode(decode(b)) == b` for all 256
/// bytes of every channel through its sRGB store and fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchProbe {
    /// No framebuffer fetch on the device (not an Apple GPU) or on the arm: the split stays.
    Unsupported,
    /// Every byte came back: Unorm-law draws blend inside the sRGB passes.
    Passed,
    /// `stage` returned `got` for `want` in `channel` (0 red … 3 alpha): the split stays.
    Failed {
        stage: FetchStage,
        channel: u8,
        want: u8,
        got: u8,
    },
    /// The probe could not run (a build, an encode or a readback failed): the split stays.
    Refused,
}

impl FetchProbe {
    /// `metrics`' `gpu_fetch_probe=` word.
    #[must_use]
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Passed => "passed",
            Self::Failed { .. } => "failed",
            Self::Refused => "refused",
        }
    }
}

impl Default for FrameShape {
    fn default() -> Self {
        Self {
            passes: 0,
            instances: [0; STREAM_COUNT],
            instance_bytes: 0,
            build_ns: 0,
            stage_ns: 0,
            ablation: UnormAblation::Off,
            unorm_draws: 0,
            unorm_instances: 0,
            route: UnormRoute::Split,
            under_store: false,
            fetch_probe: None,
            fx_instances: 0,
            fx_upload_bytes: 0,
            fx_atlas_bytes: 0,
            fx_refused: 0,
            fx_reader_waits: 0,
            fx_reader_wait_ns: 0,
        }
    }
}

impl FrameShape {
    /// Every stream's wire label, in [`Self::instances`] order.
    pub const STREAMS: [&'static str; STREAM_COUNT] = {
        let mut names = [""; STREAM_COUNT];
        let mut i = 0;
        while i < STREAM_COUNT {
            names[i] = StreamId::ALL[i].name();
            i += 1;
        }
        names
    };

    /// The non-empty streams as `(label, instances)`, in draw order.
    #[inline]
    pub fn streams(&self) -> impl Iterator<Item = (&'static str, u32)> + '_ {
        Self::STREAMS
            .iter()
            .zip(self.instances.iter())
            .filter(|&(_, &n)| n > 0)
            .map(|(&name, &n)| (name, n))
    }

    /// Instances across every stream.
    #[must_use]
    #[inline]
    pub fn total_instances(&self) -> u64 {
        self.instances.iter().map(|&n| u64::from(n)).sum()
    }
}

/// A built frame plan's Unorm-law draws and instances, its ablation, the route those draws took
/// ([`route_frame_plan`]), and whether its Exact Under fx stores in the base pass (route (1)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PlanUnorm {
    pub(crate) ablation: UnormAblation,
    pub(crate) draws: u32,
    pub(crate) instances: u64,
    pub(crate) route: UnormRoute,
    pub(crate) under_store: bool,
}

/// One draw: bind `pipe` (its fetch twin when `fetch`) and `atlas` on change, bind `stream`, and
/// draw `count` instanced quads in the pass `group` coalesces into.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DrawItem {
    pub(crate) group: u8,
    pub(crate) pipe: DrawPipe,
    pub(crate) atlas: Option<DrawAtlas>,
    pub(crate) stream: StreamId,
    pub(crate) count: u32,
    /// A Unorm-law draw routed through its fetch twin in the sRGB view ([`UnormRoute::Fetch`]).
    pub(crate) fetch: bool,
}

impl DrawItem {
    /// Whether this draw renders through the sRGB view: an sRGB group's or a fetch twin's.
    #[inline]
    pub(crate) const fn srgb(&self) -> bool {
        GROUP_SRGB[self.group as usize] || self.fetch
    }
}

/// The presence flags [`build_frame_plan`] gates on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FramePlanCtx {
    pub(crate) has_image_plane: bool,
    pub(crate) has_deco: bool,
    pub(crate) has_wallpaper: bool,
    pub(crate) cursor_opaque: bool,
    pub(crate) ablation: UnormAblation,
    /// Every Exact layer at Under draws only opaque pixels ([`fx_gpu::under_draws_opaque`]), so
    /// route (1) of the pass economy may store its fx at the end of the base pass.
    pub(crate) under_opaque: bool,
}

/// Route `plan`'s Unorm-law draws (built on the split) by `route`: under [`UnormRoute::Fetch`]
/// each takes its row's twin in the sRGB view, so the frame opens no Unorm-view pass.
#[cfg(target_os = "macos")]
pub(crate) fn route_frame_plan(plan: &mut [DrawItem], unorm: &mut PlanUnorm, route: UnormRoute) {
    unorm.route = route;
    if route == UnormRoute::Fetch {
        for item in plan
            .iter_mut()
            .filter(|item| !GROUP_SRGB[usize::from(item.group)])
        {
            item.fetch = true;
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one line per draw of the ladder; splitting would scatter the order law"
)]
pub(crate) fn build_frame_plan(
    plan: &mut Vec<DrawItem>,
    counts: &[Option<u32>; STREAM_COUNT],
    ctx: FramePlanCtx,
) -> PlanUnorm {
    plan.clear();
    let mut push = |group: usize, stream: StreamId, pipe: DrawPipe, atlas: Option<DrawAtlas>| {
        let mut count = counts[stream as usize];
        if !GROUP_SRGB[group] {
            match ctx.ablation {
                UnormAblation::Off => {}
                UnormAblation::Drop => return,
                UnormAblation::Thin => count = count.map(|n| n.min(1)),
            }
        }
        if let Some(count) = count {
            plan.push(DrawItem {
                group: group as u8,
                pipe,
                atlas,
                stream,
                count,
                fetch: false,
            });
        }
    };
    // Route (1) of the pass economy: an opaque Exact Under layer's fx is a store, so it may
    // draw at the end of the base pass through the sRGB view, where it lands in the same
    // order against every other draw unless a cursor glow under the text sits between.
    let under_store = ctx.under_opaque && counts[StreamId::GlowUnder as usize].is_none();

    push(
        G_BASE_BG,
        StreamId::Wallpaper,
        DrawPipe::Wallpaper,
        ctx.has_wallpaper.then_some(DrawAtlas::Wallpaper),
    );
    push(G_BASE_BG, StreamId::Bg, DrawPipe::Bg, None);
    if ctx.has_image_plane {
        push(
            G_BASE_BG,
            StreamId::ImageBelowBg,
            DrawPipe::Color,
            Some(DrawAtlas::Image),
        );
    }
    push(G_BASE_BG, StreamId::ImageBgCover, DrawPipe::Bg, None);
    // The Linear layers under the text but the companions: the rain's rows (lane B7 of
    // `docs/DESIGN-card-engine-2026-10-19.md`), under the trail and its light.
    push(
        G_BASE_BG,
        StreamId::FxLinearUnder,
        DrawPipe::FxLinear,
        Some(DrawAtlas::Fx),
    );
    push(G_BASE_BG, StreamId::Trail, DrawPipe::Bg, None);
    if under_store {
        push(
            G_BASE_BG,
            StreamId::FxUnder,
            DrawPipe::FxStore,
            Some(DrawAtlas::Fx),
        );
    }

    push(G_GLOW_UNDER, StreamId::GlowUnder, DrawPipe::GlowAdd, None);
    if !under_store {
        push(
            G_GLOW_UNDER,
            StreamId::FxUnder,
            DrawPipe::Fx,
            Some(DrawAtlas::Fx),
        );
    }
    push(G_GLOW_UNDER, StreamId::FireAdd, DrawPipe::FireAdd, None);
    push(G_GLOW_UNDER, StreamId::FireOver, DrawPipe::FireOver, None);

    // The companions' bodies (the knock layers) open the base-fg group: they stand on the
    // trail's light, under the ink, each cut where its letters lie
    // (`aterm_render::Renderer::cut_knock_layers`). Drawn before the light, a cat whose head
    // reached into the typed row wore the rainbow ribbon's colours, and the trail's bed cut a
    // band out of a cat on the typed row. An sRGB group, so the pass count is unchanged.
    push(
        G_BASE_FG,
        StreamId::FxKnockUnder,
        DrawPipe::FxLinear,
        Some(DrawAtlas::Fx),
    );

    if ctx.has_image_plane {
        push(
            G_BASE_FG,
            StreamId::ImageUnder,
            DrawPipe::Color,
            Some(DrawAtlas::Image),
        );
    }
    push(
        G_BASE_FG,
        StreamId::GlyphHalo,
        DrawPipe::DecoOver,
        Some(DrawAtlas::Mono),
    );
    push(
        G_BASE_FG,
        StreamId::Glyph,
        DrawPipe::Glyph,
        Some(DrawAtlas::Mono),
    );
    push(
        G_BASE_FG,
        StreamId::Color,
        DrawPipe::Color,
        Some(DrawAtlas::Color),
    );
    if ctx.has_image_plane {
        push(
            G_BASE_FG,
            StreamId::Image,
            DrawPipe::Color,
            Some(DrawAtlas::Image),
        );
    }
    push(G_BASE_FG, StreamId::Deco, DrawPipe::Bg, None);
    if ctx.has_deco {
        push(
            G_BASE_FG,
            StreamId::Curl,
            DrawPipe::DecoOver,
            Some(DrawAtlas::Deco),
        );
    }

    push(G_GLOW, StreamId::GlowAdd, DrawPipe::GlowAdd, None);
    push(G_GLOW, StreamId::FxGlow, DrawPipe::Fx, Some(DrawAtlas::Fx));
    push(G_GLOW, StreamId::GlowHalo, DrawPipe::RainGlow, None);
    push(G_GLOW, StreamId::GlowHaloOver, DrawPipe::RainGlowOver, None);
    push(G_GLOW, StreamId::NovaAdd, DrawPipe::GlowAdd, None);
    push(G_GLOW, StreamId::FxOver, DrawPipe::Fx, Some(DrawAtlas::Fx));
    push(G_GLOW, StreamId::RainAdd, DrawPipe::RainGlow, None);
    push(G_GLOW, StreamId::RainAddOver, DrawPipe::RainGlowOver, None);

    if ctx.has_deco {
        push(
            G_WDECO_OVER,
            StreamId::WdecoOver,
            DrawPipe::DecoOver,
            Some(DrawAtlas::Deco),
        );
        push(
            G_WDECO_ADD,
            StreamId::WdecoAdd,
            DrawPipe::DecoAdd,
            Some(DrawAtlas::Deco),
        );
    }

    push(
        G_FREE_OVER,
        StreamId::FxLinearOver,
        DrawPipe::FxLinear,
        Some(DrawAtlas::Fx),
    );

    let cursor_fill = if ctx.cursor_opaque {
        DrawPipe::Bg
    } else {
        DrawPipe::CursorBlend
    };
    push(G_CURSOR, StreamId::CursorBlock, cursor_fill, None);
    push(
        G_CURSOR,
        StreamId::CursorGlyph,
        DrawPipe::Glyph,
        Some(DrawAtlas::Mono),
    );
    push(
        G_CURSOR,
        StreamId::CursorColor,
        DrawPipe::Color,
        Some(DrawAtlas::Color),
    );
    push(G_CURSOR, StreamId::Cursor, cursor_fill, None);

    let unorm = plan
        .iter()
        .filter(|item| !GROUP_SRGB[usize::from(item.group)]);
    PlanUnorm {
        ablation: ctx.ablation,
        draws: u32::try_from(unorm.clone().count()).unwrap_or(u32::MAX),
        instances: unorm.map(|item| u64::from(item.count)).sum(),
        route: UnormRoute::Split,
        under_store: under_store && counts[StreamId::FxUnder as usize].is_some(),
    }
}

pub(crate) enum FrameRes<'a> {
    #[cfg(target_os = "macos")]
    Metal(&'a MetalFrameRig),
    #[cfg(gl_arm)]
    Gl(&'a GlFrameRig<'a>),
    /// The D3D11 arm's resolution set (Windows). Constructed by the D3D11
    /// differential until the flip wires the arm in.
    #[cfg(d3d11_arm)]
    D3d11(&'a D3dFrameRig<'a>),
}

impl FrameRes<'_> {
    #[inline]
    const fn view(&self, srgb: bool) -> crate::device_layer::FrameView<'_> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(rig) => crate::device_layer::FrameView::Metal(if srgb {
                &rig.offscreen_srgb
            } else {
                &rig.offscreen
            }),
            #[cfg(gl_arm)]
            Self::Gl(rig) => crate::device_layer::FrameView::Gl {
                tex: rig.offscreen,
                srgb,
            },
            #[cfg(d3d11_arm)]
            Self::D3d11(rig) => crate::device_layer::FrameView::D3d11 {
                tex: rig.offscreen,
                srgb,
            },
        }
    }

    #[inline]
    fn uniforms(&self) -> crate::device_layer::FrameUniforms<'_> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(rig) => rig.uniforms(),
            #[cfg(gl_arm)]
            Self::Gl(rig) => crate::device_layer::FrameUniforms::Gl {
                buf: rig.uniform,
                binding: rig.uniform_binding,
            },
            #[cfg(d3d11_arm)]
            Self::D3d11(rig) => crate::device_layer::FrameUniforms::D3d11 {
                buf: rig.uniform,
                binding: rig.uniform_binding,
            },
        }
    }

    #[inline]
    fn pipeline(&self, pipe: DrawPipe, fetch: bool) -> crate::device_layer::FramePipeline<'_> {
        #[cfg(any(gl_arm, d3d11_arm))]
        let split_only = || {
            assert!(
                !fetch,
                "only the Metal arm routes a draw through framebuffer fetch"
            );
        };
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(rig) => rig.pipeline(pipe, fetch),
            #[cfg(gl_arm)]
            Self::Gl(rig) => {
                split_only();
                rig.pipeline(pipe)
            }
            #[cfg(d3d11_arm)]
            Self::D3d11(rig) => {
                split_only();
                rig.pipeline(pipe)
            }
        }
    }

    #[inline]
    fn atlas(&self, atlas: DrawAtlas, pipe: DrawPipe) -> crate::device_layer::FrameAtlas<'_> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(rig) => rig.atlas(atlas, pipe),
            #[cfg(gl_arm)]
            Self::Gl(rig) => rig.atlas(atlas, pipe),
            #[cfg(d3d11_arm)]
            Self::D3d11(rig) => rig.atlas(atlas, pipe),
        }
    }

    #[inline]
    fn stream(&self, id: StreamId) -> crate::device_layer::FrameStream<'_> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(rig) => rig.stream(id),
            #[cfg(gl_arm)]
            Self::Gl(rig) => rig.stream(id),
            #[cfg(d3d11_arm)]
            Self::D3d11(rig) => rig.stream(id),
        }
    }
}

/// The GL arm's resolution set.
#[cfg(gl_arm)]
pub(crate) struct GlFrameRig<'a> {
    pub(crate) offscreen: &'a crate::gl::resources::GlTexture,
    pub(crate) pipelines:
        [Option<&'a crate::gl::pipelines::GlPipeline>; pipeline_table::PIPELINE_COUNT],
    pub(crate) uniform: &'a crate::gl::resources::GlBuffer,
    pub(crate) uniform_binding: u32,
    pub(crate) streams: [Option<&'a crate::gl::resources::GlBuffer>; STREAM_COUNT],
    pub(crate) atlases: [Option<(
        &'a crate::gl::resources::GlTexture,
        &'a crate::gl::resources::GlSampler,
    )>; DRAW_ATLAS_COUNT],
    pub(crate) fx: Option<GlFxBind<'a>>,
}

/// The GL rig's [`DrawAtlas::Fx`] resources.
#[cfg(gl_arm)]
#[derive(Clone, Copy)]
pub(crate) struct GlFxBind<'a> {
    pub(crate) r8: &'a crate::gl::resources::GlTexture,
    pub(crate) rgba: &'a crate::gl::resources::GlTexture,
    pub(crate) palette: &'a crate::gl::resources::GlTexture,
    pub(crate) sampler: &'a crate::gl::resources::GlSampler,
}

#[inline]
fn fx_bind_slots(pipe: DrawPipe) -> ([u32; 2], u32) {
    let binds = &pipe.row().spec().binds;
    let [r8, rgba] = binds.fragment_textures else {
        panic!(
            "row {:?} binds the fx atlases at two fragment textures",
            pipe.row()
        )
    };
    let palette = binds
        .fragment_palette
        .unwrap_or_else(|| panic!("row {:?} binds no fx palette", pipe.row()));
    ([*r8, *rgba], palette)
}

#[cfg(gl_arm)]
impl GlFrameRig<'_> {
    #[inline]
    fn pipeline(&self, pipe: DrawPipe) -> crate::device_layer::FramePipeline<'_> {
        let row = pipe.row();
        crate::device_layer::FramePipeline::Gl(
            self.pipelines[row as usize].unwrap_or_else(|| {
                panic!("gl frame rig: no program for row {row:?} (pipe {pipe:?})")
            }),
        )
    }

    #[inline]
    fn atlas(&self, atlas: DrawAtlas, pipe: DrawPipe) -> crate::device_layer::FrameAtlas<'_> {
        if atlas == DrawAtlas::Fx {
            let fx = self
                .fx
                .unwrap_or_else(|| panic!("gl frame rig: no fx bind (pipe {pipe:?})"));
            let ([u0, u1], u2) = fx_bind_slots(pipe);
            return crate::device_layer::FrameAtlas::GlFx {
                r8: fx.r8,
                rgba: fx.rgba,
                palette: fx.palette,
                sampler: fx.sampler,
                units: [u0, u1, u2],
            };
        }
        let unit = *pipe
            .row()
            .spec()
            .binds
            .fragment_textures
            .first()
            .unwrap_or_else(|| panic!("row {:?} samples no fragment texture", pipe.row()));
        let (tex, sampler) = self.atlases[atlas as usize]
            .unwrap_or_else(|| panic!("gl frame rig: no atlas for {atlas:?} (pipe {pipe:?})"));
        crate::device_layer::FrameAtlas::Gl { tex, sampler, unit }
    }

    #[inline]
    fn stream(&self, id: StreamId) -> crate::device_layer::FrameStream<'_> {
        crate::device_layer::FrameStream::Gl(
            self.streams[id as usize]
                .unwrap_or_else(|| panic!("gl frame rig: no stream buffer for {id:?}")),
        )
    }
}

/// The D3D11 arm's resolution set, the twin of [`GlFrameRig`].
#[cfg(d3d11_arm)]
pub(crate) struct D3dFrameRig<'a> {
    pub(crate) offscreen: &'a crate::d3d11::resources::D3dTexture,
    /// Pipelines by [`Pipeline`] row discriminant; `None` == the plan never
    /// draws that row (resolving it is a rig-construction bug, panics by name).
    pub(crate) pipelines:
        [Option<&'a crate::d3d11::pipelines::D3dPipeline>; pipeline_table::PIPELINE_COUNT],
    /// The `cell.hlsl` `Uniforms` buffer (`b0`).
    pub(crate) uniform: &'a crate::d3d11::resources::D3dBuffer,
    /// `BindSpec::vertex_uniform` of the drawn rows (one per frame).
    pub(crate) uniform_binding: u32,
    /// Instance streams by [`StreamId`] discriminant.
    pub(crate) streams: [Option<&'a crate::d3d11::resources::D3dBuffer>; STREAM_COUNT],
    /// Atlas texture + sampler by [`DrawAtlas`] discriminant.
    pub(crate) atlases: [Option<(
        &'a crate::d3d11::resources::D3dTexture,
        &'a crate::d3d11::resources::D3dSampler,
    )>; DRAW_ATLAS_COUNT],
    /// The fx bind: the Index8 and RGBA atlases, this frame's palette texture
    /// and the nearest sampler, when the frame draws fx.
    pub(crate) fx: Option<D3dFxBind<'a>>,
}

/// The D3D11 rig's [`DrawAtlas::Fx`] resources.
#[cfg(d3d11_arm)]
#[derive(Clone, Copy)]
pub(crate) struct D3dFxBind<'a> {
    pub(crate) r8: &'a crate::d3d11::resources::D3dTexture,
    pub(crate) rgba: &'a crate::d3d11::resources::D3dTexture,
    pub(crate) palette: &'a crate::d3d11::resources::D3dTexture,
    pub(crate) sampler: &'a crate::d3d11::resources::D3dSampler,
}

#[cfg(d3d11_arm)]
impl D3dFrameRig<'_> {
    #[inline]
    fn pipeline(&self, pipe: DrawPipe) -> crate::device_layer::FramePipeline<'_> {
        let row = pipe.row();
        crate::device_layer::FramePipeline::D3d11(self.pipelines[row as usize].unwrap_or_else(
            || panic!("d3d11 frame rig: no pipeline for row {row:?} (pipe {pipe:?})"),
        ))
    }

    /// The row's `BindSpec` names the slot (`t<n>` with `s<n>`).
    #[inline]
    fn atlas(&self, atlas: DrawAtlas, pipe: DrawPipe) -> crate::device_layer::FrameAtlas<'_> {
        if atlas == DrawAtlas::Fx {
            let fx = self
                .fx
                .unwrap_or_else(|| panic!("d3d11 frame rig: no fx bind (pipe {pipe:?})"));
            let ([u0, u1], u2) = fx_bind_slots(pipe);
            return crate::device_layer::FrameAtlas::D3d11Fx {
                r8: fx.r8,
                rgba: fx.rgba,
                palette: fx.palette,
                sampler: fx.sampler,
                units: [u0, u1, u2],
            };
        }
        let unit = *pipe
            .row()
            .spec()
            .binds
            .fragment_textures
            .first()
            .unwrap_or_else(|| panic!("row {:?} samples no fragment texture", pipe.row()));
        let (tex, sampler) = self.atlases[atlas as usize]
            .unwrap_or_else(|| panic!("d3d11 frame rig: no atlas for {atlas:?} (pipe {pipe:?})"));
        crate::device_layer::FrameAtlas::D3d11 { tex, sampler, unit }
    }

    #[inline]
    fn stream(&self, id: StreamId) -> crate::device_layer::FrameStream<'_> {
        crate::device_layer::FrameStream::D3d11(
            self.streams[id as usize]
                .unwrap_or_else(|| panic!("d3d11 frame rig: no stream buffer for {id:?}")),
        )
    }
}

/// The Metal arm's resolution set, minted on one loss domain.
#[cfg(target_os = "macos")]
pub(crate) struct MetalFrameRig {
    pub(crate) offscreen: crate::metal::resources::SealedTexture,
    pub(crate) offscreen_srgb: crate::metal::resources::SealedTexture,
    pub(crate) psos: [Option<crate::metal::ffi::Obj>; pipeline_table::PIPELINE_COUNT],
    /// The framebuffer-fetch twins the plan's fetch draws bind, by [`pipeline_table::FetchTwin`].
    pub(crate) fetch_psos: [Option<crate::metal::ffi::Obj>; pipeline_table::FETCH_TWIN_COUNT],
    pub(crate) uniform: crate::metal::ffi::Obj,
    pub(crate) uniform_vertex_slot: u32,
    pub(crate) uniform_fragment_slot: Option<u32>,
    pub(crate) streams: [Option<crate::metal::resources::SharedBuffer>; STREAM_COUNT],
    pub(crate) atlases: [Option<(
        crate::metal::resources::SealedTexture,
        crate::metal::ffi::Obj,
    )>; DRAW_ATLAS_COUNT],
    pub(crate) fx: Option<MetalFxBind>,
}

/// The Metal rig's [`DrawAtlas::Fx`] resources, owned +1 for the frame.
#[cfg(target_os = "macos")]
pub(crate) struct MetalFxBind {
    pub(crate) r8: crate::metal::resources::SealedTexture,
    pub(crate) rgba: crate::metal::resources::SealedTexture,
    pub(crate) palette: crate::metal::ffi::Obj,
}

#[cfg(target_os = "macos")]
impl MetalFrameRig {
    #[inline]
    const fn uniforms(&self) -> crate::device_layer::FrameUniforms<'_> {
        crate::device_layer::FrameUniforms::Metal {
            buf: &self.uniform,
            vertex_slot: self.uniform_vertex_slot,
            fragment_slot: self.uniform_fragment_slot,
        }
    }

    #[inline]
    fn pipeline(&self, pipe: DrawPipe, fetch: bool) -> crate::device_layer::FramePipeline<'_> {
        let row = pipe.row();
        let pso = if fetch {
            let twin = pipeline_table::FetchTwin::of(row)
                .unwrap_or_else(|| panic!("metal frame rig: row {row:?} has no fetch twin"));
            self.fetch_psos[twin as usize].as_ref()
        } else {
            self.psos[row as usize].as_ref()
        };
        crate::device_layer::FramePipeline::Metal(pso.unwrap_or_else(|| {
            panic!("metal frame rig: no PSO for row {row:?} (pipe {pipe:?}, fetch {fetch})")
        }))
    }

    #[inline]
    fn atlas(&self, atlas: DrawAtlas, pipe: DrawPipe) -> crate::device_layer::FrameAtlas<'_> {
        if atlas == DrawAtlas::Fx {
            let fx = self
                .fx
                .as_ref()
                .unwrap_or_else(|| panic!("metal frame rig: no fx bind (pipe {pipe:?})"));
            let (tex_slots, palette_slot) = fx_bind_slots(pipe);
            return crate::device_layer::FrameAtlas::MetalFx {
                r8: fx.r8.obj(),
                rgba: fx.rgba.obj(),
                palette: &fx.palette,
                tex_slots,
                palette_slot,
            };
        }
        let binds = &pipe.row().spec().binds;
        let (&tex_slot, &sampler_slot) = (
            binds
                .fragment_textures
                .first()
                .unwrap_or_else(|| panic!("row {:?} samples no fragment texture", pipe.row())),
            binds
                .fragment_samplers
                .first()
                .unwrap_or_else(|| panic!("row {:?} binds no fragment sampler", pipe.row())),
        );
        let (tex, sampler) = self.atlases[atlas as usize].as_ref().unwrap_or_else(|| {
            panic!("metal frame rig: no atlas minted for {atlas:?} (pipe {pipe:?})")
        });
        crate::device_layer::FrameAtlas::Metal {
            tex: tex.obj(),
            sampler,
            tex_slot,
            sampler_slot,
        }
    }

    #[inline]
    fn stream(&self, id: StreamId) -> crate::device_layer::FrameStream<'_> {
        crate::device_layer::FrameStream::Metal(
            self.streams[id as usize]
                .as_ref()
                .unwrap_or_else(|| panic!("metal frame rig: no stream buffer for {id:?}"))
                .obj(),
        )
    }
}

fn first_party_note(what: &str) {
    #[cfg(target_os = "macos")]
    metal_arm_note(what);
    #[cfg(gl_arm)]
    gl_arm::gl_arm_note(what);
    #[cfg(not(any(target_os = "macos", gl_arm)))]
    crate::stderr_line!("aterm-gpu: {what}");
}

#[cfg(target_os = "macos")]
fn metal_arm_note(what: &str) {
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::BTreeSet::new()));
    if seen
        .lock()
        .expect("the note set is never poisoned (no panics under the lock)")
        .insert(what.to_owned())
    {
        crate::stderr_line!("aterm-gpu (metal arm): {what}");
    }
}

#[cfg(target_os = "macos")]
struct ArmAtlas {
    tex: crate::device_layer::LayerTexture,
    key: (u32, u32, u64),
    write_gen: u64,
    uploaded_py: u32,
}

#[cfg(target_os = "macos")]
struct ArmStream {
    buf: crate::device_layer::LayerBuffer,
    cap: usize,
}

#[cfg(target_os = "macos")]
const ARM_FRAME_SLOTS: usize = 3;

#[cfg(target_os = "macos")]
const ARM_SUBMIT_B_SLOTS: usize = 5;

#[cfg(target_os = "macos")]
struct ArmFrameSlot {
    uniform: crate::device_layer::LayerBuffer,
    uniform_key: Option<(u32, u32, u32)>,
    streams: [Option<ArmStream>; STREAM_COUNT],
    fx_palette: Option<ArmStream>,
    inflight: Option<crate::metal::encoder::Submitted>,
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct ArmSubmitBSlot {
    glow_stream: Option<ArmStream>,
    bloom_cell_u: Option<ArmStream>,
    bloom_u: Option<ArmStream>,
    shimmer_u: Option<ArmStream>,
    tray_u: Option<ArmStream>,
    crown_u: Option<ArmStream>,
    blit_u: Option<(BlitUniform, crate::device_layer::LayerBuffer)>,
}

#[cfg(target_os = "macos")]
struct MetalArmLive {
    /// The first-frame warm this arm waits out and takes the fetch verdict from
    /// ([`crate::metal::warm::FIRST_FRAME`], or a test's own latch).
    warm: &'static crate::metal::warm::WarmLatch,
    mint: crate::metal::resources::MetalResourceDevice,
    session: crate::metal::encoder::EncodeSession,
    lib: crate::metal::ffi::Library,
    psos: [Option<crate::metal::ffi::Obj>; pipeline_table::PIPELINE_COUNT],
    /// The device's framebuffer-fetch verdict, `None` until the first frame that draws Unorm-law
    /// light settles it ([`Self::fetch_verdict`]); [`FetchProbe::Passed`] routes those draws
    /// through [`Self::fetch_psos`].
    fetch: Option<FetchProbe>,
    fetch_psos: [Option<crate::metal::ffi::Obj>; pipeline_table::FETCH_TWIN_COUNT],
    nearest: crate::metal::ffi::Obj,
    linear: crate::metal::ffi::Obj,
    frame_slots: [ArmFrameSlot; ARM_FRAME_SLOTS],
    cur_slot: usize,
    submit_b_slots: [ArmSubmitBSlot; ARM_SUBMIT_B_SLOTS],
    submit_b_idx: usize,
    atlases: [Option<ArmAtlas>; DRAW_ATLAS_COUNT],
    libs: Vec<(ShaderLibrary, crate::metal::ffi::Library)>,
    present_psos: Vec<(
        (Pipeline, crate::metal::ffi::PixelFormat),
        crate::metal::ffi::Obj,
    )>,
    pending: Vec<PendingPresent>,
    present_seq: u64,
    awaited_frames: u64,
    frame_slot_wait_ns: u64,
    pending_ring_wait_ns: u64,
    encode_inline_waits: std::cell::Cell<u64>,
    encode_band_shifts: std::cell::Cell<u64>,
    fx_tex: [Option<fx_gpu::FxArmTex>; 2],
    fx_hazard: FxHazard,
    /// The last fx sync's reader settle ([`Self::sync_fx`]): the frames in flight it waited for
    /// before rewriting a region they may read, and the nanoseconds it parked.
    fx_reader_wait: (u64, u64),
    #[cfg(any(test, debug_assertions))]
    fx_shared_palette: Option<(crate::device_layer::LayerTexture, u32)>,
    /// Pipeline states the warms ([`crate::metal::warm::spawn`]) built off this thread that this
    /// arm has not adopted yet ([`Self::adopt_warmed`]), and the sender each warm's worker takes.
    warmed: (
        std::sync::mpsc::Sender<crate::metal::warm::Warmed>,
        std::sync::mpsc::Receiver<crate::metal::warm::Warmed>,
    ),
    /// What a pipeline warm is building for this arm and has not handed over.
    warm_owed: Vec<crate::metal::warm::WarmItem>,
    /// The pipeline states this arm built on its own thread, in order.
    #[cfg(test)]
    built_here: Vec<crate::metal::warm::WarmItem>,
}

#[cfg(target_os = "macos")]
struct PendingPresent {
    cb: crate::metal::encoder::Submitted,
    ticket: crate::metal::swapchain::PresentTicket,
    seq: u64,
}

#[cfg(all(target_os = "macos", test))]
thread_local! {
    static INJECTED_MINT_FAILURE: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

/// The fx hazard drill's record of a Metal arm (zero-sized outside test and debug builds).
#[cfg(target_os = "macos")]
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct FxHazard {
    #[cfg(any(test, debug_assertions))]
    pub(crate) settles: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) settle_ns: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) park_ns: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) palette: usize,
}

#[cfg(target_os = "macos")]
impl FxHazard {
    #[inline]
    const fn parked(&mut self, ns: u64) {
        #[cfg(any(test, debug_assertions))]
        {
            self.park_ns = self.park_ns.saturating_add(ns);
        }
        #[cfg(not(any(test, debug_assertions)))]
        let _ = ns;
    }

    #[inline]
    fn settled(&mut self, ns: u64) {
        #[cfg(any(test, debug_assertions))]
        {
            self.settles += 1;
            self.settle_ns = self.settle_ns.saturating_add(ns);
        }
        self.parked(ns);
    }

    #[inline]
    fn bound(&mut self, palette: &crate::metal::ffi::Obj) {
        #[cfg(any(test, debug_assertions))]
        {
            self.palette = palette.id().as_ptr().addr();
        }
        #[cfg(not(any(test, debug_assertions)))]
        let _ = palette;
    }
}

#[cfg(target_os = "macos")]
impl MetalArmLive {
    fn new(
        latch: Arc<crate::metal::loss::LossLatch>,
        warm: &'static crate::metal::warm::WarmLatch,
    ) -> Result<Self, String> {
        #[cfg(test)]
        if let Some(injected) = INJECTED_MINT_FAILURE.with(std::cell::Cell::take) {
            return Err(injected.to_owned());
        }
        let Some(dev) = crate::metal::ffi::Device::preferred() else {
            return Err("no Metal device on this machine".to_owned());
        };
        let session = crate::metal::encoder::EncodeSession::new(&dev, Arc::clone(&latch))?;
        let mint = crate::metal::resources::MetalResourceDevice::new(&dev, latch);
        let lib = crate::metal::pipelines::compile_library(&dev, ShaderLibrary::Cell)?;
        let fetch = crate::metal::fetch::known(&dev, &lib);
        let layer = crate::device_layer::DeviceHandle::Metal(&mint);
        let nearest = layer
            .try_create_sampler(
                "aterm-gpu metal nearest sampler",
                crate::device_layer::SamplerKind::NearestClamp,
            )?
            .into_metal();
        let linear = layer
            .try_create_sampler(
                "aterm-gpu metal linear sampler",
                crate::device_layer::SamplerKind::LinearClamp,
            )?
            .into_metal();
        let mint_uniform = || {
            layer.try_create_uniform_buffer(
                "aterm-gpu metal frame uniforms",
                std::mem::size_of::<Uniforms>() as u64,
            )
        };
        let (u0, u1, u2) = (mint_uniform()?, mint_uniform()?, mint_uniform()?);
        let frame_slots = [u0, u1, u2].map(|uniform| ArmFrameSlot {
            uniform,
            uniform_key: None,
            streams: std::array::from_fn(|_| None),
            fx_palette: None,
            inflight: None,
        });
        Ok(Self {
            warm,
            mint,
            session,
            lib,
            psos: std::array::from_fn(|_| None),
            fetch,
            fetch_psos: std::array::from_fn(|_| None),
            nearest,
            linear,
            frame_slots,
            cur_slot: 0,
            submit_b_slots: std::array::from_fn(|_| ArmSubmitBSlot::default()),
            submit_b_idx: 0,
            atlases: std::array::from_fn(|_| None),
            libs: Vec::new(),
            present_psos: Vec::new(),
            pending: Vec::new(),
            present_seq: 0,
            awaited_frames: 0,
            frame_slot_wait_ns: 0,
            pending_ring_wait_ns: 0,
            encode_inline_waits: std::cell::Cell::new(0),
            encode_band_shifts: std::cell::Cell::new(0),
            fx_tex: [None, None],
            fx_hazard: FxHazard::default(),
            fx_reader_wait: (0, 0),
            #[cfg(any(test, debug_assertions))]
            fx_shared_palette: None,
            warmed: std::sync::mpsc::channel(),
            warm_owed: Vec::new(),
            #[cfg(test)]
            built_here: Vec::new(),
        })
    }

    #[inline]
    const fn layer(&self) -> crate::device_layer::DeviceHandle<'_> {
        crate::device_layer::DeviceHandle::Metal(&self.mint)
    }

    #[inline]
    const fn current_slot(&self) -> &ArmFrameSlot {
        &self.frame_slots[self.cur_slot]
    }

    #[inline]
    const fn current_slot_mut(&mut self) -> &mut ArmFrameSlot {
        &mut self.frame_slots[self.cur_slot]
    }

    fn library(&mut self, lib: ShaderLibrary) -> Result<&crate::metal::ffi::Library, String> {
        if !self.libs.iter().any(|(l, _)| *l == lib) {
            self.warm
                .await_item(crate::metal::warm::WarmItem::Library(lib));
            let compiled = crate::metal::pipelines::compile_library(self.mint.device(), lib)?;
            self.libs.push((lib, compiled));
        }
        Ok(self
            .libs
            .iter()
            .find(|(l, _)| *l == lib)
            .map(|(_, c)| c)
            .expect("inserted above"))
    }

    /// `row`'s state against a `present`-format destination: a `cell.metal` row draws into the
    /// offscreen whatever the destination (the bloom's glow extract), so it is the frame's own
    /// ([`Self::pso`]); any other is keyed by the attachment format it resolves to.
    fn present_pso(
        &mut self,
        row: Pipeline,
        present: crate::metal::ffi::PixelFormat,
    ) -> Result<crate::metal::ffi::Obj, String> {
        let spec = row.spec();
        if matches!(spec.library, ShaderLibrary::Cell) {
            return self.pso(row).map(crate::metal::ffi::Obj::clone_retained);
        }
        let key = (
            row,
            crate::metal::pipelines::metal_format(spec.target, present),
        );
        if !self.present_psos.iter().any(|(k, _)| *k == key) {
            self.adopt_warmed();
        }
        if let Some((_, pso)) = self.present_psos.iter().find(|(k, _)| *k == key) {
            return Ok(pso.clone_retained());
        }
        let _ = self.library(spec.library)?;
        let lib = self
            .libs
            .iter()
            .find(|(l, _)| *l == spec.library)
            .map(|(_, c)| c)
            .expect("compiled above");
        let item = crate::metal::warm::WarmItem::pipeline(row, present);
        self.warm.await_item(item);
        let pso = crate::metal::pipelines::build(self.mint.device(), lib, spec, present)?;
        #[cfg(test)]
        self.built_here.push(item);
        self.present_psos.push((key, pso.clone_retained()));
        Ok(pso)
    }

    fn await_frame_slot(&mut self) {
        self.cur_slot = (self.cur_slot + 1) % ARM_FRAME_SLOTS;
        for s in 0..ARM_FRAME_SLOTS {
            if s != self.cur_slot
                && self.frame_slots[s]
                    .inflight
                    .as_ref()
                    .is_some_and(|sub| sub.try_outcome().is_some())
            {
                let _ = self.frame_slots[s].inflight.take();
                self.awaited_frames += 1;
            }
        }
        if let Some(prev) = self.frame_slots[self.cur_slot].inflight.take() {
            // `try_outcome` feeds the loss latch like `wait_outcome`.
            if prev.try_outcome().is_none() {
                let parked = aterm_time::Instant::now();
                let _ = prev.wait_outcome();
                let ns = u64::try_from(parked.elapsed().as_nanos()).unwrap_or(u64::MAX);
                self.frame_slot_wait_ns = self.frame_slot_wait_ns.saturating_add(ns);
                self.fx_hazard.parked(ns);
            }
            self.awaited_frames += 1;
        }
    }

    fn await_atlas_readers(frame_slots: &mut [ArmFrameSlot]) -> (u64, u64) {
        let mut consumed = 0;
        let mut parked_ns = 0u64;
        for slot in frame_slots {
            if let Some(submitted) = slot.inflight.take() {
                if submitted.try_outcome().is_none() {
                    let parked = aterm_time::Instant::now();
                    let _ = submitted.wait_outcome();
                    parked_ns = parked_ns.saturating_add(
                        u64::try_from(parked.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                }
                consumed += 1;
            }
        }
        (consumed, parked_ns)
    }

    /// Wait for every frame in flight before an fx atlas region rewrite:
    /// how many it waited for and the nanoseconds it parked.
    fn settle_for_fx(
        frame_slots: &mut [ArmFrameSlot],
        awaited_frames: &mut u64,
        frame_slot_wait_ns: &mut u64,
        fx_hazard: &mut FxHazard,
    ) -> (u64, u64) {
        let (consumed, ns) = Self::await_atlas_readers(frame_slots);
        *awaited_frames += consumed;
        *frame_slot_wait_ns = frame_slot_wait_ns.saturating_add(ns);
        fx_hazard.settled(ns);
        (consumed, ns)
    }

    #[inline]
    const fn advance_submit_b_slot(&mut self) {
        self.submit_b_idx = (self.submit_b_idx + 1) % ARM_SUBMIT_B_SLOTS;
    }

    fn upload_scratch_stream(
        layer: crate::device_layer::DeviceHandle<'_>,
        slot: &mut Option<ArmStream>,
        bytes: &[u8],
    ) -> Result<crate::metal::ffi::Obj, String> {
        let need = bytes.len().max(16);
        if slot.as_ref().is_none_or(|s| s.cap < need) {
            let cap = need
                .next_power_of_two()
                .max(slot.as_ref().map_or(256, |s| s.cap));
            *slot = Some(ArmStream {
                buf: layer
                    .try_create_instance_buffer("aterm-gpu metal submit-b stream", cap as u64)?,
                cap,
            });
        }
        let stream = slot.as_ref().expect("sized above");
        if !bytes.is_empty() {
            // SAFETY: shared storage of at least `bytes.len()`; `drain_pending` guarantees this
            // ring slot is idle.
            unsafe { layer.write_buffer_from_zero(&stream.buf, bytes) };
        }
        Ok(stream.buf.metal().clone_retained())
    }

    fn ensure_submit_b_blit_u(
        &mut self,
        want: BlitUniform,
    ) -> Result<crate::metal::ffi::Obj, String> {
        let layer = crate::device_layer::DeviceHandle::Metal(&self.mint);
        let slot = &mut self.submit_b_slots[self.submit_b_idx].blit_u;
        if let Some((memo, buf)) = slot.as_mut() {
            if aterm_bits::bytes_of(memo) != aterm_bits::bytes_of(&want) {
                // SAFETY: `buf` is a shared-storage buffer of `size_of::<BlitUniform>()` owned by
                // this idle ring slot.
                unsafe { layer.write_buffer_from_zero(buf, aterm_bits::bytes_of(&want)) };
                *memo = want;
            }
            return Ok(buf.metal().clone_retained());
        }
        let buf = layer.try_create_uniform_buffer(
            "aterm-gpu metal blit uniform",
            std::mem::size_of::<BlitUniform>() as u64,
        )?;
        // SAFETY: freshly minted shared buffer of `size_of::<BlitUniform>()`.
        unsafe { layer.write_buffer_from_zero(&buf, aterm_bits::bytes_of(&want)) };
        let handle = buf.metal().clone_retained();
        *slot = Some((want, buf));
        Ok(handle)
    }

    #[inline]
    const fn take_present_seq(&mut self) -> u64 {
        let seq = self.present_seq;
        self.present_seq = seq.wrapping_add(1);
        seq
    }

    fn await_presents_through(&mut self, seq: u64) {
        let parked = aterm_time::Instant::now();
        let mut waited = false;
        for present in self
            .pending
            .iter()
            .filter(|present| crate::tray_card_ring::present_may_read(present.seq, seq))
        {
            if present.cb.try_outcome().is_none() {
                let _ = present.cb.wait_outcome();
                waited = true;
            }
        }
        if waited {
            self.pending_ring_wait_ns = self
                .pending_ring_wait_ns
                .saturating_add(u64::try_from(parked.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
    }

    fn drain_pending(&mut self) {
        self.pending
            .retain(|p| p.cb.try_outcome().is_none() || p.ticket.try_outcome().is_none());
        while self.pending.len() > 3 {
            let oldest = self.pending.remove(0);
            let parked = aterm_time::Instant::now();
            let _ = oldest.cb.wait_outcome();
            let _ = oldest.ticket.wait_outcome();
            self.pending_ring_wait_ns = self
                .pending_ring_wait_ns
                .saturating_add(u64::try_from(parked.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
    }

    #[inline]
    fn take_park_ns(&mut self) -> (u64, u64) {
        (
            std::mem::take(&mut self.frame_slot_wait_ns),
            std::mem::take(&mut self.pending_ring_wait_ns),
        )
    }

    #[inline]
    fn wait_inline(&self, sub: &crate::metal::encoder::Submitted) -> crate::metal::loss::CbOutcome {
        self.encode_inline_waits
            .set(self.encode_inline_waits.get().saturating_add(1));
        sub.wait_outcome()
    }

    fn pso(&mut self, row: Pipeline) -> Result<&crate::metal::ffi::Obj, String> {
        let spec = row.spec();
        if !matches!(spec.library, ShaderLibrary::Cell) {
            return Err(format!(
                "metal arm: frame row {row:?} claims library {:?} (want Cell)",
                spec.library
            ));
        }
        if self.psos[row as usize].is_none() {
            self.adopt_warmed();
        }
        let slot = &mut self.psos[row as usize];
        if slot.is_none() {
            let item = crate::metal::warm::WarmItem::pipeline(
                row,
                crate::metal::ffi::PixelFormat::Bgra8Unorm,
            );
            self.warm.await_item(item);
            *slot = Some(crate::metal::pipelines::build(
                self.mint.device(),
                &self.lib,
                spec,
                crate::metal::ffi::PixelFormat::Bgra8Unorm,
            )?);
            #[cfg(test)]
            self.built_here.push(item);
        }
        Ok(slot.as_ref().expect("filled above"))
    }

    /// The device's framebuffer-fetch verdict for a frame that draws Unorm-law light: a warm's
    /// once it landed; `None` (the split, for this frame) while the first-frame warm or this arm's
    /// warm still owes it, so no frame waits on the probe; else the process's known verdict or
    /// this arm's own probe ([`crate::metal::fetch::verdict`]).
    fn fetch_verdict(&mut self) -> Option<FetchProbe> {
        if self.fetch.is_none() {
            self.adopt_warmed();
        }
        if self.fetch.is_none()
            && !self.warm.probe_pending()
            && !self
                .warm_owed
                .contains(&crate::metal::warm::WarmItem::FetchProbe)
        {
            let known = crate::metal::fetch::known(self.mint.device(), &self.lib);
            #[cfg(test)]
            if known.is_none() {
                self.built_here
                    .push(crate::metal::warm::WarmItem::FetchProbe);
            }
            self.fetch = Some(known.unwrap_or_else(|| {
                crate::metal::fetch::verdict(&self.mint, &self.session, &self.lib)
            }));
        }
        self.fetch
    }

    /// The framebuffer-fetch twin's PSO, a pipeline warm's or built on first use.
    fn fetch_pso(
        &mut self,
        twin: pipeline_table::FetchTwin,
    ) -> Result<&crate::metal::ffi::Obj, String> {
        if self.fetch_psos[twin as usize].is_none() {
            self.adopt_warmed();
        }
        let slot = &mut self.fetch_psos[twin as usize];
        if slot.is_none() {
            let item = crate::metal::warm::WarmItem::Twin(twin);
            self.warm.await_item(item);
            *slot = Some(crate::metal::pipelines::build(
                self.mint.device(),
                &self.lib,
                &twin.spec(),
                crate::metal::ffi::PixelFormat::Bgra8Unorm,
            )?);
            #[cfg(test)]
            self.built_here.push(item);
        }
        Ok(slot.as_ref().expect("filled above"))
    }

    /// Whether this arm holds `item`'s pipeline state.
    fn holds(&self, item: crate::metal::warm::WarmItem) -> bool {
        use crate::metal::warm::WarmItem;
        match item {
            WarmItem::Pipeline(row, _) if matches!(row.spec().library, ShaderLibrary::Cell) => {
                self.psos[row as usize].is_some()
            }
            WarmItem::Pipeline(row, present) => {
                self.present_psos.iter().any(|(k, _)| *k == (row, present))
            }
            WarmItem::Twin(twin) => self.fetch_psos[twin as usize].is_some(),
            WarmItem::FetchProbe => self.fetch.is_some(),
            WarmItem::Library(_) => false,
        }
    }

    /// Take one warm's build: its state unless this arm built one meanwhile, or the verdict its
    /// probe settled for the process (none where the probe could not run, so a frame probes again).
    fn adopt(&mut self, warmed: crate::metal::warm::Warmed) {
        use crate::metal::warm::{WarmItem, Warmed};
        let Warmed { item, pso } = warmed;
        self.warm_owed.retain(|owed| *owed != item);
        if item == WarmItem::FetchProbe && self.fetch.is_none() {
            self.fetch = crate::metal::fetch::known(self.mint.device(), &self.lib);
        }
        let Some(pso) = pso else {
            return;
        };
        match item {
            WarmItem::Pipeline(row, _) if matches!(row.spec().library, ShaderLibrary::Cell) => {
                let _ = self.psos[row as usize].get_or_insert(pso);
            }
            WarmItem::Pipeline(row, present) => {
                if !self.holds(item) {
                    self.present_psos.push(((row, present), pso));
                }
            }
            WarmItem::Twin(twin) => {
                let _ = self.fetch_psos[twin as usize].get_or_insert(pso);
            }
            WarmItem::Library(_) | WarmItem::FetchProbe => {}
        }
    }

    /// Take what the pipeline warms built since the last look, without waiting.
    fn adopt_warmed(&mut self) {
        while let Ok(warmed) = self.warmed.1.try_recv() {
            self.adopt(warmed);
        }
    }

    /// Start a pipeline warm for what this arm's frames and presents can bind under `knobs`
    /// ([`crate::metal::warm::bindable`]) that it neither holds nor is owed: the twins unless the
    /// verdict is known and not a pass, led by the fetch probe where the verdict is unknown and no
    /// first-frame warm owes it, so no frame probes on this thread.
    fn warm_pipelines(&mut self, knobs: crate::metal::warm::PresentKnobs) {
        use crate::metal::warm::WarmItem;
        self.adopt_warmed();
        let verdict = self
            .fetch
            .or_else(|| crate::metal::fetch::known(self.mint.device(), &self.lib));
        let twins = verdict.is_none_or(|v| v == FetchProbe::Passed);
        let probe =
            (verdict.is_none() && !self.warm.probe_pending()).then_some(WarmItem::FetchProbe);
        let items: Vec<WarmItem> = probe
            .into_iter()
            .chain(crate::metal::warm::bindable(knobs, twins))
            .filter(|item| !self.holds(*item) && !self.warm_owed.contains(item))
            .collect();
        if items.is_empty() {
            return;
        }
        match crate::metal::warm::spawn(
            self.mint.device(),
            self.warm,
            items.clone(),
            self.warmed.0.clone(),
        ) {
            Ok(_detached) => self.warm_owed.extend(items),
            Err(e) => metal_arm_note(&format!(
                "the pipeline warm could not start ({e}); frames build their own"
            )),
        }
    }

    /// Wait (a hang detector of `hang`) until the warms built all they owe, put each build back
    /// for the frames to take, and return the items whose build failed (never the probe, which
    /// hands over no state).
    #[cfg(test)]
    fn await_warmed(&mut self, hang: std::time::Duration) -> Vec<crate::metal::warm::WarmItem> {
        let began = aterm_time::Instant::now();
        let mut built = Vec::new();
        while built.len() < self.warm_owed.len() {
            let left = hang
                .checked_sub(began.elapsed())
                .expect("the pipeline warm never built what it owed");
            built.push(
                self.warmed
                    .1
                    .recv_timeout(left)
                    .expect("the pipeline warm never built what it owed"),
            );
        }
        let failed = built
            .iter()
            .filter(|warmed| {
                warmed.pso.is_none() && warmed.item != crate::metal::warm::WarmItem::FetchProbe
            })
            .map(|warmed| warmed.item)
            .collect();
        for warmed in built {
            self.warmed
                .0
                .send(warmed)
                .expect("the arm holds its own receiver");
        }
        failed
    }

    #[inline]
    fn ensure_uniform(&mut self, w: u32, h: u32, text_blend: u32) {
        let layer = crate::device_layer::DeviceHandle::Metal(&self.mint);
        let slot = &mut self.frame_slots[self.cur_slot];
        if slot.uniform_key == Some((w, h, text_blend)) {
            return;
        }
        // SAFETY: `slot.uniform` is a shared-storage `Uniforms` buffer and `cur_slot` was awaited
        // at staging start.
        unsafe {
            layer.write_buffer_from_zero(
                &slot.uniform,
                aterm_bits::bytes_of(&Uniforms {
                    screen: [w as f32, h as f32],
                    text_blend: text_blend as f32,
                    _pad: 0.0,
                }),
            );
        }
        slot.uniform_key = Some((w, h, text_blend));
    }

    fn upload_stream(&mut self, id: StreamId, bytes: &[u8]) -> Result<bool, String> {
        if !should_slice(bytes.len()) {
            return Ok(false);
        }
        let layer = crate::device_layer::DeviceHandle::Metal(&self.mint);
        let slot = &mut self.frame_slots[self.cur_slot].streams[id as usize];
        let need = bytes.len();
        if slot.as_ref().is_none_or(|s| s.cap < need) {
            let cap = need
                .next_power_of_two()
                .max(slot.as_ref().map_or(0, |s| s.cap));
            *slot = Some(ArmStream {
                buf: layer
                    .try_create_instance_buffer("aterm-gpu metal frame stream", cap as u64)?,
                cap,
            });
        }
        let stream = slot.as_ref().expect("sized above");
        // SAFETY: shared storage of at least `bytes.len()` and `cur_slot` was awaited at staging
        // start.
        unsafe { layer.write_buffer_from_zero(&stream.buf, bytes) };
        Ok(true)
    }

    fn upload_fx_palette(&mut self, words: &[u32]) -> Result<crate::metal::ffi::Obj, String> {
        let layer = crate::device_layer::DeviceHandle::Metal(&self.mint);
        let slot = &mut self.frame_slots[self.cur_slot].fx_palette;
        let bytes: &[u8] = aterm_bits::cast_slice(words);
        let need = bytes.len().max(16);
        if slot.as_ref().is_none_or(|s| s.cap < need) {
            let cap = need
                .next_power_of_two()
                .max(slot.as_ref().map_or(256, |s| s.cap));
            *slot = Some(ArmStream {
                buf: layer.try_create_instance_buffer("aterm-gpu metal fx palette", cap as u64)?,
                cap,
            });
        }
        let stream = slot.as_ref().expect("sized above");
        if !bytes.is_empty() {
            // SAFETY: shared storage of at least `bytes.len()`, owned by
            // `cur_slot`, which staging awaited (`await_frame_slot`): no frame
            // in flight reads it. No other slot's palette is touched.
            unsafe { layer.write_buffer_from_zero(&stream.buf, bytes) };
        }
        Ok(stream.buf.metal().clone_retained())
    }

    fn sync_fx(&mut self, atlas: &fx_gpu::FxAtlas, drawing: bool) -> Result<u64, String> {
        let mut bytes = 0;
        self.fx_reader_wait = (0, 0);
        for kind in [fx_gpu::FxAtlasKind::R8, fx_gpu::FxAtlasKind::Rgba] {
            let side = atlas.side(kind);
            if !fx_gpu::fx_tex_due(&self.fx_tex[kind as usize], side, drawing) {
                continue;
            }
            let Self {
                mint,
                fx_tex,
                frame_slots,
                awaited_frames,
                frame_slot_wait_ns,
                fx_hazard,
                fx_reader_wait,
                ..
            } = self;
            let synced = fx_gpu::sync_fx_tex(
                crate::device_layer::DeviceHandle::Metal(mint),
                &mut fx_tex[kind as usize],
                side,
                |_| true,
                || {
                    let (frames, ns) = Self::settle_for_fx(
                        frame_slots,
                        awaited_frames,
                        frame_slot_wait_ns,
                        fx_hazard,
                    );
                    fx_reader_wait.0 += frames;
                    fx_reader_wait.1 = fx_reader_wait.1.saturating_add(ns);
                },
            )
            .map_err(|e| format!("fx {kind:?} atlas: {e}"))?;
            bytes += synced.bytes;
        }
        Ok(bytes)
    }

    #[cfg(any(test, debug_assertions))]
    fn write_shared_palette(&mut self, words: &[u32]) -> Result<(), String> {
        let rows = words.len().div_ceil(1024).max(1) as u32;
        let fits = self
            .fx_shared_palette
            .as_ref()
            .is_some_and(|(_, h)| *h >= rows);
        if !fits {
            let h = rows.next_power_of_two();
            let tex = self.layer().try_create_texture_2d(
                "aterm-gpu fx shared palette (hazard control)",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                1024,
                h,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            )?;
            self.fx_shared_palette = Some((tex, h));
        } else {
            let _ = Self::settle_for_fx(
                &mut self.frame_slots,
                &mut self.awaited_frames,
                &mut self.frame_slot_wait_ns,
                &mut self.fx_hazard,
            );
        }
        let mut padded = words.to_vec();
        padded.resize(rows as usize * 1024, 0);
        let (tex, _) = self.fx_shared_palette.as_ref().expect("minted above");
        self.layer().upload_texture_region(
            tex,
            0,
            0,
            1024,
            rows,
            aterm_bits::cast_slice(&padded),
            1024 * 4,
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_atlas(
        layer: crate::device_layer::DeviceHandle<'_>,
        slot: &mut Option<ArmAtlas>,
        atlas: DrawAtlas,
        key: (u32, u32, u64),
        write_gen: u64,
        cur_py: u32,
        format: crate::device_layer::TexelFormat,
        bytes: &[u8],
        frame_slots: &mut [ArmFrameSlot],
    ) -> Result<(u64, u64), String> {
        let (tw, th, _) = key;
        let bpp = format.bytes_per_texel();
        let row_bytes = (tw * bpp) as usize;
        let full = (tw * th * bpp) as usize;
        if bytes.len() > full || (row_bytes > 0 && !bytes.len().is_multiple_of(row_bytes)) {
            return Err(format!(
                "metal arm: {atlas:?} atlas bytes ({}) invalid for {tw}x{th}x{bpp}",
                bytes.len()
            ));
        }
        let occupied_rows = bytes.len().checked_div(row_bytes).unwrap_or(0) as u32;
        if let Some(a) = slot.as_mut()
            && a.key == key
        {
            if a.write_gen == write_gen {
                return Ok((0, 0));
            }
            let y0 = a.uploaded_py.min(occupied_rows);
            let rows = occupied_rows.saturating_sub(y0);
            let mut waited = (0, 0);
            if rows > 0 {
                waited = Self::await_atlas_readers(frame_slots);
                debug_assert!(frame_slots.iter().all(|slot| slot.inflight.is_none()));
                let start = y0 as usize * row_bytes;
                let end = occupied_rows as usize * row_bytes;
                layer.upload_texture_rows(
                    &a.tex,
                    y0,
                    rows,
                    &bytes[start..end],
                    row_bytes as u32,
                    tw,
                );
            }
            a.write_gen = write_gen;
            a.uploaded_py = cur_py;
            return Ok(waited);
        }
        let tex = layer
            .try_create_texture_2d(
                "aterm-gpu metal atlas",
                format,
                tw,
                th,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            )
            .map_err(|e| format!("metal arm: {atlas:?} atlas: {e}"))?;
        layer.upload_texture_rows(&tex, 0, occupied_rows, bytes, row_bytes as u32, tw);
        *slot = Some(ArmAtlas {
            tex,
            key,
            write_gen,
            uploaded_py: cur_py,
        });
        Ok((0, 0))
    }
}

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace MetalAtlasAndFrameRing

        -- Formal state machine for the Metal glyph-atlas incremental row-band
        -- upload and the 3-slot Submit-A frame ring.
        structure World where
            cpuEpoch : Nat
            cpuWriteGen : Nat
            cpuPy : Nat
            occupiedRows : Nat
            gpuEpoch : Nat
            gpuWriteGen : Nat
            uploadedPy : Nat
            gpuRows : Nat
            remintCount : Nat
            curSlot : Nat
            slot0Busy : Bool
            slot1Busy : Bool
            slot2Busy : Bool
            stalls : Nat

        inductive Event where
            | AppendShelf (newPy : Nat) (newOccupied : Nat)
            | RepackAtlas (newOccupied : Nat)
            | BeginFrame (prevCompleted : Bool)

        def initWorld : World :=
            { cpuEpoch := 1
            , cpuWriteGen := 0
            , cpuPy := 0
            , occupiedRows := 16
            , gpuEpoch := 1
            , gpuWriteGen := 0
            , uploadedPy := 0
            , gpuRows := 16
            , remintCount := 1
            , curSlot := 0
            , slot0Busy := true
            , slot1Busy := false
            , slot2Busy := false
            , stalls := 0 }

        def Synced (w : World) : Bool :=
            Nat.beq w.cpuEpoch w.gpuEpoch
                && Nat.beq w.cpuWriteGen w.gpuWriteGen
                && Nat.beq w.cpuPy w.uploadedPy
                && Nat.beq w.occupiedRows w.gpuRows

        -- Fixed ensure_atlas + 3-slot ring transition.
        def step (w : World) (ev : Event) : World :=
            match ev with
            | Event.AppendShelf newPy newOccupied =>
                let nextGen := Nat.succ w.cpuWriteGen
                if Nat.beq w.cpuEpoch w.gpuEpoch then
                    { w with
                        cpuWriteGen := nextGen
                        cpuPy := newPy
                        occupiedRows := newOccupied
                        gpuWriteGen := nextGen
                        uploadedPy := newPy
                        gpuRows := newOccupied }
                else
                    { w with
                        cpuWriteGen := nextGen
                        cpuPy := newPy
                        occupiedRows := newOccupied
                        gpuEpoch := w.cpuEpoch
                        gpuWriteGen := nextGen
                        uploadedPy := newPy
                        gpuRows := newOccupied
                        remintCount := Nat.succ w.remintCount }
            | Event.RepackAtlas newOccupied =>
                let nextEpoch := Nat.succ w.cpuEpoch
                { w with
                    cpuEpoch := nextEpoch
                    cpuWriteGen := 0
                    cpuPy := 0
                    occupiedRows := newOccupied
                    gpuEpoch := nextEpoch
                    gpuWriteGen := 0
                    uploadedPy := 0
                    gpuRows := newOccupied
                    remintCount := Nat.succ w.remintCount }
            | Event.BeginFrame prevCompleted =>
                let nextSlot := (Nat.succ w.curSlot) % 3
                let s0 := if prevCompleted then false else w.slot0Busy
                let s1 := if prevCompleted then false else w.slot1Busy
                let s2 := if prevCompleted then false else w.slot2Busy
                let targetBusy :=
                    if Nat.beq nextSlot 0 then s0
                    else if Nat.beq nextSlot 1 then s1
                    else s2
                let nextStalls := if targetBusy then Nat.succ w.stalls else w.stalls
                { w with
                    curSlot := nextSlot
                    slot0Busy := if Nat.beq nextSlot 0 then true else s0
                    slot1Busy := if Nat.beq nextSlot 1 then true else s1
                    slot2Busy := if Nat.beq nextSlot 2 then true else s2
                    stalls := nextStalls }

        -- Buggy pre-fix variant 1: folding map.len() into key forces a texture
        -- re-mint on every glyph append.
        def stepBuggyRemintOnAppend (w : World) (newPy newOccupied : Nat) : World :=
            let nextGen := Nat.succ w.cpuWriteGen
            { w with
                cpuWriteGen := nextGen
                cpuPy := newPy
                occupiedRows := newOccupied
                gpuWriteGen := nextGen
                uploadedPy := newPy
                gpuRows := newOccupied
                remintCount := Nat.succ w.remintCount }

        -- Buggy pre-fix variant 2: a single-slot frame buffer stalls whenever
        -- the immediately preceding frame is still executing on the GPU.
        def stepBuggySingleSlot (w : World) (prevCompleted : Bool) : World :=
            let nextStalls := if prevCompleted then w.stalls else Nat.succ w.stalls
            { w with slot0Busy := true, stalls := nextStalls }

        theorem initWorld_synced : Synced initWorld = true := rfl

        theorem append_shelf_preserves_sync :
            let w1 := step initWorld (Event.AppendShelf 16 32)
            let w2 := step w1 (Event.AppendShelf 32 48)
            Synced w2 = true := rfl

        theorem append_shelf_avoids_remint :
            let w1 := step initWorld (Event.AppendShelf 16 32)
            let w2 := step w1 (Event.AppendShelf 32 48)
            w2.remintCount = 1 := rfl

        theorem repack_atlas_preserves_sync :
            let w1 := step initWorld (Event.RepackAtlas 24)
            Synced w1 = true := rfl

        theorem repack_atlas_bumps_remint :
            let w1 := step initWorld (Event.RepackAtlas 24)
            w1.remintCount = 2 := rfl

        theorem counterexample_buggy_append_remints_every_time :
            let w1 := stepBuggyRemintOnAppend initWorld 16 32
            let w2 := stepBuggyRemintOnAppend w1 32 48
            w2.remintCount = 3 := rfl

        theorem three_slot_ring_avoids_stall_when_prev_frame_busy :
            let w1 := step initWorld (Event.BeginFrame false)
            let w2 := step w1 (Event.BeginFrame false)
            w2.stalls = 0 := rfl

        theorem counterexample_single_slot_stalls_when_prev_frame_busy :
            let w1 := stepBuggySingleSlot initWorld false
            let w2 := stepBuggySingleSlot w1 false
            w2.stalls = 2 := rfl

        end MetalAtlasAndFrameRing
    }
}

#[cfg(target_os = "macos")]
struct MetalArmCell {
    live: MetalArmLive,
    pinned: std::thread::ThreadId,
}

#[cfg(target_os = "macos")]
impl MetalArmCell {
    #[inline]
    fn new(live: MetalArmLive) -> Self {
        Self {
            live,
            pinned: std::thread::current().id(),
        }
    }

    #[inline]
    fn live_mut(&mut self) -> &mut MetalArmLive {
        assert!(
            std::thread::current().id() == self.pinned,
            "metal arm: thread-pinned state touched off the render thread that \
             minted it (the armed session/queue is single-thread by the metal \
             module convention)"
        );
        &mut self.live
    }

    #[inline]
    fn live_ref(&self) -> &MetalArmLive {
        assert!(
            std::thread::current().id() == self.pinned,
            "metal arm: thread-pinned state touched off the render thread that \
             minted it (the armed session/queue is single-thread by the metal \
             module convention)"
        );
        &self.live
    }
}

// SAFETY: moving the holder is sound because `live_mut` checks the minting thread and
// `objc_release` is thread-safe.
#[cfg(target_os = "macos")]
unsafe impl Send for MetalArmCell {}

/// `plan`'s passes ([`coalesce_frame_passes`]): the base group always and each group a draw names,
/// each on the view its draws take (a fetch draw takes the sRGB view in a Unorm-law group).
fn plan_passes(plan: &[DrawItem]) -> ([usize; FRAME_GROUPS], [bool; FRAME_GROUPS], usize) {
    let mut enabled = [false; FRAME_GROUPS];
    let mut srgb = GROUP_SRGB;
    enabled[G_BASE_BG] = true;
    for item in plan {
        enabled[item.group as usize] = true;
        srgb[item.group as usize] = item.srgb();
    }
    coalesce_frame_passes(&enabled, &srgb)
}

pub(crate) fn run_frame_plan(
    enc: &mut crate::device_layer::FrameEncoder<'_>,
    res: &FrameRes<'_>,
    plan: &[DrawItem],
    load0: crate::device_layer::FrameLoad,
    scissor: Option<(u32, u32, u32, u32)>,
) -> Result<usize, String> {
    let (pass_of, pass_srgb, passes) = plan_passes(plan);
    for (p, &srgb) in pass_srgb.iter().enumerate().take(passes) {
        let load = if p == 0 {
            load0
        } else {
            crate::device_layer::FrameLoad::Load
        };
        let mut pass = enc.begin_pass(
            "aterm-gpu frame pass",
            res.view(srgb),
            load,
            scissor,
            res.uniforms(),
        )?;
        let mut cur_pipe: Option<(DrawPipe, bool)> = None;
        let mut cur_atlas: Option<DrawAtlas> = None;
        for item in plan.iter().filter(|it| pass_of[it.group as usize] == p) {
            if cur_pipe != Some((item.pipe, item.fetch)) {
                pass.set_pipeline(res.pipeline(item.pipe, item.fetch));
                cur_pipe = Some((item.pipe, item.fetch));
            }
            if let Some(atlas) = item.atlas
                && cur_atlas != Some(atlas)
            {
                pass.bind_atlas(res.atlas(atlas, item.pipe));
                cur_atlas = Some(atlas);
            }
            pass.set_stream(res.stream(item.stream));
            pass.draw_quads(item.count)?;
        }
    }
    Ok(passes)
}

#[inline]
const fn frame_live_bg(input: &RenderInput, theme_bg: u32) -> u32 {
    if input.default_bg == aterm_core::render::COLOR_UNSET {
        theme_bg
    } else {
        input.default_bg
    }
}

#[inline]
fn page_bloom_strength(strength: f32, input: &RenderInput, theme_bg: u32) -> f32 {
    let bg = frame_live_bg(input, theme_bg);
    if aterm_render::theme_is_dark(bg) {
        return strength;
    }
    strength * aterm_render::hdr::light_page_bloom_share(bg)
}

struct PresentUniforms {
    live_bg: u32,
    want: BlitUniform,
    run_hdr_glow: bool,
    run_sdr_glow: bool,
    crown_scissor: Option<(u32, u32, u32, u32)>,
    crown: Option<HdrGlowUniform>,
}

#[inline]
fn visible_source_scissor(
    uniform: &BlitUniform,
    destination_width: u32,
    destination_height: u32,
) -> Option<(u32, u32, u32, u32)> {
    let left = uniform.content_off[0] as i64;
    let top = (uniform.content_off[1] + uniform.visible_y) as i64;
    let right = left.saturating_add(uniform.dims[0] as i64);
    let bottom = top.saturating_add(uniform.visible_h as i64);
    let x0 = left.clamp(0, i64::from(destination_width));
    let y0 = top.clamp(0, i64::from(destination_height));
    let x1 = right.clamp(0, i64::from(destination_width));
    let y1 = bottom.clamp(0, i64::from(destination_height));
    (x1 > x0 && y1 > y0).then_some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
}

#[inline]
fn crown_band_scissor(
    uniform: &BlitUniform,
    band_shift: Option<BandShift>,
    destination_width: u32,
    destination_height: u32,
) -> Option<(u32, u32, u32, u32)> {
    let Some(shift) = band_shift else {
        return visible_source_scissor(uniform, destination_width, destination_height);
    };
    let mut shifted = *uniform;
    let vis_y0 = uniform.visible_y.max(shift.dst_y as f32);
    let vis_y1 =
        (uniform.visible_y + uniform.visible_h).min(shift.dst_y.saturating_add(shift.moved) as f32);
    shifted.visible_y = vis_y0;
    shifted.visible_h = (vis_y1 - vis_y0).max(0.0);
    visible_source_scissor(&shifted, destination_width, destination_height)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared present uniform needs source/destination geometry plus the three independent present effects"
)]
fn present_blit_uniform(
    invert: bool,
    overlay: Option<DropOverlay>,
    source_crop: Option<PresentCrop>,
    source_width: u32,
    source_height: u32,
    destination_width: u32,
    destination_height: u32,
    band_rgb: u32,
) -> BlitUniform {
    let visible_height = source_crop.map_or(source_height, |crop| crop.height);
    match overlay {
        Some(overlay) => {
            let chan = |shift: u32| ((overlay.accent >> shift) & 0xff) as f32 / 255.0;
            BlitUniform {
                flag: invert as u32,
                overlay: 1,
                border_px: scaled_border_px(
                    drop_border_px_gpu(source_width, visible_height),
                    overlay.border_scale_q4,
                )
                .min(source_width / 2)
                .min(visible_height / 2)
                .max(1) as f32,
                accent: [chan(16), chan(8), chan(0)],
                wash_a: f32::from(overlay.wash_a) / 255.0,
                border_a: f32::from(overlay.border_a) / 255.0,
                ..BlitUniform::bell(invert)
            }
        }
        None => BlitUniform::bell(invert),
    }
    .with_bands(
        source_width,
        source_height,
        destination_width,
        destination_height,
        band_rgb,
    )
    .with_source_crop(source_crop)
}

#[derive(Clone, Copy)]
struct NeutralSurfaceConfig {
    width: u32,
    height: u32,
    format: crate::device_layer::TexelFormat,
}

#[cfg(target_os = "macos")]
fn metal_attach_format<W: WindowTarget>(
    hdr_glow: bool,
    target: &W,
) -> crate::device_layer::TexelFormat {
    let potential = crate::metal::present::screen_edr_potential_of(target);
    if crate::format_plan::hdr_swapchain_wants_f16_on_screen(hdr_glow, true, potential) {
        crate::device_layer::TexelFormat::Rgba16Float
    } else {
        crate::device_layer::TexelFormat::Bgra8Unorm
    }
}

/// An on-screen presentation target.
pub struct GpuSurface {
    acquire_id: u64,
    neutral: NeutralSurfaceConfig,
    #[cfg_attr(
        target_arch = "wasm32",
        expect(dead_code, reason = "read by the window HDR paths; a canvas has none")
    )]
    supports_f16: bool,
    #[cfg(d3d11_arm)]
    last_hdr_probe: Option<aterm_time::Instant>,
    post_mult: bool,
    #[cfg(d3d11_arm)]
    pre_mult: bool,
    copyable: bool,
    #[cfg(target_os = "macos")]
    metal: Option<crate::metal::present::MetalWindowSurface>,
    #[cfg(gl_arm)]
    gl: Option<crate::gl::WindowSurface>,
    /// The D3D11 arm's swap chain on the window (the shipped Windows build;
    /// COM and window handles, keeping `GpuSurface` on the attaching thread).
    #[cfg(d3d11_arm)]
    d3d11: Option<d3d11_window::D3dWindow>,
}

fn next_surface_acquire_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut id = NEXT.load(Ordering::Relaxed);
    loop {
        let next = id.checked_add(1).expect("surface identity exhausted");
        match NEXT.compare_exchange_weak(id, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return id,
            Err(current) => id = current,
        }
    }
}

impl GpuSurface {
    /// Stable identity used to reject late wakes after surface replacement.
    #[must_use]
    #[inline]
    pub const fn acquire_id(&self) -> u64 {
        self.acquire_id
    }

    /// Install the main-loop wake before the first attached native present.
    #[inline]
    pub fn set_acquire_ready_callback(
        &mut self,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        if let Some(metal) = self.metal.as_mut() {
            return metal.set_acquire_ready_callback(notify);
        }
        let _ = notify;
        Ok(())
    }

    /// Reject a pending drawable after resize, occlusion, or surface retirement.
    #[inline]
    pub fn discard_pending_acquire(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(metal) = self.metal.as_mut() {
            metal.discard_pending_acquire();
        }
    }

    /// How this swapchain's presents queue.
    #[must_use]
    #[inline]
    pub fn fifo_queue_depth(&self) -> Option<u32> {
        #[cfg(target_os = "macos")]
        if self.metal.is_some() {
            return None;
        }
        #[cfg(gl_arm)]
        if let Some(gl) = self.gl.as_ref() {
            return gl.synced().then_some(1);
        }
        None
    }

    /// Whether this swapchain is the HDR (`Rgba16Float` extended-linear) target.
    #[must_use]
    #[inline]
    pub fn is_hdr(&self) -> bool {
        self.neutral.format == crate::device_layer::TexelFormat::Rgba16Float
    }

    /// Whether a presented snapshot of this surface can be armed.
    #[must_use]
    #[inline]
    pub const fn snapshot_is_copyable(&self) -> bool {
        self.copyable
    }

    /// The bytes a presented snapshot holds ([`crate::video_tap::RawCapture::allocation`]).
    #[must_use]
    #[inline]
    pub fn snapshot_bytes(&self) -> Option<usize> {
        if !self.copyable {
            return None;
        }
        let (width, height, format) =
            (self.neutral.width, self.neutral.height, self.neutral.format);
        let len = crate::video_tap::presented_snapshot_len(width, height, format).ok()?;
        usize::try_from(len).ok()
    }

    #[cfg(target_os = "macos")]
    #[inline]
    const fn neutral_config(&self) -> (u32, u32, crate::device_layer::TexelFormat) {
        (self.neutral.width, self.neutral.height, self.neutral.format)
    }
}

#[cfg(d3d11_arm)]
const HDR_COLOR_SPACE_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[must_use]
#[cfg(d3d11_arm)]
#[inline]
fn hdr_color_space_probe_due(last: Option<aterm_time::Instant>, now: aterm_time::Instant) -> bool {
    match last {
        None => true,
        Some(last) => now.saturating_duration_since(last) >= HDR_COLOR_SPACE_PROBE_INTERVAL,
    }
}

#[derive(Clone, Copy)]
struct GlyphSlot {
    ax: u32,
    ay: u32,
    gw: u32,
    gh: u32,
    xmin: i32,
    ymin: i32,
    ink_rise: Option<i32>,
}

impl GlyphSlot {
    #[inline]
    fn quad(
        &self,
        x: f32,
        anchor_y: i32,
        baseline: i32,
        scale: aterm_render::Scale,
        (aw, ah): (f32, f32),
    ) -> Option<([f32; 4], [f32; 4])> {
        aterm_render::glyph_quad(
            x, anchor_y, baseline, scale, self.ax, self.ay, self.gw, self.gh, self.xmin, self.ymin,
            aw, ah,
        )
    }

    #[inline]
    fn ink_reach(&self, anchor_y: i32, baseline: i32, scale: aterm_render::Scale) -> i32 {
        self.ink_rise.map_or(i32::MAX, |rise| {
            (anchor_y + scale.ys.max(1) as i32 * (baseline - rise)).max(scale.clip_y0)
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AtlasKind {
    Mono,
    Color,
}

impl AtlasKind {
    #[inline]
    const fn bpp(self) -> u32 {
        match self {
            AtlasKind::Mono => 1,
            AtlasKind::Color => 4,
        }
    }
}

struct Atlas {
    kind: AtlasKind,
    width: u32,
    height: u32,
    data: Vec<u8>,
    map: FxHashMap<GlyphKey, GlyphSlot>,
    px: u32,
    py: u32,
    shelf_h: u32,
    write_gen: u64,
}

impl Atlas {
    #[inline]
    fn packs(kind: AtlasKind, img: &GlyphImage) -> bool {
        let real = img.width() > 0 && img.height() > 0;
        match kind {
            AtlasKind::Mono => real && matches!(img, GlyphImage::Mono { .. }),
            AtlasKind::Color => real && matches!(img, GlyphImage::Rgba { .. }),
        }
    }

    fn place(&mut self, key: GlyphKey, img: &GlyphImage) -> Option<(u32, u32)> {
        let pad = 1u32;
        if !Self::packs(self.kind, img) {
            // Mono records every key (zero slot for non-mono/empty) so a lookup
            // there always resolves; Color simply skips.
            if self.kind == AtlasKind::Mono {
                self.map.insert(
                    key,
                    GlyphSlot {
                        ax: 0,
                        ay: 0,
                        gw: 0,
                        gh: 0,
                        xmin: img.xmin(),
                        ymin: img.ymin(),
                        ink_rise: None,
                    },
                );
            }
            return None;
        }
        let (gw, gh) = (img.width() as u32, img.height() as u32);
        if gw > self.width.saturating_sub(pad) {
            if self.kind == AtlasKind::Mono {
                self.map.insert(
                    key,
                    GlyphSlot {
                        ax: 0,
                        ay: 0,
                        gw: 0,
                        gh: 0,
                        xmin: img.xmin(),
                        ymin: img.ymin(),
                        ink_rise: None,
                    },
                );
            }
            return None;
        }
        if self.px + gw + pad > self.width {
            self.px = 0;
            self.py += self.shelf_h + pad;
            self.shelf_h = 0;
        }
        let (ax, ay) = (self.px, self.py);
        self.map.insert(
            key,
            GlyphSlot {
                ax,
                ay,
                gw,
                gh,
                xmin: img.xmin(),
                ymin: img.ymin(),
                ink_rise: aterm_render::glyph_ink_box(img)
                    .map(|(ink_h, ink_ymin)| ink_ymin + ink_h as i32),
            },
        );
        self.px += gw + pad;
        self.shelf_h = self.shelf_h.max(gh);
        Some((ay, gh))
    }

    fn blit(&mut self, img: &GlyphImage, slot: &GlyphSlot) {
        if slot.gw == 0 || slot.gh == 0 {
            return;
        }
        let bpp = self.kind.bpp();
        let bytes = img.bytes();
        let row_bytes = (slot.gw * bpp) as usize;
        for j in 0..slot.gh {
            let src = ((j * slot.gw) * bpp) as usize;
            let dst = (((slot.ay + j) * self.width + slot.ax) * bpp) as usize;
            self.data[dst..dst + row_bytes].copy_from_slice(&bytes[src..src + row_bytes]);
        }
    }

    #[inline]
    fn occupied_height(&self) -> u32 {
        (self.py + self.shelf_h + 1).max(1)
    }
}

fn build_atlas(cpu: &mut Renderer, keys: &[GlyphKey], cap_h: u32) -> Atlas {
    build_kind(cpu, keys, AtlasKind::Mono, cap_h)
}

fn build_color_atlas(cpu: &mut Renderer, keys: &[GlyphKey], cap_h: u32) -> Atlas {
    build_kind(cpu, keys, AtlasKind::Color, cap_h)
}

fn build_kind(cpu: &mut Renderer, keys: &[GlyphKey], kind: AtlasKind, cap_h: u32) -> Atlas {
    let mut atlas = Atlas {
        kind,
        width: ATLAS_WIDTH,
        height: 1,
        data: Vec::new(),
        map: FxHashMap::default(),
        px: 0,
        py: 0,
        shelf_h: 0,
        write_gen: 0,
    };
    let mut placed: Vec<GlyphKey> = Vec::new();
    for &key in keys {
        let img = cpu.glyph_image(key);
        let (sx, sy, sh) = (atlas.px, atlas.py, atlas.shelf_h);
        if atlas.place(key, img).is_some() {
            if atlas.occupied_height() > cap_h {
                atlas.map.remove(&key);
                atlas.px = sx;
                atlas.py = sy;
                atlas.shelf_h = sh;
                break;
            }
            placed.push(key);
        }
    }
    atlas.height = atlas.occupied_height();
    atlas.data = vec![0u8; (atlas.width * atlas.height * kind.bpp()) as usize];
    for key in placed {
        let slot = atlas.map[&key];
        let img = cpu.glyph_image(key);
        atlas.blit(img, &slot);
    }
    atlas
}

fn grow_atlas(
    cpu: &mut Renderer,
    atlas: &mut Atlas,
    new_keys: &[GlyphKey],
    cap_h: u32,
) -> Option<(u32, u32)> {
    let (sx, sy, sh) = (atlas.px, atlas.py, atlas.shelf_h);
    let mut probe = Atlas {
        kind: atlas.kind,
        width: atlas.width,
        height: atlas.height,
        data: Vec::new(),
        map: FxHashMap::default(),
        px: sx,
        py: sy,
        shelf_h: sh,
        write_gen: atlas.write_gen,
    };
    let mut dirty_lo = u32::MAX;
    let mut dirty_hi = 0u32;
    for &key in new_keys {
        let img = cpu.glyph_image(key);
        if let Some((ay, gh)) = probe.place(key, img) {
            dirty_lo = dirty_lo.min(ay);
            dirty_hi = dirty_hi.max(ay + gh);
        }
    }
    let need_h = probe.occupied_height();
    if need_h > cap_h {
        return None; // genuine overflow: caller repacks fresh into a new texture
    }
    if dirty_hi == 0 {
        for &key in new_keys {
            let img = cpu.glyph_image(key);
            atlas.place(key, img);
        }
        return Some((0, 0));
    }

    let new_total = (atlas.width * need_h * atlas.kind.bpp()) as usize;
    if atlas.data.len() < new_total {
        atlas.data.resize(new_total, 0);
    }
    atlas.height = need_h.max(atlas.height);
    for &key in new_keys {
        let img = cpu.glyph_image(key);
        if atlas.place(key, img).is_some() {
            let slot = atlas.map[&key];
            atlas.blit(img, &slot);
        }
    }
    atlas.write_gen = atlas.write_gen.wrapping_add(1);
    Some((dirty_lo, dirty_hi))
}

const DECO_GLYPHS: [aterm_render::DecoGlyph; 7] = [
    aterm_render::DecoGlyph::Star4,
    aterm_render::DecoGlyph::Star5,
    aterm_render::DecoGlyph::Dot,
    aterm_render::DecoGlyph::Plus,
    aterm_render::DecoGlyph::Paw,
    aterm_render::DecoGlyph::RingArc,
    aterm_render::DecoGlyph::Shade,
];

const _: () = assert!(DECO_GLYPHS.len() == aterm_render::UNDERCURL_SPRITE);
const _: () = assert!(DECO_GLYPHS.len() + 1 == aterm_render::DECO_ATLAS_SPRITES);

#[inline]
const fn deco_sprite_index(g: aterm_render::DecoGlyph) -> usize {
    match g {
        aterm_render::DecoGlyph::Star4 => 0,
        aterm_render::DecoGlyph::Star5 => 1,
        aterm_render::DecoGlyph::Dot => 2,
        aterm_render::DecoGlyph::Plus => 3,
        aterm_render::DecoGlyph::Paw => 4,
        aterm_render::DecoGlyph::RingArc => 5,
        aterm_render::DecoGlyph::Shade => 6,
    }
}

struct DecoAtlas {
    data: Vec<u8>,
    cw: usize,
    ch: usize,
    curl_band: (usize, usize),
    atlas_w: usize,
}

struct SpriteTex {
    #[cfg(target_os = "macos")]
    metal_atlas: std::sync::OnceLock<ArmAtlas>,
    #[cfg(gl_arm)]
    gl_atlas: std::sync::Mutex<Option<Arc<crate::gl::resources::GlTexture>>>,
    /// The D3D11 arm's upload of these texels, made when a frame samples them
    /// (re-made when the arm's device changes).
    #[cfg(d3d11_arm)]
    d3d11_atlas: std::sync::Mutex<Option<Arc<crate::d3d11::resources::D3dTexture>>>,
    w: u32,
    h: u32,
    src: Arc<SceneAtlas>,
}

impl SpriteTex {
    #[inline]
    fn matches(&self, source: &Arc<SceneAtlas>) -> bool {
        self.w == source.width
            && self.h == source.height
            && self.src.version == source.version
            && Arc::ptr_eq(&self.src, source)
    }
}

const SPRITE_TEXTURE_LOOKUP_LIMIT: usize = 64;

#[derive(Default)]
struct SpriteTextureLookup {
    recent: Vec<Weak<SpriteTex>>,
}

impl SpriteTextureLookup {
    fn find(&mut self, source: &Arc<SceneAtlas>) -> Option<Arc<SpriteTex>> {
        self.recent.retain(|entry| entry.strong_count() != 0);
        let mut found = None;
        let index = self.recent.iter().position(|entry| {
            if let Some(texture) = entry.upgrade()
                && texture.matches(source)
            {
                found = Some(texture);
                true
            } else {
                false
            }
        })?;
        let entry = self.recent.remove(index);
        self.recent.push(entry);
        found
    }

    fn insert(&mut self, texture: &Arc<SpriteTex>) {
        if self.recent.len() == SPRITE_TEXTURE_LOOKUP_LIMIT {
            self.recent.remove(0);
        }
        self.recent.push(Arc::downgrade(texture));
    }
}

#[cfg(target_os = "macos")]
impl SpriteTex {
    fn metal_atlas(
        &self,
        layer: crate::device_layer::DeviceHandle<'_>,
        atlas: DrawAtlas,
    ) -> Result<&ArmAtlas, String> {
        if let Some(resident) = self.metal_atlas.get() {
            return Ok(resident);
        }
        let mut slot = None;
        MetalArmLive::ensure_atlas(
            layer,
            &mut slot,
            atlas,
            (self.w, self.h, Arc::as_ptr(&self.src) as u64),
            0,
            0,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            &self.src.rgba[..(self.w as usize * self.h as usize * 4)],
            &mut [],
        )?;
        let _ = self
            .metal_atlas
            .set(slot.expect("ensure_atlas filled the slot"));
        Ok(self.metal_atlas.get().expect("shared sprite initialized"))
    }
}

struct ResidentAtlas {
    atlas: Atlas,
    tex_h: u32,
}

/// Per-window GPU state.
#[derive(Default)]
pub struct WindowGpu {
    wallpaper_tex: Option<Arc<SpriteTex>>,
    last_present_work_ns: u64,
    last_present_encode_ns: u64,
    last_acquire_wait_ns: u64,
    acquire_wait_sample_ns: Option<u64>,
    last_acquire_queue_ns: u64,
    acquire_queue_sample_ns: Option<u64>,
    last_gpu_park_ns: u64,
    gpu_park_sample_ns: Option<u64>,
    pub(crate) gate_cache: Option<GpuGateCache>,
    pub(crate) image_cache: aterm_render::ImageCache,
    pub(crate) video: Option<crate::video_tap::VideoTap>,
    pub(crate) presented_snapshot: Option<crate::video_tap::PresentedFrameTap>,
    #[cfg(target_os = "macos")]
    pub(crate) copy_src_hold: CopySrcHold,
    pub(crate) image_plane: Option<ImagePlane>,
    pub(crate) present_prev: Option<PresentPrev>,
    sdr_glow_level: f32,
    sdr_glow_level_at: Option<aterm_time::Instant>,
    pub(crate) prev_input: RenderInput,
    resident_input_epoch: u64,
    resident_input_effects_transport_shift_y: i32,
    virtual_presented_input_epoch: u64,
    pub(crate) edr_max: f32,
    pub(crate) sdr_white_scale: f32,
    pub(crate) chrome_y0: u32,
    pub(crate) chrome_y1: u32,
    pub(crate) capture_color_space: crate::video_tap::CaptureColorSpace,
    pub(crate) apron_scratch: aterm_render::ApronScratch,
    pub(crate) apron_rgba: Vec<u8>,
    pub(crate) apron_rgba_gen: u64,
    #[cfg(target_os = "macos")]
    pub(crate) metal_apron_tex: Option<crate::metal::resources::SealedTexture>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_apron_bytes: Vec<u8>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_apron_gen: u64,
    pub(crate) offscreen_dirty_since_sync: Option<[u32; 4]>,
    pub(crate) present_offscreen_fx: Option<[u32; 4]>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_offscreen: Option<MetalOffscreen>,
    #[cfg(gl_arm)]
    gl_offscreen: Option<gl_arm::GlOffscreen>,
    #[cfg(gl_arm)]
    gl_shift_scratch: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    gl_tray_card: Option<gl_arm::GlTrayCard>,
    // The D3D11 arm's twins (`d3d11_arm.rs`): the armed frame's offscreen
    // (`last_frame_arm_d3d11`), its band-shift scratch and the tray card.
    #[cfg(d3d11_arm)]
    d3d11_offscreen: Option<d3d11_arm::D3dOffscreen>,
    #[cfg(d3d11_arm)]
    d3d11_shift_scratch: Option<crate::d3d11::resources::D3dTexture>,
    #[cfg(d3d11_arm)]
    d3d11_tray_card: Option<d3d11_arm::D3dTrayCard>,
    // The D3D11 arm's present residency (`d3d11_present.rs`), the GL arm's
    // twins below: the present copy, the half-resolution bloom target, the
    // shimmer scratch, the apron strip (with the apron generation it holds)
    // and the headless virtual destination.
    #[cfg(d3d11_arm)]
    d3d11_present_off: Option<crate::d3d11::resources::D3dTexture>,
    #[cfg(d3d11_arm)]
    d3d11_bloom_half: Option<crate::d3d11::resources::D3dTexture>,
    #[cfg(d3d11_arm)]
    d3d11_shimmer_scratch: Option<crate::d3d11::resources::D3dTexture>,
    #[cfg(d3d11_arm)]
    d3d11_apron_tex: Option<crate::d3d11::resources::D3dTexture>,
    #[cfg(d3d11_arm)]
    d3d11_apron_gen: u64,
    #[cfg(d3d11_arm)]
    d3d11_virtual_off: Option<crate::d3d11::resources::D3dTexture>,
    #[cfg(gl_arm)]
    gl_present_off: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    gl_bloom_half: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    gl_shimmer_scratch: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    gl_apron_tex: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    gl_apron_gen: u64,
    #[cfg(gl_arm)]
    gl_virtual_off: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    offscreen_dirty_since_post: Option<[u32; 4]>,
    #[cfg(gl_arm)]
    gl_posted: Option<gl_present::GlPosted>,
    #[cfg(gl_arm)]
    gl_bloom_inputs: Vec<u8>,
    #[cfg(gl_arm)]
    gl_posted_bloom: Vec<u8>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_present_off: Option<crate::metal::resources::SealedTexture>,
    #[cfg(all(target_os = "macos", test))]
    pub(crate) metal_last_compose_rect: Option<[u32; 4]>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_present_off_pending: Vec<crate::metal::encoder::CbProbe>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_shift_scratch: Option<crate::metal::resources::SealedTexture>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_bloom_half: Option<crate::metal::resources::SealedTexture>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_shimmer_scratch: Option<crate::metal::resources::SealedTexture>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_tray_card: Option<MetalTrayCard>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_virtual_off: Option<crate::metal::resources::SealedTexture>,
    occluded_hint: bool,
    capture_present: bool,
}

#[cfg(target_os = "macos")]
#[inline]
const fn encode_work_ns_excluding_park(work_elapsed_ns: u64, frame_slot_park_ns: u64) -> u64 {
    work_elapsed_ns.saturating_sub(frame_slot_park_ns)
}

/// How long a presented snapshot keeps its window's swapchain copyable after it is armed.
#[cfg(target_os = "macos")]
pub(crate) const SNAPSHOT_COPY_SRC_HOLD: std::time::Duration = std::time::Duration::from_secs(2);

/// The copyable-swapchain hold a presented snapshot leaves behind.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CopySrcHold {
    until: Option<aterm_time::Instant>,
}

#[cfg(target_os = "macos")]
impl CopySrcHold {
    /// A snapshot was armed at `now`: hold the bit until the hold elapses.
    #[inline]
    pub(crate) fn arm(&mut self, now: aterm_time::Instant) {
        self.until = Some(now + SNAPSHOT_COPY_SRC_HOLD);
    }

    /// Whether the hold still stands at `now`.
    #[inline]
    pub(crate) fn holds(&self, now: aterm_time::Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }
}

impl WindowGpu {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(target_os = "macos")]
    #[inline]
    fn wants_copy_src(&self, now: aterm_time::Instant) -> bool {
        GpuRenderer::tap_wants_copy_src(
            self.video.is_some(),
            self.presented_snapshot.is_some(),
            self.copy_src_hold.holds(now),
        )
    }

    /// Record winit's `WindowEvent::Occluded` state (see `occluded_hint`).
    #[inline]
    pub const fn set_occluded_hint(&mut self, occluded: bool) {
        self.occluded_hint = occluded;
    }

    /// The last frontend-reported occlusion state (see [`Self::set_occluded_hint`]).
    #[must_use]
    #[inline]
    pub const fn occluded_hint(&self) -> bool {
        self.occluded_hint
    }

    /// Mark the frontend's next presents as a capture's (`on`), or no longer (`!on`).
    #[inline]
    pub const fn set_capture_present(&mut self, on: bool) {
        self.capture_present = on;
    }

    /// Whether this window's present parks as `Occluded` (Windows): minimized, with no capture,
    /// one-shot or recording waiting on its frames.
    #[must_use]
    #[inline]
    pub const fn parks_while_minimized(&self) -> bool {
        self.occluded_hint
            && !self.capture_present
            && self.presented_snapshot.is_none()
            && self.video.is_none()
    }

    /// CPU wall time encoding and calling `queue.submit` for the latest successful present.
    #[must_use]
    #[inline]
    pub const fn last_present_work_ns(&self) -> u64 {
        self.last_present_work_ns
    }

    /// CPU wall time encoding the latest successful present up to its last commit, excluding
    /// recorded frame-slot and ring parks.
    #[must_use]
    #[inline]
    pub const fn last_present_encode_ns(&self) -> u64 {
        self.last_present_encode_ns
    }

    /// Wall time inside the latest completed drawable acquisition, including refusals.
    #[must_use]
    #[inline]
    pub const fn last_acquire_wait_ns(&self) -> u64 {
        self.last_acquire_wait_ns
    }

    /// Consume this surface-present attempt's acquire measurement exactly once.
    #[inline]
    pub const fn take_acquire_wait_sample_ns(&mut self) -> Option<u64> {
        self.acquire_wait_sample_ns.take()
    }

    /// Wall time the latest completed acquisition waited in the drawable worker's queue.
    #[must_use]
    #[inline]
    pub const fn last_acquire_queue_ns(&self) -> u64 {
        self.last_acquire_queue_ns
    }

    /// Consume this surface-present attempt's queue measurement exactly once.
    #[inline]
    pub const fn take_acquire_queue_sample_ns(&mut self) -> Option<u64> {
        self.acquire_queue_sample_ns.take()
    }

    /// Wall time the latest armed present blocked on the GPU (frame-slot wait plus ring drain).
    #[must_use]
    #[inline]
    pub const fn last_gpu_park_ns(&self) -> u64 {
        self.last_gpu_park_ns
    }

    /// Consume this present attempt's GPU-park measurement once.
    #[inline]
    pub const fn take_gpu_park_sample_ns(&mut self) -> Option<u64> {
        self.gpu_park_sample_ns.take()
    }

    #[cfg(target_os = "macos")]
    #[inline]
    fn add_gpu_park(&mut self, parked_ns: u64) {
        let total = self
            .gpu_park_sample_ns
            .unwrap_or(0)
            .saturating_add(parked_ns);
        self.gpu_park_sample_ns = Some(total);
        self.last_gpu_park_ns = total;
    }

    #[inline]
    const fn begin_surface_present(&mut self) {
        self.acquire_wait_sample_ns = None;
        self.acquire_queue_sample_ns = None;
        self.gpu_park_sample_ns = None;
    }

    #[inline]
    fn measure_surface_acquire<T>(&mut self, acquire: impl FnOnce() -> T) -> T {
        let started = aterm_time::Instant::now();
        let result = acquire();
        self.record_surface_acquire(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        result
    }

    #[inline]
    const fn record_surface_acquire(&mut self, waited_ns: u64) {
        self.last_acquire_wait_ns = waited_ns;
        self.acquire_wait_sample_ns = Some(waited_ns);
    }

    #[cfg(target_os = "macos")]
    #[inline]
    const fn record_surface_acquire_queue(&mut self, queued_ns: u64) {
        self.last_acquire_queue_ns = queued_ns;
        self.acquire_queue_sample_ns = Some(queued_ns);
    }

    /// Record the screen's raw EDR maximum for this window.
    #[inline]
    pub const fn set_edr_max(&mut self, v: f32) {
        self.edr_max = v;
    }

    /// Record the Windows scRGB reference-white scale (`SDRwhite_nits / 80`), clamped to `>= 1.0`.
    #[inline]
    pub const fn set_sdr_white_scale(&mut self, v: f32) {
        self.sdr_white_scale = if v.is_finite() { v.max(1.0) } else { 1.0 };
    }

    /// Record the colour-space tag the compositor uses for this window's presented texture.
    #[inline]
    pub const fn set_capture_color_space(
        &mut self,
        color_space: crate::video_tap::CaptureColorSpace,
    ) {
        self.capture_color_space = color_space;
    }

    /// The captured presentation colour space.
    #[must_use]
    #[inline]
    pub const fn capture_color_space(&self) -> crate::video_tap::CaptureColorSpace {
        self.capture_color_space
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HdrReconfigureRetag",
            action = "RetagSucceeds",
            project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HdrReconfigureRetag",
            action = "RetagFails",
            project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HdrReconfigureRetag",
            action = "EnterSdrFallback",
            project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HdrReconfigureRetag",
            action = "UpgradeFails",
            project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
        )
    )]
    #[cfg(d3d11_arm)]
    fn apply_hdr_reconfigure_plan(&mut self, plan: crate::format_plan::HdrReconfigurePlan) {
        if plan == crate::format_plan::HdrReconfigurePlan::FallbackToSdr {
            self.capture_color_space = crate::video_tap::CaptureColorSpace::Srgb;
            self.sdr_white_scale = 1.0;
            self.edr_max = 0.0;
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HdrReconfigureRetag",
            action = "UpgradeSucceeds",
            project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
        )
    )]
    #[cfg(any(target_os = "macos", d3d11_arm, test))]
    fn apply_hdr_surface_upgrade(&mut self) {
        self.capture_color_space = crate::video_tap::CaptureColorSpace::ExtendedLinearSrgb;
        self.sdr_white_scale = 1.0;
        self.edr_max = 0.0;
    }

    /// The sanitized scRGB reference-white multiplier used by presentation and capture.
    #[must_use]
    #[inline]
    pub const fn sdr_white_scale(&self) -> f32 {
        if self.sdr_white_scale.is_finite() {
            self.sdr_white_scale.max(1.0)
        } else {
            1.0
        }
    }

    /// The recorded raw EDR maximum, which the frontend carries across `WindowGpu` rebuilds.
    #[must_use]
    #[inline]
    pub const fn edr_max(&self) -> f32 {
        self.edr_max
    }

    /// Force the next `present_input` to be a full repaint rather than a dirty-row diff.
    #[inline]
    pub fn invalidate_present(&mut self) {
        self.present_prev = None;
        #[cfg(gl_arm)]
        {
            self.gl_posted = None;
        }
        self.apron_scratch.invalidate();
        self.advance_resident_input_epoch();
    }

    #[inline]
    const fn advance_resident_input_epoch(&mut self) {
        self.resident_input_epoch = self.resident_input_epoch.saturating_add(1);
    }

    /// Identity of the input most recently encoded for presentation.
    #[must_use]
    #[inline]
    pub const fn resident_input_epoch(&self) -> u64 {
        self.resident_input_epoch
    }

    /// The byte length of [`GpuRenderer::virtual_presented_snapshot_current`], when available.
    #[must_use]
    pub fn virtual_snapshot_bytes(&self) -> Option<usize> {
        #[cfg(gl_arm)]
        if let Some(tex) = self.gl_virtual_off.as_ref() {
            return usize::try_from(u64::from(tex.width) * u64::from(tex.height) * 4).ok();
        }
        // The D3D11 arm likewise (`GpuRenderer::d3d11_virtual_snapshot`).
        #[cfg(d3d11_arm)]
        if let Some(tex) = self.d3d11_virtual_off.as_ref() {
            return usize::try_from(u64::from(tex.width) * u64::from(tex.height) * 4).ok();
        }
        self.tapped_virtual_bytes()
    }

    #[cfg(target_os = "macos")]
    fn tapped_virtual_bytes(&self) -> Option<usize> {
        let (width, height) = {
            let target = self.metal_virtual_off.as_ref()?;
            (
                u32::try_from(target.width()).ok()?,
                u32::try_from(target.height()).ok()?,
            )
        };
        let len =
            crate::video_tap::presented_snapshot_len(width, height, VIRTUAL_PRESENT_TEXEL).ok()?;
        usize::try_from(len).ok()
    }

    #[cfg(not(target_os = "macos"))]
    #[expect(clippy::unused_self, reason = "the twin of the tapping arms' reader")]
    fn tapped_virtual_bytes(&self) -> Option<usize> {
        None
    }

    /// Clone the resident semantic model with the transport translation applied to its effects,
    /// in one read so input and translation match.
    #[must_use]
    pub fn clone_resident_input_with_transport_at(&self, epoch: u64) -> Option<(RenderInput, i32)> {
        (epoch != 0 && epoch != u64::MAX && self.resident_input_epoch == epoch).then(|| {
            (
                self.prev_input.clone(),
                self.resident_input_effects_transport_shift_y,
            )
        })
    }

    fn advance_sdr_glow_budget(
        &mut self,
        glow_present: bool,
        target: f32,
        now: aterm_time::Instant,
    ) -> f32 {
        if !glow_present {
            self.sdr_glow_level = 0.0;
            self.sdr_glow_level_at = None;
            return 0.0;
        }

        let dt = self
            .sdr_glow_level_at
            .map_or(0.016, |t| now.saturating_duration_since(t).as_secs_f32());
        self.sdr_glow_level_at = Some(now);
        if target > self.sdr_glow_level {
            let attack = 1.0 - (-dt / 0.045).exp();
            self.sdr_glow_level += (target - self.sdr_glow_level) * attack;
        } else {
            self.sdr_glow_level = target;
        }
        self.sdr_glow_level
    }
}

/// The per-frame inline-image texture.
pub(crate) struct ImagePlane {
    #[cfg(target_os = "macos")]
    metal_atlas: Option<ArmAtlas>,
    w: u32,
    h: u32,
    placements: FxHashMap<(usize, usize, usize), (u32, u32, u32)>,
    _pinned_images: Vec<std::sync::Arc<aterm_core::grid::extra::ImageData>>,
    #[cfg(target_os = "macos")]
    metal_texels: Option<Vec<u8>>,
    #[cfg(target_os = "macos")]
    epoch: u64,
    #[cfg(gl_arm)]
    gl_texels: Option<Vec<u8>>,
    #[cfg(gl_arm)]
    gl_epoch: u64,
    #[cfg(gl_arm)]
    gl_atlas: Option<gl_arm::GlArmAtlas>,
    /// The D3D11 arm's twins of `gl_texels`/`gl_epoch`/`gl_atlas`.
    #[cfg(d3d11_arm)]
    d3d11_texels: Option<Vec<u8>>,
    #[cfg(d3d11_arm)]
    d3d11_epoch: u64,
    #[cfg(d3d11_arm)]
    d3d11_atlas: Option<d3d11_arm::D3dArmAtlas>,
}

/// GPU terminal renderer.
pub struct GpuRenderer {
    ctx: GpuContext,
    cpu: Renderer,
    #[cfg(not(target_arch = "wasm32"))]
    font_family: Option<String>,
    theme: Theme,
    enable_bloom: bool,
    hdr_glow: bool,
    backdrop_margins: bool,
    sdr_glow_boost: f32,
    deco_atlas: Option<DecoAtlas>,
    sprite_textures: SpriteTextureLookup,
    bloom_strength: f32,
    bloom_radius: f32,
    enable_shimmer: bool,
    shimmer_epoch: aterm_time::Instant,
    shimmer_phase_pin: Option<f32>,
    mono_res: Option<ResidentAtlas>,
    color_res: Option<ResidentAtlas>,
    resident_keys: FxHashSet<GlyphKey>,
    atlas_tex_creations: u64,
    #[cfg(target_os = "macos")]
    metal_armed: bool,
    #[cfg(target_os = "macos")]
    metal_mint_error: Option<String>,
    #[cfg(target_os = "macos")]
    metal_arm: Option<MetalArmCell>,
    #[cfg(target_os = "macos")]
    last_frame_arm_metal: bool,
    #[cfg(target_os = "macos")]
    metal_pending_band_shift: Option<BandShift>,
    #[cfg(target_os = "macos")]
    metal_inject_loss_after: Option<u64>,
    image_plane_epoch: u64,
    atlas_epoch: u64,
    #[cfg(gl_arm)]
    gl_armed: bool,
    #[cfg(gl_arm)]
    gl_arm: Option<gl_arm::GlArmLive>,
    #[cfg(gl_arm)]
    gl_mint_error: Option<String>,
    #[cfg(all(gl_arm, not(target_arch = "wasm32")))]
    gl_window_system: Option<crate::gl::device::WindowSystem>,
    #[cfg(gl_arm)]
    last_frame_arm_gl: bool,
    #[cfg(gl_arm)]
    gl_inject_loss_after: Option<u64>,
    // The D3D11 arm (Windows, `d3d11_arm.rs`): armed by `arm_d3d11` until the
    // flip, its lazily minted residency, why a mint failed, whether the last
    // frame drew on it, and the injected-loss drill
    // (`ATERM_D3D11_INJECT_LOSS=<n>`: after the n-th successful D3D11 present
    // the device's next frame answers as a removed device, so the latch,
    // `device_lost` and the frontend's CPU fallback run end to end).
    #[cfg(d3d11_arm)]
    d3d11_armed: bool,
    #[cfg(d3d11_arm)]
    d3d11_arm: Option<d3d11_arm::D3dArmLive>,
    #[cfg(d3d11_arm)]
    d3d11_mint_error: Option<String>,
    #[cfg(d3d11_arm)]
    last_frame_arm_d3d11: bool,
    #[cfg(d3d11_arm)]
    d3d11_inject_loss_after: Option<u64>,
    inst: Instances,
    fx: fx_gpu::FxGpu,
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    fx_palette_shared_seam: bool,
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    fx_fail_seam: bool,
    /// A test's opening rule for the frame's ramps ([`fx_gpu::ramps_open_fx`]).
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    ramp_cost_seam: Option<RampsFx>,
    gate_hits: u64,
    gate_misses: u64,
    dirty_scratch: Vec<bool>,
    row_plans: Vec<Vec<aterm_render::ColumnGlyph>>,
    deco_rects_scratch: Vec<[usize; 4]>,
    deco_skip_scratch: Vec<[usize; 4]>,
    curl_ink_scratch: Vec<bool>,
    curl_spans_scratch: Vec<(usize, usize)>,
    ligature_break_scratch: Vec<usize>,
    bloom_glow_scratch: Vec<GlowInstance>,
    bloom_quad_scratch: Vec<aterm_render::GlowQuad>,
    /// The bloom's item list, the frame's bloom stream where
    /// [`Self::bloom_items_live`].
    bloom_items: aterm_render::bloom::BloomItems,
    /// Whether this frame's bloom stream is [`Self::bloom_items`] (its glow
    /// carries ramps) rather than [`Self::bloom_glow_scratch`].
    bloom_items_live: bool,
    /// A glow stream's fx instances, laid before its slot's layers.
    glow_fx_scratch: Vec<fx_gpu::FxInstance>,
    key_scratch: Vec<Option<GlyphKey>>,
    glyph_win_scratch: Vec<Option<(i32, i32)>>,
    overshoot_surprise: bool,
    image_order_scratch: Vec<(
        std::sync::Arc<aterm_core::grid::extra::ImageData>,
        usize,
        usize,
        usize,
    )>,
    image_seen_scratch: FxHashMap<(usize, usize, usize), usize>,
    image_placements_scratch: FxHashMap<(usize, usize, usize), (u32, u32, u32)>,
    scissor_taken: u64,
    scroll_rescues: u64,
    full_repaints: u64,
    tray_uploads: u64,
    tray_upload_bytes: u64,
    last_instances: usize,
    last_bg_instances: usize,
    last_frame_passes: u32,
    frame_shape: FrameShape,
    ablation: UnormAblation,
    /// The route the Metal arm's frame plans take where the device passed the fetch probe:
    /// [`UnormRoute::Fetch`] unless the `ATERM_GPU_UNORM_ROUTE` seam or [`Self::set_unorm_route`]
    /// chose the split.
    unorm_route: UnormRoute,
    /// A verdict standing in for the Metal arm's ([`Self::set_fetch_probe_seam`]).
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    fetch_probe_seam: Option<FetchProbe>,
    /// The first-frame warm the Metal arm's mint waits out ([`crate::metal::warm::FIRST_FRAME`],
    /// or a test's own, `Self::set_first_frame_latch_for_test`).
    #[cfg(target_os = "macos")]
    first_frame: &'static crate::metal::warm::WarmLatch,
    /// [`Self::warm_effect_pipelines`] was asked for: the Metal arm's mint starts the warm that a
    /// request before it could not.
    #[cfg(target_os = "macos")]
    metal_warm_wanted: bool,
    /// A window surface attached on this renderer, whose swapchain may be f16 (an EDR screen,
    /// `hdr_glow`); a headless renderer presents to a Bgra8Unorm target only.
    #[cfg(target_os = "macos")]
    metal_on_screen: bool,
    last_gpu_time_serial: Option<u64>,
    frame_plan: Vec<DrawItem>,
    last_frame_load: crate::device_layer::FrameLoad,
    last_frame_scissor: Option<(u32, u32, u32, u32)>,
}

/// The previous presented frame's overlay state, for the scissored dirty-row repaint.
pub(crate) struct PresentPrev {
    blink_phase: bool,
    cursor_style_override: Option<CursorStyle>,
    grid_top: usize,
    #[cfg(target_os = "macos")]
    on_metal: bool,
    #[cfg(gl_arm)]
    on_gl: bool,
    /// The D3D11 arm's twin of `on_metal`.
    #[cfg(d3d11_arm)]
    on_d3d11: bool,
}

enum RepaintScope<'a> {
    Full,
    Dirty(&'a [bool]),
}

/// The GPU dirty-gate cache.
pub(crate) struct GpuGateCache {
    input: RenderInput,
    blink_phase: bool,
    cursor_style_override: Option<CursorStyle>,
    grid_top: usize,
    frame: Frame,
}

/// Projection of both GPU layout caches onto the `AsymmetricPadLayout` cache-origin variables.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AsymmetricPadGpuCacheProjection {
    pub grid_top: usize,
    pub present_cached_grid_top: Option<usize>,
    pub gate_cached_grid_top: Option<usize>,
}

/// The armed arm's per-window offscreen pair.
#[cfg(target_os = "macos")]
pub(crate) struct MetalOffscreen {
    pub(crate) tex: crate::metal::resources::SealedTexture,
    pub(crate) srgb: crate::metal::resources::SealedTexture,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

/// The armed arm's resident settings card (`TrayOverlay` twin).
#[cfg(target_os = "macos")]
pub(crate) struct MetalTrayCard {
    texes: Vec<crate::device_layer::LayerTexture>,
    ring: crate::tray_card_ring::TrayCardRing,
    pub(crate) w: u32,
    pub(crate) h: u32,
    pixels: Vec<u8>,
}

#[cfg(target_os = "macos")]
impl MetalTrayCard {
    /// The texture the card's draw samples: the shown slot's.
    #[inline]
    pub(crate) fn tex(&self) -> &crate::metal::resources::SealedTexture {
        self.texes[self.ring.shown()].metal()
    }
}

/// Return the dirty row band (`y0..y1`) where `old` and `new` differ.
#[cfg(target_os = "macos")]
pub(crate) fn changed_row_band(old: &[u8], new: &[u8], stride: usize) -> Option<(u32, u32)> {
    if stride == 0 || old.len() != new.len() {
        return None;
    }
    let rows = new.len() / stride;
    let differs =
        |row: usize| old[row * stride..(row + 1) * stride] != new[row * stride..(row + 1) * stride];
    let first = (0..rows).find(|row| differs(*row))?;
    let last = (first..rows)
        .rev()
        .find(|row| differs(*row))
        .unwrap_or(first);
    Some((first as u32, last as u32 + 1))
}

#[cfg(target_os = "macos")]
const VIRTUAL_PRESENT_TEXEL: crate::device_layer::TexelFormat =
    crate::device_layer::TexelFormat::Bgra8Unorm;

#[derive(Default)]
struct Instances {
    keys: FxHashSet<GlyphKey>,
    key_order: Vec<GlyphKey>,
    bg: Vec<BgInstance>,
    image_below_bg: Vec<GlyphInstance>,
    image_bg_cover: Vec<BgInstance>,
    image_under: Vec<GlyphInstance>,
    image: Vec<GlyphInstance>,
    glyph: Vec<GlyphInstance>,
    glyph_halo: Vec<GlyphInstance>,
    color: Vec<GlyphInstance>,
    cursor: Vec<BgInstance>,
    deco: Vec<BgInstance>,
    curl: Vec<GlyphInstance>,
    trail: Vec<BgInstance>,
    glow_add: Vec<GlowInstance>,
    glow_halo: Vec<RainGlowInstance>,
    glow_halo_over: Vec<RainGlowInstance>,
    glow_under: Vec<GlowInstance>,
    fire_add: Vec<FireInstance>,
    fire_over: Vec<FireInstance>,
    nova_add: Vec<GlowInstance>,
    rain_add: Vec<RainGlowInstance>,
    rain_add_over: Vec<RainGlowInstance>,
    wdeco_over: Vec<GlyphInstance>,
    wdeco_add: Vec<GlyphInstance>,
    wallpaper: Vec<GlyphInstance>,
    cursor_block: Vec<BgInstance>,
    cursor_glyph: Vec<GlyphInstance>,
    cursor_color: Vec<GlyphInstance>,
    fx: [Vec<fx_gpu::FxInstance>; fx_gpu::FX_STREAM_COUNT],
}

impl Instances {
    /// One stream's CPU-side bytes and instance count, as both tails upload them.
    #[inline]
    pub(crate) fn stream_bytes(&self, id: StreamId) -> (&[u8], u32) {
        macro_rules! stream {
            ($field:ident) => {
                (
                    aterm_bits::cast_slice(&self.$field),
                    self.$field.len() as u32,
                )
            };
        }
        match id {
            StreamId::Wallpaper => stream!(wallpaper),
            StreamId::Bg => stream!(bg),
            StreamId::ImageBelowBg => stream!(image_below_bg),
            StreamId::ImageBgCover => stream!(image_bg_cover),
            StreamId::FxLinearUnder => self.fx_stream(fx_gpu::FxStream::LinearUnder),
            StreamId::FxKnockUnder => self.fx_stream(fx_gpu::FxStream::KnockUnder),
            StreamId::Trail => stream!(trail),
            StreamId::GlowUnder => stream!(glow_under),
            StreamId::FxUnder => self.fx_stream(fx_gpu::FxStream::Under),
            StreamId::FireAdd => stream!(fire_add),
            StreamId::FireOver => stream!(fire_over),
            StreamId::ImageUnder => stream!(image_under),
            StreamId::GlyphHalo => stream!(glyph_halo),
            StreamId::Glyph => stream!(glyph),
            StreamId::Color => stream!(color),
            StreamId::Image => stream!(image),
            StreamId::Deco => stream!(deco),
            StreamId::Curl => stream!(curl),
            StreamId::GlowAdd => stream!(glow_add),
            StreamId::FxGlow => self.fx_stream(fx_gpu::FxStream::Glow),
            StreamId::GlowHalo => stream!(glow_halo),
            StreamId::GlowHaloOver => stream!(glow_halo_over),
            StreamId::NovaAdd => stream!(nova_add),
            StreamId::FxOver => self.fx_stream(fx_gpu::FxStream::Over),
            StreamId::RainAdd => stream!(rain_add),
            StreamId::RainAddOver => stream!(rain_add_over),
            StreamId::WdecoOver => stream!(wdeco_over),
            StreamId::WdecoAdd => stream!(wdeco_add),
            StreamId::FxLinearOver => self.fx_stream(fx_gpu::FxStream::LinearOver),
            StreamId::CursorBlock => stream!(cursor_block),
            StreamId::CursorGlyph => stream!(cursor_glyph),
            StreamId::CursorColor => stream!(cursor_color),
            StreamId::Cursor => stream!(cursor),
        }
    }

    #[inline]
    fn fx_stream(&self, s: fx_gpu::FxStream) -> (&[u8], u32) {
        let v = &self.fx[s as usize];
        (aterm_bits::cast_slice(v), v.len() as u32)
    }

    #[inline]
    fn fx_instances(&self) -> usize {
        self.fx.iter().map(Vec::len).sum()
    }

    fn clear(&mut self) {
        self.keys.clear();
        self.key_order.clear();
        self.bg.clear();
        self.image_below_bg.clear();
        self.image_bg_cover.clear();
        self.image_under.clear();
        self.image.clear();
        self.glyph.clear();
        self.glyph_halo.clear();
        self.color.clear();
        self.cursor.clear();
        self.deco.clear();
        self.curl.clear();
        self.trail.clear();
        self.glow_add.clear();
        self.glow_halo.clear();
        self.glow_halo_over.clear();
        self.glow_under.clear();
        self.fire_add.clear();
        self.fire_over.clear();
        self.nova_add.clear();
        self.rain_add.clear();
        self.rain_add_over.clear();
        self.wdeco_over.clear();
        self.wdeco_add.clear();
        self.wallpaper.clear();
        self.cursor_block.clear();
        self.cursor_glyph.clear();
        self.cursor_color.clear();
        for v in &mut self.fx {
            v.clear();
        }
    }
}

trust_contract_fn! {
    /// Whether a vertex stream may be sliced and bound.
    #[must_use]
    pub fn should_slice(byte_len: usize) -> bool
    clauses [ensures result == (byte_len != 0) by should_slice_is_nonempty]
    {
        byte_len != 0
    }
}

verify_island! {
    clean {
        theorem should_slice_is_nonempty (byte_len : UInt64) :
            trust_import_aterm_gpu__renderer__should_slice byte_len
              = !(decide (byte_len = 0)) := by
          simp only [trust_import_aterm_gpu__renderer__should_slice]
    }
}

use aterm_render::bloom::{BLOOM_DOWNSCALE, BLOOM_RADIUS, BLOOM_STRENGTH};

/// The chroma radius paired with a white blur `radius`, used by every uniform write site.
#[inline]
pub(crate) const fn bloom_chroma_radius(radius: f32) -> f32 {
    aterm_render::bloom::chroma_radius(radius)
}

const SHIMMER_BANDS: usize = 64;
const SHIMMER_AMP_PX: f32 = 1.5;
const SHIMMER_RISE_CELLS: f32 = 3.5;
const SHIMMER_HEAT_GAIN: f32 = 1.35;
const SHIMMER_COPY_MARGIN: u32 = 4;
const SHIMMER_PHASE_WRAP_S: f32 = 100.0;

struct ShimmerRegion {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    hot_top: f32,
    rise: f32,
    band_x0: f32,
    band_w: f32,
    heat: [f32; SHIMMER_BANDS],
}

/// One of the effect-only cell pipelines, ordered by [`EFFECT_PIPELINE_NAMES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub enum EffectPipeline {
    CursorBlend = 0,
    GlowAdd = 1,
    RainGlow = 2,
    RainGlowOver = 3,
    FireAdd = 4,
    FireOver = 5,
    DecoOver = 6,
    DecoAdd = 7,
    SpriteOver = 8,
    Fx = 9,
    FxLinear = 10,
    FxStore = 11,
}

/// How many effect pipelines exist.
pub const EFFECT_PIPELINE_COUNT: usize = 12;

/// Names of the [`EFFECT_PIPELINE_COUNT`] slots, in [`EffectPipeline`] order.
pub const EFFECT_PIPELINE_NAMES: [&str; EFFECT_PIPELINE_COUNT] = [
    "cursor_blend",
    "glow_add",
    "rain_glow",
    "rain_glow_over",
    "fire_add",
    "fire_over",
    "deco_over",
    "deco_add",
    "sprite_over",
    "fx",
    "fx_linear",
    "fx_store",
];

/// Every [`EffectPipeline`], in slot order.
pub const EFFECT_PIPELINES: [EffectPipeline; EFFECT_PIPELINE_COUNT] = [
    EffectPipeline::CursorBlend,
    EffectPipeline::GlowAdd,
    EffectPipeline::RainGlow,
    EffectPipeline::RainGlowOver,
    EffectPipeline::FireAdd,
    EffectPipeline::FireOver,
    EffectPipeline::DecoOver,
    EffectPipeline::DecoAdd,
    EffectPipeline::SpriteOver,
    EffectPipeline::Fx,
    EffectPipeline::FxLinear,
    EffectPipeline::FxStore,
];

impl GpuRenderer {
    /// Acquire a GPU and a CPU font face.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(px: f32, theme: Theme) -> Result<Self, String> {
        Self::new_with_family(None, px, theme)
    }

    /// Like [`GpuRenderer::new`], but resolves a configured font family (or font file path) first,
    /// mirroring [`Renderer::from_system_with_family`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_with_family(family: Option<&str>, px: f32, theme: Theme) -> Result<Self, String> {
        let family_owned = family.map(String::from);
        Self::new_loading(family.map(String::from), theme, move || {
            Renderer::from_system_with_family(family_owned.as_deref(), px, theme)
        })
    }

    /// Like [`GpuRenderer::new_with_family`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_with_face(
        face: Option<aterm_render::FaceRef>,
        family: Option<&str>,
        px: f32,
        theme: Theme,
    ) -> Result<Self, String> {
        Self::new_loading(family.map(String::from), theme, move || {
            Renderer::from_system_with_face(face.as_ref(), px, theme)
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn new_loading(
        family: Option<String>,
        theme: Theme,
        load: impl FnOnce() -> Option<Renderer> + Send + 'static,
    ) -> Result<Self, String> {
        let font_handle = std::thread::spawn(move || {
            let font_started = aterm_time::Instant::now();
            let cpu = load();
            crate::startup_probe::record(
                crate::startup_probe::Leg::FontThread,
                font_started.elapsed(),
            );
            cpu
        });
        let ctx = GpuContext::new()?;
        let font_join_started = aterm_time::Instant::now();
        let cpu = font_handle
            .join()
            .map_err(|_| "font-load thread panicked".to_string())?
            .ok_or("no system monospace font")?;
        crate::startup_probe::record(
            crate::startup_probe::Leg::FontJoin,
            font_join_started.elapsed(),
        );
        Self::from_parts(ctx, cpu, family, theme)
    }

    /// Assemble a `GpuRenderer` from an acquired [`GpuContext`] and a CPU [`Renderer`].
    pub fn from_parts(
        ctx: GpuContext,
        cpu: Renderer,
        font_family: Option<String>,
        theme: Theme,
    ) -> Result<Self, String> {
        #[cfg(target_arch = "wasm32")]
        let _ = font_family; // no system font discovery in the browser
        let from_parts_started = aterm_time::Instant::now();

        let uniform_atlas_started = aterm_time::Instant::now();
        crate::startup_probe::record(
            crate::startup_probe::Leg::PipeUniformAtlas,
            uniform_atlas_started.elapsed(),
        );

        let pipe_tail_started = aterm_time::Instant::now();
        let renderer = Self {
            ctx,
            cpu,
            #[cfg(not(target_arch = "wasm32"))]
            font_family,
            theme,
            enable_bloom: true,
            hdr_glow: false,
            backdrop_margins: false,
            sdr_glow_boost: 0.0,
            deco_atlas: None,
            sprite_textures: SpriteTextureLookup::default(),
            bloom_strength: BLOOM_STRENGTH,
            bloom_radius: BLOOM_RADIUS,
            enable_shimmer: true,
            shimmer_epoch: aterm_time::Instant::now(),
            shimmer_phase_pin: None,
            mono_res: None,
            color_res: None,
            resident_keys: FxHashSet::default(),
            atlas_tex_creations: 0,
            #[cfg(target_os = "macos")]
            metal_armed: crate::metal_backend_selected(),
            #[cfg(target_os = "macos")]
            metal_mint_error: None,
            #[cfg(target_os = "macos")]
            metal_arm: None,
            #[cfg(target_os = "macos")]
            last_frame_arm_metal: false,
            #[cfg(target_os = "macos")]
            metal_pending_band_shift: None,
            image_plane_epoch: 0,
            atlas_epoch: 0,
            // Windows draws, presents and reads back on the D3D11 arm (GW.5's
            // flip).
            #[cfg(d3d11_arm)]
            d3d11_armed: true,
            #[cfg(d3d11_arm)]
            d3d11_arm: None,
            #[cfg(d3d11_arm)]
            d3d11_mint_error: None,
            #[cfg(d3d11_arm)]
            last_frame_arm_d3d11: false,
            // A development seam (`dev_seam!`), never read by a shipped binary.
            #[cfg(d3d11_arm)]
            d3d11_inject_loss_after: aterm_types::dev_seam!("ATERM_D3D11_INJECT_LOSS")
                .and_then(|v| v.to_str()?.parse::<u64>().ok())
                .filter(|n| *n > 0),
            #[cfg(gl_arm)]
            gl_armed: true,
            #[cfg(gl_arm)]
            gl_arm: None,
            #[cfg(gl_arm)]
            gl_mint_error: None,
            #[cfg(all(gl_arm, not(target_arch = "wasm32")))]
            gl_window_system: None,
            #[cfg(gl_arm)]
            last_frame_arm_gl: false,
            #[cfg(gl_arm)]
            gl_inject_loss_after: aterm_types::dev_seam!("ATERM_GL_INJECT_LOSS")
                .and_then(|v| v.to_str()?.parse::<u64>().ok())
                .filter(|n| *n > 0),
            #[cfg(target_os = "macos")]
            metal_inject_loss_after: aterm_types::dev_seam!("ATERM_METAL_INJECT_LOSS")
                .and_then(|v| v.to_str()?.parse::<u64>().ok())
                .filter(|n| *n > 0),
            inst: Instances::default(),
            fx: fx_gpu::FxGpu::default(),
            #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
            fx_palette_shared_seam: false,
            #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
            fx_fail_seam: false,
            #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
            ramp_cost_seam: None,
            gate_hits: 0,
            gate_misses: 0,
            dirty_scratch: Vec::new(),
            row_plans: Vec::new(),
            deco_rects_scratch: Vec::new(),
            deco_skip_scratch: Vec::new(),
            curl_ink_scratch: Vec::new(),
            curl_spans_scratch: Vec::new(),
            ligature_break_scratch: Vec::new(),
            bloom_glow_scratch: Vec::new(),
            bloom_quad_scratch: Vec::new(),
            bloom_items: aterm_render::bloom::BloomItems::default(),
            bloom_items_live: false,
            glow_fx_scratch: Vec::new(),
            key_scratch: Vec::new(),
            glyph_win_scratch: Vec::new(),
            overshoot_surprise: false,
            image_order_scratch: Vec::new(),
            image_seen_scratch: FxHashMap::default(),
            image_placements_scratch: FxHashMap::default(),
            scissor_taken: 0,
            scroll_rescues: 0,
            full_repaints: 0,
            tray_uploads: 0,
            tray_upload_bytes: 0,
            last_instances: 0,
            last_bg_instances: 0,
            last_frame_passes: 0,
            frame_shape: FrameShape::default(),
            ablation: aterm_types::dev_seam!("ATERM_GPU_ABLATE_UNORM")
                .and_then(|v| v.to_str().map(UnormAblation::from_seam))
                .unwrap_or_default(),
            unorm_route: UnormRoute::initial(
                aterm_types::dev_seam!("ATERM_GPU_UNORM_ROUTE").as_deref(),
            ),
            #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
            fetch_probe_seam: None,
            #[cfg(target_os = "macos")]
            first_frame: &crate::metal::warm::FIRST_FRAME,
            #[cfg(target_os = "macos")]
            metal_warm_wanted: false,
            #[cfg(target_os = "macos")]
            metal_on_screen: false,
            last_gpu_time_serial: None,
            frame_plan: Vec::new(),
            last_frame_load: crate::device_layer::FrameLoad::Load,
            last_frame_scissor: None,
        };
        crate::startup_probe::record(
            crate::startup_probe::Leg::PipeTail,
            pipe_tail_started.elapsed(),
        );
        crate::startup_probe::record(
            crate::startup_probe::Leg::PipeTotal,
            from_parts_started.elapsed(),
        );
        Ok(renderer)
    }

    /// Install an already-admitted face and publish its configured family as one transaction.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn install_prepared_font(
        &mut self,
        family: Option<String>,
        renderer: Renderer,
        theme: Theme,
    ) {
        self.font_family = family;
        self.set_face(renderer, theme);
    }

    /// Forwards [`fallback_parse_pending`](Renderer::fallback_parse_pending).
    #[inline]
    pub fn fallback_parse_pending(&mut self) -> bool {
        self.cpu.fallback_parse_pending()
    }

    /// Forwards [`take_missing_font_classes`](Renderer::take_missing_font_classes).
    #[inline]
    pub fn take_missing_font_classes(&mut self) -> u8 {
        self.cpu.take_missing_font_classes()
    }

    /// Forward of the inner CPU rasterizer's
    /// [`set_runtime_font_discovery`](Renderer::set_runtime_font_discovery).
    #[inline]
    pub fn set_runtime_font_discovery(&mut self, enabled: bool) {
        self.raster_changed(|cpu| cpu.set_runtime_font_discovery(enabled));
    }

    /// Test/debug forward of [`Renderer::debug_block_on_lazy_fallbacks`], so CPU-vs-GPU
    /// comparisons are deterministic.
    #[doc(hidden)]
    #[inline]
    pub fn debug_block_on_lazy_fallbacks(&mut self) {
        self.cpu.debug_block_on_lazy_fallbacks();
    }

    /// A test's negative control: draw companions under the text without the knock-out
    /// (`aterm_render::Renderer::set_knock_out`), on the glyph-knock stream as on the CPU twin.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn set_knock_out(&mut self, on: bool) {
        self.cpu.set_knock_out(on);
    }

    /// Swap in an already-built CPU face and theme in place. The face keeps its own
    /// variation requests (`font_weight`, `font_variation`): a prepared face was built
    /// with the config it installs, and a rebuild carries the live ones
    /// ([`Renderer::rebuild_from_admitted`]).
    pub fn set_face(&mut self, cpu: Renderer, theme: Theme) {
        let prior_shaping = self.cpu.text_shaping().clone();
        let prior_blending = self.cpu.text_blending();
        let prior_thicken = self.cpu.font_thicken();
        let prior_stem_gamma = self.cpu.stem_gamma();
        self.cpu = cpu;
        self.cpu.set_text_shaping(prior_shaping);
        self.cpu.set_text_blending(prior_blending);
        self.cpu.set_font_thicken(prior_thicken);
        self.cpu.set_stem_gamma(prior_stem_gamma);
        self.theme = theme;
        self.resident_keys.clear();
        self.mono_res = None;
        self.color_res = None;
    }

    /// Install the text-shaping config (ligatures, OpenType features) and drop the atlas.
    #[inline]
    pub fn set_text_shaping(&mut self, shaping: aterm_render::TextShapingConfig) {
        self.raster_changed(|cpu| cpu.set_text_shaping(shaping));
    }

    /// Configured primary-face `font_features` tags the active font cannot apply.
    #[must_use]
    #[inline]
    pub fn unsupported_user_feature_tags(&self) -> Vec<[u8; 4]> {
        self.cpu.unsupported_user_feature_tags()
    }

    /// Install the coverage blend mode (config `text_blending`) on the wrapped CPU face, which
    /// `encode_frame` reads for the shader uniform.
    #[inline]
    pub fn set_text_blending(&mut self, mode: aterm_render::TextBlending) {
        self.cpu.set_text_blending(mode);
    }

    /// The active coverage blend mode (off the wrapped CPU face).
    #[must_use]
    #[inline]
    pub const fn text_blending(&self) -> aterm_render::TextBlending {
        self.cpu.text_blending()
    }

    /// Set macOS `font_thicken` on the wrapped CPU face and drop the atlas when it changed.
    pub fn set_font_thicken(&mut self, on: bool) {
        self.raster_changed(|cpu| {
            let changed = cpu.font_thicken() != on;
            if changed {
                cpu.set_font_thicken(on);
            }
            changed
        });
    }

    /// Install the variable-font config (`font_variation`, `font_weight`) and rebuild the atlas.
    pub fn set_font_variations(&mut self, requests: &[(u32, f32)], dark_nudge: f32) {
        self.raster_changed(|cpu| cpu.set_font_variations(requests, dark_nudge));
    }

    /// Set `stem_gamma` on the wrapped CPU face and drop the atlas on a change.
    pub fn set_stem_gamma(&mut self, gamma: f32) {
        self.raster_changed(|cpu| {
            let before = cpu.stem_gamma();
            cpu.set_stem_gamma(gamma);
            (cpu.stem_gamma() - before).abs() > f32::EPSILON
        });
    }

    /// Set the Linux/Windows grid-fitting mode (`font_hinting`) and drop the atlas on a change.
    pub fn set_font_hinting(&mut self, mode: &str) {
        self.raster_changed(|cpu| cpu.set_font_hinting(mode));
    }

    /// Record the Linux subpixel-RGB mode (`font_subpixel`) on the wrapped CPU face.
    pub fn set_font_subpixel(&mut self, mode: &str) {
        let _ = self.cpu.set_font_subpixel(mode);
    }

    /// Replace the theme live on the GPU state and the CPU face without a rebuild.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.raster_changed(|cpu| {
            let coords_before: Option<Vec<(u32, f32)>> = cpu.variation_coords().map(<[_]>::to_vec);
            cpu.set_theme(theme);
            cpu.variation_coords() != coords_before.as_deref()
        });
    }

    /// Selected-text foreground (theme `selectionForeground`), or `None` for the contrast floor.
    #[inline]
    pub fn set_selection_fg(&mut self, fg: Option<u32>) {
        self.cpu.set_selection_fg(fg);
    }

    /// Per-cell minimum contrast ratio (xterm's `minimumContrastRatio`; `<= 1.0` is off).
    #[inline]
    pub fn set_minimum_contrast(&mut self, ratio: f32) {
        self.cpu.set_minimum_contrast(ratio);
    }

    /// Default-background opacity (0..=1; `1.0` = opaque, the default).
    #[inline]
    pub fn set_background_opacity(&mut self, opacity: f32) {
        self.cpu.set_background_opacity(opacity);
    }

    /// The configured default-background opacity (off the wrapped CPU face).
    #[must_use]
    #[inline]
    pub const fn background_opacity(&self) -> f32 {
        self.cpu.background_opacity()
    }

    /// Cursor fill opacity (0..=1; `1.0` = opaque fill plus block cut-out, the default).
    #[inline]
    pub fn set_cursor_opacity(&mut self, opacity: f32) {
        self.cpu.set_cursor_opacity(opacity);
    }

    /// The configured cursor-fill opacity (off the wrapped CPU face).
    #[must_use]
    #[inline]
    pub const fn cursor_opacity(&self) -> f32 {
        self.cpu.cursor_opacity()
    }

    /// Mark the pane unfocused (`true`) or focused for selection theming.
    #[inline]
    pub fn set_selection_inactive(&mut self, inactive: bool) {
        self.cpu.set_selection_inactive(inactive);
    }

    /// Inactive (unfocused) selection bg (0x00RRGGBB), or `None` to derive it from the theme.
    #[inline]
    pub fn set_selection_inactive_bg(&mut self, bg: Option<u32>) {
        self.cpu.set_selection_inactive_bg(bg);
    }

    /// Re-rasterize at a new pixel size (DPI change).
    #[inline]
    pub fn set_px(&mut self, px: f32) {
        self.raster_changed(|cpu| cpu.set_px(px));
    }

    /// Per-window size switch.
    #[inline]
    pub fn activate_px(&mut self, px: f32) {
        self.cpu.activate_px(px);
    }

    /// Cell geometry `(cell_w, cell_h, baseline)` at `px`, from the CPU face.
    #[inline]
    pub fn cell_geometry(&self, px: f32) -> (usize, usize, i32) {
        self.cpu.cell_geometry(px)
    }

    /// The band's `(head, foot)` room at the active size, from the wrapped CPU face's one memo.
    #[inline]
    pub fn band_room(&mut self) -> (usize, usize) {
        self.cpu.band_room()
    }

    /// Pure per-window decoration bands at `px`, delegated like [`Self::cell_geometry`].
    #[inline]
    pub fn deco_metrics_for(&self, px: f32) -> aterm_render::deco::DecoMetrics {
        self.cpu.deco_metrics_for(px)
    }

    /// Set a broad-coverage fallback face from font bytes and invalidate the atlas.
    pub fn set_fallback_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_fallback_bytes(bytes))
    }

    /// Append a fallback face, so several scripts can find coverage, and invalidate the atlas.
    pub fn add_fallback_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.add_fallback_bytes(bytes))
    }

    /// Set a colour-emoji (sbix) face from font bytes and invalidate the atlas.
    pub fn set_emoji_font_bytes(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_color_font_bytes(bytes))
    }

    /// Set a real bold face (`FaceId::BoldPrimary`) so SGR bold is not synthetic.
    pub fn set_bold_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.set_bold_face(bytes, 0, None)
    }

    /// [`Self::set_bold_font_bytes`] for face `index` of `bytes`, drawn at `named` where
    /// its font name chose an instance ([`aterm_render::FaceRef::instance`]).
    pub fn set_bold_face(
        &mut self,
        bytes: &[u8],
        index: u32,
        named: Option<aterm_render::NamedInstance>,
    ) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_named_bold_face(bytes, index, named))
    }

    /// Set a symbol fallback face (config `symbol_font`) from font bytes and invalidate the atlas.
    pub fn set_symbol_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_symbol_fallback_bytes(bytes))
    }

    /// Set a real styled face and invalidate the atlas.
    pub fn set_styled_font_bytes(&mut self, slot: usize, bytes: &[u8]) -> Result<(), String> {
        self.set_styled_face(slot, bytes, 0, None)
    }

    /// [`Self::set_styled_font_bytes`] for face `index` of `bytes`, drawn at `named` where
    /// its font name chose an instance ([`aterm_render::FaceRef::instance`]).
    pub fn set_styled_face(
        &mut self,
        slot: usize,
        bytes: &[u8],
        index: u32,
        named: Option<aterm_render::NamedInstance>,
    ) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_named_styled_face(slot, bytes, index, named))
    }

    /// Enable/disable synthetic bold/italic (`font_synthetic_style`).
    pub fn set_synthetic_styles(&mut self, on: bool) {
        self.raster_changed(|cpu| cpu.set_synthetic_styles(on));
    }

    /// Install the config fallback chain (`fallback_fonts`), ordered config before built-ins.
    pub fn set_config_fallback_fonts(&mut self, paths: &[String]) {
        self.raster_changed(|cpu| cpu.set_config_fallback_fonts(paths));
    }

    /// Install the config symbol-fallback font (`symbol_font`).
    pub fn set_config_symbol_font(&mut self, path: Option<&str>) {
        self.raster_changed(|cpu| cpu.set_config_symbol_font(path));
    }

    /// Install the config colour-emoji font (`emoji_font`).
    pub fn set_config_emoji_font(&mut self, path: Option<&str>) {
        self.raster_changed(|cpu| cpu.set_config_emoji_font(path));
    }

    /// Install the display layer (`display_font`), invalidating the atlas when it changed.
    pub fn set_display_faces(&mut self, ids: &[&str]) -> Result<(), String> {
        let before = self.cpu.display_faces();
        self.cpu.set_display_faces(ids)?;
        if self.cpu.display_faces() != before {
            self.invalidate_atlas();
        }
        Ok(())
    }

    /// Swap the primary face and invalidate the atlas.
    pub fn set_primary_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_primary_font(bytes))
    }

    /// Scale the cell box height without changing glyph px.
    pub fn set_line_height(&mut self, scale: f32) {
        self.raster_changed(|cpu| cpu.set_line_height(scale));
    }

    /// Shift the baseline by a signed px delta (`adjust_baseline`).
    pub fn set_adjust_baseline(&mut self, px: i32) {
        self.raster_changed(|cpu| {
            let before = cpu.adjust_baseline();
            cpu.set_adjust_baseline(px);
            cpu.adjust_baseline() != before
        });
    }

    /// Underline position/thickness adjustments (`adjust_underline_*`).
    #[inline]
    pub fn set_adjust_underline(&mut self, pos_px: i32, thick_px: i32) {
        self.cpu.set_adjust_underline(pos_px, thick_px);
    }

    /// Descender ink-skip toggle, read by both paths from the CPU face.
    #[inline]
    pub fn set_underline_skip_descenders(&mut self, on: bool) {
        self.cpu.set_underline_skip_descenders(on);
    }

    /// The CPU face's resolved primary font `(bytes, collection index)` for GUI chrome text.
    #[must_use]
    #[inline]
    pub fn chrome_primary_face(&self) -> Option<(std::sync::Arc<[u8]>, u32)> {
        self.cpu.chrome_primary_face()
    }

    /// Immutable source-path metadata for the wrapped primary generation.
    #[must_use]
    #[inline]
    pub fn primary_source_path(&self) -> Option<&str> {
        self.cpu.primary_source_path()
    }

    /// Arc-backed source snapshot of the wrapped font generation.
    #[must_use]
    #[inline]
    pub fn admitted_font_sources(&self) -> aterm_render::AdmittedFontSources {
        self.cpu.admitted_font_sources()
    }

    /// Whether the wrapped CPU generation is sealed against later font I/O.
    #[must_use]
    #[inline]
    pub const fn admitted_font_sources_sealed(&self) -> bool {
        self.cpu.admitted_font_sources_sealed()
    }

    /// Worker-only settlement of every path-backed face in the wrapped generation, for hosts that
    /// did not seal before installing.
    #[inline]
    pub fn seal_admitted_font_sources(&mut self) -> aterm_render::AdmittedFontSources {
        self.cpu.seal_admitted_font_sources()
    }

    /// Pre-rasterize regular and bold printable ASCII so the first atlas build is a copy.
    #[inline]
    pub fn prewarm_ascii(&mut self) {
        self.cpu.prewarm_ascii();
    }

    /// Diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub fn glyph_cache_len(&self) -> usize {
        self.cpu.glyph_cache_len()
    }

    /// Rebuild the font at `px`/`theme` from the sealed sources, keeping the device, pipelines and
    /// surfaces, and invalidating the atlases once.
    pub fn rebuild_font_from_admitted(&mut self, px: f32, theme: Theme) -> Result<(), String> {
        let cpu = self.cpu.rebuild_from_admitted(px, theme)?;
        self.set_face(cpu, theme);
        Ok(())
    }

    /// Rebuild this renderer from the sealed font sources on a fresh GPU context.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn rebuild_on_fresh_context(&self, px: f32) -> Result<Self, String> {
        let cpu = self.cpu.rebuild_from_admitted(px, self.theme)?;
        let ctx = GpuContext::new()?;
        Self::from_parts(ctx, cpu, self.font_family.clone(), self.theme)
    }

    /// What the wrapped sealed generation draws each style with.
    #[must_use]
    #[inline]
    pub fn drawn_faces(&self) -> Option<&aterm_render::DrawnFaces> {
        self.cpu.drawn_faces()
    }

    /// The wrapped CPU face's real bold sibling of the primary family for the GUI chrome.
    #[must_use]
    #[inline]
    pub fn chrome_bold_face(&mut self) -> Option<(std::sync::Arc<[u8]>, u32)> {
        self.cpu.chrome_bold_face()
    }

    /// Fork the wrapped CPU font engine for a renderer-native semantic surface.
    #[must_use]
    #[inline]
    pub fn fork_semantic_surface(&self, px: f32, theme: Theme) -> Option<aterm_render::Renderer> {
        self.cpu.fork_semantic_surface(px, theme)
    }

    #[inline]
    fn raster_changed<R: RasterEdit>(&mut self, edit: impl FnOnce(&mut Renderer) -> R) -> R {
        let out = edit(&mut self.cpu);
        if out.dropped_rasters() {
            self.invalidate_atlas();
        }
        out
    }

    #[inline]
    fn invalidate_atlas(&mut self) {
        self.resident_keys.clear();
        self.mono_res = None;
        self.color_res = None;
    }

    /// Test/diagnostic: `render_input_cached` calls that hit the dirty gate.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn gate_hits(&self) -> u64 {
        self.gate_hits
    }

    /// Test/diagnostic: `render_input_cached` calls that missed the dirty gate.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn gate_misses(&self) -> u64 {
        self.gate_misses
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn scissor_taken(&self) -> u64 {
        self.scissor_taken
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn scroll_rescues(&self) -> u64 {
        self.scroll_rescues
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[cfg(target_os = "macos")]
    #[inline]
    pub fn metal_encode_inline_waits(&self) -> u64 {
        self.metal_arm
            .as_ref()
            .map_or(0, |c| c.live_ref().encode_inline_waits.get())
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[cfg(target_os = "macos")]
    #[inline]
    pub fn metal_encode_band_shifts(&self) -> u64 {
        self.metal_arm
            .as_ref()
            .map_or(0, |c| c.live_ref().encode_band_shifts.get())
    }

    /// Test seam.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_fx_hazard(&self) -> FxHazard {
        self.metal_arm
            .as_ref()
            .map_or_else(FxHazard::default, |c| c.live_ref().fx_hazard)
    }

    /// Test seam: where the frame's glow ramps draw, against the measured
    /// rule ([`fx_gpu::ramps_open_fx`]; `None` restores it).
    #[doc(hidden)]
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    pub const fn set_ramps_fx_for_test(&mut self, route: Option<RampsFx>) {
        self.ramp_cost_seam = route;
    }

    /// Whether a frame whose fx stream draws `fx_open` puts `ramps` in it
    /// ([`fx_gpu::ramps_open_fx`] under this renderer's rule).
    #[inline]
    fn ramps_open_fx(&self, fx_open: bool, ramps: [&aterm_render::GlowRamps; 2]) -> bool {
        #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
        match self.ramp_cost_seam {
            Some(RampsFx::Always) => return ramps.iter().any(|r| !r.is_empty()),
            Some(RampsFx::Never) => return false,
            None => {}
        }
        fx_gpu::ramps_open_fx(fx_open, ramps, fx_gpu::ramp_cost(self.ramp_arm()))
    }

    /// The arm this renderer's frames draw on, as the glow ramps' route
    /// costs know it ([`fx_gpu::ramp_cost`]).
    #[inline]
    fn ramp_arm(&self) -> fx_gpu::RampArm {
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            return fx_gpu::RampArm::Metal;
        }
        #[cfg(gl_arm)]
        if self.gl_armed {
            return fx_gpu::RampArm::Gl;
        }
        #[cfg(d3d11_arm)]
        if self.d3d11_armed {
            return fx_gpu::RampArm::D3d11;
        }
        fx_gpu::RampArm::Unarmed
    }

    /// Test seam, the fx hazard drill's control.
    #[doc(hidden)]
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    pub const fn set_fx_palette_shared_for_test(&mut self, on: bool) {
        self.fx_palette_shared_seam = on;
    }

    /// Test seam.
    #[doc(hidden)]
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    pub const fn fail_fx_staging_for_test(&mut self, on: bool) {
        self.fx_fail_seam = on;
    }

    /// Test seam.
    #[doc(hidden)]
    #[cfg(any(test, debug_assertions))]
    pub fn drop_fx_arm_residency_for_test(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(c) = self.metal_arm.as_mut() {
            c.live_mut().fx_tex = [None, None];
        }
        #[cfg(gl_arm)]
        if let Some(live) = self.gl_arm.as_mut() {
            live.drop_fx_residency();
        }
        #[cfg(d3d11_arm)]
        if let Some(live) = self.d3d11_arm.as_mut() {
            live.drop_fx_residency();
        }
    }

    /// Test/diagnostic: `present_input` frames that did a full repaint.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn full_repaints(&self) -> u64 {
        self.full_repaints
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub fn overshoot_rule_widened(&self) -> bool {
        self.cpu.overshoot_rule_widened()
    }

    /// The `PresentPrev` and `GpuGateCache` origin stamps the `AsymmetricPadLayout` anchors read.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub fn project_asymmetric_pad_layout(
        &self,
        win: &WindowGpu,
    ) -> AsymmetricPadGpuCacheProjection {
        AsymmetricPadGpuCacheProjection {
            grid_top: self.cpu.grid_top(),
            present_cached_grid_top: win.present_prev.as_ref().map(|cache| cache.grid_top),
            gate_cached_grid_top: win.gate_cache.as_ref().map(|cache| cache.grid_top),
        }
    }

    /// Test/diagnostic: settings-card uploads that ran; an unchanged card skips.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn tray_uploads(&self) -> u64 {
        self.tray_uploads
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn tray_upload_bytes(&self) -> u64 {
        self.tray_upload_bytes
    }

    /// Test/diagnostic: total instances built in the last encoded frame.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn last_instances(&self) -> usize {
        self.last_instances
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn last_bg_instances(&self) -> usize {
        self.last_bg_instances
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn last_frame_passes(&self) -> u32 {
        self.last_frame_passes
    }

    /// The latest drawn frame's shape.
    #[must_use]
    #[inline]
    pub const fn frame_shape(&self) -> FrameShape {
        self.frame_shape
    }

    /// The pass-economy ablation frame plans run under ([`UnormAblation`]).
    #[must_use]
    #[inline]
    pub const fn unorm_ablation(&self) -> UnormAblation {
        self.ablation
    }

    /// Set the pass-economy ablation (the `ATERM_GPU_ABLATE_UNORM` seam's programmatic twin).
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    #[inline]
    pub const fn set_unorm_ablation(&mut self, ablation: UnormAblation) {
        self.ablation = ablation;
    }

    /// The route frame plans take where the device allows it ([`UnormRoute::Fetch`] by default);
    /// [`FrameShape::route`] is the route the latest frame took.
    #[must_use]
    #[inline]
    pub const fn unorm_route(&self) -> UnormRoute {
        self.unorm_route
    }

    /// Choose the route frame plans take where the device allows it (the `ATERM_GPU_UNORM_ROUTE`
    /// seam's programmatic twin): [`UnormRoute::Split`] keeps a fetch-capable device on the split.
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    #[inline]
    pub const fn set_unorm_route(&mut self, route: UnormRoute) {
        self.unorm_route = route;
    }

    /// The framebuffer-fetch verdict: the Metal arm's device probe, [`FetchProbe::Unsupported`] on
    /// any other arm, and `None` until the Metal arm has minted and a frame that draws Unorm-law
    /// light settled it.
    #[must_use]
    pub fn fetch_probe(&self) -> Option<FetchProbe> {
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            #[cfg(any(test, debug_assertions))]
            if self.fetch_probe_seam.is_some() {
                return self.fetch_probe_seam;
            }
            return self.metal_arm.as_ref().and_then(|c| c.live_ref().fetch);
        }
        Some(FetchProbe::Unsupported)
    }

    /// Stand `verdict` in for the Metal arm's fetch verdict (`None`: the device's own), so a test
    /// can drive a device that failed, refused or lacks the probe.
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    #[inline]
    pub const fn set_fetch_probe_seam(&mut self, verdict: Option<FetchProbe>) {
        self.fetch_probe_seam = verdict;
    }

    /// The verdict [`Self::set_fetch_probe_seam`] stands in, `None` in a release build.
    #[cfg(target_os = "macos")]
    #[inline]
    const fn fetch_seam(&self) -> Option<FetchProbe> {
        #[cfg(any(test, debug_assertions))]
        return self.fetch_probe_seam;
        #[cfg(not(any(test, debug_assertions)))]
        None
    }

    /// Mint the Metal arm against `latch` rather than the process's first-frame warm, so a test
    /// that runs its own warm never holds another test's frames on it.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) const fn set_first_frame_latch_for_test(
        &mut self,
        latch: &'static crate::metal::warm::WarmLatch,
    ) {
        self.first_frame = latch;
    }

    /// The route the Metal arm gives a frame (`unorm`: it draws Unorm-law light):
    /// [`UnormRoute::Fetch`] only where `want` asks for it and the verdict (`seam`'s, else the
    /// device's) is [`FetchProbe::Passed`]. Only a frame with that light settles a verdict
    /// ([`MetalArmLive::fetch_verdict`]), so a session that draws none never probes.
    #[cfg(target_os = "macos")]
    fn metal_unorm_route(
        arm: Option<&mut MetalArmCell>,
        want: UnormRoute,
        seam: Option<FetchProbe>,
        unorm: bool,
    ) -> UnormRoute {
        if want != UnormRoute::Fetch {
            return UnormRoute::Split;
        }
        let verdict = seam.or_else(|| {
            let live = arm?.live_mut();
            if unorm {
                live.fetch_verdict()
            } else {
                live.fetch
            }
        });
        if verdict == Some(FetchProbe::Passed) {
            UnormRoute::Fetch
        } else {
            UnormRoute::Split
        }
    }

    /// The `crate::gpu_time` serial of the latest present this renderer timed.
    #[must_use]
    #[inline]
    pub const fn last_gpu_time_serial(&self) -> Option<u64> {
        self.last_gpu_time_serial
    }

    /// Whether this renderer's presents can report GPU time once a sink is installed.
    #[must_use]
    pub const fn gpu_time_supported(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.metal_armed;
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// Cell size (pixels), straight from the CPU renderer so geometry matches.
    #[inline]
    pub const fn cell_size(&self) -> (usize, usize) {
        self.cpu.cell_size()
    }

    /// Interior padding (px per edge), delegated to the inner CPU renderer.
    #[inline]
    pub const fn pad(&self) -> usize {
        self.cpu.pad()
    }

    /// Set the interior padding on the inner CPU renderer (read each frame).
    #[inline]
    pub fn set_pad(&mut self, pad: usize) {
        self.cpu.set_pad(pad);
    }

    /// Chrome headroom (px above the padded grid), delegated to the inner CPU renderer.
    #[inline]
    pub const fn head(&self) -> usize {
        self.cpu.head()
    }

    /// Set the chrome headroom on the inner CPU renderer.
    #[inline]
    pub fn set_head(&mut self, head_px: usize) {
        self.cpu.set_head(head_px);
    }

    /// Effective top interior pad (px), delegated to the inner CPU renderer.
    #[inline]
    pub fn pad_top(&self) -> usize {
        self.cpu.pad_top()
    }

    /// Override the top-only interior pad (read each frame) without changing the framebuffer size.
    #[inline]
    pub fn set_pad_top(&mut self, pad_top: usize) {
        self.cpu.set_pad_top(pad_top);
    }

    /// Declare the host-chrome top rows and the tone their surface extends into the padding.
    #[inline]
    pub fn set_chrome_bleed(&mut self, bleed: Option<aterm_render::ChromeBleed>) {
        self.cpu.set_chrome_bleed(bleed);
    }

    /// The chrome bleed in force, read from the inner CPU renderer.
    #[must_use]
    #[inline]
    pub const fn chrome_bleed(&self) -> Option<aterm_render::ChromeBleed> {
        self.cpu.chrome_bleed()
    }

    /// The chrome rows' frame extent, stamped on a window by `encode_frame`.
    #[must_use]
    #[inline]
    pub fn chrome_extent_px(&self, frame_h: usize) -> std::ops::Range<usize> {
        self.cpu.chrome_extent_px(frame_h)
    }

    /// Padded pixel size of a `rows`×`cols` grid (`cols·cell_w + 2·pad`, etc.).
    #[inline]
    pub const fn frame_size(&self, rows: usize, cols: usize) -> (usize, usize) {
        self.cpu.frame_size(rows, cols)
    }

    /// Whether the GPU device was reported lost (driver update, TDR, eGPU unplug).
    #[inline]
    pub fn device_lost(&self) -> bool {
        #[cfg(gl_arm)]
        if self
            .gl_arm
            .as_ref()
            .is_some_and(|live| live.dev.shared().lost())
        {
            return true;
        }
        // The D3D11 arm's latch: a call answered with a removal code (or the
        // injected-loss drill).
        #[cfg(d3d11_arm)]
        if self
            .d3d11_arm
            .as_ref()
            .is_some_and(|live| live.dev.shared().lost())
        {
            return true;
        }
        self.ctx.device_lost()
    }

    /// The loss's cause — see [`GpuContext::device_loss_reason`].
    #[must_use]
    pub fn device_loss_reason(&self) -> Option<String> {
        #[cfg(gl_arm)]
        if let Some(live) = self.gl_arm.as_ref().filter(|live| live.dev.shared().lost()) {
            #[cfg(target_arch = "wasm32")]
            {
                let _ = live;
                return Some("webglcontextlost (the WebGL2 context was lost)".to_owned());
            }
            #[cfg(not(target_arch = "wasm32"))]
            return Some(
                live.dev
                    .shared()
                    .loss_reason()
                    .unwrap_or("EGL_CONTEXT_LOST (the GL context was lost)")
                    .to_owned(),
            );
        }
        #[cfg(d3d11_arm)]
        if let Some(live) = self
            .d3d11_arm
            .as_ref()
            .filter(|live| live.dev.shared().lost())
        {
            return Some(live.dev.shared().loss_reason());
        }
        self.ctx.device_loss_reason()
    }

    #[cfg(target_os = "macos")]
    fn metal_arm_ready(&mut self) -> bool {
        if !self.metal_armed {
            return false;
        }
        if self.metal_arm.is_some() {
            return true;
        }
        self.first_frame
            .await_item(crate::metal::warm::WarmItem::Library(ShaderLibrary::Cell));
        match MetalArmLive::new(self.ctx.metal_loss_latch_or_wire(), self.first_frame) {
            Ok(live) => {
                self.metal_arm = Some(MetalArmCell::new(live));
                if self.metal_warm_wanted {
                    self.metal_warm_pipelines();
                }
                true
            }
            Err(e) => {
                self.metal_mint_error = Some(e);
                self.metal_armed = false;
                false
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn metal_start_error(&self) -> String {
        match &self.metal_mint_error {
            Some(e) => format!("the Metal renderer could not start: {e}"),
            None => "the Metal renderer could not start".to_owned(),
        }
    }

    /// Arm the Metal renderer path for this renderer in-process, bypassing the normal constructor.
    #[cfg(target_os = "macos")]
    #[cfg(test)]
    pub(crate) fn arm_metal_for_test(&mut self) {
        self.metal_armed = true;
    }

    /// Whether the armed Metal arm is live on this renderer.
    #[cfg(target_os = "macos")]
    #[must_use]
    #[inline]
    pub const fn metal_render_armed(&self) -> bool {
        self.metal_armed && self.metal_arm.is_some()
    }

    #[inline]
    fn resident_present_crop(
        &self,
        input: &RenderInput,
        crop: Option<PresentCrop>,
    ) -> Result<Option<PresentCrop>, ()> {
        let Some(crop) = crop else {
            return Ok(None);
        };
        let (_, logical_raw_height) = self.frame_size(input.rows, input.cols);
        let logical_raw_height = u32::try_from(logical_raw_height).unwrap_or(u32::MAX).max(1);
        let resident_raw_height = self.clamp_fb_dim(logical_raw_height);
        normalize_present_crop(crop, logical_raw_height, resident_raw_height)
            .map(Some)
            .ok_or(())
    }

    #[inline]
    fn fx_atlas_cap(&self) -> u32 {
        self.clamp_fb_dim(fx_gpu::FX_ATLAS_CAP)
    }

    #[inline]
    fn fx_frame_for(&self, input: &RenderInput) -> aterm_render::FxFrame {
        let (fw, fh) = self.cpu.frame_size(input.rows, input.cols);
        self.cpu.fx_frame(
            self.clamp_fb_dim(fw as u32) as usize,
            self.clamp_fb_dim(fh as u32) as usize,
            input.rows,
        )
    }

    #[inline]
    fn clamp_fb_dim(&self, d: u32) -> u32 {
        d.clamp(1, self.first_party_max_texture_dim())
    }

    #[inline]
    fn first_party_max_texture_dim(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            crate::device_layer::METAL_MAX_TEXTURE_DIM
        }
        #[cfg(gl_arm)]
        {
            self.ctx.gl_max_texture_dim
        }
        // Every feature level the D3D11 arm takes (11_0 and up) holds this.
        #[cfg(d3d11_arm)]
        {
            aterm_win32::d3d11::D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION
        }
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub fn max_texture_dim(&self) -> u32 {
        self.clamp_fb_dim(u32::MAX)
    }

    /// The adapter/backend the GPU device is running on (for diagnostics).
    #[inline]
    pub fn adapter(&self) -> (&str, &str) {
        (&self.ctx.adapter_name, &self.ctx.backend)
    }

    /// Whether the additive streams (aurora, sparkle-add, bloom halo) match CPU `add_sat` exactly.
    #[inline]
    pub const fn additive_is_byte_exact(&self) -> bool {
        self.ctx.srgb_offscreen
    }

    /// Mirror of [`Renderer::set_cursor_blink_phase`].
    #[inline]
    pub fn set_cursor_blink_phase(&mut self, on: bool) {
        self.cpu.set_cursor_blink_phase(on);
    }

    /// Mirror of [`Renderer::set_cursor_style_override`].
    #[inline]
    pub fn set_cursor_style_override(&mut self, style: Option<CursorStyle>) {
        self.cpu.set_cursor_style_override(style);
    }

    #[inline]
    fn drawable(cell: &aterm_core::terminal::RenderCell) -> bool {
        !cell.wide && cell.ch != ' ' && !cell.ch.is_control()
    }

    #[inline]
    fn cell_key(
        &mut self,
        cluster: Option<&str>,
        cell: &aterm_core::terminal::RenderCell,
        cell_span: u8,
    ) -> GlyphKey {
        self.cpu.resolve_cell_key_for_span(cluster, cell, cell_span)
    }

    fn create_atlas_texture(&mut self, atlas: Atlas) -> ResidentAtlas {
        let max_tex_dim = self.first_party_max_texture_dim();
        let tex_h = (atlas.height + ATLAS_GROW_HEADROOM).min(max_tex_dim);
        self.atlas_tex_creations += 1;
        ResidentAtlas { atlas, tex_h }
    }

    fn ensure_deco_atlas(&mut self, cw: usize, ch: usize) {
        let atlas_w = aterm_render::DECO_ATLAS_SPRITES * cw;
        if !aterm_render::undercurl_supported(cw, ch) {
            self.deco_atlas = None;
            return;
        }
        let dm = self.cpu.deco_metrics();
        let curl_band = (dm.underline_y, dm.underline_t);
        if let Some(d) = &self.deco_atlas
            && d.cw == cw
            && d.ch == ch
            && d.curl_band == curl_band
        {
            return;
        }
        let mut data = vec![0u8; atlas_w * ch];
        for (i, g) in DECO_GLYPHS.iter().enumerate() {
            let x0 = i * cw;
            let mask = aterm_render::procedural::deco_coverage(*g, cw, ch);
            for my in 0..ch {
                for mx in 0..cw {
                    data[my * atlas_w + x0 + mx] = mask[my * cw + mx];
                }
            }
        }
        {
            let x0 = aterm_render::UNDERCURL_SPRITE * cw;
            let mask = self.cpu.undercurl_mask();
            for my in 0..ch {
                for mx in 0..cw {
                    data[my * atlas_w + x0 + mx] = mask[my * cw + mx];
                }
            }
        }
        self.deco_atlas = Some(DecoAtlas {
            cw,
            ch,
            curl_band,
            atlas_w,
            data,
        });
    }

    fn ensure_atlases(&mut self, keys: &mut [GlyphKey]) {
        // Fast path: every key is resident in both atlases. Runs before the sort
        // (membership needs no order), so the common frame never sorts.
        if self.mono_res.is_some()
            && self.color_res.is_some()
            && keys.iter().all(|k| self.resident_keys.contains(k))
        {
            return;
        }

        keys.sort_unstable();

        let new_keys: Vec<GlyphKey> = keys
            .iter()
            .copied()
            .filter(|k| !self.resident_keys.contains(k))
            .collect();

        if self.mono_res.is_none() || self.color_res.is_none() {
            self.rebuild_atlases(keys);
            return;
        }

        let cap_mono = self.mono_res.as_ref().unwrap().tex_h;
        let cap_color = self.color_res.as_ref().unwrap().tex_h;
        let mono_band = {
            let res = self.mono_res.as_mut().unwrap();
            grow_atlas(&mut self.cpu, &mut res.atlas, &new_keys, cap_mono)
        };
        let color_band = {
            let res = self.color_res.as_mut().unwrap();
            grow_atlas(&mut self.cpu, &mut res.atlas, &new_keys, cap_color)
        };
        match (mono_band, color_band) {
            // Each arm uploads the grown rows from the atlas at its next sync.
            (Some(_), Some(_)) => self.resident_keys.extend(new_keys),
            _ => self.rebuild_atlases(keys),
        }
    }

    fn rebuild_atlases(&mut self, keys: &[GlyphKey]) {
        let cap_h = self.first_party_max_texture_dim();
        {
            self.atlas_epoch += 1;
        }
        let mono = build_atlas(&mut self.cpu, keys, cap_h);
        self.mono_res = Some(self.create_atlas_texture(mono));
        let color = build_color_atlas(&mut self.cpu, keys, cap_h);
        self.color_res = Some(self.create_atlas_texture(color));
        self.resident_keys = keys.iter().copied().collect();
    }

    #[must_use]
    fn pack_image_plane(items: &[(u32, u32, u32, &[u8])], tw: u32, th: u32) -> Vec<u8> {
        let mut data = vec![0u8; (tw * th * 4) as usize];
        for &(y0, dw, dh, rgba) in items {
            if dw == tw {
                let dst = (y0 * tw) as usize * 4;
                let len = (dw * dh * 4) as usize;
                data[dst..dst + len].copy_from_slice(&rgba[..len]);
            } else {
                for y in 0..dh {
                    let src = (y * dw * 4) as usize;
                    let dst = ((y0 + y) * tw) as usize * 4;
                    data[dst..dst + (dw * 4) as usize]
                        .copy_from_slice(&rgba[src..src + (dw * 4) as usize]);
                }
            }
        }
        data
    }

    fn build_image_plane(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        let (cw, ch) = self.cpu.cell_size();
        let mut order = std::mem::take(&mut self.image_order_scratch);
        let mut seen = std::mem::take(&mut self.image_seen_scratch);
        let mut placements = std::mem::take(&mut self.image_placements_scratch);
        order.clear();
        seen.clear();
        placements.clear();
        let mut last: Option<((usize, usize, usize), usize)> = None;
        for row in &input.images {
            for cell in row.chunk_by(|a, b| a.0 == b.0) {
                for (depth, (_c, image)) in cell.iter().rev().enumerate() {
                    let (fp_w, fp_h) = aterm_render::placement_footprint(&image.image, (cw, ch));
                    if fp_w == 0 || fp_h == 0 {
                        continue;
                    }
                    let key = (std::sync::Arc::as_ptr(&image.image) as usize, fp_w, fp_h);
                    if last == Some((key, depth)) {
                        continue;
                    }
                    last = Some((key, depth));
                    match seen.get(&key) {
                        Some(&at) => order[at].3 = order[at].3.min(depth),
                        None => {
                            seen.insert(key, order.len());
                            order.push((image.image.clone(), fp_w, fp_h, depth));
                        }
                    }
                }
            }
        }
        order.sort_by_key(|&(.., depth)| depth);
        if order.is_empty() {
            win.image_plane = None;
            self.image_order_scratch = order;
            self.image_seen_scratch = seen;
            self.image_placements_scratch = placements;
            return;
        }

        win.image_cache.begin_frame(
            order
                .iter()
                .map(|(image, fp_w, fp_h, _)| (image, *fp_w, *fp_h)),
        );
        let max_tex_dim = self.first_party_max_texture_dim();
        let mut total_h: u32 = 0;
        let mut max_w: u32 = 0;
        for (image, fp_w, fp_h, _) in &order {
            let key = (std::sync::Arc::as_ptr(image) as usize, *fp_w, *fp_h);
            let d = win.image_cache.get_or_decode(image, *fp_w, *fp_h);
            let side = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
            let (empty, iw, ih) = (d.rgba.is_empty(), side(d.w), side(d.h));
            if empty || iw == 0 || ih == 0 {
                continue;
            }
            if iw > max_tex_dim || ih > max_tex_dim {
                continue;
            }
            if total_h + ih > max_tex_dim {
                break;
            }
            placements.insert(key, (total_h, iw, ih));
            total_h += ih;
            max_w = max_w.max(iw);
        }
        if placements.is_empty() || max_w == 0 || total_h == 0 {
            win.image_plane = None;
            order.clear();
            self.image_order_scratch = order;
            self.image_seen_scratch = seen;
            self.image_placements_scratch = placements;
            return;
        }

        let (tw, th) = (max_w, total_h);
        if let Some(p) = &win.image_plane
            && p.w == tw
            && p.h == th
            && p.placements == placements
        {
            order.clear();
            self.image_order_scratch = order;
            self.image_seen_scratch = seen;
            self.image_placements_scratch = placements;
            return;
        }
        let mut items: Vec<(u32, u32, u32, &[u8])> = Vec::with_capacity(order.len());
        let mut pinned_images: Vec<std::sync::Arc<aterm_core::grid::extra::ImageData>> =
            Vec::with_capacity(placements.len());
        for (image, fp_w, fp_h, _) in &order {
            let key = (std::sync::Arc::as_ptr(image) as usize, *fp_w, *fp_h);
            let Some(&(y0, dw, dh)) = placements.get(&key) else {
                continue;
            };
            pinned_images.push(std::sync::Arc::clone(image));
            let Some(decoded) = win.image_cache.peek(image, *fp_w, *fp_h) else {
                continue;
            };
            items.push((y0, dw, dh, decoded.rgba.as_slice()));
        }
        let data = Self::pack_image_plane(&items, tw, th);

        #[cfg(target_os = "macos")]
        let (metal_texels, epoch) = if self.metal_armed {
            self.image_plane_epoch += 1;
            (Some(data), self.image_plane_epoch)
        } else {
            (None, 0)
        };
        #[cfg(gl_arm)]
        let (gl_texels, gl_epoch) = if self.gl_armed {
            self.image_plane_epoch += 1;
            (Some(data), self.image_plane_epoch)
        } else {
            (None, 0)
        };
        // The D3D11 arm's twin.
        #[cfg(d3d11_arm)]
        let (d3d11_texels, d3d11_epoch) = if self.d3d11_armed {
            self.image_plane_epoch += 1;
            (Some(data), self.image_plane_epoch)
        } else {
            (None, 0)
        };
        win.image_plane = Some(ImagePlane {
            #[cfg(target_os = "macos")]
            metal_atlas: None,
            w: tw,
            h: th,
            placements: std::mem::take(&mut placements),
            _pinned_images: pinned_images,
            #[cfg(target_os = "macos")]
            metal_texels,
            #[cfg(target_os = "macos")]
            epoch,
            #[cfg(gl_arm)]
            gl_texels,
            #[cfg(gl_arm)]
            gl_epoch,
            #[cfg(gl_arm)]
            gl_atlas: None,
            #[cfg(d3d11_arm)]
            d3d11_texels,
            #[cfg(d3d11_arm)]
            d3d11_epoch,
            #[cfg(d3d11_arm)]
            d3d11_atlas: None,
        });
        order.clear();
        self.image_order_scratch = order;
        self.image_seen_scratch = seen;
        self.image_placements_scratch = placements;
    }

    /// Enable/disable the cursor-comet bloom (default on), read each present.
    #[inline]
    pub fn set_bloom(&mut self, on: bool) {
        let was = std::mem::replace(&mut self.enable_bloom, on);
        self.present_pass_switched(was, on);
    }

    /// A present switch went from `was` to `on` (or a first window went on screen, which may bring
    /// an f16 swapchain): once the Metal arm's warm was asked for
    /// ([`Self::warm_effect_pipelines`]), a pass it turned on gets its pipelines built on a worker
    /// before a frame binds them.
    #[inline]
    fn present_pass_switched(&mut self, was: bool, on: bool) {
        #[cfg(target_os = "macos")]
        if on && !was && self.metal_warm_wanted {
            self.metal_warm_pipelines();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (was, on);
    }

    /// Whether the GPU bloom layer is currently enabled.
    #[must_use]
    #[inline]
    pub const fn bloom_enabled(&self) -> bool {
        self.enable_bloom
    }

    /// Set the bloom tunables (`cursor_trail_bloom_strength`/`_radius`).
    #[inline]
    pub const fn set_bloom_params(&mut self, strength: f32, radius: f32) {
        self.bloom_strength = strength;
        self.bloom_radius = radius;
    }

    /// Enable/disable the heat shimmer (config `cursor_fire_shimmer`, default on).
    #[inline]
    pub fn set_shimmer(&mut self, on: bool) {
        let was = std::mem::replace(&mut self.enable_shimmer, on);
        self.present_pass_switched(was, on);
    }

    /// Whether the GPU heat-shimmer layer is currently enabled.
    #[must_use]
    #[inline]
    pub const fn shimmer_enabled(&self) -> bool {
        self.enable_shimmer
    }

    /// Test hook.
    #[doc(hidden)]
    #[inline]
    pub const fn set_shimmer_phase_for_test(&mut self, phase: Option<f32>) {
        self.shimmer_phase_pin = phase;
    }

    /// Test helper.
    #[doc(hidden)]
    #[must_use]
    pub fn shimmer_region_for_test(&self, input: &RenderInput) -> Option<(u32, u32, u32, u32)> {
        let (fw, fh) = self.frame_size(input.rows, input.cols);
        let w = self.clamp_fb_dim(fw as u32);
        let h = self.clamp_fb_dim(fh as u32);
        self.shimmer_region(input, w, h)
            .map(|r| (r.x0, r.y0, r.x1, r.y1))
    }

    /// Whether the heat shimmer runs: on, over fire, with additive glow laid (its quads or
    /// its ramps' lit rows).
    #[inline]
    const fn shimmer_live(&self, input: &RenderInput) -> bool {
        self.enable_shimmer
            && !input.fire_patch.is_empty()
            && input
                .cursor_glow_add_ramps
                .laid(input.cursor_glow_add.len())
                > 0
    }

    /// Config `hdr_glow` (default on).
    pub fn set_hdr_glow(&mut self, on: bool) {
        // The HDR swapchain and the DirectComposition visual swapchain are mutually exclusive:
        // once the instance is visual, `hdr_glow` stays off (its composition there is unverified,
        // and rebuilding the instance would tear down every window). Every config path funnels
        // through here.
        if on && self.ctx.visual_swapchain {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                crate::stderr_line!(
                    "aterm-gpu: hdr_glow is unavailable while background_material is active \
                     (the DirectComposition backdrop swapchain and the scRGB EDR swapchain \
                     are mutually exclusive); keeping the backdrop — restart with \
                     background_material = \"none\" for HDR"
                );
            });
            self.hdr_glow = false;
            return;
        }
        let was = std::mem::replace(&mut self.hdr_glow, on);
        self.present_pass_switched(was, on);
    }

    /// Mirror the frontend's `background_material != none` knob.
    #[inline]
    pub const fn set_backdrop_margins(&mut self, on: bool) {
        self.backdrop_margins = on;
    }

    #[inline]
    const fn backdrop_margins_active(&self) -> bool {
        self.backdrop_margins && self.ctx.visual_swapchain
    }

    /// Set the SDR glow-boost strength (`cursor_glow_sdr_boost`, 0..=1; 0 disables).
    #[inline]
    pub fn set_sdr_glow_boost(&mut self, strength: f32) {
        let was = std::mem::replace(&mut self.sdr_glow_boost, strength) > 0.0;
        self.present_pass_switched(was, strength > 0.0);
    }

    /// Start recording this window's swapchain frames submitted through the WSI present path, with
    /// [`crate::video_tap::CaptureOpts`].
    pub fn video_begin(
        &self,
        win: &mut WindowGpu,
        surf: &GpuSurface,
        opts: crate::video_tap::CaptureOpts,
    ) -> Result<(), String> {
        if !surf.copyable {
            return Err("surface not copyable on this backend (no COPY_SRC)".into());
        }
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            let Some(cell) = self.metal_arm.as_ref() else {
                return Err(
                    "the Metal arm has not rendered its first frame yet — retry after \
                     the next present"
                        .into(),
                );
            };
            let (nw, nh, nf) = surf.neutral_config();
            let tap = crate::video_tap::VideoTap::new_metal(
                &cell.live_ref().mint,
                nw,
                nh,
                nf,
                win.capture_color_space,
                win.sdr_white_scale(),
                opts,
            )?;
            win.video = Some(tap);
            return Ok(());
        }
        #[cfg(gl_arm)]
        if self.gl_armed && surf.gl.is_some() {
            let tap = crate::video_tap::VideoTap::new_gl(
                surf.neutral.width,
                surf.neutral.height,
                crate::device_layer::TexelFormat::Rgba8Unorm,
                win.capture_color_space,
                win.sdr_white_scale(),
                opts,
            )?;
            win.video = Some(tap);
            return Ok(());
        }
        // The D3D11 arm records its window's buffer 0 as presented, in the
        // chain's format (an EDR chain's f16 included).
        #[cfg(d3d11_arm)]
        if self.d3d11_armed && surf.d3d11.is_some() {
            let tap = crate::video_tap::VideoTap::new_gl(
                surf.neutral.width,
                surf.neutral.height,
                surf.neutral.format,
                win.capture_color_space,
                win.sdr_white_scale(),
                opts,
            )?;
            win.video = Some(tap);
            return Ok(());
        }
        Err("video: the renderer is disarmed: no arm records this window".to_string())
    }

    /// Start recording a glass-less window's presents through a persistent `VirtualTarget`.
    pub fn virtual_begin(
        &mut self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
        opts: crate::video_tap::CaptureOpts,
    ) -> Result<(), String> {
        let w = self.clamp_fb_dim(w.max(1));
        let h = self.clamp_fb_dim(h.max(1));
        #[cfg(gl_arm)]
        if self.gl_armed && self.gl_arm_ready() {
            let tap = crate::video_tap::VideoTap::new_gl(
                w,
                h,
                crate::device_layer::TexelFormat::Rgba8Unorm,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
                opts,
            )?;
            win.virtual_presented_input_epoch = 0;
            win.video = Some(tap);
            return Ok(());
        }
        // The D3D11 arm likewise (Windows; its headless presents draw there).
        #[cfg(d3d11_arm)]
        if self.d3d11_armed && self.d3d11_arm_ready() {
            let tap = crate::video_tap::VideoTap::new_gl(
                w,
                h,
                crate::device_layer::TexelFormat::Rgba8Unorm,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
                opts,
            )?;
            win.virtual_presented_input_epoch = 0;
            win.video = Some(tap);
            return Ok(());
        }
        #[cfg(gl_arm)]
        {
            Err(format!("video: {}", self.gl_start_error()))
        }
        #[cfg(d3d11_arm)]
        {
            Err(format!("video: {}", self.d3d11_start_error()))
        }
        #[cfg(target_os = "macos")]
        {
            if !self.metal_arm_ready() {
                return Err(format!("video: {}", self.metal_start_error()));
            }
            let cell = self.metal_arm.as_ref().expect("metal_arm_ready above");
            let tap = crate::video_tap::VideoTap::new_metal(
                &cell.live_ref().mint,
                w,
                h,
                VIRTUAL_PRESENT_TEXEL,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
                opts,
            )?;
            win.virtual_presented_input_epoch = 0;
            win.video = Some(tap);
            Ok(())
        }
    }

    /// Post-submit hook.
    pub fn video_after_present(&self, win: &mut WindowGpu, t_us: u64) {
        if let Some(tap) = win.video.as_mut() {
            #[cfg(target_os = "macos")]
            if tap.ring_is_metal() {
                tap.metal_after_present(t_us);
            }
            // A host-readback ring (the GL or D3D11 arm's).
            #[cfg(any(gl_arm, d3d11_arm))]
            let on_gl = tap.ring_is_gl();
            #[cfg(any(gl_arm, d3d11_arm))]
            if on_gl {
                tap.gl_after_present(t_us);
            }
        }
    }

    /// Finalize the `video` recording (a blocking drain, off the hot path) and hand the frames out.
    pub fn video_finish(&self, win: &mut WindowGpu) -> Option<crate::video_tap::UnsettledTake> {
        win.video.take().map(|t| {
            #[cfg(target_os = "macos")]
            if t.ring_is_metal() {
                return t.metal_finish();
            }
            #[cfg(any(gl_arm, d3d11_arm))]
            if t.ring_is_gl() {
                return t.gl_finish();
            }
            unreachable!("every video ring stages through an arm")
        })
    }

    /// The `video` recording's status: `(frames_so_far, resized_early_stop)`.
    #[must_use]
    pub fn video_status(&self, win: &WindowGpu) -> Option<(usize, bool)> {
        win.video.as_ref().map(|t| (t.frames_so_far(), t.resized()))
    }

    /// Arm a one-shot copy of this window's next successfully presented destination.
    pub fn presented_snapshot_begin(
        &self,
        win: &mut WindowGpu,
        surface: &GpuSurface,
    ) -> Result<(), String> {
        if win.presented_snapshot.is_some() {
            return Err("presented snapshot: capture already armed".to_string());
        }
        if !surface.copyable {
            return Err(
                "presented snapshot: surface not copyable on this backend (no COPY_SRC)"
                    .to_string(),
            );
        }
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            let Some(cell) = self.metal_arm.as_ref() else {
                return Err(
                    "the Metal arm has not rendered its first frame yet — retry after \
                     the next present"
                        .to_string(),
                );
            };
            let (nw, nh, nf) = surface.neutral_config();
            win.presented_snapshot = Some(crate::video_tap::PresentedFrameTap::new_metal(
                &cell.live_ref().mint,
                nw,
                nh,
                nf,
                win.capture_color_space,
                win.sdr_white_scale(),
            )?);
            win.copy_src_hold.arm(aterm_time::Instant::now());
            return Ok(());
        }
        #[cfg(gl_arm)]
        if self.gl_armed && surface.gl.is_some() {
            win.presented_snapshot = Some(crate::video_tap::PresentedFrameTap::new_gl(
                surface.neutral.width,
                surface.neutral.height,
                crate::device_layer::TexelFormat::Rgba8Unorm,
                win.capture_color_space,
                win.sdr_white_scale(),
            )?);
            return Ok(());
        }
        // The D3D11 arm's one-shot over its window's buffer 0.
        #[cfg(d3d11_arm)]
        if self.d3d11_armed && surface.d3d11.is_some() {
            win.presented_snapshot = Some(crate::video_tap::PresentedFrameTap::new_gl(
                surface.neutral.width,
                surface.neutral.height,
                surface.neutral.format,
                win.capture_color_space,
                win.sdr_white_scale(),
            )?);
            return Ok(());
        }
        Err("snapshot: the renderer is disarmed: no arm copies this window".to_string())
    }

    /// Post-present half of [`Self::presented_snapshot_begin`].
    pub fn presented_snapshot_after_present(
        &self,
        win: &mut WindowGpu,
        t_us: u64,
    ) -> Result<(), String> {
        let tap = win
            .presented_snapshot
            .as_mut()
            .ok_or_else(|| "presented snapshot: no capture is armed".to_string())?;
        #[cfg(target_os = "macos")]
        if tap.tap_is_metal() {
            return tap.metal_after_present(t_us);
        }
        #[cfg(any(gl_arm, d3d11_arm))]
        if tap.tap_is_gl() {
            return tap.gl_after_present(t_us);
        }
        unreachable!("every one-shot stages through an arm")
    }

    /// Complete the one-shot map, blocking only after the capture has left the present hot path.
    pub fn presented_snapshot_finish(&self, win: &mut WindowGpu) -> Result<(), String> {
        let tap = win
            .presented_snapshot
            .as_mut()
            .ok_or_else(|| "presented snapshot: no capture is armed".to_string())?;
        #[cfg(target_os = "macos")]
        if tap.tap_is_metal() {
            return tap.metal_finish();
        }
        #[cfg(any(gl_arm, d3d11_arm))]
        if tap.tap_is_gl() {
            return tap.gl_finish();
        }
        unreachable!("every one-shot stages through an arm")
    }

    /// Take the completed exact destination as its staging buffer mapped it
    /// ([`crate::video_tap::RawCapture`]).
    pub fn presented_snapshot_take(
        &self,
        win: &mut WindowGpu,
    ) -> Result<crate::video_tap::RawCapture, String> {
        win.presented_snapshot
            .take()
            .ok_or_else(|| "presented snapshot: no capture is armed".to_string())?
            .take_raw()
    }

    /// Copy and synchronously read the current persistent virtual destination without arming or
    /// forcing a present.
    pub fn virtual_presented_snapshot_current(
        &self,
        win: &WindowGpu,
        expected_input_epoch: u64,
        t_us: u64,
    ) -> Result<crate::video_tap::RawCapture, String> {
        if expected_input_epoch == 0 || expected_input_epoch == u64::MAX {
            return Err("presented snapshot: invalid virtual destination epoch".to_string());
        }
        if win.virtual_presented_input_epoch != expected_input_epoch {
            return Err(
                "presented snapshot: virtual destination advanced before capture".to_string(),
            );
        }
        #[cfg(gl_arm)]
        if self.gl_armed {
            return self.gl_virtual_snapshot(win, t_us);
        }
        // The D3D11 arm's likewise (the immediate context's readback waits).
        #[cfg(d3d11_arm)]
        if self.d3d11_armed {
            return self.d3d11_virtual_snapshot(win, t_us);
        }
        #[cfg(target_os = "macos")]
        {
            let target = win.metal_virtual_off.as_ref().ok_or_else(|| {
                "presented snapshot: current virtual destination is unavailable".to_string()
            })?;
            let cell = self
                .metal_arm
                .as_ref()
                .ok_or_else(|| "presented snapshot: the Metal arm is not live".to_string())?;
            let live = cell.live_ref();
            let mut tap = crate::video_tap::PresentedFrameTap::new_metal(
                &live.mint,
                target.width() as u32,
                target.height() as u32,
                VIRTUAL_PRESENT_TEXEL,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
            )?;
            let mut cb = live.session.begin()?;
            tap.metal_enqueue_copy(
                &mut cb,
                target,
                VIRTUAL_PRESENT_TEXEL,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
            )?;
            let submitted = cb.commit();
            tap.metal_note_submitted(crate::metal::encoder::CbProbe::of(&submitted));
            if submitted.wait_outcome() != crate::metal::loss::CbOutcome::Completed {
                return Err("presented snapshot: the copy did not complete".to_string());
            }
            tap.metal_after_present(t_us)?;
            tap.metal_finish()?;
            tap.take_raw()
        }
        #[cfg(gl_arm)]
        {
            Err(format!("presented snapshot: {}", self.gl_start_error()))
        }
        #[cfg(d3d11_arm)]
        {
            Err(format!("presented snapshot: {}", self.d3d11_start_error()))
        }
    }

    /// Render a [`RenderInput`] snapshot (from [`aterm_core::terminal::Terminal::cell_frame_into`])
    /// on the GPU and read it back.
    pub fn render_input(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        tray: Option<TrayQuad<'_>>,
    ) -> Frame {
        self.cpu
            .checked_frame_size(input.rows, input.cols)
            .expect("renderable framebuffer");
        let (width, height) = self.render_input_target(win, input, tray);
        #[cfg(d3d11_arm)]
        if self.last_frame_arm_d3d11 {
            return self
                .d3d11_read_back_offscreen(win, width, height)
                .unwrap_or_else(|e| {
                    crate::stderr_line!(
                        "aterm-gpu: d3d11 readback failed ({e}); returning blank frame"
                    );
                    Frame {
                        width: width as usize,
                        height: height as usize,
                        pixels: vec![0; (width as usize) * (height as usize)],
                    }
                });
        }
        #[cfg(gl_arm)]
        if self.last_frame_arm_gl {
            return self
                .gl_read_back_offscreen(win, width, height)
                .unwrap_or_else(|e| {
                    crate::stderr_line!(
                        "aterm-gpu: gl readback failed ({e}); returning blank frame"
                    );
                    Frame {
                        width: width as usize,
                        height: height as usize,
                        pixels: vec![0; (width as usize) * (height as usize)],
                    }
                });
        }
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            return self
                .metal_read_back_offscreen(win, width, height)
                .unwrap_or_else(|e| {
                    crate::stderr_line!(
                        "aterm-gpu: metal readback failed ({e}); returning blank frame"
                    );
                    Frame {
                        width: width as usize,
                        height: height as usize,
                        pixels: vec![0; (width as usize) * (height as usize)],
                    }
                });
        }
        crate::stderr_line!("aterm-gpu: render_input on a disarmed renderer; blank frame");
        Frame {
            width: width as usize,
            height: height as usize,
            pixels: vec![0; (width as usize) * (height as usize)],
        }
    }

    /// Fallible twin of [`Self::render_input`], rejecting excessive dimensions before changing
    /// window tracking or encoding a frame.
    pub fn try_render_input(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        tray: Option<TrayQuad<'_>>,
    ) -> Result<Frame, String> {
        self.cpu.checked_frame_size(input.rows, input.cols)?;
        let (width, height) = self.render_input_target(win, input, tray);
        #[cfg(d3d11_arm)]
        if self.last_frame_arm_d3d11 {
            return self.d3d11_read_back_offscreen(win, width, height);
        }
        #[cfg(gl_arm)]
        if self.last_frame_arm_gl {
            return self.gl_read_back_offscreen(win, width, height);
        }
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            return self.metal_read_back_offscreen(win, width, height);
        }
        Err("the renderer is disarmed: no arm drew the frame".to_string())
    }

    fn render_input_target(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        tray: Option<TrayQuad<'_>>,
    ) -> (u32, u32) {
        let mut dims;
        let mut pass = 1;
        loop {
            let epoch = self.cpu.font_epoch();
            win.present_prev = None;
            dims = self.encode_frame(win, input, &RepaintScope::Full);
            let _ = self.cpu.settle_in_flight_fallback_parses();
            if self.cpu.font_epoch() == epoch || pass == Renderer::SNAPSHOT_SETTLE_PASSES {
                break;
            }
            pass += 1;
        }
        let (w, h) = dims;
        // An armed D3D11 frame: the same order as the other arms (bloom,
        // shimmer, the glide, the tray card with its scrim), each degrading by
        // name, then the frame is read off `win.d3d11_offscreen`.
        #[cfg(d3d11_arm)]
        if self.last_frame_arm_d3d11 {
            if self.enable_bloom
                && fx_gpu::bloom_source_live(input)
                && let Err(e) = self.d3d11_composite_bloom_in_place(win, input)
            {
                d3d11_arm::d3d11_arm_note(&format!(
                    "armed readback: in-place bloom failed ({e}); frame has no halo"
                ));
            }
            if self.shimmer_live(input)
                && let Err(e) = self.d3d11_shimmer_in_place(win, input)
            {
                d3d11_arm::d3d11_arm_note(&format!(
                    "armed readback: in-place shimmer failed ({e}); frame has no refraction"
                ));
            }
            if input.scroll_frac_px != 0 {
                self.d3d11_shift_offscreen_band(win, input);
            }
            if let Some(t) = tray
                && let Err(e) = self.d3d11_tray_into_offscreen(win, t)
            {
                d3d11_arm::d3d11_arm_note(&format!(
                    "armed readback: tray bake failed ({e}); frame has no settings card"
                ));
            }
            return (w, h);
        }
        #[cfg(gl_arm)]
        if self.last_frame_arm_gl {
            if self.enable_bloom
                && fx_gpu::bloom_source_live(input)
                && let Err(e) = self.gl_composite_bloom_in_place(win, input)
            {
                gl_arm::gl_arm_note(&format!(
                    "armed readback: in-place bloom failed ({e}); frame has no halo"
                ));
            }
            if self.shimmer_live(input)
                && let Err(e) = self.gl_shimmer_in_place(win, input)
            {
                gl_arm::gl_arm_note(&format!(
                    "armed readback: in-place shimmer failed ({e}); frame has no refraction"
                ));
            }
            if input.scroll_frac_px != 0 {
                self.gl_shift_offscreen_band(win, input);
            }
            if let Some(t) = tray
                && let Err(e) = self.gl_tray_into_offscreen(win, t)
            {
                gl_arm::gl_arm_note(&format!(
                    "armed readback: tray bake failed ({e}); frame has no settings card"
                ));
            }
            return (w, h);
        }
        // An armed Metal frame: bloom, shimmer, the glide, then the tray card.
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            if self.enable_bloom
                && fx_gpu::bloom_source_live(input)
                && let Err(e) = self.metal_composite_bloom_in_place(win, input)
            {
                metal_arm_note(&format!(
                    "armed readback: in-place bloom failed ({e}); frame has no halo"
                ));
            }
            if self.shimmer_live(input)
                && let Err(e) = self.metal_shimmer_in_place(win, input)
            {
                metal_arm_note(&format!(
                    "armed readback: in-place shimmer failed ({e}); frame has no refraction"
                ));
            }
            self.shift_offscreen_band(win, input);
            if let Some(t) = tray
                && let Err(e) = self.metal_tray_into_offscreen(win, t)
            {
                metal_arm_note(&format!(
                    "armed readback: tray bake failed ({e}); frame has no settings card"
                ));
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = tray;
        (w, h)
    }

    #[cfg(target_os = "macos")]
    fn metal_read_back_offscreen(
        &mut self,
        win: &WindowGpu,
        width: u32,
        height: u32,
    ) -> Result<Frame, String> {
        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let off = win
            .metal_offscreen
            .as_ref()
            .ok_or("armed frame lacks its metal offscreen")?;
        let live = self
            .metal_arm
            .as_mut()
            .ok_or("metal arm not minted")?
            .live_mut();
        crate::device_layer::metal_try_read_back(&live.session, &live.mint, &off.tex, width, height)
    }

    #[cfg(target_os = "macos")]
    fn metal_compose_present_readback(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        width: u32,
        height: u32,
    ) -> Result<Frame, String> {
        use crate::metal::ffi as mtl;
        let _pool = mtl::AutoreleasePool::new();
        let throwaway = {
            let live = self
                .metal_arm
                .as_mut()
                .ok_or("metal arm not minted")?
                .live_mut();
            live.layer()
                .try_create_texture_2d(
                    "aterm-gpu metal throwaway present target",
                    crate::device_layer::TexelFormat::Bgra8Unorm,
                    width,
                    height,
                    crate::device_layer::TexUsage {
                        render: true,
                        ..crate::device_layer::TexUsage::default()
                    },
                    None,
                )?
                .into_metal()
        };
        let (submitted, composed_present_off) = self.metal_encode_submit_b(
            win,
            input,
            false,
            None,
            None,
            None,
            None,
            (width, height),
            (width, height),
            VIRTUAL_PRESENT_TEXEL,
            false,
            false,
            false,
            &throwaway,
        )?;
        if submitted.wait_outcome() != crate::metal::loss::CbOutcome::Completed {
            return Err("the effect-compose Submit B did not complete".to_owned());
        }
        let composed = if composed_present_off {
            win.metal_present_off
                .as_ref()
                .ok_or("Submit B composed without minting the present copy")?
                .clone_handle()
        } else {
            win.metal_offscreen
                .as_ref()
                .ok_or("armed frame lacks its metal offscreen")?
                .tex
                .clone_handle()
        };
        let live = self
            .metal_arm
            .as_mut()
            .ok_or("metal arm not minted")?
            .live_mut();
        crate::device_layer::metal_try_read_back(
            &live.session,
            &live.mint,
            &composed,
            width,
            height,
        )
    }

    #[cfg(target_os = "macos")]
    fn ensure_metal_tray_card(
        live: &mut MetalArmLive,
        win: &mut WindowGpu,
        t: &TrayQuad<'_>,
        reader: Option<u64>,
        tray_uploads: &mut u64,
        tray_upload_bytes: &mut u64,
    ) -> Result<(), String> {
        debug_assert!(
            t.scrim.is_none(),
            "the Metal arm draws no tray scrim (`TrayQuad::scrim`): macOS shows none"
        );
        let stride = t.pw as usize * 4;
        if let Some(card) = win.metal_tray_card.as_mut()
            && card.w == t.pw
            && card.h == t.ph
        {
            let Some(band) = changed_row_band(&card.pixels, t.rgba, stride) else {
                card.ring.read_unchanged(reader);
                return Ok(());
            };
            let write = card.ring.plan_change(band);
            if write.mint {
                let tex = live
                    .layer()
                    .try_create_texture_2d(
                        "aterm-gpu metal tray card",
                        crate::device_layer::TexelFormat::Rgba8Unorm,
                        t.pw,
                        t.ph,
                        crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                        None,
                    )
                    .map_err(|e| format!("tray card mint: {e}"))?;
                card.texes.push(tex);
            }
            if let Some(seq) = write.settle_through {
                live.await_presents_through(seq);
            }
            let (y0, y1) = write.rows;
            let bytes = &t.rgba[y0 as usize * stride..y1 as usize * stride];
            live.layer().upload_texture_rows(
                &card.texes[write.slot],
                y0,
                y1 - y0,
                bytes,
                t.pw * 4,
                t.pw,
            );
            *tray_uploads += 1;
            *tray_upload_bytes += bytes.len() as u64;
            let (b0, b1) = (band.0 as usize * stride, band.1 as usize * stride);
            card.pixels[b0..b1].copy_from_slice(&t.rgba[b0..b1]);
            card.ring.land_change(write, band, reader);
            return Ok(());
        }
        let layer = live.layer();
        let tex = layer
            .try_create_texture_2d(
                "aterm-gpu metal tray card",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                t.pw,
                t.ph,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            )
            .map_err(|e| format!("tray card mint: {e}"))?;
        layer.upload_texture_full(&tex, t.rgba, t.pw * 4, t.pw, t.ph);
        *tray_uploads += 1;
        *tray_upload_bytes += t.rgba.len() as u64;
        let mut pixels = win
            .metal_tray_card
            .take()
            .map_or_else(Vec::new, |c| c.pixels);
        pixels.clear();
        pixels.extend_from_slice(t.rgba);
        win.metal_tray_card = Some(MetalTrayCard {
            texes: vec![tex],
            ring: crate::tray_card_ring::TrayCardRing::fresh(t.ph, reader),
            w: t.pw,
            h: t.ph,
            pixels,
        });
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn metal_composite_bloom_in_place(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
    ) -> Result<(), String> {
        use crate::metal::encoder::{RenderPassDesc, StoreAction};
        use crate::metal::ffi::{
            self as mtl, ClearColor, LoadAction, MtlScissorRect, PixelFormat as MPix, PrimitiveType,
        };
        let _pool = mtl::AutoreleasePool::new();
        let (glow_count, bbox, extract_first) = self.build_bloom_glow(input, true);
        if glow_count == 0 {
            return Ok(());
        }
        let (fw, fh, off_tex) = {
            let off = win
                .metal_offscreen
                .as_ref()
                .ok_or("armed frame lacks its metal offscreen")?;
            (off.w, off.h, off.tex.clone_handle())
        };
        let text_blend =
            u32::from(self.cpu.text_blending() == aterm_render::TextBlending::LinearCorrected);
        let live = self
            .metal_arm
            .as_mut()
            .ok_or("the Metal arm is not live")?
            .live_mut();
        let (bw, bh) = (
            (fw / BLOOM_DOWNSCALE).max(1) as usize,
            (fh / BLOOM_DOWNSCALE).max(1) as usize,
        );
        let half = live
            .layer()
            .try_create_texture_2d(
                "aterm-gpu metal bloom half",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                bw as u32,
                bh as u32,
                crate::device_layer::TexUsage::OFFSCREEN,
                None,
            )?
            .into_metal();
        let mint_bytes = |live: &mut MetalArmLive, bytes: &[u8]| -> Result<mtl::Obj, String> {
            let layer = live.layer();
            let buf = layer
                .try_create_uniform_buffer("aterm-gpu metal bloom bytes", bytes.len() as u64)?;
            // SAFETY: fresh exactly-sized shared buffer; no GPU work on it.
            unsafe { layer.write_buffer_from_zero(&buf, bytes) };
            Ok(buf.into_metal().into_obj())
        };
        let (stream, extract_row) = if self.bloom_items_live {
            let words = self.bloom_items.words();
            (
                mint_bytes(live, aterm_bits::cast_slice(words))?,
                Pipeline::GlowItems,
            )
        } else {
            let insts: &[GlowInstance] = &self.bloom_glow_scratch;
            (
                mint_bytes(live, aterm_bits::cast_slice(insts))?,
                Pipeline::GlowAdd,
            )
        };
        let extract_offset = extract_first as usize * std::mem::size_of::<GlowInstance>();
        let cell_u = mint_bytes(
            live,
            aterm_bits::bytes_of(&Uniforms {
                screen: [fw as f32, fh as f32],
                text_blend: text_blend as f32,
                _pad: 0.0,
            }),
        )?;
        let bu = mint_bytes(
            live,
            aterm_bits::bytes_of(&BloomUniform {
                texel: [1.0 / bw as f32, 1.0 / bh as f32],
                strength: page_bloom_strength(self.bloom_strength, input, self.theme.bg),
                radius: self.bloom_radius,
                chroma_radius: bloom_chroma_radius(self.bloom_radius),
                extent: [1.0; 2],
                _pad: 0.0,
            }),
        )?;
        let clip = input.fx_clip.map(|(cx0, cy0, cx1, cy1)| {
            [
                u32::from(cx0),
                u32::from(cy0),
                u32::from(cx1),
                u32::from(cy1),
            ]
        });
        let scissor = if let Some([x0, y0, x1, y1]) = bbox {
            let d = aterm_render::bloom::reach_px(self.bloom_radius);
            Some([
                x0.saturating_sub(d),
                y0.saturating_sub(d),
                x1.saturating_add(d).min(fw),
                y1.saturating_add(d).min(fh),
            ])
        } else {
            clip.map(|_| [0, 0, fw, fh])
        };
        let scissor = match (scissor, clip) {
            (Some([sx0, sy0, sx1, sy1]), Some([cx0, cy0, cx1, cy1])) => Some([
                sx0.max(cx0),
                sy0.max(cy0),
                sx1.min(cx1).min(fw),
                sy1.min(cy1).min(fh),
            ]),
            (s, None) => s,
            (None, Some(_)) => unreachable!("clip-only case handled above"),
        };
        let extract_pso = live.present_pso(extract_row, MPix::Rgba8Unorm)?;
        let bloom_pso = live.present_pso(Pipeline::Bloom, MPix::Rgba8Unorm)?;
        let linear = &live.linear;
        let extract_binds = extract_row.spec().binds;
        let bloom_binds = Pipeline::Bloom.spec().binds;

        let mut cb = live.session.begin()?;
        {
            let pass = cb.render_pass(&RenderPassDesc {
                target: &half,
                load: LoadAction::Clear(ClearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                }),
                store: StoreAction::Store,
                viewport: None,
                scissor: None,
            })?;
            pass.set_pipeline(&extract_pso);
            pass.set_vertex_buffer(
                &cell_u,
                extract_binds
                    .vertex_uniform
                    .expect("row 4 has a vertex uniform") as usize,
            )?;
            pass.set_instance_stream_at(&stream, extract_offset);
            pass.draw_instanced(PrimitiveType::Triangle, 6, glow_count as usize)?;
        }
        let halo = match (scissor, clip) {
            (Some([sx0, sy0, sx1, sy1]), _) if sx1 > sx0 && sy1 > sy0 => {
                let pass = cb.render_pass(&RenderPassDesc {
                    target: &off_tex,
                    load: LoadAction::Load,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: Some(MtlScissorRect {
                        x: sx0 as usize,
                        y: sy0 as usize,
                        width: (sx1 - sx0) as usize,
                        height: (sy1 - sy0) as usize,
                    }),
                })?;
                pass.set_pipeline(&bloom_pso);
                pass.set_fragment_texture(half.obj(), bloom_binds.fragment_textures[0] as usize);
                pass.set_fragment_sampler(linear, bloom_binds.fragment_samplers[0] as usize);
                pass.set_fragment_buffer(&bu, bloom_binds.fragment_buffers[0] as usize);
                pass.draw_fullscreen_triangle()?;
                Some([sx0, sy0, sx1, sy1])
            }
            (Some(_), Some(_)) => Some([0, 0, 0, 0]),
            (Some(_), None) | (None, _) => {
                let pass = cb.render_pass(&RenderPassDesc {
                    target: &off_tex,
                    load: LoadAction::Load,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: None,
                })?;
                pass.set_pipeline(&bloom_pso);
                pass.set_fragment_texture(half.obj(), bloom_binds.fragment_textures[0] as usize);
                pass.set_fragment_sampler(linear, bloom_binds.fragment_samplers[0] as usize);
                pass.set_fragment_buffer(&bu, bloom_binds.fragment_buffers[0] as usize);
                pass.draw_fullscreen_triangle()?;
                None
            }
        };
        if live.wait_inline(&cb.commit()) != crate::metal::loss::CbOutcome::Completed {
            return Err("in-place bloom command buffer did not complete".to_owned());
        }
        note_offscreen_written(win, halo, (fw, fh));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn metal_shimmer_in_place(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
    ) -> Result<(), String> {
        use crate::metal::encoder::{RenderPassDesc, StoreAction};
        use crate::metal::ffi::{self as mtl, LoadAction, MtlScissorRect, PixelFormat as MPix};
        let _pool = mtl::AutoreleasePool::new();
        let (fw, fh, off_tex) = {
            let off = win
                .metal_offscreen
                .as_ref()
                .ok_or("armed frame lacks its metal offscreen")?;
            (off.w, off.h, off.tex.clone_handle())
        };
        let Some(region) = self.shimmer_region(input, fw, fh) else {
            return Ok(());
        };
        let phase = self.shimmer_phase();
        let (cell_w, cell_h) = self.cpu.cell_size();
        let live = self
            .metal_arm
            .as_mut()
            .ok_or("the Metal arm is not live")?
            .live_mut();
        let scratch = live
            .layer()
            .try_create_texture_2d(
                "aterm-gpu metal shimmer scratch",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                fw,
                fh,
                crate::device_layer::TexUsage::OFFSCREEN,
                None,
            )?
            .into_metal();
        let mut heat = [[0f32; 4]; SHIMMER_BANDS / 4];
        for (i, v) in region.heat.iter().enumerate() {
            heat[i / 4][i % 4] = *v;
        }
        let su = ShimmerUniform {
            frame: [fw as f32, fh as f32],
            region_min: [region.x0 as f32, region.y0 as f32],
            region_max: [region.x1 as f32, region.y1 as f32],
            hot_top: region.hot_top,
            rise: region.rise,
            amp: (cell_h as f32 / 18.0).clamp(0.75, SHIMMER_AMP_PX),
            period: (cell_h as f32).max(4.0),
            phase,
            band_x0: region.band_x0,
            band_w: region.band_w,
            rolloff: (cell_w as f32).max(1.0),
            _pad: [0.0; 2],
            heat,
        };
        let ub = {
            let layer = live.layer();
            let buf = layer.try_create_uniform_buffer(
                "aterm-gpu metal shimmer uniform",
                std::mem::size_of::<ShimmerUniform>() as u64,
            )?;
            // SAFETY: fresh exactly-sized shared buffer; no GPU work on it.
            unsafe { layer.write_buffer_from_zero(&buf, aterm_bits::bytes_of(&su)) };
            buf.into_metal().into_obj()
        };
        let m = SHIMMER_COPY_MARGIN;
        let [sx0, sy0, sx1, sy1] = [
            region.x0.saturating_sub(m),
            region.y0.saturating_sub(m),
            region.x1.saturating_add(m).min(fw),
            region.y1.saturating_add(m).min(fh),
        ];
        let pso = live.present_pso(Pipeline::Shimmer, MPix::Rgba8Unorm)?;
        let linear = &live.linear;
        let binds = Pipeline::Shimmer.spec().binds;
        let mut cb = live.session.begin()?;
        cb.copy_texture_sub_rect(
            &off_tex,
            (sx0 as usize, sy0 as usize),
            &scratch,
            (sx0 as usize, sy0 as usize),
            (sx1 - sx0) as usize,
            (sy1 - sy0) as usize,
        )?;
        {
            let pass = cb.render_pass(&RenderPassDesc {
                target: &off_tex,
                load: LoadAction::Load,
                store: StoreAction::Store,
                viewport: None,
                scissor: Some(MtlScissorRect {
                    x: region.x0 as usize,
                    y: region.y0 as usize,
                    width: (region.x1 - region.x0) as usize,
                    height: (region.y1 - region.y0) as usize,
                }),
            })?;
            pass.set_pipeline(&pso);
            pass.set_fragment_texture(scratch.obj(), binds.fragment_textures[0] as usize);
            pass.set_fragment_sampler(linear, binds.fragment_samplers[0] as usize);
            pass.set_fragment_buffer(&ub, binds.fragment_buffers[0] as usize);
            pass.draw_fullscreen_triangle()?;
        }
        if live.wait_inline(&cb.commit()) != crate::metal::loss::CbOutcome::Completed {
            return Err("in-place shimmer command buffer did not complete".to_owned());
        }
        note_offscreen_written(
            win,
            Some([region.x0, region.y0, region.x1, region.y1]),
            (fw, fh),
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn metal_tray_into_offscreen(
        &mut self,
        win: &mut WindowGpu,
        t: TrayQuad<'_>,
    ) -> Result<(), String> {
        use crate::metal::encoder::{RenderPassDesc, StoreAction};
        use crate::metal::ffi::{self as mtl, LoadAction, PixelFormat as MPix};
        let _pool = mtl::AutoreleasePool::new();
        let (fw, fh, off_tex) = {
            let off = win
                .metal_offscreen
                .as_ref()
                .ok_or("armed frame lacks its metal offscreen")?;
            (off.w, off.h, off.tex.clone_handle())
        };
        note_offscreen_written(win, None, (fw, fh));
        let live = self
            .metal_arm
            .as_mut()
            .ok_or("the Metal arm is not live")?
            .live_mut();
        Self::ensure_metal_tray_card(
            live,
            win,
            &t,
            None,
            &mut self.tray_uploads,
            &mut self.tray_upload_bytes,
        )?;
        let tu = {
            let layer = live.layer();
            let buf = layer.try_create_uniform_buffer(
                "aterm-gpu metal tray uniform",
                std::mem::size_of::<TrayUniform>() as u64,
            )?;
            // SAFETY: fresh exactly-sized shared buffer; no GPU work on it.
            unsafe {
                layer.write_buffer_from_zero(
                    &buf,
                    aterm_bits::bytes_of(&TrayUniform {
                        rect: [t.dx as f32, t.dy as f32, t.pw as f32, t.ph as f32],
                        fb: [fw as f32, fh as f32],
                        _pad: [0.0, 0.0],
                    }),
                );
            }
            buf.into_metal().into_obj()
        };
        let pso = live.present_pso(Pipeline::Tray, MPix::Rgba8Unorm)?;
        let linear = &live.linear;
        let binds = Pipeline::Tray.spec().binds;
        let card = win
            .metal_tray_card
            .as_ref()
            .expect("ensure_metal_tray_card set it above");
        let mut cb = live.session.begin()?;
        {
            let pass = cb.render_pass(&RenderPassDesc {
                target: &off_tex,
                load: LoadAction::Load,
                store: StoreAction::Store,
                viewport: None,
                scissor: None,
            })?;
            pass.set_pipeline(&pso);
            pass.set_vertex_buffer(
                &tu,
                binds.vertex_uniform.expect("row 17 binds at 2") as usize,
            )?;
            pass.set_fragment_texture(card.tex().obj(), binds.fragment_textures[0] as usize);
            pass.set_fragment_sampler(linear, binds.fragment_samplers[0] as usize);
            pass.draw_strip_quad()?;
        }
        if live.wait_inline(&cb.commit()) != crate::metal::loss::CbOutcome::Completed {
            return Err("tray bake command buffer did not complete".to_owned());
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn metal_ensure_shift_scratch(
        &mut self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
    ) -> Result<crate::metal::resources::SealedTexture, String> {
        let resident = win
            .metal_shift_scratch
            .as_ref()
            .is_some_and(|t| (t.width(), t.height()) == (w as usize, h as usize));
        if !resident {
            let _pool = crate::metal::ffi::AutoreleasePool::new();
            let cell = self.metal_arm.as_mut().ok_or("the Metal arm is not live")?;
            let live = cell.live_mut();
            win.metal_shift_scratch = Some(
                live.layer()
                    .try_create_texture_2d(
                        "aterm-gpu metal scroll shift scratch",
                        crate::device_layer::TexelFormat::Rgba8Unorm,
                        w,
                        h,
                        crate::device_layer::TexUsage::COPY_STAGING,
                        None,
                    )?
                    .into_metal(),
            );
        }
        Ok(win
            .metal_shift_scratch
            .as_ref()
            .expect("ensured above")
            .clone_handle())
    }

    #[cfg(target_os = "macos")]
    fn metal_band_shift_plan(
        &mut self,
        win: &mut WindowGpu,
        y0: usize,
        y1: usize,
        delta: i64,
    ) -> Option<BandShift> {
        let shift = band_shift_ops(y0, y1, delta)?;
        let (w, h) = win.metal_offscreen.as_ref().map(|o| (o.w, o.h))?;
        if let Err(e) = self.metal_ensure_shift_scratch(win, w, h) {
            metal_arm_note(&format!(
                "armed scroll shift: no scratch ({e}); band unshifted"
            ));
            return None;
        }
        Some(shift)
    }

    #[cfg(target_os = "macos")]
    fn metal_shift_offscreen_band_px(
        &mut self,
        win: &mut WindowGpu,
        y0: usize,
        y1: usize,
        delta: i64,
    ) -> bool {
        use crate::device_layer::{FrameEncoder, SubmittedFrame};
        let Some(shift) = self.metal_band_shift_plan(win, y0, y1, delta) else {
            return false;
        };
        let Some(scratch) = win.metal_shift_scratch.as_ref().map(|t| t.clone_handle()) else {
            return false;
        };
        if let Some(dims) = win.metal_offscreen.as_ref().map(|o| (o.w, o.h)) {
            note_offscreen_written(win, None, dims);
        }
        let Some(off) = win.metal_offscreen.as_ref() else {
            return false;
        };
        let w = off.w;
        let Some(cell) = self.metal_arm.as_mut() else {
            return false;
        };
        let live = cell.live_mut();
        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let staged = (|| -> Result<(), String> {
            let mut enc = FrameEncoder::metal(&live.session)?;
            record_band_shift(&mut enc, &off.tex, &scratch, shift, w)?;
            match enc.submit() {
                SubmittedFrame::Metal(sub) => {
                    let outcome = live.wait_inline(&sub);
                    if outcome != crate::metal::loss::CbOutcome::Completed {
                        return Err(format!("scroll-shift command buffer ended {outcome:?}"));
                    }
                    Ok(())
                }
            }
        })();
        match staged {
            Ok(()) => true,
            Err(e) => {
                metal_arm_note(&format!("armed scroll shift failed ({e}); band unshifted"));
                false
            }
        }
    }

    /// Drop a window's Metal swapchain once the renderer is disarmed, so its stale layer is
    /// unparented.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_disarm_sweep(
        &mut self,
        metal_surf: &mut Option<crate::metal::present::MetalWindowSurface>,
    ) {
        if !self.metal_armed && metal_surf.take().is_some() {
            metal_arm_note("disarmed: dropped this window's armed swapchain");
        }
    }

    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_arguments,
        reason = "the shared swapchain seam's established tuple"
    )]
    fn metal_present_input_with_crop(
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
        let out = self.metal_present_input_with_crop_timed(
            win,
            surf,
            input,
            invert,
            overlay,
            tray,
            source_crop,
            effects_transport_shift_y,
        );
        self.metal_seal_gpu_frame();
        out
    }

    #[cfg(target_os = "macos")]
    fn metal_open_gpu_frame(&mut self) {
        self.last_gpu_time_serial = None;
        if !crate::gpu_time::sink_installed() || !self.metal_arm_ready() {
            return;
        }
        self.last_gpu_time_serial = self
            .metal_arm
            .as_ref()
            .and_then(|cell| cell.live_ref().session.open_gpu_frame());
    }

    #[cfg(target_os = "macos")]
    fn metal_seal_gpu_frame(&self) {
        if let Some(cell) = self.metal_arm.as_ref() {
            cell.live_ref().session.seal_gpu_frame();
        }
    }

    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_arguments,
        reason = "the shared swapchain seam's established tuple"
    )]
    fn metal_present_input_with_crop_timed(
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
        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let source_crop = self
            .resident_present_crop(input, source_crop)
            .map_err(|()| SurfacePresentFailure::Validation)?;
        if surf.metal.is_none() {
            metal_arm_note("armed present refused: window has no armed swapchain (re-attach it)");
            return Err(SurfacePresentFailure::Validation);
        }
        let (translucent, premult, want) = self.metal_present_want(win, surf);
        {
            let Some(cell) = self.metal_arm.as_mut() else {
                metal_arm_note("armed present: the arm is not live");
                return Err(SurfacePresentFailure::Validation);
            };
            let live = cell.live_mut();
            live.drain_pending();
            let (_, ring_parked_ns) = live.take_park_ns();
            win.add_gpu_park(ring_parked_ns);
            let ms = surf.metal.as_mut().expect("checked above");
            let reconciled = ms.reconcile(live.mint.device(), &want);
            if let Some(waited_ns) = ms.take_completed_acquire_wait_ns() {
                win.record_surface_acquire(waited_ns);
            }
            if let Some(queued_ns) = ms.take_completed_acquire_queue_ns() {
                win.record_surface_acquire_queue(queued_ns);
            }
            match reconciled {
                Ok(_) => {}
                Err(e) => {
                    metal_arm_note(&format!("armed present: reconcile failed ({e})"));
                    return Err(SurfacePresentFailure::Reconfigured);
                }
            }
        }
        let hint = win.occluded_hint();
        let (dest_w, dest_h, dest_texel) = surf.neutral_config();
        let ms = surf.metal.as_mut().expect("checked above");
        let acquired = if ms.async_acquire() {
            ms.acquire()
        } else {
            win.measure_surface_acquire(|| ms.acquire())
        };
        let frame = match acquired {
            Ok(f) => f,
            Err(refusal) => {
                return Err(crate::metal::present::surface_present_failure(
                    &refusal, hint,
                ));
            }
        };
        let work_started = aterm_time::Instant::now();
        self.metal_open_gpu_frame();

        let (fw, fh) = self.encode_present_frame(win, input);
        if !self.last_frame_arm_metal {
            metal_arm_note("armed present dropped: the frame did not draw on Metal");
            return Err(SurfacePresentFailure::Validation);
        }
        win.resident_input_effects_transport_shift_y = effects_transport_shift_y;
        if tray.is_none() {
            win.metal_tray_card = None;
        }

        let Some(seq) = self
            .metal_arm
            .as_mut()
            .map(|cell| cell.live_mut().take_present_seq())
        else {
            metal_arm_note("armed present: the arm vanished mid-present");
            return Err(SurfacePresentFailure::Validation);
        };
        let target = frame.render_target();
        let (submitted, _composed_present_off) = match self.metal_encode_submit_b(
            win,
            input,
            invert,
            overlay,
            tray,
            Some(seq),
            source_crop,
            (fw, fh),
            (dest_w, dest_h),
            dest_texel,
            translucent,
            premult,
            !want.framebuffer_only,
            &target,
        ) {
            Ok(sub) => sub,
            Err(e) => {
                metal_arm_note(&format!("armed present dropped: Submit B failed ({e})"));
                return Err(SurfacePresentFailure::Validation);
            }
        };
        let (frame_slot_parked_ns, late_ring_parked_ns) = self
            .metal_arm
            .as_mut()
            .map_or((0, 0), |cell| cell.live_mut().take_park_ns());
        win.add_gpu_park(frame_slot_parked_ns.saturating_add(late_ring_parked_ns));
        win.last_present_work_ns = encode_work_ns_excluding_park(
            u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            frame_slot_parked_ns,
        );
        win.last_present_encode_ns = win.last_present_work_ns.saturating_sub(late_ring_parked_ns);
        self.metal_seal_gpu_frame();
        let Some(cell) = self.metal_arm.as_mut() else {
            metal_arm_note("armed present: the arm vanished mid-present");
            return Err(SurfacePresentFailure::Validation);
        };
        let live = cell.live_mut();
        let ticket = match frame.present(&live.session) {
            Ok(t) => t,
            Err(e) => {
                let _ = submitted.wait_outcome();
                metal_arm_note(&format!("armed present: present refused ({e})"));
                return Err(SurfacePresentFailure::Validation);
            }
        };
        live.pending.push(PendingPresent {
            cb: submitted,
            ticket,
            seq,
        });
        if let Some(ms) = surf.metal.as_mut() {
            ms.prefetch_drawable();
        }
        self.metal_maybe_inject_loss();
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn metal_maybe_inject_loss(&mut self) {
        let Some(n) = self.metal_inject_loss_after.as_mut() else {
            return;
        };
        if *n > 1 {
            *n -= 1;
            return;
        }
        self.metal_inject_loss_after = None;
        let Some(cell) = self.metal_arm.as_mut() else {
            return;
        };
        let outcome = crate::metal::loss::classify(
            crate::metal::loss::STATUS_ERROR,
            Some(crate::metal::loss::ERROR_PAGE_FAULT),
        );
        cell.live_mut().session.latch().record(&outcome);
        crate::stderr_line!(
            "aterm-gpu: ATERM_METAL_INJECT_LOSS fired — recorded the synthetic \
             MTLCommandBufferErrorPageFault into the armed loss latch (drill lever)"
        );
    }

    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_arguments,
        reason = "the armed twin preserves the shared virtual-present seam's tuple"
    )]
    fn metal_present_virtual_with_crop(
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
        let out = self.metal_present_virtual_with_crop_timed(
            win,
            input,
            invert,
            overlay,
            tray,
            source_crop,
            effects_transport_shift_y,
            destination,
        );
        self.metal_seal_gpu_frame();
        out
    }

    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_arguments,
        reason = "the armed twin preserves the shared virtual-present seam's tuple"
    )]
    fn metal_present_virtual_with_crop_timed(
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
        use crate::metal::ffi as mtl;
        let _pool = mtl::AutoreleasePool::new();
        let source_crop = self
            .resident_present_crop(input, source_crop)
            .map_err(|()| "virtual present crop outside the resident frame")?;
        let w = self.clamp_fb_dim(destination.0.max(1));
        let h = self.clamp_fb_dim(destination.1.max(1));
        let work_started = aterm_time::Instant::now();
        self.metal_open_gpu_frame();
        let (fw, fh) = self.encode_present_frame(win, input);
        if !self.last_frame_arm_metal {
            return Err("the frame did not draw on Metal".to_owned());
        }
        win.resident_input_effects_transport_shift_y = effects_transport_shift_y;
        if tray.is_none() {
            win.metal_tray_card = None;
        }
        let needs_dest = win
            .metal_virtual_off
            .as_ref()
            .is_none_or(|t| (t.width(), t.height()) != (w as usize, h as usize));
        if needs_dest {
            let live = self
                .metal_arm
                .as_mut()
                .ok_or("the arm is not live")?
                .live_mut();
            win.metal_virtual_off = Some(
                live.layer()
                    .try_create_texture_2d(
                        "aterm-gpu metal virtual present target",
                        crate::device_layer::TexelFormat::Bgra8Unorm,
                        w,
                        h,
                        crate::device_layer::TexUsage::OFFSCREEN,
                        None,
                    )?
                    .into_metal(),
            );
        }
        let target = win
            .metal_virtual_off
            .as_ref()
            .expect("ensured above")
            .clone_handle();
        let (submitted, _composed_present_off) = self.metal_encode_submit_b(
            win,
            input,
            invert,
            overlay,
            tray,
            None,
            source_crop,
            (fw, fh),
            (w, h),
            VIRTUAL_PRESENT_TEXEL,
            false,
            false,
            true,
            &target,
        )?;
        self.metal_seal_gpu_frame();
        let encode_parked_ns = self.metal_arm.as_mut().map_or(0, |cell| {
            let (slot, ring) = cell.live_mut().take_park_ns();
            slot.saturating_add(ring)
        });
        win.last_present_encode_ns = elapsed_ns(work_started).saturating_sub(encode_parked_ns);
        if submitted.wait_outcome() != crate::metal::loss::CbOutcome::Completed {
            return Err("Submit B did not complete".to_owned());
        }
        win.virtual_presented_input_epoch = win.resident_input_epoch;
        if let Some(cell) = self.metal_arm.as_mut() {
            let _ = cell.live_mut().take_park_ns();
        }
        win.last_present_work_ns =
            u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.metal_maybe_inject_loss();
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one §2 Submit B transcript, matching the replay's pinned order"
    )]
    fn metal_encode_submit_b(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        tray_reader: Option<u64>,
        source_crop: Option<PresentCrop>,
        (fw, fh): (u32, u32),
        (dw, dh): (u32, u32),
        dest_format: crate::device_layer::TexelFormat,
        translucent: bool,
        premult: bool,
        copy_armed: bool,
        target: &crate::metal::resources::SealedTexture,
    ) -> Result<(crate::metal::encoder::Submitted, bool), String> {
        use crate::metal::ffi::{self as mtl, PixelFormat as MPix};
        use crate::metal::present::{
            ApronStripPass, BandShiftPass, BloomPasses, CrownPass, PostPass, PresentSequence,
            ShimmerPass, TrayPass, encode_present_sequence,
        };

        let PresentUniforms {
            live_bg,
            want,
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
            dest_format == crate::device_layer::TexelFormat::Rgba16Float,
            translucent,
            premult,
        );
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
        let shift_scratch = match band_shift {
            Some(_) => Some(self.metal_ensure_shift_scratch(win, fw, fh)?),
            None => None,
        };
        let apron_strip = match band_shift {
            Some(_) => match self.apron_strip_plan(win, input, fh as usize) {
                Some(strip) => Some((strip, self.metal_ensure_apron_tex(win, strip)?)),
                None => None,
            },
            None => None,
        };

        let off_tex = win
            .metal_offscreen
            .as_ref()
            .ok_or("the armed frame's offscreen is absent")?
            .tex
            .clone_handle();
        let use_present_off = bloom_state.is_some()
            || shimmer_region.is_some()
            || tray.is_some()
            || band_shift.is_some();
        let needs_off = use_present_off
            && win
                .metal_present_off
                .as_ref()
                .is_none_or(|t| (t.width(), t.height()) != (fw as usize, fh as usize));
        let cell = self.metal_arm.as_mut().ok_or("the Metal arm is not live")?;
        let live = cell.live_mut();
        let mint_scratch = |live: &MetalArmLive, label, w: usize, h: usize| {
            live.layer()
                .try_create_texture_2d(
                    label,
                    crate::device_layer::TexelFormat::Rgba8Unorm,
                    w as u32,
                    h as u32,
                    crate::device_layer::TexUsage::OFFSCREEN,
                    None,
                )
                .map(crate::device_layer::LayerTexture::into_metal)
        };
        if needs_off {
            win.offscreen_dirty_since_sync = None;
            win.present_offscreen_fx = None;
            win.metal_present_off = Some(mint_scratch(
                live,
                "aterm-gpu metal present offscreen",
                fw as usize,
                fh as usize,
            )?);
        }
        let present_off = use_present_off.then(|| {
            win.metal_present_off
                .as_ref()
                .expect("ensured above when `use_present_off`")
                .clone_handle()
        });
        let dest_mpix = dest_format.metal();
        live.advance_submit_b_slot();

        let (bw, bh) = (
            (fw / BLOOM_DOWNSCALE).max(1) as usize,
            (fh / BLOOM_DOWNSCALE).max(1) as usize,
        );
        if bloom_state.is_some()
            && win
                .metal_bloom_half
                .as_ref()
                .is_none_or(|t| (t.width(), t.height()) != (bw, bh))
        {
            win.metal_bloom_half = Some(mint_scratch(live, "aterm-gpu metal bloom half", bw, bh)?);
        }
        let bloom_parts = if let Some((count, bbox, extract_first)) = bloom_state {
            let half = win
                .metal_bloom_half
                .as_ref()
                .expect("ensured above when `bloom_state` is Some")
                .clone_handle();
            let sb_idx = live.submit_b_idx;
            let stream = MetalArmLive::upload_scratch_stream(
                crate::device_layer::DeviceHandle::Metal(&live.mint),
                &mut live.submit_b_slots[sb_idx].glow_stream,
                if self.bloom_items_live {
                    aterm_bits::cast_slice(self.bloom_items.words())
                } else {
                    aterm_bits::cast_slice(&self.bloom_glow_scratch)
                },
            )?;
            let extract_offset = extract_first as usize * std::mem::size_of::<GlowInstance>();
            let cell_u = MetalArmLive::upload_scratch_stream(
                crate::device_layer::DeviceHandle::Metal(&live.mint),
                &mut live.submit_b_slots[sb_idx].bloom_cell_u,
                aterm_bits::bytes_of(&Uniforms {
                    screen: [fw as f32, fh as f32],
                    text_blend: text_blend as f32,
                    _pad: 0.0,
                }),
            )?;
            let bu = MetalArmLive::upload_scratch_stream(
                crate::device_layer::DeviceHandle::Metal(&live.mint),
                &mut live.submit_b_slots[sb_idx].bloom_u,
                aterm_bits::bytes_of(&BloomUniform {
                    texel: [1.0 / bw as f32, 1.0 / bh as f32],
                    strength: page_bloom_strength(self.bloom_strength, input, self.theme.bg),
                    radius: self.bloom_radius,
                    chroma_radius: bloom_chroma_radius(self.bloom_radius),
                    extent: [1.0; 2],
                    _pad: 0.0,
                }),
            )?;
            let clip = input.fx_clip.map(|(cx0, cy0, cx1, cy1)| {
                [
                    u32::from(cx0),
                    u32::from(cy0),
                    u32::from(cx1),
                    u32::from(cy1),
                ]
            });
            let scissor = if let Some([x0, y0, x1, y1]) = bbox {
                let d = aterm_render::bloom::reach_px(self.bloom_radius);
                Some([
                    x0.saturating_sub(d),
                    y0.saturating_sub(d),
                    x1.saturating_add(d).min(fw),
                    y1.saturating_add(d).min(fh),
                ])
            } else {
                clip.map(|_| [0, 0, fw, fh])
            };
            let scissor = match (scissor, clip) {
                (Some([sx0, sy0, sx1, sy1]), Some([cx0, cy0, cx1, cy1])) => Some([
                    sx0.max(cx0),
                    sy0.max(cy0),
                    sx1.min(cx1).min(fw),
                    sy1.min(cy1).min(fh),
                ]),
                (s, None) => s,
                (None, Some(_)) => unreachable!("clip-only case handled above"),
            }
            .filter(|[sx0, sy0, sx1, sy1]| sx1 > sx0 && sy1 > sy0);
            let extract_row = if self.bloom_items_live {
                Pipeline::GlowItems
            } else {
                Pipeline::GlowAdd
            };
            let extract_pso = live.present_pso(extract_row, dest_mpix)?;
            let bloom_pso = live.present_pso(Pipeline::Bloom, dest_mpix)?;
            Some((
                half,
                extract_pso,
                cell_u,
                extract_row.spec().binds,
                (stream, extract_offset),
                count as usize,
                bloom_pso,
                bu,
                Pipeline::Bloom.spec().binds,
                scissor,
            ))
        } else {
            None
        };

        if shimmer_region.is_some()
            && win
                .metal_shimmer_scratch
                .as_ref()
                .is_none_or(|t| (t.width(), t.height()) != (fw as usize, fh as usize))
        {
            win.metal_shimmer_scratch = Some(mint_scratch(
                live,
                "aterm-gpu metal shimmer scratch",
                fw as usize,
                fh as usize,
            )?);
        }
        let shimmer_parts = shimmer_region
            .map(|region| -> Result<_, String> {
                let scratch = win
                    .metal_shimmer_scratch
                    .as_ref()
                    .expect("ensured above when `shimmer_region` is Some")
                    .clone_handle();
                let mut heat = [[0f32; 4]; SHIMMER_BANDS / 4];
                for (i, v) in region.heat.iter().enumerate() {
                    heat[i / 4][i % 4] = *v;
                }
                let su = ShimmerUniform {
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
                };
                let sb_idx = live.submit_b_idx;
                let ub = MetalArmLive::upload_scratch_stream(
                    crate::device_layer::DeviceHandle::Metal(&live.mint),
                    &mut live.submit_b_slots[sb_idx].shimmer_u,
                    aterm_bits::bytes_of(&su),
                )?;
                let m = SHIMMER_COPY_MARGIN;
                let stage = [
                    region.x0.saturating_sub(m),
                    region.y0.saturating_sub(m),
                    region.x1.saturating_add(m).min(fw),
                    region.y1.saturating_add(m).min(fh),
                ];
                let pso = live.present_pso(Pipeline::Shimmer, dest_mpix)?;
                Ok((
                    scratch,
                    stage,
                    pso,
                    ub,
                    Pipeline::Shimmer.spec().binds,
                    [region.x0, region.y0, region.x1, region.y1],
                ))
            })
            .transpose()?;

        if let Some(t) = tray.as_ref() {
            Self::ensure_metal_tray_card(
                live,
                win,
                t,
                tray_reader,
                &mut self.tray_uploads,
                &mut self.tray_upload_bytes,
            )?;
        }
        let tray_parts = tray
            .map(|t| -> Result<_, String> {
                let sb_idx = live.submit_b_idx;
                let tu = MetalArmLive::upload_scratch_stream(
                    crate::device_layer::DeviceHandle::Metal(&live.mint),
                    &mut live.submit_b_slots[sb_idx].tray_u,
                    aterm_bits::bytes_of(&TrayUniform {
                        rect: [t.dx as f32, t.dy as f32, t.pw as f32, t.ph as f32],
                        fb: [fw as f32, fh as f32],
                        _pad: [0.0, 0.0],
                    }),
                )?;
                let pso = live.present_pso(Pipeline::Tray, MPix::Rgba8Unorm)?;
                Ok((tu, pso, Pipeline::Tray.spec().binds))
            })
            .transpose()?;

        let bu = live.ensure_submit_b_blit_u(want)?;
        let blit_pso = live.present_pso(Pipeline::Blit, dest_mpix)?;
        let crown_parts = if let Some(hu) = crown
            && crown_count > 0
        {
            if let Some((x, y, w, h)) = crown_scissor {
                let row = match (run_hdr_glow, self.bloom_items_live) {
                    (true, false) => Pipeline::HdrGlow,
                    (false, false) => Pipeline::SdrGlow,
                    (true, true) => Pipeline::HdrGlowItems,
                    (false, true) => Pipeline::SdrGlowItems,
                };
                let sb_idx = live.submit_b_idx;
                let ubuf = MetalArmLive::upload_scratch_stream(
                    crate::device_layer::DeviceHandle::Metal(&live.mint),
                    &mut live.submit_b_slots[sb_idx].crown_u,
                    aterm_bits::bytes_of(&hu),
                )?;
                let stream = match bloom_parts.as_ref() {
                    Some((_, _, _, _, (bloom_stream, _), ..)) => bloom_stream.clone_retained(),
                    None => MetalArmLive::upload_scratch_stream(
                        crate::device_layer::DeviceHandle::Metal(&live.mint),
                        &mut live.submit_b_slots[sb_idx].glow_stream,
                        if self.bloom_items_live {
                            aterm_bits::cast_slice(self.bloom_items.words())
                        } else {
                            aterm_bits::cast_slice(&self.bloom_glow_scratch[..crown_count as usize])
                        },
                    )?,
                };
                let pso = live.present_pso(row, dest_mpix)?;
                let binds = row.spec().binds;
                Some((pso, ubuf, binds, stream, crown_count as usize, (x, y, w, h)))
            } else {
                None
            }
        } else {
            None
        };

        debug_assert_eq!(
            use_present_off,
            bloom_parts.is_some()
                || shimmer_parts.is_some()
                || tray_parts.is_some()
                || band_shift.is_some(),
            "the present-copy gate and the passes that write it must agree"
        );
        let mut lost_compose = false;
        win.metal_present_off_pending
            .retain(|probe| match probe.try_terminal() {
                None => true,
                Some(crate::metal::loss::CbOutcome::Completed) => false,
                Some(_) => {
                    lost_compose = true;
                    false
                }
            });
        if lost_compose {
            win.offscreen_dirty_since_sync = None;
            win.present_offscreen_fx = None;
        }
        let copy_rect = present_copy_rect(
            win.offscreen_dirty_since_sync,
            win.present_offscreen_fx,
            (fw, fh),
        );
        #[cfg(test)]
        {
            win.metal_last_compose_rect = use_present_off.then_some(copy_rect);
        }
        let seq = PresentSequence {
            offscreen: &off_tex,
            present_off: present_off.as_ref(),
            copy_rect,
            bloom: bloom_parts.as_ref().map(
                |(
                    half,
                    extract_pso,
                    cell_u,
                    extract_binds,
                    stream,
                    count,
                    pso,
                    ubuf,
                    binds,
                    sc,
                )| {
                    BloomPasses {
                        half_target: half,
                        extract_pso,
                        cell_uniform: cell_u,
                        cell_uniform_slot: extract_binds
                            .vertex_uniform
                            .expect("row 4 has a vertex uniform")
                            as usize,
                        extract_stream: &stream.0,
                        extract_offset: stream.1,
                        extract_count: *count,
                        composite: PostPass {
                            pso,
                            tex: half.obj(),
                            tex_slot: binds.fragment_textures[0] as usize,
                            sampler: &live.linear,
                            sampler_slot: binds.fragment_samplers[0] as usize,
                            uniform: ubuf,
                            uniform_slot: binds.fragment_buffers[0] as usize,
                        },
                        composite_scissor: *sc,
                    }
                },
            ),
            shimmer: shimmer_parts
                .as_ref()
                .map(|(scratch, stage, pso, ubuf, binds, region)| ShimmerPass {
                    scratch,
                    stage_rect: *stage,
                    refract: PostPass {
                        pso,
                        tex: scratch.obj(),
                        tex_slot: binds.fragment_textures[0] as usize,
                        sampler: &live.linear,
                        sampler_slot: binds.fragment_samplers[0] as usize,
                        uniform: ubuf,
                        uniform_slot: binds.fragment_buffers[0] as usize,
                    },
                    region: *region,
                }),
            band_shift: band_shift
                .zip(shift_scratch.as_ref())
                .map(|(shift, scratch)| BandShiftPass { shift, scratch }),
            apron: apron_strip.as_ref().map(|(strip, tex)| ApronStripPass {
                tex,
                dst_y: strip.dst_y as usize,
                rows: strip.rows as usize,
            }),
            tray: tray_parts.as_ref().map(|(tu, pso, binds)| TrayPass {
                pso,
                card_tex: win
                    .metal_tray_card
                    .as_ref()
                    .expect("ensured alongside the tray above")
                    .tex()
                    .obj(),
                tex_slot: binds.fragment_textures[0] as usize,
                sampler: &live.linear,
                sampler_slot: binds.fragment_samplers[0] as usize,
                uniform: tu,
                uniform_slot: binds.vertex_uniform.expect("row 17 binds at 2") as usize,
            }),
            blit: PostPass {
                pso: &blit_pso,
                tex: match present_off.as_ref() {
                    Some(copy) => copy.obj(),
                    None => off_tex.obj(),
                },
                tex_slot: Pipeline::Blit.spec().binds.fragment_textures[0] as usize,
                sampler: &live.nearest,
                sampler_slot: Pipeline::Blit.spec().binds.fragment_samplers[0] as usize,
                uniform: &bu,
                uniform_slot: Pipeline::Blit.spec().binds.fragment_buffers[0] as usize,
            },
            clear: mtl::ClearColor {
                r: f64::from((live_bg >> 16) & 0xff) / 255.0,
                g: f64::from((live_bg >> 8) & 0xff) / 255.0,
                b: f64::from(live_bg & 0xff) / 255.0,
                a: 1.0,
            },
            crown: crown_parts
                .as_ref()
                .map(|(pso, ubuf, binds, stream, count, scissor)| CrownPass {
                    pso,
                    uniform: ubuf,
                    vertex_uniform_slot: binds
                        .vertex_uniform
                        .expect("the crown rows bind a vertex uniform")
                        as usize,
                    fragment_uniform_slot: binds.fragment_buffers[0] as usize,
                    stream,
                    count: *count,
                    scissor: *scissor,
                }),
        };
        let mut cb = live.session.begin()?;
        encode_present_sequence(&mut cb, target, (dw, dh), &seq)?;
        if use_present_off {
            let mut fx: Option<[u32; 4]> = Some([0, 0, 0, 0]);
            if let Some((.., sc)) = bloom_parts.as_ref() {
                fx = union_rect_opt(fx, *sc, (fw, fh));
            }
            if let Some((.., region)) = shimmer_parts.as_ref() {
                fx = union_rect_opt(fx, Some(*region), (fw, fh));
            }
            for footprint in [band_fx, tray_fx].into_iter().flatten() {
                fx = union_rect_opt(fx, Some(footprint), (fw, fh));
            }
            win.offscreen_dirty_since_sync = Some([0, 0, 0, 0]);
            win.present_offscreen_fx = fx;
        }
        if copy_armed {
            let capture_color_space = win.capture_color_space();
            let capture_sdr_white_scale = win.sdr_white_scale();
            let dest_texel = dest_format;
            if let Some(tap) = win.video.as_mut()
                && let Err(e) = tap.metal_enqueue_copy(
                    &mut cb,
                    target,
                    dest_texel,
                    capture_color_space,
                    capture_sdr_white_scale,
                )
            {
                metal_arm_note(&format!("armed present: video tap copy refused ({e})"));
            }
            if let Some(tap) = win.presented_snapshot.as_mut()
                && let Err(e) = tap.metal_enqueue_copy(
                    &mut cb,
                    target,
                    dest_texel,
                    capture_color_space,
                    capture_sdr_white_scale,
                )
            {
                metal_arm_note(&format!("armed present: snapshot tap copy refused ({e})"));
            }
        }
        let submitted = cb.commit();
        if use_present_off {
            win.metal_present_off_pending
                .push(crate::metal::encoder::CbProbe::of(&submitted));
        }
        if let Some(tap) = win.video.as_mut() {
            tap.metal_note_submitted(crate::metal::encoder::CbProbe::of(&submitted));
        }
        if let Some(tap) = win.presented_snapshot.as_mut() {
            tap.metal_note_submitted(crate::metal::encoder::CbProbe::of(&submitted));
        }
        Ok((submitted, use_present_off))
    }

    /// Test hook.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_present_bytes_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        (dw, dh): (u32, u32),
        translucent: bool,
    ) -> Result<Vec<u8>, String> {
        self.metal_present_bytes_with_scope_for_test(win, input, (dw, dh), translucent, true)
    }

    /// [`Self::metal_present_bytes_for_test`] with the repaint scope as an axis.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_present_bytes_with_scope_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        (dw, dh): (u32, u32),
        translucent: bool,
        full_repaint: bool,
    ) -> Result<Vec<u8>, String> {
        use crate::metal::ffi as mtl;
        self.metal_present_standalone_for_test(
            win,
            input,
            (dw, dh),
            (translucent, full_repaint),
            crate::device_layer::TexelFormat::Bgra8Unorm,
            |gpu, target| {
                let live = gpu
                    .metal_arm
                    .as_mut()
                    .ok_or("the arm is not live")?
                    .live_mut();
                let row = (dw * 4) as usize;
                let rb = live
                    .layer()
                    .try_create_readback_buffer(
                        "aterm-gpu metal readback",
                        (row * dh as usize) as u64,
                    )?
                    .into_metal()
                    .into_obj();
                let mut cb = live.session.begin()?;
                cb.copy_texture_to_buffer(target, dw as usize, dh as usize, &rb, row)?;
                if cb.commit().wait_outcome() != crate::metal::loss::CbOutcome::Completed {
                    return Err("the readback copy did not complete".to_owned());
                }
                // SAFETY: shared storage sized `row * dh` whose writing command buffer has
                // completed.
                let bgra = unsafe { mtl::buffer_bytes(&rb, row * dh as usize) };
                let mut rgba = vec![0u8; bgra.len()];
                for (dst, src) in rgba
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(bgra.as_chunks::<4>().0)
                {
                    *dst = [src[2], src[1], src[0], src[3]];
                }
                Ok(rgba)
            },
        )
    }

    /// [`Self::metal_present_bytes_with_scope_for_test`]'s present onto a standalone drawable of
    /// `format` (an EDR window's `Rgba16Float` too).
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_present_standalone_for_test<T>(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        (dw, dh): (u32, u32),
        (translucent, full_repaint): (bool, bool),
        format: crate::device_layer::TexelFormat,
        read: impl FnOnce(&mut Self, &crate::metal::resources::SealedTexture) -> Result<T, String>,
    ) -> Result<T, String> {
        use crate::metal::ffi as mtl;
        let _pool = mtl::AutoreleasePool::new();
        if full_repaint {
            win.present_prev = None;
        }
        let (fw, fh) = self.encode_present_frame(win, input);
        if !self.last_frame_arm_metal {
            return Err("the frame did not draw on Metal".to_owned());
        }
        let (device, latch) = {
            let live = self
                .metal_arm
                .as_mut()
                .ok_or("the arm is not live")?
                .live_mut();
            (
                live.mint.device().clone_ref(),
                Arc::clone(live.session.latch()),
            )
        };
        let config = crate::metal::swapchain::SwapchainConfig {
            format: format.metal(),
            width: dw as usize,
            height: dh as usize,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 3,
            opaque: !translucent,
        };
        let mut surface =
            crate::metal::present::MetalWindowSurface::standalone(&device, config, latch)?;
        let frame = surface
            .acquire()
            .map_err(|r| format!("acquire refused: {r:?}"))?;
        let target = frame.render_target();
        let (submitted, _composed_present_off) = self.metal_encode_submit_b(
            win,
            input,
            false,
            None,
            None,
            None,
            None,
            (fw, fh),
            (dw, dh),
            format,
            translucent,
            false,
            true,
            &target,
        )?;
        if submitted.wait_outcome() != crate::metal::loss::CbOutcome::Completed {
            return Err("Submit B did not complete".to_owned());
        }
        read(self, &target)
    }

    /// Test probe.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_built_for_test(&self) -> Vec<crate::metal::warm::WarmItem> {
        use crate::metal::warm::WarmItem;
        let Some(cell) = self.metal_arm.as_ref() else {
            return Vec::new();
        };
        let live = cell.live_ref();
        let mut built = vec![WarmItem::Library(ShaderLibrary::Cell)];
        built.extend(live.libs.iter().map(|(lib, _)| WarmItem::Library(*lib)));
        for (i, pso) in live.psos.iter().enumerate() {
            if pso.is_some() {
                let row = pipeline_table::ALL_PIPELINES[i];
                built.push(WarmItem::pipeline(
                    row,
                    crate::metal::ffi::PixelFormat::Bgra8Unorm,
                ));
            }
        }
        for ((row, present), _) in &live.present_psos {
            let item = WarmItem::pipeline(*row, *present);
            if !built.contains(&item) {
                built.push(item);
            }
        }
        for twin in pipeline_table::FetchTwin::ALL {
            if live.fetch_psos[twin as usize].is_some() {
                built.push(WarmItem::Twin(twin));
            }
        }
        built
    }

    #[cfg(target_os = "macos")]
    fn shift_offscreen_band(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        let frac = i64::from(input.scroll_frac_px);
        if frac == 0 {
            return;
        }
        let Some(h) = win.metal_offscreen.as_ref().map(|o| o.h) else {
            return;
        };
        let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
            self.cpu.grid_top(),
            self.cell_size().1,
            input.grid_top_row,
            input.grid_bot_row,
            h as usize,
        );
        if self.metal_shift_offscreen_band_px(win, y0, y1, frac) {
            self.apron_into_offscreen_metal(win, input);
        }
    }

    fn shift_offscreen_rows(&mut self, win: &mut WindowGpu, delta_rows: i32, rows: usize) {
        #[cfg(gl_arm)]
        let on_first_party = self.last_frame_arm_gl;
        // The D3D11 arm's resident frame likewise (`present_prev.on_d3d11`);
        // the immediate context runs the copies before the encode's Load.
        #[cfg(d3d11_arm)]
        let on_first_party = self.last_frame_arm_d3d11;
        #[cfg(d3d11_arm)]
        if on_first_party && let Some(h) = win.d3d11_offscreen.as_ref().map(|o| o.h as usize) {
            let cell_h = self.cell_size().1;
            let grid_top = self.cpu.grid_top();
            let y0 = grid_top.min(h);
            let y1 = grid_top.saturating_add(rows.saturating_mul(cell_h)).min(h);
            self.d3d11_shift_offscreen_band_px(
                win,
                y0,
                y1,
                i64::from(delta_rows) * cell_h as i64,
                None,
            );
        }
        #[cfg(gl_arm)]
        if on_first_party && let Some(h) = win.gl_offscreen.as_ref().map(|o| o.h as usize) {
            let cell_h = self.cell_size().1;
            let grid_top = self.cpu.grid_top();
            let y0 = grid_top.min(h);
            let y1 = grid_top.saturating_add(rows.saturating_mul(cell_h)).min(h);
            self.gl_shift_offscreen_band_px(
                win,
                y0,
                y1,
                i64::from(delta_rows) * cell_h as i64,
                None,
            );
        }
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            let Some(h) = win.metal_offscreen.as_ref().map(|o| o.h as usize) else {
                return;
            };
            let cell_h = self.cell_size().1;
            let grid_top = self.cpu.grid_top();
            let y0 = grid_top.min(h);
            let y1 = grid_top.saturating_add(rows.saturating_mul(cell_h)).min(h);
            self.metal_pending_band_shift =
                self.metal_band_shift_plan(win, y0, y1, i64::from(delta_rows) * cell_h as i64);
            if self.metal_pending_band_shift.is_some()
                && let Some(dims) = win.metal_offscreen.as_ref().map(|o| (o.w, o.h))
            {
                note_offscreen_written(win, None, dims);
            }
        }
    }

    /// Create an on-screen presentation surface for `target` at the given pixel size, on the
    /// offscreen renderer's instance and adapter.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn create_window_surface<W: WindowTarget>(
        &mut self,
        target: W,
        width: u32,
        height: u32,
    ) -> Result<GpuSurface, String> {
        #[cfg(target_os = "macos")]
        let metal_parent = if self.metal_armed {
            crate::metal::present::parent_layer_of(&target)
        } else {
            None
        };
        #[cfg(target_os = "macos")]
        {
            let was = std::mem::replace(&mut self.metal_on_screen, true);
            self.present_pass_switched(was, true);
        }
        #[cfg(target_os = "macos")]
        {
            let format = metal_attach_format(self.hdr_glow, &target);
            let _ = (target, &metal_parent); // `target`'s layer was resolved above.
            let width = self.clamp_fb_dim(width.max(1));
            let height = self.clamp_fb_dim(height.max(1));
            let parent = metal_parent
                .as_ref()
                .ok_or("no AppKit view layer to attach the Metal swapchain under")?;
            self.attach_metal_window_surface(parent, format, width, height)
        }
        #[cfg(gl_arm)]
        {
            self.gl_create_window_surface(target, width, height, None)
        }
        // The shipped Windows build: the D3D11 arm's flip-model swap chain, on
        // the window or on a DirectComposition visual.
        #[cfg(d3d11_arm)]
        {
            self.d3d11_create_window_surface(target, width, height)
        }
    }

    /// [`Self::create_window_surface`] with `sink`, the frontend's presenter ([`FrameSink`]), which
    /// the GL arm posts the window's frames through where a window surface would stall the window
    /// system.
    #[cfg(target_os = "linux")]
    pub fn create_window_surface_with_sink<W: WindowTarget>(
        &mut self,
        target: W,
        width: u32,
        height: u32,
        sink: Box<dyn FrameSink>,
    ) -> Result<GpuSurface, String> {
        #[cfg(gl_arm)]
        {
            self.gl_create_window_surface(target, width, height, Some(sink))
        }
        #[cfg(not(gl_arm))]
        {
            drop(sink);
            self.create_window_surface(target, width, height)
        }
    }

    /// Configure the canvas surface [`crate::GpuContext::for_canvas`] made, at `width` x `height`
    /// (clamped like every framebuffer).
    #[cfg(target_arch = "wasm32")]
    pub fn configure_canvas_surface(
        &self,
        surface: crate::web::CanvasSurface,
        width: u32,
        height: u32,
    ) -> Result<GpuSurface, String> {
        if !Arc::ptr_eq(self.ctx.gl_canvas.shared(), surface.0.shared()) {
            return Err("canvas surface belongs to a different GPU context".to_owned());
        }
        let width = self.clamp_fb_dim(width.max(1));
        let height = self.clamp_fb_dim(height.max(1));
        let canvas = crate::gl::canvas::GlCanvasSurface::new(&surface.0, width, height)?;
        Ok(GpuSurface {
            acquire_id: next_surface_acquire_id(),
            neutral: NeutralSurfaceConfig {
                width,
                height,
                format: crate::device_layer::TexelFormat::Rgba8Unorm,
            },
            supports_f16: false,
            post_mult: false,
            copyable: true,
            gl: Some(canvas),
        })
    }

    #[must_use]
    #[cfg(target_os = "macos")]
    #[inline]
    const fn tap_wants_copy_src(recording: bool, snapshot: bool, held: bool) -> bool {
        recording || snapshot || held
    }

    const BACKDROP_MARGIN_ALPHA: f32 = 0.45;

    #[inline]
    fn backdrop_margin_alpha(&self) -> f32 {
        let glass = f32::from(aterm_render::vibrancy::bg_quad_alpha(
            self.cpu.background_opacity(),
        )) / 255.0;
        if self.backdrop_margins_active() {
            glass.min(Self::BACKDROP_MARGIN_ALPHA)
        } else {
            glass
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the present tuple both arms carry, plus the destination's three format/alpha facts"
    )]
    fn present_uniforms(
        &self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        source_crop: Option<PresentCrop>,
        (fw, fh): (u32, u32),
        (dw, dh): (u32, u32),
        f16_dest: bool,
        translucent: bool,
        premult: bool,
    ) -> PresentUniforms {
        let glow_present = fx_gpu::crown_glow_present(input);
        let plan = crate::format_plan::hdr_present_plan(self.hdr_glow, f16_dest, glow_present);
        let headroom = aterm_render::hdr::additive_headroom(win.edr_max);
        let run_hdr_glow = plan.glow_boost_pass && headroom > 0.0;
        let live_bg = frame_live_bg(input, self.theme.bg);
        let sdr_budget_target = aterm_render::hdr::sdr_glow_budget(
            aterm_render::hdr::packed_luma(live_bg),
            self.sdr_glow_boost,
        );
        let sdr_budget = win.advance_sdr_glow_budget(
            glow_present,
            sdr_budget_target,
            aterm_time::Instant::now(),
        );
        let run_sdr_glow = crate::format_plan::sdr_boost_pass(f16_dest, glow_present, sdr_budget);

        let mut want = present_blit_uniform(invert, overlay, source_crop, fw, fh, dw, dh, live_bg);
        want.chrome_y0 = win.chrome_y0 as f32;
        want.chrome_y1 = win.chrome_y1 as f32;
        want.encode_srgb = if self.ctx.srgb_offscreen { 0.0 } else { 1.0 };
        want.hdr = if plan.blit_linear_encode { 1.0 } else { 0.0 };
        want.sdr_white_scale = if plan.blit_linear_encode {
            win.sdr_white_scale.max(1.0)
        } else {
            1.0
        };
        if translucent {
            want = want.with_translucency(self.backdrop_margin_alpha());
            if premult {
                want = want.with_premultiplied_output();
            }
        }
        let (band_y0, band_y1) = aterm_render::scroll_translate::grid_band_px(
            self.cpu.grid_top(),
            self.cell_size().1,
            input.grid_top_row,
            input.grid_bot_row,
            fh as usize,
        );
        let band_shift = band_shift_ops(band_y0, band_y1, i64::from(input.scroll_frac_px));
        let crown_scissor = crown_band_scissor(&want, band_shift, dw, dh);
        let crown_content_off = match band_shift {
            Some(_) => [
                want.content_off[0],
                want.content_off[1] - input.scroll_frac_px as f32,
            ],
            None => want.content_off,
        };
        let crown = if run_hdr_glow {
            Some(HdrGlowUniform {
                screen: [dw as f32, dh as f32],
                content_off: crown_content_off,
                boost: aterm_render::hdr::HDR_GLOW_BOOST * win.sdr_white_scale.max(1.0),
                headroom: headroom * win.sdr_white_scale.max(1.0),
                _pad: [0.0, 0.0],
            })
        } else if run_sdr_glow {
            Some(HdrGlowUniform {
                screen: [dw as f32, dh as f32],
                content_off: crown_content_off,
                boost: 1.0,
                headroom: sdr_budget,
                _pad: [0.0, 0.0],
            })
        } else {
            None
        };
        PresentUniforms {
            live_bg,
            want,
            run_hdr_glow,
            run_sdr_glow,
            crown_scissor,
            crown,
        }
    }

    /// Resize an on-screen surface and reconfigure it.
    pub fn resize_surface(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        width: u32,
        height: u32,
    ) {
        let w = self.clamp_fb_dim(width);
        let h = self.clamp_fb_dim(height);
        {
            let _ = win;
            if surf.neutral.width == w && surf.neutral.height == h {
                return;
            }
            surf.discard_pending_acquire();
            surf.neutral.width = w;
            surf.neutral.height = h;
            #[cfg(target_arch = "wasm32")]
            if let Some(gs) = surf.gl.as_mut() {
                gs.resize(w, h);
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn metal_present_want(
        &self,
        win: &WindowGpu,
        surf: &GpuSurface,
    ) -> (bool, bool, crate::metal::swapchain::SwapchainConfig) {
        let translucent = self.cpu.background_opacity() < 1.0 && surf.post_mult;
        let copy = surf.copyable && win.wants_copy_src(aterm_time::Instant::now());
        (
            translucent,
            false,
            crate::metal::swapchain::SwapchainConfig {
                format: surf.neutral.format.metal(),
                width: surf.neutral.width as usize,
                height: surf.neutral.height as usize,
                framebuffer_only: !copy,
                display_sync: false,
                maximum_drawables: 3,
                opaque: !translucent,
            },
        )
    }

    #[cfg(target_os = "macos")]
    fn attach_metal_window_surface(
        &mut self,
        parent: &crate::metal::ffi::Obj,
        format: crate::device_layer::TexelFormat,
        width: u32,
        height: u32,
    ) -> Result<GpuSurface, String> {
        if !self.metal_arm_ready() {
            return Err(self.metal_start_error());
        }
        let want = crate::metal::swapchain::SwapchainConfig {
            format: format.metal(),
            width: width as usize,
            height: height as usize,
            framebuffer_only: true,
            display_sync: false,
            maximum_drawables: 3,
            opaque: self.cpu.background_opacity() >= 1.0,
        };
        let live = self
            .metal_arm
            .as_mut()
            .expect("metal_arm_ready above")
            .live_mut();
        let latch = Arc::clone(live.session.latch());
        let ms = crate::metal::present::MetalWindowSurface::attached(
            live.mint.device(),
            parent,
            want,
            latch,
        )?;
        Ok(GpuSurface {
            acquire_id: next_surface_acquire_id(),
            neutral: NeutralSurfaceConfig {
                width,
                height,
                format,
            },
            supports_f16: true,
            post_mult: true,
            copyable: true,
            metal: Some(ms),
        })
    }

    /// Re-pick the swapchain format for the screen the window now sits on (macOS Metal arm).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn upgrade_surface_for_screen<W: WindowTarget>(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        target: &W,
    ) -> bool {
        #[cfg(gl_arm)]
        {
            let _ = (win, target);
            debug_assert!(!surf.supports_f16, "a GL surface offers no f16 format");
            surf.supports_f16 && surf.is_hdr()
        }
        // Windows follows HDR at the present's probe (`d3d11_window`); there
        // is no screen to re-read here.
        #[cfg(d3d11_arm)]
        {
            let _ = (win, surf, target);
            false
        }
        #[cfg(target_os = "macos")]
        {
            self.upgrade_surface_on_screen(win, surf, || {
                crate::metal::present::screen_edr_potential_of(target)
            })
        }
    }

    #[cfg(target_os = "macos")]
    fn upgrade_surface_on_screen(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        read_potential: impl FnOnce() -> Option<f32>,
    ) -> bool {
        // The decision reads the screen only past the cheap gates: f16 surfaces
        // and gate-refused windows never pay the AppKit read.
        if !crate::format_plan::hdr_screen_upgrade_decision(
            self.hdr_glow,
            surf.supports_f16,
            surf.is_hdr(),
            read_potential,
        ) {
            return false;
        }
        let previous = surf.neutral.format;
        surf.neutral.format = crate::device_layer::TexelFormat::Rgba16Float;
        let (_, _, want) = self.metal_present_want(win, surf);
        let flipped = match (self.metal_arm.as_mut(), surf.metal.as_mut()) {
            (Some(cell), Some(ms)) => {
                let live = cell.live_mut();
                live.drain_pending();
                let device = live.mint.device().clone_ref();
                ms.reconcile(&device, &want).and_then(|_| {
                    if ms.config().format == want.format {
                        Ok(())
                    } else {
                        Err("a drawable acquisition is in flight, so the layer was not \
                             reconfigured in this call"
                            .to_owned())
                    }
                })
            }
            (None, _) => Err("the arm is not live".to_owned()),
            (_, None) => Err("this window has no armed swapchain".to_owned()),
        };
        if let Err(e) = flipped {
            surf.neutral.format = previous;
            metal_arm_note(&format!(
                "screen re-pick: f16 flip refused ({e}); keeping 8-bit swapchain"
            ));
            return false;
        }
        win.apply_hdr_surface_upgrade();
        true
    }

    /// Render the frame offscreen and present it by blitting that texture into `surf`'s swapchain.
    #[inline]
    pub fn present_input(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
    ) -> Result<(), SurfacePresentFailure> {
        self.present_input_with_crop(win, surf, input, invert, overlay, tray, None, 0)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the cropped public twin preserves the present tuple and records one transport translation"
    )]
    #[inline]
    pub fn present_input_cropped_with_effects_transport(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        crop: PresentCrop,
        effects_transport_shift_y: i32,
    ) -> Result<(), SurfacePresentFailure> {
        self.present_input_with_crop(
            win,
            surf,
            input,
            invert,
            overlay,
            tray,
            Some(crop),
            effects_transport_shift_y,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the shared swapchain seam carries present_input's established tuple plus the optional source crop"
    )]
    fn present_input_with_crop(
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
        self.cpu
            .checked_frame_size(input.rows, input.cols)
            .map_err(|_| SurfacePresentFailure::Validation)?;
        win.begin_surface_present();
        #[cfg(target_os = "macos")]
        self.metal_disarm_sweep(&mut surf.metal);
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            return self.metal_present_input_with_crop(
                win,
                surf,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
            );
        }
        // The D3D11 arm's window present (the shipped Windows build).
        #[cfg(d3d11_arm)]
        if self.d3d11_armed {
            return self.d3d11_present_input_with_crop(
                win,
                surf,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
            );
        }
        #[cfg(gl_arm)]
        if self.gl_armed {
            return self.gl_present_input_with_crop(
                win,
                surf,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
            );
        }
        first_party_note("present refused: the renderer is disarmed");
        Err(SurfacePresentFailure::Validation)
    }

    fn apron_strip_plan(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        h: usize,
    ) -> Option<ApronStrip> {
        if !aterm_render::scroll_translate::incoming_row_applies(input) {
            return None;
        }
        let cell_h = self.cell_size().1;
        let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
            self.cpu.grid_top(),
            cell_h,
            input.grid_top_row,
            input.grid_bot_row,
            h,
        );
        let (dst_y, rows) = aterm_render::scroll_translate::incoming_strip(
            y0,
            y1,
            i64::from(input.scroll_frac_px),
        )?;
        if !self.cpu.rasterize_apron_row(input, &mut win.apron_scratch) {
            return None;
        }
        let (w, band_rows) = (win.apron_scratch.width(), win.apron_scratch.rows());
        if w == 0 || band_rows == 0 {
            return None;
        }
        let rows = rows.min(band_rows);
        let raster_gen = win.apron_scratch.generation();
        if win.apron_rgba_gen != raster_gen || win.apron_rgba.len() != w * band_rows * 4 {
            win.apron_rgba.clear();
            win.apron_rgba.reserve(w * band_rows * 4);
            for p in win.apron_scratch.band() {
                win.apron_rgba.extend_from_slice(&[
                    (p >> 16) as u8,
                    (p >> 8) as u8,
                    *p as u8,
                    255 - (p >> 24) as u8,
                ]);
            }
            win.apron_rgba_gen = raster_gen;
        }
        Some(ApronStrip {
            dst_y: dst_y as u32,
            rows: rows as u32,
            w: w as u32,
            cell_h: band_rows as u32,
        })
    }

    #[cfg(target_os = "macos")]
    fn metal_ensure_apron_tex(
        &mut self,
        win: &mut WindowGpu,
        strip: ApronStrip,
    ) -> Result<crate::metal::resources::SealedTexture, String> {
        let (w, h) = (strip.w as usize, strip.cell_h as usize);
        let unchanged = win
            .metal_apron_tex
            .as_ref()
            .is_some_and(|t| (t.width(), t.height()) == (w, h))
            && (win.metal_apron_gen == win.apron_rgba_gen
                || win.metal_apron_bytes.as_slice() == win.apron_rgba.as_slice());
        if !unchanged {
            let _pool = crate::metal::ffi::AutoreleasePool::new();
            let cell = self.metal_arm.as_mut().ok_or("the Metal arm is not live")?;
            let live = cell.live_mut();
            let layer = live.layer();
            let tex = layer
                .try_create_texture_2d(
                    "aterm-gpu metal apron row",
                    crate::device_layer::TexelFormat::Rgba8Unorm,
                    w as u32,
                    h as u32,
                    crate::device_layer::TexUsage::COPY_STAGING,
                    None,
                )
                .map_err(|e| format!("apron row mint: {e}"))?;
            layer.upload_texture_full(&tex, &win.apron_rgba, (w * 4) as u32, w as u32, h as u32);
            win.metal_apron_tex = Some(tex.into_metal());
            win.metal_apron_bytes.clear();
            win.metal_apron_bytes.extend_from_slice(&win.apron_rgba);
        }
        win.metal_apron_gen = win.apron_rgba_gen;
        Ok(win
            .metal_apron_tex
            .as_ref()
            .expect("ensured above")
            .clone_handle())
    }

    #[cfg(target_os = "macos")]
    fn apron_into_offscreen_metal(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        use crate::device_layer::{FrameCopyTexture, FrameEncoder, SubmittedFrame};
        let Some(h) = win.metal_offscreen.as_ref().map(|o| o.h as usize) else {
            return;
        };
        let Some(strip) = self.apron_strip_plan(win, input, h) else {
            return;
        };
        let tex = match self.metal_ensure_apron_tex(win, strip) {
            Ok(t) => t,
            Err(e) => {
                metal_arm_note(&format!(
                    "armed apron strip: no texture ({e}); strip left as placeholder"
                ));
                return;
            }
        };
        let Some(off) = win.metal_offscreen.as_ref() else {
            return;
        };
        let Some(cell) = self.metal_arm.as_mut() else {
            return;
        };
        let live = cell.live_mut();
        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let copied = (|| -> Result<(), String> {
            let mut enc = FrameEncoder::metal(&live.session)?;
            enc.copy_texture_rect(
                FrameCopyTexture::Metal(&tex),
                (0, 0),
                FrameCopyTexture::Metal(&off.tex),
                (0, strip.dst_y),
                strip.w.min(off.w),
                strip.rows,
            )?;
            match enc.submit() {
                SubmittedFrame::Metal(sub) => {
                    let outcome = live.wait_inline(&sub);
                    if outcome != crate::metal::loss::CbOutcome::Completed {
                        return Err(format!("apron strip command buffer ended {outcome:?}"));
                    }
                    Ok(())
                }
            }
        })();
        if let Err(e) = copied {
            metal_arm_note(&format!(
                "armed apron strip failed ({e}); strip left as placeholder"
            ));
        }
    }

    /// Present `input` into this window's persistent `VirtualTarget`.
    #[inline]
    pub fn present_virtual(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
    ) -> bool {
        let (fw, fh) = self.frame_size(input.rows, input.cols);
        self.present_virtual_with_crop(
            win,
            input,
            invert,
            overlay,
            tray,
            None,
            0,
            (fw as u32, fh as u32),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the virtual cropped twin preserves the present tuple and records one transport translation"
    )]
    #[inline]
    pub fn present_virtual_cropped_with_effects_transport(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        crop: PresentCrop,
        effects_transport_shift_y: i32,
        destination: (u32, u32),
    ) -> bool {
        self.present_virtual_with_crop(
            win,
            input,
            invert,
            overlay,
            tray,
            Some(crop),
            effects_transport_shift_y,
            destination,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the internal shared virtual-present seam carries the existing present tuple plus source crop and destination geometry"
    )]
    fn present_virtual_with_crop(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        effects_transport_shift_y: i32,
        destination: (u32, u32),
    ) -> bool {
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            return match self.metal_present_virtual_with_crop(
                win,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
                destination,
            ) {
                Ok(()) => true,
                Err(e) => {
                    metal_arm_note(&format!(
                        "armed virtual present refused ({e}) — dropping this present"
                    ));
                    false
                }
            };
        }
        #[cfg(gl_arm)]
        if self.gl_armed && self.gl_arm_ready() {
            return match self.gl_present_virtual_with_crop(
                win,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
                destination,
            ) {
                Ok(()) => true,
                Err(e) => {
                    gl_arm::gl_arm_note(&format!(
                        "armed virtual present refused ({e}); dropping this present"
                    ));
                    false
                }
            };
        }
        // The D3D11 arm's headless present: into its resident virtual target.
        #[cfg(d3d11_arm)]
        if self.d3d11_armed && self.d3d11_arm_ready() {
            return match self.d3d11_present_virtual_with_crop(
                win,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
                destination,
            ) {
                Ok(()) => true,
                Err(e) => {
                    d3d11_arm::d3d11_arm_note(&format!(
                        "armed virtual present refused ({e}); dropping this present"
                    ));
                    false
                }
            };
        }
        first_party_note("virtual present: the renderer is disarmed; dropping this present");
        false
    }

    /// Build every effect pipeline not yet resident, off the frame path: the Metal arm's on a
    /// worker that hands each over as it lands, covering every row and twin its frames and
    /// presents can bind under the present switches, from its mint if it has not minted yet and
    /// again for a pass a switch turns on later. A no-op on the GL and D3D11 arms.
    pub fn warm_effect_pipelines(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.metal_warm_wanted = true;
            self.metal_warm_pipelines();
        }
    }

    /// Start the Metal arm's pipeline warm under the present switches, if the arm is live.
    #[cfg(target_os = "macos")]
    fn metal_warm_pipelines(&mut self) {
        let knobs = crate::metal::warm::PresentKnobs {
            bloom: self.enable_bloom,
            shimmer: self.enable_shimmer,
            hdr_glow: self.hdr_glow && self.metal_on_screen,
            sdr_glow: self.sdr_glow_boost > 0.0,
        };
        if let Some(cell) = self.metal_arm.as_mut() {
            cell.live_mut().warm_pipelines(knobs);
        }
    }

    /// Test seam: wait (a hang detector of `hang`) until the Metal arm's warms built all they owe,
    /// leaving each build for the frames to take; returns the items whose build failed.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn await_pipeline_warm_for_test(
        &mut self,
        hang: std::time::Duration,
    ) -> Vec<crate::metal::warm::WarmItem> {
        self.metal_arm
            .as_mut()
            .map_or_else(Vec::new, |cell| cell.live_mut().await_warmed(hang))
    }

    /// Test seam: what the Metal arm's pipeline warms were asked to build and it has not taken.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_warm_owed_for_test(&self) -> Vec<crate::metal::warm::WarmItem> {
        self.metal_arm
            .as_ref()
            .map_or_else(Vec::new, |cell| cell.live_ref().warm_owed.clone())
    }

    /// Test seam: the pipeline states the Metal arm built on its own thread, in order.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_built_here_for_test(&self) -> Vec<crate::metal::warm::WarmItem> {
        self.metal_arm
            .as_ref()
            .map_or_else(Vec::new, |cell| cell.live_ref().built_here.clone())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "PrimeLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "RenderWithLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    fn encode_present_frame(&mut self, win: &mut WindowGpu, input: &RenderInput) -> (u32, u32) {
        let cur_blink = self.cpu.cursor_blink_phase();
        let cur_override = self.cpu.cursor_style_override();
        let (cw, ch) = self.cpu.cell_size();
        let pad = self.cpu.pad();
        let head = self.cpu.head();
        let (w, h) = (
            self.clamp_fb_dim((input.cols * cw + 2 * pad) as u32),
            self.clamp_fb_dim((input.rows * ch + 2 * pad + head) as u32),
        );

        #[cfg(target_os = "macos")]
        let offscreen_holds_prev = if self.metal_armed {
            win.present_prev.as_ref().is_some_and(|p| p.on_metal)
                && matches!(&win.metal_offscreen, Some(o) if o.w == w && o.h == h)
        } else {
            // Disarmed: nothing resident to diff.
            false
        };
        #[cfg(gl_arm)]
        let offscreen_holds_prev = if self.gl_armed {
            win.present_prev.as_ref().is_some_and(|p| p.on_gl)
                && self.gl_arm.as_ref().is_some_and(|live| {
                    matches!(&win.gl_offscreen,
                        Some(o) if o.w == w && o.h == h && o.tex.on(&live.dev))
                })
        } else {
            false
        };
        // The D3D11 arm's twin (Windows).
        #[cfg(d3d11_arm)]
        let offscreen_holds_prev = if self.d3d11_armed {
            win.present_prev.as_ref().is_some_and(|p| p.on_d3d11)
                && self.d3d11_arm.as_ref().is_some_and(|live| {
                    matches!(&win.d3d11_offscreen,
                        Some(o) if o.w == w && o.h == h && o.tex.on(&live.dev))
                })
        } else {
            false
        };

        let mut dirty = std::mem::take(&mut self.dirty_scratch);
        let prev_present = win.present_prev.take();
        let prev_input = std::mem::take(&mut win.prev_input);
        let mut scroll_delta: Option<i32> = None;
        let scope = match (&prev_present, offscreen_holds_prev) {
            (Some(prev), true) if prev.grid_top == self.cpu.grid_top() => match compute_dirty_rows(
                &prev_input,
                input,
                prev.blink_phase,
                prev.cursor_style_override,
                cur_blink,
                cur_override,
                self.cpu.row_bands(),
                &mut dirty,
            ) {
                DirtyDecision::Rows(_) => {
                    self.cpu.mark_overshoot_rows(&prev_input, input, &mut dirty);
                    RepaintScope::Dirty(&dirty)
                }
                DirtyDecision::FullRepaint => {
                    scroll_delta = aterm_render::scroll_blit_plan(
                        &prev_input,
                        input,
                        prev.blink_phase,
                        prev.cursor_style_override,
                        cur_blink,
                        cur_override,
                        self.cpu.row_bands(),
                        &mut dirty,
                    );
                    if scroll_delta.is_some() {
                        RepaintScope::Dirty(&dirty)
                    } else {
                        RepaintScope::Full
                    }
                }
            },
            _ => RepaintScope::Full,
        };

        match &scope {
            RepaintScope::Dirty(_) => self.scissor_taken += 1,
            RepaintScope::Full => self.full_repaints += 1,
        }
        if let Some(delta) = scroll_delta {
            self.scroll_rescues += 1;
            self.shift_offscreen_rows(win, delta, input.rows);
        }
        let was_dirty = matches!(scope, RepaintScope::Dirty(_));
        let dims = self.encode_frame(win, input, &scope);
        self.dirty_scratch = dirty;
        #[cfg(target_os = "macos")]
        let dims = if self.metal_armed && !self.last_frame_arm_metal && was_dirty {
            metal_arm_note(
                "armed present: mid-frame degrade under a Dirty scope — re-encoded \
                 FULL for coherence",
            );
            self.full_repaints += 1;
            self.encode_frame(win, input, &RepaintScope::Full)
        } else {
            dims
        };
        #[cfg(gl_arm)]
        let dims = if self.gl_armed && !self.last_frame_arm_gl && was_dirty {
            gl_arm::gl_arm_note(
                "armed present: mid-frame degrade under a Dirty scope; re-encoded FULL for \
                 coherence",
            );
            self.full_repaints += 1;
            self.encode_frame(win, input, &RepaintScope::Full)
        } else {
            dims
        };
        // The D3D11 twin.
        #[cfg(d3d11_arm)]
        let dims = if self.d3d11_armed && !self.last_frame_arm_d3d11 && was_dirty {
            d3d11_arm::d3d11_arm_note(
                "armed present: mid-frame degrade under a Dirty scope; re-encoded FULL for \
                 coherence",
            );
            self.full_repaints += 1;
            self.encode_frame(win, input, &RepaintScope::Full)
        } else {
            dims
        };
        let dims = if was_dirty && self.overshoot_surprise {
            self.full_repaints += 1;
            self.encode_frame(win, input, &RepaintScope::Full)
        } else {
            dims
        };

        let mut prev_input = prev_input;
        aterm_render::clone_input_stamped(&mut prev_input, input);
        win.prev_input = prev_input;
        win.advance_resident_input_epoch();
        win.present_prev = Some(PresentPrev {
            blink_phase: cur_blink,
            cursor_style_override: cur_override,
            grid_top: self.cpu.grid_top(),
            #[cfg(target_os = "macos")]
            on_metal: self.last_frame_arm_metal,
            #[cfg(gl_arm)]
            on_gl: self.last_frame_arm_gl,
            #[cfg(d3d11_arm)]
            on_d3d11: self.last_frame_arm_d3d11,
        });
        dims
    }

    /// The frame's bloom stream: `(count, bbox, extract_first)`, the extract
    /// drawing `count` lights from `extract_first` and the crown `count` from
    /// the first. A glow stream with ramps blooms as its item list where the
    /// arm reads one (`items`; [`aterm_render::bloom::items_into`],
    /// [`Self::bloom_items_live`]): its readers widen the extract's lights
    /// themselves, so both draw every instance from the first. Any other as
    /// its glow instances, the extract's widened copies after the crown's.
    fn build_bloom_glow(
        &mut self,
        input: &RenderInput,
        items: bool,
    ) -> (u32, Option<[u32; 4]>, u32) {
        let f = self.fx_frame_for(input);
        self.bloom_items_live = items && !input.cursor_glow_add_ramps.is_empty();
        if self.bloom_items_live {
            let extra = self.fx.bloom_layers(&f, input).unwrap_or(&[]);
            let bbox = aterm_render::bloom::items_into(
                &input.cursor_glow_add,
                &input.cursor_glow_add_ramps,
                extra,
                input.rows,
                &mut self.bloom_items,
            );
            return (self.bloom_items.instances(), bbox, 0);
        }
        let mut quads = std::mem::take(&mut self.bloom_quad_scratch);
        let (bbox, extract_first) = match self.fx.bloom_source(&f, input) {
            Some(src) => aterm_render::bloom::source_quads_into(src, input.rows, &mut quads),
            None => aterm_render::bloom::source_stream_into(
                &input.cursor_glow_add,
                &input.cursor_glow_add_ramps,
                input.rows,
                &mut quads,
            ),
        };
        let mut v = std::mem::take(&mut self.bloom_glow_scratch);
        v.clear();
        v.extend(
            quads
                .iter()
                .map(|q| GlowInstance::of([q.x, q.y, q.w, q.h], q)),
        );
        let n = if extract_first > 0 {
            extract_first
        } else {
            v.len()
        } as u32;
        self.bloom_glow_scratch = v;
        self.bloom_quad_scratch = quads;
        (n, bbox, extract_first as u32)
    }

    fn shimmer_phase(&self) -> f32 {
        self.shimmer_phase_pin
            .unwrap_or_else(|| self.shimmer_epoch.elapsed().as_secs_f32() % SHIMMER_PHASE_WRAP_S)
    }

    /// The foot (frame px) of the lowest row holding a glyph in columns `x0..x1` that reaches into
    /// the band `y0..y1`, or `0` when every such row is blank. A row in a smooth scroll reaches its
    /// offset further each way.
    fn lettered_foot(
        &self,
        input: &RenderInput,
        (x0, x1): (u32, u32),
        (y0, y1): (u32, u32),
        (cw, ch): (u32, u32),
    ) -> u32 {
        if cw == 0 || ch == 0 {
            return 0;
        }
        let top = self.cpu.grid_top() as i64;
        let pad = self.cpu.pad() as u32;
        let slack = i64::from(input.scroll_frac_px).abs();
        let c0 = (x0.saturating_sub(pad) / cw) as usize;
        let c1 = (x1.saturating_sub(pad)).div_ceil(cw) as usize;
        for (r, line) in input.cells.iter().enumerate().take(input.rows).rev() {
            let r_top = top + r as i64 * i64::from(ch) - slack;
            let r_foot = r_top + i64::from(ch) + 2 * slack;
            if r_foot <= i64::from(y0) || r_top >= i64::from(y1) {
                continue;
            }
            let lettered = line
                .get(c0.min(line.len())..c1.min(line.len()))
                .is_some_and(|cells| cells.iter().any(|c| c.ch != ' ' && c.ch != '\0'));
            if lettered {
                return u32::try_from(r_foot.max(0)).unwrap_or(u32::MAX);
            }
        }
        0
    }

    fn shimmer_region(&self, input: &RenderInput, w: u32, h: u32) -> Option<ShimmerRegion> {
        let (cw, ch) = self.cpu.cell_size();
        let (cw, ch) = (cw as u32, ch as u32);
        let rows = input.rows;
        let quad_brightness = |q: &aterm_render::GlowQuad| -> u32 {
            if (q.row as usize) >= rows {
                return 0;
            }
            ((q.color >> 16) & 0xff)
                .max((q.color >> 8) & 0xff)
                .max(q.color & 0xff)
        };
        // The additive stream's lights, each lit ramp row the quad it stands for: the hot
        // box and the heat bands are a min, a max and a max over them, so their order is free.
        let ramps = &input.cursor_glow_add_ramps;
        let lights = || {
            input
                .cursor_glow_add
                .iter()
                .copied()
                .chain(ramps.items.iter().flat_map(|r| r.quads(&ramps.ends)))
        };
        let (mut hx0, mut hx1, mut hot_top) = (u32::MAX, 0u32, u32::MAX);
        for q in lights() {
            if quad_brightness(&q) == 0 {
                continue;
            }
            let qx0 = u32::from(q.x);
            hx0 = hx0.min(qx0);
            hx1 = hx1.max(qx0 + u32::from(q.w));
            hot_top = hot_top.min(u32::from(q.y));
        }
        if hot_top == u32::MAX || hx1 <= hx0 {
            return None;
        }
        let rise = SHIMMER_RISE_CELLS * ch as f32;
        let x0 = hx0.saturating_sub(cw).min(w);
        let x1 = (hx1.saturating_add(cw)).min(w);
        let y1 = hot_top.min(h);
        let y0 = hot_top.saturating_sub(rise.ceil() as u32).min(y1);
        // The haze never bends a letter: it rises through the blank rows over the hot band and
        // fades out at the foot of the first lettered row.
        let foot = self.lettered_foot(input, (x0, x1), (y0, y1), (cw, ch));
        let (y0, rise) = if foot > y0 {
            (foot.min(y1), y1.saturating_sub(foot) as f32)
        } else {
            (y0, rise)
        };
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let fx = input.fx_clip.map_or((0, 0, w, h), |(cx0, cy0, cx1, cy1)| {
            (
                u32::from(cx0),
                u32::from(cy0),
                u32::from(cx1),
                u32::from(cy1),
            )
        });
        let (rx0, ry0, rx1, ry1) = (x0.max(fx.0), y0.max(fx.1), x1.min(fx.2), y1.min(fx.3));
        if rx0 >= rx1 || ry0 >= ry1 {
            return None;
        }
        let band_w = (x1 - x0) as f32 / SHIMMER_BANDS as f32;
        let mut heat = [0f32; SHIMMER_BANDS];
        for q in lights() {
            let b = quad_brightness(&q);
            if b == 0 {
                continue;
            }
            let strength = (b as f32 / 255.0 * SHIMMER_HEAT_GAIN).min(1.0);
            let qx0 = u32::from(q.x) as f32 - x0 as f32;
            let qx1 = qx0 + f32::from(q.w);
            let lo = ((qx0 / band_w).floor().max(0.0) as usize).min(SHIMMER_BANDS);
            let hi = ((qx1 / band_w).ceil().max(0.0) as usize).min(SHIMMER_BANDS);
            for slot in &mut heat[lo..hi] {
                *slot = slot.max(strength);
            }
        }
        for _ in 0..2 {
            let prev = heat;
            for (i, slot) in heat.iter_mut().enumerate() {
                let l = prev[i.saturating_sub(1)];
                let r = prev[(i + 1).min(SHIMMER_BANDS - 1)];
                *slot = 0.25 * l + 0.5 * prev[i] + 0.25 * r;
            }
        }
        Some(ShimmerRegion {
            x0: rx0,
            y0: ry0,
            x1: rx1,
            y1: ry1,
            hot_top: hot_top as f32,
            rise,
            band_x0: x0 as f32,
            band_w,
            heat,
        })
    }

    /// Test helper.
    #[doc(hidden)]
    pub fn present_input_readback(&mut self, win: &mut WindowGpu, input: &RenderInput) -> Frame {
        let (w, h) = self.encode_present_frame(win, input);
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            let effect_frame =
                (self.enable_bloom && fx_gpu::bloom_source_live(input)) || self.shimmer_live(input);
            let read = if effect_frame {
                self.metal_compose_present_readback(win, input, w, h)
            } else {
                self.metal_read_back_offscreen(win, w, h)
            };
            return read.unwrap_or_else(|e| {
                crate::stderr_line!(
                    "aterm-gpu: armed present readback failed ({e}); returning \
                     blank frame"
                );
                Frame {
                    width: w as usize,
                    height: h as usize,
                    pixels: vec![0; (w as usize) * (h as usize)],
                }
            });
        }
        // The D3D11 arm (see `d3d11_present_readback`).
        #[cfg(d3d11_arm)]
        if self.last_frame_arm_d3d11 {
            return self
                .d3d11_present_readback(win, input, (w, h))
                .unwrap_or_else(|e| {
                    crate::stderr_line!(
                        "aterm-gpu: armed D3D11 present readback failed ({e}); returning blank \
                         frame"
                    );
                    Frame {
                        width: w as usize,
                        height: h as usize,
                        pixels: vec![0; (w as usize) * (h as usize)],
                    }
                });
        }
        #[cfg(gl_arm)]
        if self.last_frame_arm_gl {
            return self
                .gl_present_readback(win, input, (w, h))
                .unwrap_or_else(|e| {
                    crate::stderr_line!(
                        "aterm-gpu: armed GL present readback failed ({e}); returning blank frame"
                    );
                    Frame {
                        width: w as usize,
                        height: h as usize,
                        pixels: vec![0; (w as usize) * (h as usize)],
                    }
                });
        }
        // Disarmed: no arm drew the frame, so it reads back blank.
        Frame {
            width: w as usize,
            height: h as usize,
            pixels: vec![0; (w as usize) * (h as usize)],
        }
    }

    /// Test helper.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn video_begin_metal_standin_for_test(
        &self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
        opts: crate::video_tap::CaptureOpts,
    ) -> Result<(), String> {
        let cell = self
            .metal_arm
            .as_ref()
            .ok_or("arm not minted — render an armed frame first")?;
        let tap = crate::video_tap::VideoTap::new_metal(
            &cell.live_ref().mint,
            w,
            h,
            crate::device_layer::TexelFormat::Bgra8Unorm,
            win.capture_color_space,
            win.sdr_white_scale(),
            opts,
        )?;
        win.video = Some(tap);
        Ok(())
    }

    /// Test helper: the armed one-shot stand-in.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn presented_snapshot_begin_metal_standin_for_test(
        &self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
    ) -> Result<(), String> {
        let cell = self
            .metal_arm
            .as_ref()
            .ok_or("arm not minted — render an armed frame first")?;
        win.presented_snapshot = Some(crate::video_tap::PresentedFrameTap::new_metal(
            &cell.live_ref().mint,
            w,
            h,
            crate::device_layer::TexelFormat::Bgra8Unorm,
            win.capture_color_space,
            win.sdr_white_scale(),
        )?);
        Ok(())
    }

    /// Test helper: reset one window's SDR glow-boost envelope.
    #[doc(hidden)]
    pub const fn reset_glow_ease_for_test(&self, win: &mut WindowGpu) {
        win.sdr_glow_level = 0.0;
        win.sdr_glow_level_at = None;
    }

    /// Test helper for the HDR clamp laws.
    #[doc(hidden)]
    #[cfg(d3d11_arm)]
    pub fn present_hdr_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        boost_pass: bool,
    ) -> (Vec<f32>, u32, u32) {
        self.present_hdr_sized_for_test(win, input, boost_pass, 0, 0)
    }

    /// Test helper; an armed D3D11 renderer runs its real present into an f16 destination
    /// (`d3d11_present_hdr_for_test`).
    #[doc(hidden)]
    #[cfg(d3d11_arm)]
    pub fn present_hdr_sized_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        boost_pass: bool,
        extra_w: u32,
        extra_h: u32,
    ) -> (Vec<f32>, u32, u32) {
        #[cfg(d3d11_arm)]
        if self.d3d11_armed && self.d3d11_arm_ready() {
            return self
                .d3d11_present_hdr_for_test(win, input, boost_pass, (extra_w, extra_h))
                .unwrap_or_else(|e| panic!("the D3D11 EDR present: {e}"));
        }
        panic!("the EDR present seam: {}", self.d3d11_start_error())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "PrimeLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "RenderWithLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    pub fn render_input_cached<'a>(
        &mut self,
        win: &'a mut WindowGpu,
        input: &RenderInput,
    ) -> RenderView<'a> {
        let cur_blink = self.cpu.cursor_blink_phase();
        let cur_override = self.cpu.cursor_style_override();

        let (raw_w, raw_h) = self
            .cpu
            .checked_frame_size(input.rows, input.cols)
            .expect("renderable framebuffer");
        let (w, h) = (
            self.clamp_fb_dim(raw_w as u32) as usize,
            self.clamp_fb_dim(raw_h as u32) as usize,
        );

        let hit = match &win.gate_cache {
            Some(c) => {
                c.frame.width == w
                    && c.frame.height == h
                    && c.grid_top == self.cpu.grid_top()
                    && self.fx.prefetch_admitted(&input.fx_prefetch)
                    && is_unchanged_frame(
                        &c.input,
                        c.blink_phase,
                        c.cursor_style_override,
                        input,
                        cur_blink,
                        cur_override,
                        self.cpu.row_bands(),
                    )
            }
            None => false,
        };

        if hit {
            self.gate_hits += 1;
            let frame = &win.gate_cache.as_ref().expect("hit implies Some").frame;
            return RenderView::Borrowed {
                width: frame.width,
                height: frame.height,
                pixels: &frame.pixels,
            };
        }

        self.gate_misses += 1;
        let mut damage_input = win
            .gate_cache
            .take()
            .map_or_else(RenderInput::empty, |c| c.input);
        aterm_render::clone_damage_cache_from_stamped(&mut damage_input, input);
        let frame = self.render_input(win, input, None);
        win.gate_cache = Some(GpuGateCache {
            input: damage_input,
            blink_phase: cur_blink,
            cursor_style_override: cur_override,
            grid_top: self.cpu.grid_top(),
            frame,
        });
        let frame = &win.gate_cache.as_ref().expect("just stored").frame;
        RenderView::Borrowed {
            width: frame.width,
            height: frame.height,
            pixels: &frame.pixels,
        }
    }

    fn ensure_sprite_texture(
        &mut self,
        slot: &mut Option<Arc<SpriteTex>>,
        source: Option<&Arc<SceneAtlas>>,
    ) {
        let Some(source) = source else {
            *slot = None;
            return;
        };
        let src = source.as_ref();
        let need = (src.width as usize)
            .saturating_mul(src.height as usize)
            .saturating_mul(4);
        if src.width == 0 || src.height == 0 || src.rgba.len() < need {
            *slot = None;
            return;
        }
        if slot.as_ref().is_some_and(|s| s.matches(source)) {
            return;
        }
        if let Some(texture) = self.sprite_textures.find(source) {
            *slot = Some(texture);
            return;
        }
        let texture = Arc::new(SpriteTex {
            #[cfg(target_os = "macos")]
            metal_atlas: std::sync::OnceLock::new(),
            #[cfg(gl_arm)]
            gl_atlas: std::sync::Mutex::new(None),
            #[cfg(d3d11_arm)]
            d3d11_atlas: std::sync::Mutex::new(None),
            w: src.width,
            h: src.height,
            src: Arc::clone(source),
        });
        self.sprite_textures.insert(&texture);
        *slot = Some(texture);
    }

    #[inline]
    fn ensure_wallpaper_tex(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        self.ensure_sprite_texture(&mut win.wallpaper_tex, input.wallpaper.as_ref());
    }

    fn record_frame_instance_totals(
        &mut self,
        passes: u32,
        build_ns: u64,
        stage_ns: u64,
        unorm: PlanUnorm,
        fx_staged: bool,
    ) {
        let mut shape = FrameShape {
            passes,
            build_ns,
            stage_ns,
            ablation: unorm.ablation,
            unorm_draws: unorm.draws,
            unorm_instances: unorm.instances,
            route: unorm.route,
            under_store: unorm.under_store,
            fetch_probe: self.fetch_probe(),
            ..FrameShape::default()
        };
        for id in StreamId::ALL {
            if id.is_fx() && !fx_staged {
                continue;
            }
            let (bytes, count) = self.inst.stream_bytes(id);
            shape.instances[id as usize] = count;
            shape.instance_bytes = shape.instance_bytes.saturating_add(bytes.len() as u64);
        }
        let fx_instances = if fx_staged {
            self.inst.fx_instances()
        } else {
            0
        };
        shape.fx_instances = u32::try_from(fx_instances).unwrap_or(u32::MAX);
        shape.fx_upload_bytes = (fx_instances * std::mem::size_of::<fx_gpu::FxInstance>()) as u64
            + if fx_instances > 0 {
                (self.fx.palette.len() * 4) as u64
            } else {
                0
            }
            + self.fx.atlas_bytes;
        shape.fx_atlas_bytes = self.fx.atlas_bytes;
        shape.fx_refused = self.fx.refused;
        (shape.fx_reader_waits, shape.fx_reader_wait_ns) = self.fx.reader_wait;
        self.frame_shape = shape;
        self.last_bg_instances = self.inst.bg.len();
        self.last_instances = self.inst.bg.len()
            + self.inst.image_below_bg.len()
            + self.inst.image_bg_cover.len()
            + self.inst.image_under.len()
            + self.inst.image.len()
            + self.inst.glyph.len()
            + self.inst.color.len()
            + self.inst.cursor.len()
            + self.inst.deco.len()
            + self.inst.curl.len()
            + self.inst.trail.len()
            + self.inst.glow_add.len()
            + self.inst.nova_add.len()
            + self.inst.cursor_block.len()
            + self.inst.cursor_glyph.len()
            + self.inst.cursor_color.len();
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "gpu_encode",
            action = "Append",
            project = "aterm_gpu::renderer::project_bg_encode"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "gpu_encode",
            action = "Encode",
            project = "aterm_gpu::renderer::project_bg_encode"
        )
    )]
    fn encode_frame(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        scope: &RepaintScope,
    ) -> (u32, u32) {
        let build_started = aterm_time::Instant::now();
        self.overshoot_surprise = false;
        #[cfg(target_os = "macos")]
        let pending_band_shift = self.metal_pending_band_shift.take();
        #[cfg(gl_arm)]
        {
            self.last_frame_arm_gl = false;
        }
        #[cfg(d3d11_arm)]
        {
            self.last_frame_arm_d3d11 = false;
        }
        self.cpu.evict_caches_if_large();
        let (rows, cols) = (input.rows, input.cols);
        let (cw, ch) = self.cpu.cell_size();
        let baseline = self.cpu.baseline();
        let chrome_room = self.cpu.chrome_room_for(input);
        let ring_floor = self.cpu.chrome_ring_floor();
        let pad = self.cpu.pad();
        let grid_top = self.cpu.grid_top();
        let (fb_w, fb_h) = self.cpu.frame_size(rows, cols);
        let w = self.clamp_fb_dim(fb_w as u32);
        let h = self.clamp_fb_dim(fb_h as u32);
        let chrome = self.cpu.chrome_extent_px(h as usize);
        win.chrome_y0 = u32::try_from(chrome.start).unwrap_or(h);
        win.chrome_y1 = u32::try_from(chrome.end).unwrap_or(h);
        let vis_rows = (h as usize / ch).saturating_add(2).min(rows);
        let clip_cols = (w as usize) < fb_w;
        let place = |r: usize, c: usize, row_uniform: bool, rcw: usize| {
            if row_uniform {
                (c * rcw, rcw, None)
            } else {
                let (x, cw_run, x0, x1) = aterm_render::cell_x_run(input, r, c, cw);
                (x, cw_run, Some((pad + x0, pad + x1)))
            }
        };
        let off_right = |cx: usize, ccw: usize| clip_cols && pad + cx >= w as usize + ccw;
        let run_scale = |r: usize, c: usize, (rx0, rx1): (usize, usize)| {
            let (run_size, _, _) = input.line_size_run_at(r, c);
            let (mut s, a) = aterm_render::chrome_lifted(
                input,
                r,
                grid_top + r * ch,
                ch,
                chrome_room,
                aterm_render::row_scale(run_size, grid_top + r * ch, ch, r + 1 == input.rows),
            );
            s.clip_x0 = s.clip_x0.max(rx0 as i32);
            s.clip_x1 = s.clip_x1.min(rx1 as i32);
            (s, a)
        };

        self.ensure_deco_atlas(cw, ch);
        self.ensure_wallpaper_tex(win, input);

        let row_active = |r: usize| -> bool {
            match scope {
                RepaintScope::Full => true,
                RepaintScope::Dirty(dirty) => dirty.get(r).copied().unwrap_or(false),
            }
        };
        let dirty_band = match scope {
            RepaintScope::Dirty(dirty) => {
                let mut first = None;
                let mut last = 0usize;
                for (r, &d) in dirty.iter().enumerate() {
                    if d {
                        first.get_or_insert(r);
                        last = r;
                    }
                }
                first.map(|f| (f, last))
            }
            RepaintScope::Full => None,
        };

        let wallpaper_on = input.wallpaper.is_some()
            && win
                .wallpaper_tex
                .as_ref()
                .is_some_and(|s| s.w == w && s.h == h);
        let wallpaper_quad = |x: f32, y: f32, rw: f32, rh: f32| GlyphInstance {
            rect: [x, y, rw, rh],
            uv: [x / w as f32, y / h as f32, rw / w as f32, rh / h as f32],
            color: [255, 255, 255, 255],
            bg: [0, 0, 0, 0],
        };
        let rendered: &[Vec<aterm_core::terminal::RenderCell>] = &input.cells;

        let (cr, cc) = (input.cursor_row, input.cursor_col);
        let cursor_in = cr < rows && cc < cols;
        let style = aterm_render::effective_cursor_style(input, self.cpu.cursor_style_override());
        let cursor_drawn = cursor_in
            && input.cursor_visible
            && aterm_render::cursor_shown(style, self.cpu.cursor_blink_phase());
        let block_cursor =
            cursor_drawn && matches!(style, CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock);

        self.inst.clear();

        if wallpaper_on && matches!(scope, RepaintScope::Full) {
            self.inst
                .wallpaper
                .push(wallpaper_quad(0.0, 0.0, w as f32, h as f32));
        }

        let mut glyph_win = std::mem::take(&mut self.glyph_win_scratch);
        glyph_windows(
            &mut self.cpu,
            input,
            scope,
            grid_top,
            ch,
            vis_rows,
            &mut glyph_win,
        );
        let mut row_plans = std::mem::take(&mut self.row_plans);
        row_plans.resize_with(rendered.len(), Vec::new);
        let mut break_cols = std::mem::take(&mut self.ligature_break_scratch);
        for (r, plan) in row_plans.iter_mut().enumerate().take(vis_rows) {
            if glyph_win[r].is_none() {
                continue;
            }
            self.cpu
                .ligature_break_cols_for_row_into(input, r, &mut break_cols);
            self.cpu.row_glyph_plan(input, r, &break_cols, plan);
        }
        self.ligature_break_scratch = break_cols;

        let (cur_x, cur_cw, cur_run) = if cr < rows {
            if aterm_render::row_is_uniform(input, cr) {
                let rcw = aterm_render::row_cell_w(input.line_sizes[cr], cw);
                (cc * rcw, rcw, None)
            } else {
                let (x, rcw, x0, x1) = aterm_render::cell_x_run(input, cr, cc, cw);
                (x, rcw, Some((pad + x0, pad + x1)))
            }
        } else {
            (cc * cw, cw, None)
        };
        let cursor_wide_lead = block_cursor
            && rendered
                .get(cr)
                .and_then(|row| row.get(cc + 1))
                .is_some_and(|n| n.wide);
        let cur_block_w = if cursor_wide_lead { 2 * cur_cw } else { cur_cw };
        let cursor_cell = rendered.get(cr).and_then(|row| row.get(cc));
        let cut_out_on = block_cursor
            && cursor_cell
                .is_some_and(|cell| !cell.wide && cell.ch != ' ' && !cell.ch.is_control());
        let (cut_lo, cut_hi) = if cut_out_on {
            aterm_render::cursor_cutout_cols(row_plans.get(cr).map_or(&[][..], Vec::as_slice), cc)
        } else {
            (1, 0)
        };
        let (cut_x0, cut_x1) = if cut_out_on {
            let x0 = (pad + cur_x) as i32;
            let (mut lo, mut hi) = (x0, x0 + cur_block_w as i32);
            if let Some((rx0, rx1)) = cur_run {
                lo = lo.max(rx0 as i32);
                hi = hi.min(rx1 as i32);
            }
            (lo, hi)
        } else {
            (0, 0)
        };
        let cutout_color = cursor_cell.map_or([0, 0, 0, 0], |cell| rgb4(cell.bg));

        let mut keys = std::mem::take(&mut self.inst.keys);
        let mut key_order = std::mem::take(&mut self.inst.key_order);
        let mut key_scratch = std::mem::take(&mut self.key_scratch);
        key_scratch.resize(vis_rows * cols, None);
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            // A Dirty repaint needs keys only for the rows it draws glyphs on;
            // resident atlases hold the rest.
            if glyph_win[r].is_none() {
                continue;
            }
            let plan = &row_plans[r];
            let row_raster = input.chrome_raster(r);
            let rcw = aterm_render::row_cell_w(
                input
                    .line_sizes
                    .get(r)
                    .copied()
                    .unwrap_or(aterm_core::grid::LineSize::SingleWidth),
                cw,
            );
            let row_uniform = aterm_render::row_is_uniform(input, r);
            let row_has_images = input.images.get(r).is_some_and(|v| !v.is_empty());
            let row_has_clusters = input.clusters.get(r).is_some_and(|v| !v.is_empty());
            let row_has_combining = input.combining.get(r).is_some_and(|v| !v.is_empty());
            for (c, cell) in cells.iter().take(cols).enumerate() {
                let (cell_x, ccw, _) = place(r, c, row_uniform, rcw);
                if off_right(cell_x, ccw) {
                    if row_uniform {
                        break; // off the right edge of the clamped framebuffer — see vis_rows
                    }
                    continue; // a mixed row's runs are not monotone in x
                }
                let slot = &mut key_scratch[r * cols + c];
                if !Self::drawable(cell) || (row_has_images && input.image_hides_glyph_at(r, c)) {
                    *slot = None;
                    continue;
                }
                let cluster = if row_has_clusters {
                    input.cluster_at(r, c)
                } else {
                    None
                };
                let key = match plan
                    .get(c)
                    .copied()
                    .unwrap_or(aterm_render::ColumnGlyph::PerCell)
                {
                    aterm_render::ColumnGlyph::Ligated(gid) => {
                        self.cpu.ligature_key(gid, aterm_render::cell_style(cell))
                    }
                    aterm_render::ColumnGlyph::LigatedSlice { gid, k, .. } => self
                        .cpu
                        .ligature_slice_key(gid, k, aterm_render::cell_style(cell)),
                    aterm_render::ColumnGlyph::PerCell => self.cell_key(
                        cluster,
                        cell,
                        aterm_render::materialized_cell_span(cells, c),
                    ),
                };
                let key = self
                    .cpu
                    .chrome_icon_key(row_raster, cells, c)
                    .unwrap_or(key);
                let key = aterm_render::shade_phase_key(key, pad + cell_x, grid_top + r * ch);
                *slot = Some(key);
                if keys.insert(key) {
                    key_order.push(key);
                }
                if row_has_combining
                    && cluster.is_none()
                    && let Some(marks) = input.combining_at(r, c)
                {
                    for &m in marks {
                        let mk = self.cpu.combining_mark_key(m, cell);
                        if keys.insert(mk) {
                            key_order.push(mk);
                        }
                    }
                }
            }
        }
        self.ensure_atlases(&mut key_order);
        self.inst.keys = keys;
        self.inst.key_order = key_order;
        #[cfg(target_os = "macos")]
        let armed = {
            let was_armed = self.metal_armed;
            let armed = self.metal_arm_ready();
            if was_armed && !armed {
                crate::stderr_line!("aterm-gpu: {}", self.metal_start_error());
            }
            armed
        };
        #[cfg(gl_arm)]
        let gl_armed = self.gl_arm_ready();
        // The D3D11 arm likewise (Windows).
        #[cfg(d3d11_arm)]
        let d3d11_armed = self.d3d11_arm_ready();
        self.build_image_plane(win, input);
        let mono_res = self
            .mono_res
            .as_ref()
            .expect("ensure_atlases sets mono_res");
        let color_res = self
            .color_res
            .as_ref()
            .expect("ensure_atlases sets color_res");
        let atlas = &mono_res.atlas;
        let color_atlas = &color_res.atlas;
        let (aw, ah) = (atlas.width as f32, mono_res.tex_h as f32);
        let (caw, cah) = (color_atlas.width as f32, color_res.tex_h as f32);

        let backdrop_margins_active = self.backdrop_margins_active();
        let backdrop_margin_alpha_byte = (self.backdrop_margin_alpha() * 255.0).round() as u8;
        let bg_inst = &mut self.inst.bg;
        let glyph_inst = &mut self.inst.glyph;
        let glyph_halo_inst = &mut self.inst.glyph_halo;
        let fire_present = !input.fire_patch.is_empty();
        let halo_rgb = {
            let rgb = aterm_render::HALO_IN_FIRE_RGB;
            [
                ((rgb >> 16) & 0xff) as u8,
                ((rgb >> 8) & 0xff) as u8,
                (rgb & 0xff) as u8,
            ]
        };
        let color_inst = &mut self.inst.color;
        let cursor_glyph_inst = &mut self.inst.cursor_glyph;
        let cursor_color_inst = &mut self.inst.cursor_color;
        let frame_bg = frame_live_bg(input, self.theme.bg);
        let frame_cursor = if input.cursor_color == aterm_core::render::COLOR_UNSET {
            self.theme.cursor
        } else {
            input.cursor_color
        };
        let bg_opacity = self.cpu.background_opacity();
        let bg_alpha: u8 = aterm_render::vibrancy::bg_quad_alpha(bg_opacity);
        let cursor_cov: u8 = (self.cpu.cursor_opacity() * 255.0).round() as u8;
        let cursor_opaque = cursor_cov == 255;
        let theme_bg = {
            let mut c = rgb4_u32(frame_bg);
            c[3] = bg_alpha;
            c
        };
        let backdrop_margins_on = backdrop_margins_active && !wallpaper_on;
        let margin_bg = {
            let mut c = rgb4_u32(frame_bg);
            c[3] = backdrop_margin_alpha_byte;
            c
        };
        let strip_bg = if backdrop_margins_on {
            margin_bg
        } else {
            theme_bg
        };
        let selection_palette = self.cpu.selection_palette(input);
        let min_contrast = self.cpu.effective_minimum_contrast();
        let may_have_sel = input.any_selection();
        let cell_selection =
            |r: usize,
             c: usize,
             cells: &[aterm_core::terminal::RenderCell],
             cell: &aterm_core::terminal::RenderCell| {
                let hit = if may_have_sel {
                    input.selection_hit(r, c, cells.get(c + 1).is_some_and(|n| n.wide), cell.wide)
                } else {
                    None
                };
                let colors = selection_palette.at_hit(hit);
                let bg_under = if hit.is_some() {
                    rgb4_u32(colors.bg)
                } else {
                    rgb4(cell.bg)
                };
                (hit.is_some(), colors, bg_under)
            };
        let base_cursor = match input.cursor_fill_override {
            Some(fill) => fill,
            None => frame_cursor,
        };
        let cursor_default_bg = aterm_render::resolved_default_bg_at(input, cr, cc, self.theme.bg);
        let cursor_fill = aterm_render::floor_cursor_fill(
            base_cursor,
            cursor_cell.map_or(cursor_default_bg, |cell| aterm_render::rgb_to_u32(cell.bg)),
            min_contrast,
        );
        if matches!(scope, RepaintScope::Dirty(_))
            && let Some((f, last)) = dirty_band
        {
            if f == 0 && grid_top > 0 {
                if wallpaper_on {
                    self.inst
                        .wallpaper
                        .push(wallpaper_quad(0.0, 0.0, w as f32, grid_top as f32));
                } else {
                    bg_inst.push(BgInstance {
                        rect: [0, 0, w as u16, grid_top as u16],
                        color: strip_bg,
                    });
                }
            }
            let grid_bot = ((grid_top + rows * ch) as u32).min(h);
            if last + 1 >= rows && grid_bot < h {
                if wallpaper_on {
                    self.inst.wallpaper.push(wallpaper_quad(
                        0.0,
                        grid_bot as f32,
                        w as f32,
                        (h - grid_bot) as f32,
                    ));
                } else {
                    bg_inst.push(BgInstance {
                        rect: [0, grid_bot as u16, w as u16, (h - grid_bot) as u16],
                        color: strip_bg,
                    });
                }
            }
        }
        if backdrop_margins_on && matches!(scope, RepaintScope::Full) {
            let grid_bot = ((grid_top + rows * ch) as u32).min(h);
            if grid_top > 0 {
                bg_inst.push(BgInstance {
                    rect: [0, 0, w as u16, sat_pos_u16(grid_top)],
                    color: margin_bg,
                });
            }
            if grid_bot < h {
                bg_inst.push(BgInstance {
                    rect: [0, grid_bot as u16, w as u16, (h - grid_bot) as u16],
                    color: margin_bg,
                });
            }
            if pad > 0 {
                let gutter_h = grid_bot.saturating_sub(grid_top as u32) as u16;
                let right = sat_pos_u16((w as usize).saturating_sub(pad));
                bg_inst.push(BgInstance {
                    rect: [0, sat_pos_u16(grid_top), sat_pos_u16(pad), gutter_h],
                    color: margin_bg,
                });
                bg_inst.push(BgInstance {
                    rect: [right, sat_pos_u16(grid_top), sat_pos_u16(pad), gutter_h],
                    color: margin_bg,
                });
            }
        }
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            if !row_active(r) {
                continue;
            }
            let y0u = sat_pos_u16(grid_top + r * ch);
            let rcw = aterm_render::row_cell_w(input.line_sizes[r], cw);
            let row_uniform = aterm_render::row_is_uniform(input, r);
            let mut bg_run: Option<BgRun> = None;
            if matches!(scope, RepaintScope::Dirty(_)) {
                if wallpaper_on {
                    self.inst.wallpaper.push(wallpaper_quad(
                        0.0,
                        (grid_top + r * ch) as f32,
                        w as f32,
                        ch as f32,
                    ));
                } else {
                    bg_inst.push(BgInstance {
                        rect: [0, y0u, w as u16, ch as u16],
                        color: theme_bg,
                    });
                    if backdrop_margins_on && pad > 0 {
                        let right = sat_pos_u16((w as usize).saturating_sub(pad));
                        bg_inst.push(BgInstance {
                            rect: [0, y0u, sat_pos_u16(pad), ch as u16],
                            color: margin_bg,
                        });
                        bg_inst.push(BgInstance {
                            rect: [right, y0u, sat_pos_u16(pad), ch as u16],
                            color: margin_bg,
                        });
                    }
                }
            }
            let (bg_y, bg_h) = if r == 0
                && grid_top > 0
                && self
                    .cpu
                    .chrome_bleed()
                    .is_some_and(|b| b.first == 0 && b.rows > 0 && b.top_extends_cells)
            {
                (0u16, sat_pos_u16(grid_top + ch))
            } else {
                (y0u, ch as u16)
            };
            if let Some(bleed) = self
                .cpu
                .chrome_bleed()
                .filter(|b| (b.first..b.rows).contains(&r))
            {
                let tone = |rgb: u32| {
                    let mut c = rgb4_u32(rgb);
                    if backdrop_margins_on {
                        c[3] = margin_bg[3];
                    }
                    c
                };
                let color = tone(bleed.color);
                let (left_tone, right_tone) = bleed.gutter_tones(r);
                let right = sat_pos_u16((w as usize).saturating_sub(pad));
                bg_inst.push(BgInstance {
                    rect: [0, y0u, sat_pos_u16(pad), ch as u16],
                    color: tone(left_tone),
                });
                bg_inst.push(BgInstance {
                    rect: [right, y0u, sat_pos_u16(pad), ch as u16],
                    color: tone(right_tone),
                });
                if r == 0 && grid_top > 0 {
                    bg_inst.push(BgInstance {
                        rect: [0, 0, w as u16, grid_top as u16],
                        color,
                    });
                }
                if let Some(seam) = bleed.seam.filter(|_| {
                    r + 1 == bleed.rows
                        && !aterm_render::chrome_rail_covers_gutters(input.chrome_raster(r))
                }) {
                    let deco = self.cpu.deco_metrics();
                    let t = deco.underline_t.max(1).min(ch);
                    let uy = sat_pos_u16(grid_top + r * ch + deco.underline_y.min(ch - t));
                    let seam = rgb4_u32(seam);
                    bg_inst.push(BgInstance {
                        rect: [0, uy, sat_pos_u16(pad), t as u16],
                        color: seam,
                    });
                    bg_inst.push(BgInstance {
                        rect: [right, uy, sat_pos_u16(pad), t as u16],
                        color: seam,
                    });
                }
            }
            let has_pane_defaults = input
                .default_bg_spans
                .get(r)
                .is_some_and(|spans| !spans.is_empty());
            let row_has_below_bg_images = input.images.get(r).is_some_and(|imgs| {
                imgs.iter().any(|(_, image)| {
                    aterm_render::kitty_image_is_below_non_default_bg(image.image.z_index)
                })
            });
            for (c, cell) in cells.iter().take(cols).enumerate() {
                let (cx, ccw, run) = place(r, c, row_uniform, rcw);
                if off_right(cx, ccw) {
                    if row_uniform {
                        break;
                    }
                    continue;
                }
                let x0u = sat_pos_u16(pad + cx);
                let hit = if may_have_sel {
                    let is_wide_lead = cells.get(c + 1).is_some_and(|n| n.wide);
                    input.selection_hit(r, c, is_wide_lead, cell.wide)
                } else {
                    None
                };
                let selected = hit.is_some();
                let theme_selection = selection_palette.at_hit(hit).bg;
                let cell_bg = aterm_render::rgb_to_u32(cell.bg);
                let default_bg = if has_pane_defaults {
                    aterm_render::resolved_default_bg_at(input, r, c, self.theme.bg)
                } else {
                    frame_bg
                };
                if wallpaper_on && !selected && cell_bg == default_bg {
                    continue;
                }
                let color = if selected {
                    rgb4_u32(theme_selection)
                } else {
                    let mut c = rgb4(cell.bg);
                    if bg_alpha != 255 && cell_bg == default_bg {
                        c[3] = bg_alpha;
                    }
                    c
                };
                let (bx, bw) = match run {
                    None => (x0u, ccw as u16),
                    Some((rx0, rx1)) => {
                        let Some((sx, sw)) = clip_x_span(pad + cx, ccw, rx0, rx1) else {
                            continue;
                        };
                        (sat_pos_u16(sx), sw as u16)
                    }
                };
                let instance = BgInstance {
                    rect: [bx, y0u, bw, ch as u16],
                    color,
                };
                push_bg_run(bg_inst, &mut bg_run, bg_y, bg_h, bx, bw, color);
                if row_has_below_bg_images
                    && (selected || cell_bg != default_bg)
                    && input.images_at(r, c).iter().any(|(_, image)| {
                        aterm_render::kitty_image_is_below_non_default_bg(image.image.z_index)
                    })
                {
                    self.inst.image_bg_cover.push(instance);
                }
            }
            let materialized = cells.len().min(cols);
            let selection_reaches_tail = input
                .selection_row_span(r)
                .is_some_and(|(_, end)| usize::from(end) >= materialized);
            if materialized < cols && (selection_reaches_tail || has_pane_defaults) {
                for c in materialized..cols {
                    let (cx, ccw, run) = place(r, c, row_uniform, rcw);
                    if off_right(cx, ccw) {
                        if row_uniform {
                            break;
                        }
                        continue;
                    }
                    let hit = if selection_reaches_tail {
                        input.selection_hit(r, c, false, false)
                    } else {
                        None
                    };
                    let selected = hit.is_some();
                    let theme_selection = selection_palette.at_hit(hit).bg;
                    let default_bg = if has_pane_defaults {
                        aterm_render::resolved_default_bg_at(input, r, c, self.theme.bg)
                    } else {
                        frame_bg
                    };
                    let color = if selected {
                        rgb4_u32(theme_selection)
                    } else if wallpaper_on {
                        continue;
                    } else if default_bg != frame_bg {
                        let mut color = rgb4_u32(default_bg);
                        color[3] = bg_alpha;
                        color
                    } else {
                        continue;
                    };
                    let (bx, bw) = match run {
                        None => (sat_pos_u16(pad + cx), ccw as u16),
                        Some((rx0, rx1)) => {
                            let Some((sx, sw)) = clip_x_span(pad + cx, ccw, rx0, rx1) else {
                                continue;
                            };
                            (sat_pos_u16(sx), sw as u16)
                        }
                    };
                    let instance = BgInstance {
                        rect: [bx, y0u, bw, ch as u16],
                        color,
                    };
                    push_bg_run(bg_inst, &mut bg_run, bg_y, bg_h, bx, bw, color);
                    if selected
                        && row_has_below_bg_images
                        && input.images_at(r, c).iter().any(|(_, image)| {
                            aterm_render::kitty_image_is_below_non_default_bg(image.image.z_index)
                        })
                    {
                        self.inst.image_bg_cover.push(instance);
                    }
                }
            }
            flush_bg_run(bg_inst, &mut bg_run, bg_y, bg_h);
            let raster = input.chrome_raster(r);
            if let Some(m) = raster {
                let rail_h = aterm_render::chrome_fit(m, chrome_room).1.min(ch);
                aterm_render::visit_chrome_raster_runs(
                    m,
                    w as usize,
                    pad,
                    cw,
                    cols,
                    |x0, x1, color, rail| {
                        let (y, hgt) = if rail {
                            (y0u + (ch - rail_h) as u16, rail_h as u16)
                        } else {
                            (y0u, ch as u16)
                        };
                        bg_inst.push(BgInstance {
                            rect: [sat_pos_u16(x0), y, sat_pos_u16(x1 - x0), hgt],
                            color: rgb4_u32(color & 0x00ff_ffff),
                        });
                    },
                );
                for (x0, x1, ya, yb, color) in
                    aterm_render::chrome_ring_runs(m, w as usize, pad, cw, ch, ring_floor)
                {
                    let (ya, yb) = (ya.min(ch), yb.min(ch));
                    if yb <= ya || x1 <= x0 {
                        continue;
                    }
                    bg_inst.push(BgInstance {
                        rect: [
                            sat_pos_u16(x0),
                            y0u + ya as u16,
                            sat_pos_u16(x1 - x0),
                            (yb - ya) as u16,
                        ],
                        color: rgb4_u32(color),
                    });
                }
            }
        }
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            let Some(win) = glyph_win[r] else {
                continue;
            };
            let line_size = input.line_sizes[r];
            let rcw = aterm_render::row_cell_w(line_size, cw);
            let (row_own, row_anchor) = aterm_render::chrome_lifted(
                input,
                r,
                grid_top + r * ch,
                ch,
                chrome_room,
                aterm_render::row_scale(line_size, grid_top + r * ch, ch, r + 1 == input.rows),
            );
            let row_uniform = aterm_render::row_is_uniform(input, r);
            let (mut ink_walk, mut char_fg_walk) = fg_walks(input, r);
            let mut halo_walk =
                aterm_render::FireHaloWalk::new(u16::try_from(r).map_or(&[][..], |row| {
                    aterm_render::fire_halo_row_slice(&input.fire_halo, row)
                }));
            let raster_split = input.chrome_raster(r).and_then(|m| m.split);
            let windowed = win != (i32::MIN, i32::MAX);
            let mut ink_top = i32::MAX;
            type FgMemo = ((u32, [u8; 3], Option<usize>), ([u8; 4], [u8; 4]));
            let mut fg_memo: Option<FgMemo> = None;
            for (c, cell) in cells.iter().take(cols).enumerate() {
                let (cx, ccw, run) = place(r, c, row_uniform, rcw);
                if off_right(cx, ccw) {
                    if row_uniform {
                        break; // off the right edge of the clamped framebuffer — see vis_rows
                    }
                    continue; // a mixed row's runs are not monotone in x
                }
                let Some(key) = key_scratch[r * cols + c] else {
                    continue;
                };
                let (own, anchor_y) = match run {
                    None => (row_own, row_anchor),
                    Some(run_box) => run_scale(r, c, run_box),
                };
                let scale = clip_rows(own, win);
                let in_cutout =
                    cut_out_on && cursor_opaque && r == cr && c >= cut_lo && c <= cut_hi;
                let cut_scale = aterm_render::Scale {
                    clip_x0: scale.clip_x0.max(cut_x0),
                    clip_x1: scale.clip_x1.min(cut_x1),
                    ..scale
                };
                if let Some(slot) = color_atlas.map.get(&key) {
                    if slot.gw == 0 || slot.gh == 0 {
                        continue;
                    }
                    ink_top = ink_top.min(slot.ink_reach(anchor_y, baseline, own));
                    let Some((rect, uv)) =
                        slot.quad((pad + cx) as f32, anchor_y, baseline, scale, (caw, cah))
                    else {
                        continue;
                    };
                    color_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: [0, 0, 0, 0],
                        bg: [0, 0, 0, 0],
                    });
                    if in_cutout
                        && let Some((rect, uv)) =
                            slot.quad((pad + cx) as f32, anchor_y, baseline, cut_scale, (caw, cah))
                    {
                        cursor_color_inst.push(GlyphInstance {
                            rect,
                            uv,
                            color: [0, 0, 0, 0],
                            bg: [0, 0, 0, 0],
                        });
                    }
                    continue;
                }
                let Some(slot) = atlas.map.get(&key) else {
                    continue;
                };
                if slot.gw == 0 || slot.gh == 0 {
                    continue;
                };
                ink_top = ink_top.min(slot.ink_reach(anchor_y, baseline, own));
                let base_fg = cell_base_fg(&mut ink_walk, &mut char_fg_walk, c, cell);
                let hit = if may_have_sel {
                    input.selection_hit(r, c, cells.get(c + 1).is_some_and(|n| n.wide), cell.wide)
                } else {
                    None
                };
                let cell_selected = hit.is_some();
                let aterm_render::SelectionColors {
                    bg: theme_selection,
                    fg: selection_fg,
                } = selection_palette.at_hit(hit);
                let fg_key = (base_fg, cell.bg, hit);
                let (glyph_color, bg_under) = match fg_memo {
                    Some((k, pair)) if k == fg_key => pair,
                    _ => {
                        let gc = rgb4_u32(aterm_render::effective_glyph_fg(
                            selection_fg,
                            min_contrast,
                            base_fg,
                            aterm_render::rgb_to_u32(cell.bg),
                            cell_selected,
                            theme_selection,
                        ));
                        let bu = if cell_selected {
                            rgb4_u32(theme_selection)
                        } else {
                            rgb4(cell.bg)
                        };
                        fg_memo = Some((fg_key, (gc, bu)));
                        (gc, bu)
                    }
                };
                let quad = slot.quad((pad + cx) as f32, anchor_y, baseline, scale, (aw, ah));
                if quad.is_none() && !windowed {
                    continue;
                }
                if fire_present && let Some(strength) = halo_walk.at(c as u16) {
                    let a = aterm_render::fire_halo_alpha(strength);
                    for &(dx, dy) in &aterm_render::HALO_DILATE_OFFSETS {
                        let (hrect, huv) = match (run, quad) {
                            (None, Some((rect, uv))) if !windowed => (
                                [rect[0] + dx as f32, rect[1] + dy as f32, rect[2], rect[3]],
                                uv,
                            ),
                            _ => {
                                let stamp = if windowed {
                                    clip_rows(
                                        own,
                                        (win.0.saturating_sub(dy), win.1.saturating_sub(dy)),
                                    )
                                } else {
                                    scale
                                };
                                let Some((hr, hu)) = slot.quad(
                                    (pad + cx) as f32 + dx as f32,
                                    anchor_y,
                                    baseline,
                                    stamp,
                                    (aw, ah),
                                ) else {
                                    continue;
                                };
                                ([hr[0], hr[1] + dy as f32, hr[2], hr[3]], hu)
                            }
                        };
                        glyph_halo_inst.push(GlyphInstance {
                            rect: hrect,
                            uv: huv,
                            color: [halo_rgb[0], halo_rgb[1], halo_rgb[2], a],
                            bg: [0, 0, 0, 0],
                        });
                    }
                }
                let Some((rect, uv)) = quad else {
                    continue;
                };
                match raster_split.filter(|sp| usize::from(sp.col) == c && !cell_selected) {
                    Some(sp) => {
                        let at = i32::try_from(sp.x).unwrap_or(i32::MAX);
                        let left_color = rgb4_u32(aterm_render::effective_glyph_fg(
                            selection_fg,
                            min_contrast,
                            sp.ink,
                            sp.bg,
                            false,
                            theme_selection,
                        ));
                        let halves = [
                            (
                                aterm_render::Scale {
                                    clip_x1: scale.clip_x1.min(at),
                                    ..scale
                                },
                                left_color,
                                rgb4_u32(sp.bg),
                            ),
                            (
                                aterm_render::Scale {
                                    clip_x0: scale.clip_x0.max(at),
                                    ..scale
                                },
                                glyph_color,
                                bg_under,
                            ),
                        ];
                        for (half, color, bg) in halves {
                            if let Some((rect, uv)) =
                                slot.quad((pad + cx) as f32, anchor_y, baseline, half, (aw, ah))
                            {
                                glyph_inst.push(GlyphInstance {
                                    rect,
                                    uv,
                                    color,
                                    bg,
                                });
                            }
                        }
                    }
                    None => glyph_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: glyph_color,
                        bg: bg_under,
                    }),
                }
                if in_cutout
                    && let Some((rect, uv)) =
                        slot.quad((pad + cx) as f32, anchor_y, baseline, cut_scale, (aw, ah))
                {
                    cursor_glyph_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: cutout_color,
                        bg: rgb4_u32(cursor_fill),
                    });
                }
            }
            let band_top = i32::try_from(grid_top + r * ch).unwrap_or(i32::MAX);
            if self
                .cpu
                .catch_unpredicted_overshoot(input, r, ink_top, band_top)
            {
                self.overshoot_surprise = true;
            }
        }
        self.row_plans = row_plans;
        self.key_scratch = key_scratch;
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            let Some(win) = glyph_win[r] else {
                continue;
            };
            if input.combining[r].is_empty() {
                continue;
            }
            let line_size = input.line_sizes[r];
            let rcw = aterm_render::row_cell_w(line_size, cw);
            let (scale, anchor_y) = aterm_render::chrome_lifted(
                input,
                r,
                grid_top + r * ch,
                ch,
                chrome_room,
                aterm_render::row_scale(line_size, grid_top + r * ch, ch, r + 1 == input.rows),
            );
            let row_uniform = aterm_render::row_is_uniform(input, r);
            let (mut ink_walk, mut char_fg_walk) = fg_walks(input, r);
            for entries in input.combining[r].chunk_by(|a, b| a.0 == b.0) {
                let c = entries[0].0;
                if c >= cols {
                    break;
                }
                let Some(cell) = cells.get(c) else {
                    break;
                };
                let (cx, ccw, run) = place(r, c, row_uniform, rcw);
                if off_right(cx, ccw) {
                    if row_uniform {
                        break; // off the right edge of the clamped framebuffer — see vis_rows
                    }
                    continue; // a mixed row's runs are not monotone in x
                }
                if !Self::drawable(cell)
                    || input.cluster_at(r, c).is_some()
                    || input.image_hides_glyph_at(r, c)
                {
                    continue;
                }
                let Some(marks) = input.combining_at(r, c) else {
                    continue;
                };
                let (scale, anchor_y) = match run {
                    None => (scale, anchor_y),
                    Some(run_box) => run_scale(r, c, run_box),
                };
                let scale = clip_rows(scale, win);
                let base_fg = cell_base_fg(&mut ink_walk, &mut char_fg_walk, c, cell);
                let (
                    cell_selected,
                    aterm_render::SelectionColors {
                        bg: theme_selection,
                        fg: selection_fg,
                    },
                    bg_under,
                ) = cell_selection(r, c, cells, cell);
                let mark_fg = rgb4_u32(aterm_render::effective_glyph_fg(
                    selection_fg,
                    min_contrast,
                    base_fg,
                    aterm_render::rgb_to_u32(cell.bg),
                    cell_selected,
                    theme_selection,
                ));
                let mut stack = aterm_render::MarkStack::default();
                for &m in marks {
                    let key = self.cpu.combining_mark_key(m, cell);
                    let Some(slot) = atlas.map.get(&key) else {
                        continue;
                    };
                    if slot.gw == 0 || slot.gh == 0 {
                        continue;
                    }
                    let Some((ink_h, ink_ymin)) = self.cpu.glyph_ink_box_cached(key) else {
                        continue;
                    };
                    if ink_h == 0 {
                        continue;
                    }
                    let mx = if row_uniform {
                        aterm_render::mark_cell_x(c, rcw, slot.gw as usize, slot.xmin, scale)
                            + pad as i32
                    } else {
                        aterm_render::mark_cell_x(0, ccw, slot.gw as usize, slot.xmin, scale)
                            + (pad + cx) as i32
                    } + aterm_render::mark_script_shift(cell.script, ccw);
                    let dy = stack.dy(ink_h, ink_ymin, baseline, ch, scale.ys);
                    let Some((rect, uv)) =
                        slot.quad(mx as f32, anchor_y + dy, baseline, scale, (aw, ah))
                    else {
                        continue;
                    };
                    glyph_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: mark_fg,
                        bg: bg_under,
                    });
                }
            }
        }
        self.glyph_win_scratch = glyph_win;
        if let Some(plane) = win.image_plane.as_ref() {
            let (pw, ph) = (plane.w as f32, plane.h as f32);
            for (r, row_images) in input.images.iter().enumerate().take(vis_rows) {
                if !row_active(r) || row_images.is_empty() {
                    continue;
                }
                let rcw = aterm_render::row_cell_w(input.line_sizes[r], cw);
                let row_uniform = aterm_render::row_is_uniform(input, r);
                let y0 = (grid_top + r * ch) as f32;
                for (c, image) in row_images {
                    if *c >= cols {
                        continue;
                    }
                    let (cx, ccw, run) = place(r, *c, row_uniform, rcw);
                    if off_right(cx, ccw) {
                        continue;
                    }
                    let fp_w = image.image.cols as usize * cw;
                    let lift = usize::from(image.image.band_lift_px);
                    let fp_h = image.image.rows as usize * ch + lift;
                    let key = (std::sync::Arc::as_ptr(&image.image) as usize, fp_w, fp_h);
                    let Some(&(img_y0, dw, dh)) = plane.placements.get(&key) else {
                        continue;
                    };
                    let (fw, fh) = (fp_w as u32, fp_h as u32);
                    let lip = if image.cell_row == 0 { lift } else { 0 };
                    let sx0 = (image.cell_col as usize * cw) as u32;
                    let sy0 = (image.cell_row as usize * ch + lift - lip) as u32;
                    if sx0 >= fw || sy0 >= fh {
                        continue;
                    }
                    let mut tile_w = (cw as u32).min(fw - sx0);
                    let tile_h = ((ch + lip) as u32).min(fh - sy0);
                    if let Some((_, rx1)) = run {
                        let vis = u32::try_from(rx1.saturating_sub(pad + cx)).unwrap_or(u32::MAX);
                        tile_w = tile_w.min(vis);
                        if tile_w == 0 {
                            continue;
                        }
                    }
                    let x0 = (pad + cx) as f32;
                    let rect = [x0, y0 - lip as f32, tile_w as f32, tile_h as f32];
                    let uv = if (dw, dh) == (fw, fh) {
                        [
                            sx0 as f32 / pw,
                            (img_y0 + sy0) as f32 / ph,
                            tile_w as f32 / pw,
                            tile_h as f32 / ph,
                        ]
                    } else {
                        let (kx, ky) = (dw as f32 / fw as f32, dh as f32 / fh as f32);
                        [
                            sx0 as f32 * kx / pw,
                            (img_y0 as f32 + sy0 as f32 * ky) / ph,
                            tile_w as f32 * kx / pw,
                            tile_h as f32 * ky / ph,
                        ]
                    };
                    let image_inst =
                        if aterm_render::kitty_image_is_below_non_default_bg(image.image.z_index) {
                            &mut self.inst.image_below_bg
                        } else if image.image.z_index < 0 {
                            &mut self.inst.image_under
                        } else {
                            &mut self.inst.image
                        };
                    image_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: [0, 0, 0, 0],
                        bg: [0, 0, 0, 0],
                    });
                }
            }
        }
        if block_cursor && row_active(cr) {
            let mut color = rgb4_u32(cursor_fill);
            color[3] = cursor_cov;
            let fill = match cur_run {
                None => Some((pad + cur_x, cur_block_w)),
                Some((rx0, rx1)) => clip_x_span(pad + cur_x, cur_block_w, rx0, rx1),
            };
            if let Some((fx, fw)) = fill {
                self.inst.cursor_block.push(BgInstance {
                    rect: [
                        sat_pos_u16(fx),
                        sat_pos_u16(grid_top + cr * ch),
                        fw as u16,
                        ch as u16,
                    ],
                    color,
                });
            }
        }

        let dm = self.cpu.deco_metrics();
        let curly_mask_ok = aterm_render::undercurl_supported(cw, ch) && self.deco_atlas.is_some();
        let curl_atlas_w = self.deco_atlas.as_ref().map_or(0, |d| d.atlas_w);
        let mut deco_rects = std::mem::take(&mut self.deco_rects_scratch);
        let mut skip_rects = std::mem::take(&mut self.deco_skip_scratch);
        let mut curl_ink = std::mem::take(&mut self.curl_ink_scratch);
        let mut curl_spans = std::mem::take(&mut self.curl_spans_scratch);
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            if !row_active(r) {
                continue;
            }
            let y0 = grid_top + r * ch;
            let rcw = aterm_render::row_cell_w(input.line_sizes[r], cw);
            let row_uniform = aterm_render::row_is_uniform(input, r);
            let (mut ink_walk, mut char_fg_walk) = fg_walks(input, r);
            for (c, cell) in cells.iter().take(cols).enumerate() {
                let (cx, ccw, run) = place(r, c, row_uniform, rcw);
                if off_right(cx, ccw) {
                    if row_uniform {
                        break; // off the right edge of the clamped framebuffer — see vis_rows
                    }
                    continue; // a mixed row's runs are not monotone in x
                }
                if cell.wide
                    || (matches!(cell.underline, UnderlineStyle::None)
                        && !cell.strikethrough
                        && !cell.overline)
                {
                    continue;
                }
                let x = pad + cx;
                let is_wide_lead = cells.get(c + 1).is_some_and(|n| n.wide);
                let dw = if is_wide_lead { 2 * ccw } else { ccw };
                let base_fg = cell_base_fg(&mut ink_walk, &mut char_fg_walk, c, cell);
                let hit = if may_have_sel {
                    input.selection_hit(r, c, is_wide_lead, cell.wide)
                } else {
                    None
                };
                let cell_selected = hit.is_some();
                let aterm_render::SelectionColors {
                    bg: theme_selection,
                    fg: selection_fg,
                } = selection_palette.at_hit(hit);
                let inks = aterm_render::deco_inks(
                    cell,
                    selection_fg,
                    min_contrast,
                    base_fg,
                    cell_selected,
                    theme_selection,
                );
                let ucolor = rgb4_u32(inks.underline);
                let is_curly = matches!(cell.underline, UnderlineStyle::Curly);
                aterm_render::underline_rects_into(
                    &mut deco_rects,
                    cell.underline,
                    x,
                    y0,
                    dw,
                    ccw,
                    ch,
                    dm,
                    curly_mask_ok,
                );
                let plan_entry = self
                    .row_plans
                    .get(r)
                    .and_then(|p| p.get(c))
                    .copied()
                    .unwrap_or(aterm_render::ColumnGlyph::PerCell);
                let want_spans = !deco_rects.is_empty() || (is_curly && curly_mask_ok);
                let skip = want_spans
                    && self.cpu.underline_keep_spans_into(
                        input,
                        r,
                        c,
                        plan_entry,
                        x,
                        dw,
                        &mut curl_ink,
                        &mut curl_spans,
                    );
                if skip && !deco_rects.is_empty() {
                    skip_rects.clear();
                    for &rect in &deco_rects {
                        aterm_render::deco::intersect_rect_spans(
                            &mut skip_rects,
                            rect,
                            &curl_spans,
                        );
                    }
                    std::mem::swap(&mut deco_rects, &mut skip_rects);
                }
                for &[rx, ry, rw, rh] in &deco_rects {
                    let clipped = match run {
                        None => Some((rx, rw)),
                        Some((qx0, qx1)) => clip_x_span(rx, rw, qx0, qx1),
                    };
                    let Some((rx, rw)) = clipped else {
                        continue;
                    };
                    self.inst.deco.push(BgInstance {
                        rect: [sat_pos_u16(rx), sat_pos_u16(ry), rw as u16, rh as u16],
                        color: ucolor,
                    });
                }
                if is_curly && curly_mask_ok {
                    if !skip {
                        curl_spans.clear();
                        curl_spans.push((x, dw));
                    }
                    let curl_x0 = aterm_render::UNDERCURL_SPRITE * cw;
                    for ti in 0..dw / ccw.max(1) {
                        let tx0 = x + ti * ccw;
                        for &(sx, sw) in &curl_spans {
                            let mut lo = sx.max(tx0);
                            let mut hi = (sx + sw).min(tx0 + ccw);
                            if let Some((qx0, qx1)) = run {
                                lo = lo.max(qx0);
                                hi = hi.min(qx1);
                            }
                            if lo >= hi {
                                continue;
                            }
                            let scale = cw as f32 / ccw as f32;
                            let u0 = curl_x0 as f32 + (lo - tx0) as f32 * scale;
                            let uw = (hi - lo) as f32 * scale;
                            self.inst.curl.push(GlyphInstance {
                                rect: [lo as f32, y0 as f32, (hi - lo) as f32, ch as f32],
                                uv: [u0 / curl_atlas_w as f32, 0.0, uw / curl_atlas_w as f32, 1.0],
                                color: ucolor,
                                bg: [0, 0, 0, 0],
                            });
                        }
                    }
                }
                for (rect, ink) in [
                    (
                        cell.strikethrough
                            .then(|| aterm_render::strike_rect(x, y0, dw, ch, dm))
                            .flatten(),
                        inks.strike,
                    ),
                    (
                        cell.overline
                            .then(|| aterm_render::overline_rect(x, y0, dw, ch, dm))
                            .flatten(),
                        inks.overline,
                    ),
                ] {
                    let Some([rx, ry, rw, rh]) = rect else {
                        continue;
                    };
                    let clipped = match run {
                        None => Some((rx, rw)),
                        Some((qx0, qx1)) => clip_x_span(rx, rw, qx0, qx1),
                    };
                    let Some((rx, rw)) = clipped else {
                        continue;
                    };
                    self.inst.deco.push(BgInstance {
                        rect: [sat_pos_u16(rx), sat_pos_u16(ry), rw as u16, rh as u16],
                        color: rgb4_u32(ink),
                    });
                }
            }
        }
        self.deco_rects_scratch = deco_rects;
        self.deco_skip_scratch = skip_rects;
        self.curl_ink_scratch = curl_ink;
        self.curl_spans_scratch = curl_spans;
        if !input.cursor_trail.is_empty() {
            let trail_color = input.cursor_trail_color;
            for t in &input.cursor_trail {
                if t.row >= rows || t.col >= cols || !row_active(t.row) {
                    continue;
                }
                let (tx, tcw, bound) = if aterm_render::row_is_uniform(input, t.row) {
                    let rcw = aterm_render::row_cell_w(input.line_sizes[t.row], cw);
                    (t.col * rcw, rcw, cols * cw)
                } else {
                    let (x, cw_run, _, x1) = aterm_render::cell_x_run(input, t.row, t.col, cw);
                    (x, cw_run, x1)
                };
                if tx + tcw > bound {
                    continue;
                }
                let default_bg =
                    aterm_render::resolved_default_bg_at(input, t.row, t.col, self.theme.bg);
                let cell_bg = rendered
                    .get(t.row)
                    .and_then(|r| r.get(t.col))
                    .map_or(default_bg, |c| aterm_render::rgb_to_u32(c.bg));
                let color = aterm_render::blend_rgb(cell_bg, trail_color, t.alpha);
                let mut c4 = rgb4_u32(color);
                if bg_alpha != 255 && cell_bg == default_bg {
                    c4[3] = bg_alpha;
                }
                self.inst.trail.push(BgInstance {
                    rect: [
                        sat_pos_u16(pad + tx),
                        sat_pos_u16(grid_top + t.row * ch),
                        tcw as u16,
                        ch as u16,
                    ],
                    color: c4,
                });
            }
        }
        if !input.fire_patch.is_empty() {
            let pad16 = pad as u16;
            for q in &input.fire_patch {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                let inst = FireInstance {
                    rect: [q.x, q.y, q.w, q.h],
                    geo: [q.base_y, q.peak_h, q.cell_h, pad16],
                    phase: q.phase,
                    tsl: [q.temp, q.strength, q.cov_cap, q.lean as u8],
                };
                match q.mode {
                    aterm_render::FireMode::Add => self.inst.fire_add.push(inst),
                    aterm_render::FireMode::Over => self.inst.fire_over.push(inst),
                }
            }
        }

        self.fx.atlas_bytes = 0;
        self.fx.reader_wait = (0, 0);
        self.fx.refused = 0;
        let fx_built = !(input.fx_layers.is_empty() && input.fx_prefetch.is_empty());
        if fx_built {
            let cap = self.fx_atlas_cap();
            // The companions' bodies cut where their letters lie, as the CPU composite draws
            // them (`aterm_render::Renderer::cut_knock_layers`).
            let cut = self.cpu.cut_knock_layers(input, w as usize, h as usize);
            let layers = if cut {
                self.cpu.cut_layers()
            } else {
                input.fx_layers.as_slice()
            };
            self.fx.admit(layers, &input.fx_prefetch, cap);
            let f = self.cpu.fx_frame(w as usize, h as usize, rows);
            self.fx.build(
                &f,
                layers,
                &input.fx_occluders,
                row_active,
                &mut self.inst.fx,
            );
        }
        // A glow stream carrying ramps draws its ramps in its fx stream (the
        // Under and Glow slots sit just after the glow streams) where their
        // lit rows cost the arm less there than as glow instances, in an fx
        // stream the frame draws anyway or one they open
        // ([`fx_gpu::ramps_open_fx`]): with its quads in their order there,
        // or, where the order cannot change a byte ([`fx_gpu::ramps_commute`]
        // on a target whose adds store raw bytes; a WebGL2 offscreen adds in
        // linear light, rounding each store, so there order counts), its
        // quads as glow instances first. Elsewhere, and where fx cannot draw,
        // it draws as the quads it stands for.
        let ramps_fx = self.ramps_open_fx(
            self.inst.fx_instances() > 0,
            [&input.glow_under_ramps, &input.cursor_glow_add_ramps],
        );
        let raw_adds = self.additive_is_byte_exact();
        let fx_route = |quads: &[aterm_render::GlowQuad], ramps: &aterm_render::GlowRamps| {
            (ramps_fx && !ramps.is_empty()).then(|| raw_adds && fx_gpu::ramps_commute(quads, ramps))
        };
        let under_fx = fx_route(&input.glow_under, &input.glow_under_ramps);
        let add_fx = fx_route(&input.cursor_glow_add, &input.cursor_glow_add_ramps);
        if !fx_built && (under_fx.is_some() || add_fx.is_some()) {
            self.fx.palette.clear();
        }
        match under_fx {
            None => glow_instances_into(
                &mut self.inst.glow_under,
                (&input.glow_under, &input.glow_under_ramps),
                rows,
                row_active,
            ),
            Some(true) => glow_instances_into(
                &mut self.inst.glow_under,
                (&input.glow_under, &aterm_render::GlowRamps::default()),
                rows,
                row_active,
            ),
            Some(false) => {}
        }
        for (route, stream, (quads, ramps)) in [
            (
                under_fx,
                fx_gpu::FxStream::Under,
                (&input.glow_under[..], &input.glow_under_ramps),
            ),
            (
                add_fx,
                fx_gpu::FxStream::Glow,
                (&input.cursor_glow_add[..], &input.cursor_glow_add_ramps),
            ),
        ] {
            let Some(apart) = route else {
                continue;
            };
            let quads = if apart { &[][..] } else { quads };
            let mut first = std::mem::take(&mut self.glow_fx_scratch);
            first.clear();
            fx_gpu::glow_stream_into(
                (quads, ramps),
                rows,
                row_active,
                &mut self.fx.palette,
                &mut first,
            );
            first.extend_from_slice(&self.inst.fx[stream as usize]);
            std::mem::swap(&mut self.inst.fx[stream as usize], &mut first);
            first.clear();
            self.glow_fx_scratch = first;
        }

        match add_fx {
            None => glow_instances_into(
                &mut self.inst.glow_add,
                (&input.cursor_glow_add, &input.cursor_glow_add_ramps),
                rows,
                row_active,
            ),
            Some(true) => glow_instances_into(
                &mut self.inst.glow_add,
                (&input.cursor_glow_add, &aterm_render::GlowRamps::default()),
                rows,
                row_active,
            ),
            Some(false) => {}
        }

        if !input.glow_halo.is_empty() {
            for q in &input.glow_halo {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                let inst = RainGlowInstance {
                    rect: [q.x, q.y, q.w, q.h],
                    color: rgb4_u32(q.color),
                    falloff: [q.cx, q.cy, q.rx, q.ry],
                };
                match q.mode {
                    aterm_render::HaloMode::Add => self.inst.glow_halo.push(inst),
                    aterm_render::HaloMode::Over => {
                        self.inst.glow_halo_over.push(with_over_cap(inst, q.color));
                    }
                }
            }
        }

        if !input.nova_add.is_empty() {
            let pad16 = pad as u16;
            let grid_top16 = grid_top as u16;
            for q in &input.nova_add {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                self.inst.nova_add.push(GlowInstance::of(
                    [
                        pad16.saturating_add(q.x),
                        grid_top16.saturating_add(q.y),
                        q.w,
                        q.h,
                    ],
                    q,
                ));
            }
        }

        if !input.rain_add.is_empty() {
            let pad16 = pad as u16;
            let grid_top16 = grid_top as u16;
            for q in &input.rain_add {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                let inst = RainGlowInstance {
                    rect: [
                        pad16.saturating_add(q.x),
                        grid_top16.saturating_add(q.y),
                        q.w,
                        q.h,
                    ],
                    color: rgb4_u32(q.color),
                    falloff: [
                        pad16.saturating_add(q.cx),
                        grid_top16.saturating_add(q.cy),
                        q.rx,
                        q.ry,
                    ],
                };
                match q.mode {
                    aterm_render::HaloMode::Add => self.inst.rain_add.push(inst),
                    aterm_render::HaloMode::Over => {
                        self.inst.rain_add_over.push(with_over_cap(inst, q.color));
                    }
                }
            }
        }

        if !input.word_decorations.is_empty()
            && let Some(datlas_w) = self.deco_atlas.as_ref().map(|d| d.atlas_w)
        {
            for d in &input.word_decorations {
                let (row, col) = (d.row as usize, d.col as usize);
                if row >= rows || col >= cols || !row_active(row) {
                    continue;
                }
                if matches!(d.blend, aterm_render::DecoBlend::Add) && input.any_selection() {
                    let row_cells = &input.cells[row];
                    let is_wide_lead = row_cells.get(col + 1).is_some_and(|n| n.wide);
                    let cell_wide = row_cells.get(col).is_some_and(|n| n.wide);
                    if input.selection_contains_cell(row, col, is_wide_lead, cell_wide) {
                        continue;
                    }
                }
                let (deco_x, rcw, run) = if aterm_render::row_is_uniform(input, row) {
                    let rcw = aterm_render::row_cell_w(input.line_sizes[row], cw);
                    (col * rcw, rcw, None)
                } else {
                    let (x, cw_run, x0, x1) = aterm_render::cell_x_run(input, row, col, cw);
                    (x, cw_run, Some((pad + x0, pad + x1)))
                };
                let idx = deco_sprite_index(d.glyph);
                let mut rect = [
                    (pad + deco_x) as f32 + f32::from(d.dx),
                    grid_top as f32 + (row * ch) as f32 + f32::from(d.dy),
                    rcw as f32,
                    ch as f32,
                ];
                let mut uv = [
                    (idx * cw) as f32 / datlas_w as f32,
                    0.0,
                    cw as f32 / datlas_w as f32,
                    1.0,
                ];
                if !clip_textured_quad_x(&mut rect, &mut uv, run) {
                    continue;
                }
                let color = [
                    (d.color >> 16) as u8,
                    (d.color >> 8) as u8,
                    d.color as u8,
                    d.alpha,
                ];
                let gi = GlyphInstance {
                    rect,
                    uv,
                    color,
                    bg: [0, 0, 0, 0],
                };
                match d.blend {
                    aterm_render::DecoBlend::Over => self.inst.wdeco_over.push(gi),
                    aterm_render::DecoBlend::Add => self.inst.wdeco_add.push(gi),
                }
            }
        }

        if cursor_drawn && !block_cursor && row_active(cr) {
            let cursor_color = {
                let mut c = rgb4_u32(cursor_fill);
                c[3] = cursor_cov;
                c
            };
            aterm_render::for_each_cursor_rect(
                style,
                pad + cur_x,
                grid_top + cr * ch,
                cur_cw,
                ch,
                |[x, y, rw, rh]| {
                    let clipped = match cur_run {
                        None => Some((x, rw)),
                        Some((rx0, rx1)) => clip_x_span(x, rw, rx0, rx1),
                    };
                    if let Some((x, rw)) = clipped {
                        self.inst.cursor.push(BgInstance {
                            rect: [sat_pos_u16(x), sat_pos_u16(y), rw as u16, rh as u16],
                            color: cursor_color,
                        });
                    }
                },
            );
        }

        let text_blend =
            u32::from(self.cpu.text_blending() == aterm_render::TextBlending::LinearCorrected);

        #[cfg(gl_arm)]
        if gl_armed {
            let build_ns = elapsed_ns(build_started);
            let live = self.gl_arm.as_mut().expect("gl_arm_ready minted it");
            let tail = gl_arm::encode_tail(
                live,
                win,
                &mut self.frame_plan,
                gl_arm::GlTailIn {
                    inst: &self.inst,
                    mono: mono_res,
                    color: color_res,
                    atlas_epoch: self.atlas_epoch,
                    deco: self.deco_atlas.as_ref(),
                    scope,
                    dirty_band,
                    grid_top,
                    ch,
                    rows,
                    dims: (w, h),
                    clear: theme_color_alpha(frame_bg, f64::from(bg_opacity)),
                    text_blend,
                    cursor_opaque,
                    ablation: self.ablation,
                    under_opaque: fx_gpu::under_draws_opaque(input),
                    fx: &self.fx,
                },
            );
            match tail {
                Ok(out) => {
                    self.fx.atlas_bytes = out.fx_atlas_bytes;
                    self.last_frame_passes = out.passes as u32;
                    self.last_frame_load = out.load0;
                    self.last_frame_scissor = out.scissor;
                    self.record_frame_instance_totals(
                        out.passes as u32,
                        build_ns,
                        out.stage_ns,
                        out.unorm,
                        out.fx_staged,
                    );
                    self.last_frame_arm_gl = true;
                    return (w, h);
                }
                Err(e) => {
                    gl_arm::gl_arm_note(&format!("armed frame failed ({e}); this frame is lost"))
                }
            }
        }
        // The D3D11 tail (Windows, `d3d11_arm.rs`): the same instance bytes,
        // plan and rig law on the D3D11 arm. A failure takes one named note and
        // the frame is lost.
        #[cfg(d3d11_arm)]
        if d3d11_armed {
            let build_ns = elapsed_ns(build_started);
            let live = self.d3d11_arm.as_mut().expect("d3d11_arm_ready minted it");
            let tail = d3d11_arm::encode_tail(
                live,
                win,
                &mut self.frame_plan,
                d3d11_arm::D3dTailIn {
                    inst: &self.inst,
                    mono: mono_res,
                    color: color_res,
                    atlas_epoch: self.atlas_epoch,
                    deco: self.deco_atlas.as_ref(),
                    scope,
                    dirty_band,
                    grid_top,
                    ch,
                    rows,
                    dims: (w, h),
                    clear: theme_color_alpha(frame_bg, f64::from(bg_opacity)),
                    text_blend,
                    cursor_opaque,
                    ablation: self.ablation,
                    under_opaque: fx_gpu::under_draws_opaque(input),
                    fx: &self.fx,
                },
            );
            match tail {
                Ok(out) => {
                    self.fx.atlas_bytes = out.fx_atlas_bytes;
                    self.last_frame_passes = out.passes as u32;
                    self.last_frame_load = out.load0;
                    self.last_frame_scissor = out.scissor;
                    self.record_frame_instance_totals(
                        out.passes as u32,
                        build_ns,
                        out.stage_ns,
                        out.unorm,
                        out.fx_staged,
                    );
                    self.last_frame_arm_d3d11 = true;
                    return (w, h);
                }
                Err(e) => d3d11_arm::d3d11_arm_note(&format!(
                    "armed frame failed ({e}); this frame is lost"
                )),
            }
        }
        #[cfg(target_os = "macos")]
        if armed {
            'armed: {
                let _pool = crate::metal::ffi::AutoreleasePool::new();
                use crate::metal::loss::CbOutcome;

                let mut counts: [Option<u32>; STREAM_COUNT] = [None; STREAM_COUNT];
                let build_ns = elapsed_ns(build_started);
                let stage_ns;
                let mut fx_palette: Option<crate::metal::ffi::Obj> = None;
                {
                    let live = self
                        .metal_arm
                        .as_mut()
                        .expect("armed implies the arm minted")
                        .live_mut();
                    live.await_frame_slot();
                    let stage_started = aterm_time::Instant::now();
                    let fx_drawing = self.inst.fx_instances() > 0;
                    let fx_staged = live.sync_fx(&self.fx.atlas, fx_drawing).and_then(|bytes| {
                        self.fx.atlas_bytes = bytes;
                        self.fx.reader_wait = live.fx_reader_wait;
                        if !fx_drawing {
                            return Ok(None);
                        }
                        #[cfg(any(test, debug_assertions))]
                        if self.fx_fail_seam {
                            return Err("the staging seam refused".to_owned());
                        }
                        #[cfg(any(test, debug_assertions))]
                        if self.fx_palette_shared_seam {
                            live.write_shared_palette(&self.fx.palette)?;
                        }
                        let palette = live.upload_fx_palette(&self.fx.palette)?;
                        live.fx_hazard.bound(&palette);
                        Ok(Some(palette))
                    });
                    match fx_staged {
                        Ok(p) => fx_palette = p,
                        Err(e) => metal_arm_note(&format!(
                            "armed frame: fx residency failed ({e}); fx draws nothing"
                        )),
                    }
                    for id in StreamId::ALL {
                        if id.is_fx() && fx_palette.is_none() {
                            continue;
                        }
                        let (bytes, count) = self.inst.stream_bytes(id);
                        match live.upload_stream(id, bytes) {
                            Ok(true) => counts[id as usize] = Some(count),
                            Ok(false) => {}
                            Err(e) => {
                                metal_arm_note(&format!(
                                    "armed frame: stream staging failed ({e})"
                                ));
                                break 'armed;
                            }
                        }
                    }
                    stage_ns = elapsed_ns(stage_started);
                }
                let fx_staged = fx_palette.is_some();

                let armed_image_plane = win
                    .image_plane
                    .as_ref()
                    .is_some_and(|p| p.metal_texels.is_some());
                if win.image_plane.is_some() && !armed_image_plane {
                    metal_arm_note(
                        "armed frame: an image plane without resident texels — image \
                         draws withheld from the Metal plan (built before arming?)",
                    );
                }
                let mut plan = std::mem::take(&mut self.frame_plan);
                let mut unorm = build_frame_plan(
                    &mut plan,
                    &counts,
                    FramePlanCtx {
                        has_image_plane: armed_image_plane,
                        has_deco: self.deco_atlas.is_some(),
                        has_wallpaper: win.wallpaper_tex.is_some(),
                        cursor_opaque,
                        ablation: self.ablation,
                        under_opaque: fx_gpu::under_draws_opaque(input),
                    },
                );
                let seam = self.fetch_seam();
                let route = Self::metal_unorm_route(
                    self.metal_arm.as_mut(),
                    self.unorm_route,
                    seam,
                    unorm.draws > 0,
                );
                route_frame_plan(&mut plan, &mut unorm, route);
                macro_rules! armed_try {
                    ($what:literal, $e:expr) => {
                        match $e {
                            Ok(v) => v,
                            Err(e) => {
                                metal_arm_note(&format!(
                                    concat!("armed frame: ", $what, " ({})"),
                                    e
                                ));
                                self.frame_plan = plan;
                                break 'armed;
                            }
                        }
                    };
                }

                let recreate = win
                    .metal_offscreen
                    .as_ref()
                    .is_none_or(|o| o.w != w || o.h != h);
                if recreate {
                    let live = self
                        .metal_arm
                        .as_mut()
                        .expect("armed implies the arm minted")
                        .live_mut();
                    let tex = armed_try!(
                        "offscreen mint failed",
                        live.layer()
                            .try_create_texture_2d(
                                "aterm-gpu metal offscreen",
                                crate::device_layer::TexelFormat::Rgba8Unorm,
                                w,
                                h,
                                crate::device_layer::TexUsage::OFFSCREEN,
                                Some(crate::device_layer::TexelFormat::Rgba8UnormSrgb),
                            )
                            .map(crate::device_layer::LayerTexture::into_metal)
                    );
                    let srgb = armed_try!(
                        "offscreen sRGB alias failed",
                        tex.alias_view(crate::device_layer::TexelFormat::Rgba8UnormSrgb.metal())
                            .ok_or_else(|| "newTextureViewWithPixelFormat returned nil".to_owned())
                    );
                    win.metal_offscreen = Some(MetalOffscreen { tex, srgb, w, h });
                }

                let (load_op, scissor) = frame_load_and_scissor(
                    scope,
                    recreate,
                    dirty_band,
                    grid_top,
                    ch,
                    rows,
                    (w, h),
                    theme_color_alpha(frame_bg, f64::from(bg_opacity)),
                );
                note_offscreen_written(
                    win,
                    scissor.map(|(sx, sy, sw, sh)| [sx, sy, sx + sw, sy + sh]),
                    (w, h),
                );
                let load0 = load_op;

                let rig = {
                    let live = self
                        .metal_arm
                        .as_mut()
                        .expect("armed implies the arm minted")
                        .live_mut();
                    live.ensure_uniform(w, h, text_blend);
                    let mut uniform_vertex_slot: Option<u32> = None;
                    let mut uniform_fragment_slot: Option<u32> = None;
                    let mut psos: [Option<crate::metal::ffi::Obj>; pipeline_table::PIPELINE_COUNT] =
                        std::array::from_fn(|_| None);
                    let mut fetch_psos: [Option<crate::metal::ffi::Obj>;
                        pipeline_table::FETCH_TWIN_COUNT] = std::array::from_fn(|_| None);
                    let mut rig_err: Option<String> = None;
                    for item in &plan {
                        let row = item.pipe.row();
                        let spec = row.spec();
                        let vu = spec.binds.vertex_uniform.unwrap_or_else(|| {
                            panic!("frame row {row:?} declares no vertex uniform")
                        });
                        match uniform_vertex_slot {
                            None => uniform_vertex_slot = Some(vu),
                            Some(prev) => assert_eq!(
                                prev, vu,
                                "one shared uniform bind per pass requires one slot; \
                                 {row:?} disagrees"
                            ),
                        }
                        if let Some(&fb) = spec.binds.fragment_buffers.first() {
                            assert_eq!(
                                spec.binds.fragment_buffers.len(),
                                1,
                                "frame rows re-bind at most the one shared uniform block"
                            );
                            match uniform_fragment_slot {
                                None => uniform_fragment_slot = Some(fb),
                                Some(prev) => {
                                    assert_eq!(prev, fb, "{row:?} disagrees on the fragment slot")
                                }
                            }
                        }
                        let built = if item.fetch {
                            let twin = pipeline_table::FetchTwin::of(row).unwrap_or_else(|| {
                                panic!("a fetch draw's row {row:?} has no fetch twin")
                            });
                            let slot = &mut fetch_psos[twin as usize];
                            if slot.is_some() {
                                continue;
                            }
                            live.fetch_pso(twin)
                                .map(|pso| *slot = Some(pso.clone_retained()))
                        } else {
                            let slot = &mut psos[row as usize];
                            if slot.is_some() {
                                continue;
                            }
                            live.pso(row).map(|pso| *slot = Some(pso.clone_retained()))
                        };
                        if let Err(e) = built {
                            rig_err = Some(e);
                            break;
                        }
                    }

                    let mut atlases: [Option<(
                        crate::metal::resources::SealedTexture,
                        crate::metal::ffi::Obj,
                    )>; DRAW_ATLAS_COUNT] = std::array::from_fn(|_| None);
                    if rig_err.is_none() {
                        for item in &plan {
                            let Some(a) = item.atlas else { continue };
                            if atlases[a as usize].is_some() {
                                continue;
                            }
                            let (format, key, write_gen, cur_py, bytes, slot) = match a {
                                DrawAtlas::Fx => continue,
                                DrawAtlas::Mono => (
                                    crate::device_layer::TexelFormat::R8Unorm,
                                    (mono_res.atlas.width, mono_res.tex_h, self.atlas_epoch),
                                    mono_res.atlas.write_gen,
                                    mono_res.atlas.py,
                                    &mono_res.atlas.data
                                        [..(mono_res.atlas.width * mono_res.atlas.height) as usize],
                                    &mut live.atlases[a as usize],
                                ),
                                DrawAtlas::Color => (
                                    crate::device_layer::TexelFormat::Rgba8Unorm,
                                    (color_res.atlas.width, color_res.tex_h, self.atlas_epoch),
                                    color_res.atlas.write_gen,
                                    color_res.atlas.py,
                                    &color_res.atlas.data[..(color_res.atlas.width
                                        * color_res.atlas.height
                                        * 4)
                                        as usize],
                                    &mut live.atlases[a as usize],
                                ),
                                DrawAtlas::Deco => {
                                    let d =
                                        self.deco_atlas.as_ref().expect("has_deco gated this item");
                                    (
                                        crate::device_layer::TexelFormat::R8Unorm,
                                        (
                                            d.atlas_w as u32,
                                            d.ch as u32,
                                            ((d.curl_band.0 as u64) << 32)
                                                | (d.curl_band.1 as u64 & 0xffff_ffff),
                                        ),
                                        0,
                                        0,
                                        &d.data[..],
                                        &mut live.atlases[a as usize],
                                    )
                                }
                                DrawAtlas::Wallpaper => {
                                    let sprite = win.wallpaper_tex.as_ref().unwrap_or_else(|| {
                                        panic!("plan samples {a:?} but its SpriteTex is absent")
                                    });
                                    match sprite.metal_atlas(
                                        crate::device_layer::DeviceHandle::Metal(&live.mint),
                                        a,
                                    ) {
                                        Ok(resident) => {
                                            atlases[a as usize] = Some((
                                                resident.tex.metal().clone_handle(),
                                                live.nearest.clone_retained(),
                                            ));
                                        }
                                        Err(e) => {
                                            rig_err = Some(e);
                                            break;
                                        }
                                    }
                                    continue;
                                }
                                DrawAtlas::Image => {
                                    let p = win
                                        .image_plane
                                        .as_mut()
                                        .expect("has_image_plane gated this item");
                                    (
                                        crate::device_layer::TexelFormat::Rgba8Unorm,
                                        (p.w, p.h, p.epoch),
                                        0,
                                        0,
                                        p.metal_texels
                                            .as_deref()
                                            .expect("armed_image_plane requires resident texels"),
                                        &mut p.metal_atlas,
                                    )
                                }
                            };
                            match MetalArmLive::ensure_atlas(
                                crate::device_layer::DeviceHandle::Metal(&live.mint),
                                slot,
                                a,
                                key,
                                write_gen,
                                cur_py,
                                format,
                                bytes,
                                &mut live.frame_slots,
                            ) {
                                Ok((consumed, parked_ns)) => {
                                    live.awaited_frames += consumed;
                                    live.frame_slot_wait_ns =
                                        live.frame_slot_wait_ns.saturating_add(parked_ns);
                                }
                                Err(e) => {
                                    rig_err = Some(e);
                                    break;
                                }
                            }
                            let resident = slot.as_ref().expect("ensure_atlas filled the slot");
                            atlases[a as usize] = Some((
                                resident.tex.metal().clone_handle(),
                                live.nearest.clone_retained(),
                            ));
                        }
                    }
                    if let Some(e) = rig_err {
                        metal_arm_note(&format!("armed frame: rig assembly failed ({e})"));
                        self.frame_plan = plan;
                        break 'armed;
                    }
                    let fx = fx_palette.map(|palette| {
                        let tex = |k: fx_gpu::FxAtlasKind| {
                            live.fx_tex[k as usize]
                                .as_ref()
                                .expect("sync_fx mints both atlases on an fx frame")
                                .tex
                                .metal()
                                .clone_handle()
                        };
                        MetalFxBind {
                            r8: tex(fx_gpu::FxAtlasKind::R8),
                            rgba: tex(fx_gpu::FxAtlasKind::Rgba),
                            palette,
                        }
                    });

                    let mut streams: [Option<crate::metal::resources::SharedBuffer>; STREAM_COUNT] =
                        std::array::from_fn(|_| None);
                    for item in &plan {
                        let idx = item.stream as usize;
                        if streams[idx].is_none() {
                            streams[idx] = Some(crate::metal::resources::SharedBuffer::new(
                                live.current_slot().streams[idx]
                                    .as_ref()
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "planned stream {:?} was not staged — the \
                                             staging loop's count coupling broke",
                                            item.stream
                                        )
                                    })
                                    .buf
                                    .metal()
                                    .clone_retained(),
                            ));
                        }
                    }

                    let off = win.metal_offscreen.as_ref().expect("ensured above");
                    MetalFrameRig {
                        offscreen: off.tex.clone_handle(),
                        offscreen_srgb: off.srgb.clone_handle(),
                        psos,
                        fetch_psos,
                        uniform: live.current_slot().uniform.metal().clone_retained(),
                        uniform_vertex_slot: uniform_vertex_slot.unwrap_or(0),
                        uniform_fragment_slot,
                        streams,
                        atlases,
                        fx,
                    }
                };

                let passes = {
                    let live = self
                        .metal_arm
                        .as_mut()
                        .expect("armed implies the arm minted")
                        .live_mut();
                    let mut enc = armed_try!(
                        "encoder refused",
                        crate::device_layer::FrameEncoder::metal(&live.session)
                    );
                    if let Some(shift) = pending_band_shift {
                        let scratch = armed_try!(
                            "band shift lacks its resident scratch",
                            win.metal_shift_scratch
                                .as_ref()
                                .filter(|t| (t.width(), t.height()) == (w as usize, h as usize))
                                .ok_or_else(|| format!("no {w}x{h} scratch resident"))
                        );
                        armed_try!(
                            "band shift copies refused",
                            record_band_shift(&mut enc, &rig.offscreen, scratch, shift, w)
                        );
                        live.encode_band_shifts
                            .set(live.encode_band_shifts.get().saturating_add(1));
                    }
                    let res = FrameRes::Metal(&rig);
                    let passes = armed_try!(
                        "frame-plan walk failed",
                        run_frame_plan(&mut enc, &res, &plan, load0, scissor)
                    );
                    match enc.submit() {
                        crate::device_layer::SubmittedFrame::Metal(submitted) => {
                            match submitted.try_outcome() {
                                Some(outcome) if outcome != CbOutcome::Completed => {
                                    metal_arm_note(&format!(
                                        "armed frame: command buffer ended {outcome:?} — the \
                                         loss latch has it; frame lost; the loss poll will downgrade to the CPU renderer"
                                    ));
                                    self.frame_plan = plan;
                                    break 'armed;
                                }
                                _ => live.current_slot_mut().inflight = Some(submitted),
                            }
                        }
                    }
                    passes
                };

                self.last_frame_passes = passes as u32;
                self.frame_plan = plan;
                self.last_frame_load = load0;
                self.last_frame_scissor = scissor;
                self.record_frame_instance_totals(
                    passes as u32,
                    build_ns,
                    stage_ns,
                    unorm,
                    fx_staged,
                );
                self.last_frame_arm_metal = true;
                return (w, h);
            }
        }

        #[cfg(target_os = "macos")]
        {
            self.last_frame_arm_metal = false;
        }
        (w, h)
    }
}

#[inline]
fn sat_pos_u16(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

#[inline]
fn clip_x_span(x: usize, w: usize, x0: usize, x1: usize) -> Option<(usize, usize)> {
    let lo = x.max(x0);
    let hi = (x + w).min(x1);
    if hi <= lo {
        return None;
    }
    Some((lo, hi - lo))
}

#[inline]
fn clip_textured_quad_x(
    rect: &mut [f32; 4],
    uv: &mut [f32; 4],
    clip_x: Option<(usize, usize)>,
) -> bool {
    let Some((clip_x0, clip_x1)) = clip_x else {
        return rect[2] > 0.0;
    };
    let old_x = rect[0];
    let old_w = rect[2];
    if old_w <= 0.0 {
        return false;
    }
    let x0 = old_x.max(clip_x0 as f32);
    let x1 = (old_x + old_w).min(clip_x1 as f32);
    if x1 <= x0 {
        return false;
    }
    let left = (x0 - old_x) / old_w;
    let kept = (x1 - x0) / old_w;
    uv[0] += uv[2] * left;
    uv[2] *= kept;
    rect[0] = x0;
    rect[2] = x1 - x0;
    true
}

#[cfg(any(d3d11_arm, all(test, target_os = "macos")))]
#[inline]
pub(crate) fn f16_bits_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exp = (bits >> 10) & 0x1f;
    let man = u32::from(bits & 0x3ff);
    let mag = match (exp, man) {
        (0, 0) => 0.0f32,
        (0, m) => (m as f32) * (-24f32).exp2(),
        (0x1f, 0) => f32::INFINITY,
        (0x1f, _) => f32::NAN,
        (e, m) => (1.0 + m as f32 / 1024.0) * f32::from(e as i16 - 15).exp2(),
    };
    sign * mag
}

#[inline]
fn fg_walks(
    input: &RenderInput,
    r: usize,
) -> (aterm_render::InkWalk<'_>, aterm_render::CharFgWalk<'_>) {
    (
        aterm_render::InkWalk::new(aterm_render::sparse_row_slice(&input.ink, r)),
        aterm_render::CharFgWalk::new(aterm_render::sparse_row_slice(&input.char_fg, r)),
    )
}

#[inline]
fn cell_base_fg(
    ink_walk: &mut aterm_render::InkWalk<'_>,
    char_fg_walk: &mut aterm_render::CharFgWalk<'_>,
    c: usize,
    cell: &aterm_core::terminal::RenderCell,
) -> u32 {
    match ink_walk.at(c as u16) {
        Some(ink) => aterm_render::rgb_to_u32(ink),
        None => char_fg_walk
            .at(c as u16)
            .unwrap_or_else(|| aterm_render::rgb_to_u32(cell.fg)),
    }
}

#[inline]
const fn rgb4([r, g, b]: [u8; 3]) -> [u8; 4] {
    [r, g, b, 255]
}

#[inline]
const fn rgb4_u32(c: u32) -> [u8; 4] {
    rgb4([(c >> 16) as u8, (c >> 8) as u8, c as u8])
}

#[inline]
const fn glow8(q: &aterm_render::GlowQuad) -> [u8; 8] {
    [
        (q.color >> 16) as u8,
        (q.color >> 8) as u8,
        q.color as u8,
        q.alpha,
        (q.color2 >> 16) as u8,
        (q.color2 >> 8) as u8,
        q.color2 as u8,
        q.alpha2,
    ]
}

#[inline]
const fn with_over_cap(mut inst: RainGlowInstance, color: u32) -> RainGlowInstance {
    inst.color[3] = (color >> 24) as u8;
    inst
}

fn glyph_windows(
    cpu: &mut Renderer,
    input: &RenderInput,
    scope: &RepaintScope,
    grid_top: usize,
    ch: usize,
    rows: usize,
    out: &mut Vec<Option<(i32, i32)>>,
) {
    const OPEN: (i32, i32) = (i32::MIN, i32::MAX);
    out.clear();
    let RepaintScope::Dirty(dirty) = scope else {
        out.resize(rows, Some(OPEN));
        return;
    };
    let top = |r: usize| i32::try_from(grid_top + r * ch).unwrap_or(i32::MAX);
    let dirty_at = |r: usize| dirty.get(r).copied().unwrap_or(false);
    out.extend((0..rows).map(|r| {
        let above = r > 0 && dirty_at(r - 1);
        if dirty_at(r) {
            Some(if r == 0 || above {
                OPEN
            } else {
                (top(r), i32::MAX)
            })
        } else if above && cpu.row_may_ink_above(input, r) {
            Some((top(r - 1), top(r)))
        } else {
            None
        }
    }));
}

#[inline]
fn clip_rows(scale: aterm_render::Scale, (y0, y1): (i32, i32)) -> aterm_render::Scale {
    aterm_render::Scale {
        clip_y0: scale.clip_y0.max(y0),
        clip_y1: scale.clip_y1.min(y1),
        ..scale
    }
}

type FrameLoadScissor = (crate::device_layer::FrameLoad, Option<(u32, u32, u32, u32)>);

#[allow(
    clippy::too_many_arguments,
    reason = "the frame-geometry tuple of one seam"
)]
fn frame_load_and_scissor(
    scope: &RepaintScope,
    recreate: bool,
    dirty_band: Option<(usize, usize)>,
    grid_top: usize,
    ch: usize,
    rows: usize,
    (w, h): (u32, u32),
    clear: crate::device_layer::ClearColor4,
) -> FrameLoadScissor {
    match &scope {
        RepaintScope::Dirty(_) if !recreate => match dirty_band {
            Some((f, last)) => {
                let y0 = if f == 0 {
                    0
                } else {
                    ((grid_top + f * ch) as u32).min(h)
                };
                let y1 = if last + 1 >= rows {
                    h
                } else {
                    ((grid_top + (last + 1) * ch) as u32).min(h)
                };
                (
                    crate::device_layer::FrameLoad::Load,
                    Some((0u32, y0, w, y1.saturating_sub(y0))),
                )
            }
            None => (crate::device_layer::FrameLoad::Load, Some((0, 0, 0, 0))),
        },
        _ => {
            debug_assert!(
                matches!(scope, RepaintScope::Full),
                "scissored Load requires the prior frame resident (no recreate)"
            );
            (crate::device_layer::FrameLoad::Clear(clear), None)
        }
    }
}

#[inline]
fn theme_color_alpha(c: u32, a: f64) -> crate::device_layer::ClearColor4 {
    fn s2l(b: u32) -> f64 {
        let c = b as f64 / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    crate::device_layer::ClearColor4 {
        r: s2l((c >> 16) & 0xff),
        g: s2l((c >> 8) & 0xff),
        b: s2l(c & 0xff),
        a,
    }
}

/// A GPU renderer for a test, on `family` (the system default for `None`).
#[cfg(test)]
pub(crate) fn gpu_renderer_for_test(family: Option<&str>, px: f32, theme: Theme) -> GpuRenderer {
    let ctx = GpuContext::new()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU: {e}")));
    let cpu = Renderer::from_system_with_family(family, px, theme)
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
    GpuRenderer::from_parts(ctx, cpu, family.map(String::from), theme)
        .expect("the GPU renderer assembles on an acquired device")
}

#[cfg(test)]
mod tests {
    use aterm_render::{FaceId, GlowQuad, GlyphClass, StyleBits};

    use super::{FRAME_GROUPS, GROUP_SRGB, WindowGpu};

    #[test]
    fn a_minimized_window_parks_all_but_a_captures_present() {
        let mut win = WindowGpu::new();
        assert!(!win.parks_while_minimized(), "a visible window presents");
        win.set_occluded_hint(true);
        assert!(win.parks_while_minimized(), "a minimized one parks");
        win.set_capture_present(true);
        assert!(!win.parks_while_minimized(), "but presents for a capture");
        win.set_capture_present(false);
        assert!(win.parks_while_minimized(), "and parks again after it");
        win.set_occluded_hint(false);
        win.set_capture_present(true);
        assert!(!win.parks_while_minimized());
    }

    #[test]
    fn band_shift_ops_states_the_staged_move_law() {
        use super::{BandShift, band_shift_ops};
        assert_eq!(
            band_shift_ops(10, 30, 3),
            Some(BandShift {
                src_y: 13,
                dst_y: 10,
                moved: 17
            })
        );
        assert_eq!(
            band_shift_ops(10, 30, -3),
            Some(BandShift {
                src_y: 10,
                dst_y: 13,
                moved: 17
            })
        );
        assert_eq!(band_shift_ops(10, 30, 0), None);
        assert_eq!(band_shift_ops(10, 10, 5), None);
        assert_eq!(band_shift_ops(30, 10, 5), None);
        assert_eq!(band_shift_ops(10, 30, 20), None);
        assert_eq!(band_shift_ops(10, 30, -20), None);
        assert_eq!(
            band_shift_ops(0, 8, 7),
            Some(BandShift {
                src_y: 7,
                dst_y: 0,
                moved: 1
            })
        );
    }

    #[test]
    fn sdr_glow_attack_is_owned_per_window() {
        let now = aterm_time::Instant::now();
        let mut a = WindowGpu::new();
        let mut b = WindowGpu::new();

        let a_first = a.advance_sdr_glow_budget(true, 0.30, now);
        let a_second =
            a.advance_sdr_glow_budget(true, 0.30, now + std::time::Duration::from_millis(16));
        assert!(a_first > 0.0 && a_second > a_first);

        assert_eq!(b.advance_sdr_glow_budget(false, 0.30, now), 0.0);
        assert_eq!(
            a.sdr_glow_level, a_second,
            "an empty present in window B cannot reset window A's crown"
        );
        assert!(a.sdr_glow_level_at.is_some());

        let b_first = b.advance_sdr_glow_budget(true, 0.30, now);
        assert!(
            (b_first - a_first).abs() < f32::EPSILON,
            "window B begins its own attack instead of inheriting A's level"
        );
        assert_eq!(a.advance_sdr_glow_budget(false, 0.30, now), 0.0);
        assert_eq!(
            b.sdr_glow_level, b_first,
            "resetting window A cannot disturb window B"
        );
    }

    #[test]
    fn frame_passes_coalesce_maximally_without_reordering() {
        for bits in 0u32..(1 << FRAME_GROUPS) {
            let mut enabled = [false; FRAME_GROUPS];
            for (g, e) in enabled.iter_mut().enumerate() {
                *e = bits & (1 << g) != 0;
            }
            let (pass_of, pass_srgb, passes) = super::coalesce_frame_passes(&enabled, &GROUP_SRGB);
            let live: Vec<usize> = (0..FRAME_GROUPS).filter(|&g| enabled[g]).collect();
            assert_eq!(
                passes,
                live.windows(2)
                    .filter(|w| GROUP_SRGB[w[0]] != GROUP_SRGB[w[1]])
                    .count()
                    + usize::from(!live.is_empty()),
                "pass count must be exactly one plus the number of VIEW CHANGES \
                 between consecutive live groups (bits {bits:#010b})"
            );
            for &g in &live {
                assert_eq!(
                    pass_srgb[pass_of[g]], GROUP_SRGB[g],
                    "group {g} must be emitted into a pass attaching ITS view"
                );
            }
            for w in live.windows(2) {
                let (a, b) = (w[0], w[1]);
                if GROUP_SRGB[a] == GROUP_SRGB[b] {
                    assert_eq!(
                        pass_of[a], pass_of[b],
                        "same-view neighbours {a}/{b} must share one pass — an \
                         unmerged boundary is a whole-framebuffer tile load+store"
                    );
                } else {
                    assert_eq!(
                        pass_of[b],
                        pass_of[a] + 1,
                        "a view change must advance by exactly one pass, never \
                         reorder ({a} -> {b})"
                    );
                }
            }
            for g in 0..FRAME_GROUPS {
                if !enabled[g] {
                    assert_eq!(
                        pass_of[g],
                        usize::MAX,
                        "a disabled group must match no pass index"
                    );
                }
            }
        }
    }

    #[test]
    fn the_unorm_ablation_drops_or_thins_exactly_the_one_one_groups() {
        let counts = [Some(7u32); STREAM_COUNT];
        let ctx = |ablation| FramePlanCtx {
            has_image_plane: true,
            has_deco: true,
            has_wallpaper: true,
            cursor_opaque: true,
            ablation,
            under_opaque: false,
        };
        let passes_of = |plan: &[DrawItem]| super::plan_passes(plan).2;
        let plan = |ablation| {
            let mut plan = Vec::new();
            let record = build_frame_plan(&mut plan, &counts, ctx(ablation));
            let unorm: Vec<&DrawItem> = plan
                .iter()
                .filter(|i| !GROUP_SRGB[i.group as usize])
                .collect();
            assert_eq!(
                record,
                PlanUnorm {
                    ablation,
                    draws: unorm.len() as u32,
                    instances: unorm.iter().map(|i| u64::from(i.count)).sum(),
                    route: UnormRoute::Split,
                    under_store: false,
                }
            );
            plan
        };
        let (full, dropped, thinned) = (
            plan(UnormAblation::Off),
            plan(UnormAblation::Drop),
            plan(UnormAblation::Thin),
        );
        let unorm = |i: &DrawItem| !GROUP_SRGB[i.group as usize];
        assert!(
            full.iter().any(unorm),
            "the full plan draws One/One streams, or there is nothing to ablate"
        );
        let kept: Vec<(u8, StreamId)> = full
            .iter()
            .filter(|i| !unorm(i))
            .map(|i| (i.group, i.stream))
            .collect();
        let got: Vec<(u8, StreamId)> = dropped.iter().map(|i| (i.group, i.stream)).collect();
        assert_eq!(got, kept, "Drop removes only Unorm items, in order");
        assert_eq!(
            passes_of(&dropped),
            1,
            "all-sRGB groups coalesce to one pass"
        );
        assert!(
            passes_of(&full) > passes_of(&dropped),
            "the Unorm groups cost passes: {} vs {}",
            passes_of(&full),
            passes_of(&dropped)
        );
        assert_eq!(
            thinned
                .iter()
                .map(|i| (i.group, i.stream))
                .collect::<Vec<_>>(),
            full.iter().map(|i| (i.group, i.stream)).collect::<Vec<_>>(),
            "Thin keeps every item, in order"
        );
        assert_eq!(passes_of(&thinned), passes_of(&full), "and so every pass");
        for (thin, item) in thinned.iter().zip(&full) {
            let want = if unorm(item) { 1 } else { item.count };
            assert_eq!(thin.count, want, "{:?}", item.stream);
        }
    }

    /// The fetch route moves exactly the Unorm-law draws onto their twins, in order and count, so a
    /// full plan is one pass.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_fetch_route_draws_every_unorm_law_row_through_its_twin_in_one_pass() {
        let counts = [Some(7u32); STREAM_COUNT];
        let ctx = FramePlanCtx {
            has_image_plane: true,
            has_deco: true,
            has_wallpaper: true,
            cursor_opaque: false,
            ablation: UnormAblation::Off,
            under_opaque: false,
        };
        let (mut split, mut fetch) = (Vec::new(), Vec::new());
        let mut split_record = build_frame_plan(&mut split, &counts, ctx);
        route_frame_plan(&mut split, &mut split_record, UnormRoute::Split);
        let mut fetch_record = build_frame_plan(&mut fetch, &counts, ctx);
        route_frame_plan(&mut fetch, &mut fetch_record, UnormRoute::Fetch);
        assert_eq!(
            fetch_record,
            PlanUnorm {
                route: UnormRoute::Fetch,
                ..split_record
            }
        );
        assert!(
            split_record.draws > 0,
            "the full plan holds Unorm-law draws"
        );
        let key = |i: &DrawItem| (i.group, i.pipe, i.atlas, i.stream, i.count);
        assert_eq!(
            fetch.iter().map(key).collect::<Vec<_>>(),
            split.iter().map(key).collect::<Vec<_>>(),
            "the route changes no draw but its view and pipeline"
        );
        let mut twins = std::collections::BTreeSet::new();
        for item in &fetch {
            let unorm = !GROUP_SRGB[item.group as usize];
            assert_eq!(item.fetch, unorm, "{:?}", item.stream);
            assert!(item.srgb(), "{:?} draws in the sRGB view", item.stream);
            if unorm {
                let row = item.pipe.row();
                let twin = pipeline_table::FetchTwin::of(row)
                    .unwrap_or_else(|| panic!("{row:?} draws by the Unorm law but has no twin"));
                assert_eq!(twin.row(), row);
                let (spec, base) = (twin.spec(), row.spec());
                assert_eq!(spec.target, pipeline_table::TargetRole::OffscreenSrgb);
                assert!(spec.blend.is_none(), "{twin:?} blends in its fragment");
                assert_eq!(
                    (spec.vs, spec.write_mask, spec.library),
                    (base.vs, base.write_mask, base.library),
                    "{twin:?} keeps its row's vertex stage, mask and library"
                );
                twins.insert(twin);
            }
        }
        assert_eq!(
            twins.into_iter().collect::<Vec<_>>(),
            pipeline_table::FetchTwin::ALL.to_vec(),
            "every twin stands in for a draw of the full plan"
        );
        assert!(!split.iter().any(|i| i.fetch), "the split routes nothing");
        assert!(super::plan_passes(&split).2 > 1);
        assert_eq!(
            super::plan_passes(&fetch).2,
            1,
            "the fetch route is one pass"
        );
    }

    /// Route (1): an opaque Exact Under fx stores at the end of the base pass only while no cursor
    /// glow draws under the text.
    #[test]
    fn an_opaque_under_fx_stores_in_the_base_pass_only_without_a_glow_under() {
        let ctx = |under_opaque| FramePlanCtx {
            has_image_plane: false,
            has_deco: false,
            has_wallpaper: false,
            cursor_opaque: true,
            ablation: UnormAblation::Off,
            under_opaque,
        };
        let mut counts = [None; STREAM_COUNT];
        for id in [
            StreamId::Bg,
            StreamId::Trail,
            StreamId::FxUnder,
            StreamId::FireAdd,
            StreamId::Glyph,
        ] {
            counts[id as usize] = Some(3);
        }
        let order = |plan: &[DrawItem]| plan.iter().map(|i| i.stream).collect::<Vec<_>>();
        let under = |plan: &[DrawItem]| {
            let item = plan
                .iter()
                .find(|i| i.stream == StreamId::FxUnder)
                .expect("the Under fx draws");
            (usize::from(item.group), item.pipe)
        };
        let (mut kept, mut stored) = (Vec::new(), Vec::new());
        let kept_rec = build_frame_plan(&mut kept, &counts, ctx(false));
        let stored_rec = build_frame_plan(&mut stored, &counts, ctx(true));
        assert!(!kept_rec.under_store && stored_rec.under_store);
        assert_eq!(under(&kept), (G_GLOW_UNDER, DrawPipe::Fx));
        assert_eq!(under(&stored), (G_BASE_BG, DrawPipe::FxStore));
        assert_eq!(
            order(&stored),
            order(&kept),
            "the store draws where the CPU draws the layer"
        );
        assert_eq!(stored_rec.draws + 1, kept_rec.draws);
        assert_eq!(
            (super::plan_passes(&kept).2, super::plan_passes(&stored).2),
            (3, 3),
            "the fire still opens its own group"
        );
        counts[StreamId::FireAdd as usize] = None;
        build_frame_plan(&mut kept, &counts, ctx(false));
        build_frame_plan(&mut stored, &counts, ctx(true));
        assert_eq!(
            (super::plan_passes(&kept).2, super::plan_passes(&stored).2),
            (3, 1),
            "with the Under group empty its passes merge away"
        );

        counts[StreamId::GlowUnder as usize] = Some(1);
        let mut glow = Vec::new();
        let glow_rec = build_frame_plan(&mut glow, &counts, ctx(true));
        assert!(!glow_rec.under_store);
        assert_eq!(
            under(&glow),
            (G_GLOW_UNDER, DrawPipe::Fx),
            "a cursor glow under the text keeps the Under fx after it"
        );

        counts[StreamId::GlowUnder as usize] = None;
        counts[StreamId::FxUnder as usize] = None;
        let mut none = Vec::new();
        assert!(
            !build_frame_plan(&mut none, &counts, ctx(true)).under_store,
            "a frame without Under fx stores nothing"
        );
    }

    #[test]
    fn the_unorm_route_seam_reads_its_modes() {
        assert_eq!(UnormRoute::from_seam("split"), UnormRoute::Split);
        assert_eq!(UnormRoute::from_seam("fetch"), UnormRoute::Fetch);
        assert_eq!(UnormRoute::from_seam(""), UnormRoute::Fetch);
        for route in [UnormRoute::Split, UnormRoute::Fetch] {
            assert_eq!(UnormRoute::from_seam(route.name()), route);
        }
    }

    #[test]
    fn the_ablation_seam_reads_its_modes() {
        assert_eq!(UnormAblation::from_seam("drop"), UnormAblation::Drop);
        assert_eq!(UnormAblation::from_seam("1"), UnormAblation::Drop);
        assert_eq!(UnormAblation::from_seam("thin"), UnormAblation::Thin);
        assert_eq!(UnormAblation::from_seam("0"), UnormAblation::Off);
        assert_eq!(UnormAblation::from_seam(""), UnormAblation::Off);
        for mode in [UnormAblation::Off, UnormAblation::Drop, UnormAblation::Thin] {
            let back = UnormAblation::from_seam(mode.name());
            assert_eq!(back, mode, "{}", mode.name());
        }
    }

    #[test]
    fn union_rect_opt_is_conservative_and_clamped() {
        let dims = (100, 50);
        assert_eq!(super::union_rect_opt(None, Some([0, 0, 1, 1]), dims), None);
        assert_eq!(super::union_rect_opt(Some([0, 0, 1, 1]), None, dims), None);
        assert_eq!(
            super::union_rect_opt(Some([0, 0, 0, 0]), Some([2, 3, 4, 5]), dims),
            Some([2, 3, 4, 5])
        );
        assert_eq!(
            super::union_rect_opt(Some([2, 3, 4, 5]), Some([9, 9, 9, 9]), dims),
            Some([2, 3, 4, 5])
        );
        assert_eq!(
            super::union_rect_opt(Some([1, 2, 3, 4]), Some([10, 0, 200, 80]), dims),
            Some([1, 0, 100, 50])
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn copy_src_is_armed_only_by_a_live_tap_on_a_capable_surface() {
        let wants = super::GpuRenderer::tap_wants_copy_src;
        assert!(!wants(false, false, false), "idle: no readback, no cost");
        assert!(
            wants(true, false, false),
            "a live recording needs the readback"
        );
        assert!(
            wants(false, true, false),
            "a ONE-SHOT still needs it just as much"
        );
        assert!(
            wants(true, true, false),
            "a still taken mid-recording needs it too"
        );
        assert!(wants(false, false, true), "a recent still holds it");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn polled_snapshots_inside_the_hold_reconfigure_the_swapchain_once() {
        struct Swapchain {
            copyable: bool,
            reconfigures: u32,
        }
        impl Swapchain {
            fn present(
                &mut self,
                hold: super::CopySrcHold,
                snapshot: bool,
                at: aterm_time::Instant,
            ) {
                let want = super::GpuRenderer::tap_wants_copy_src(false, snapshot, hold.holds(at));
                if want != self.copyable {
                    self.copyable = want;
                    self.reconfigures += 1;
                }
            }
        }
        let frame = std::time::Duration::from_millis(16);
        let t0 = aterm_time::Instant::now();
        let mut hold = super::CopySrcHold::default();
        let mut swapchain = Swapchain {
            copyable: false,
            reconfigures: 0,
        };
        let mut capture = |swapchain: &mut Swapchain, at: aterm_time::Instant| {
            hold.arm(at);
            swapchain.present(hold, true, at);
            swapchain.present(hold, false, at + frame);
            hold
        };

        capture(&mut swapchain, t0);
        let hold = capture(&mut swapchain, t0 + std::time::Duration::from_millis(500));
        assert_eq!(
            swapchain.reconfigures, 1,
            "two polled captures: one reconfigure"
        );
        assert!(swapchain.copyable);

        let expired = t0 + std::time::Duration::from_millis(500) + super::SNAPSHOT_COPY_SRC_HOLD;
        swapchain.present(hold, false, expired);
        assert!(!swapchain.copyable, "past the hold the bit drops");
        capture(&mut swapchain, expired + frame);
        assert_eq!(
            swapchain.reconfigures, 3,
            "a capture after the hold reconfigures again"
        );
    }

    use super::*;
    use aterm_core::terminal::Terminal;
    use aterm_render::Theme;

    #[test]
    fn present_crop_normalizes_only_after_logical_validation() {
        let crop = PresentCrop {
            source_y: 3,
            height: 92,
        };
        assert_eq!(
            normalize_present_crop(crop, 100, 100),
            Some(crop),
            "an entirely resident crop is unchanged"
        );
        assert_eq!(
            normalize_present_crop(crop, 100, 64),
            Some(PresentCrop {
                source_y: 3,
                height: 61,
            }),
            "a valid oversized-grid crop is trimmed to the resident top prefix"
        );

        for malformed in [
            PresentCrop {
                source_y: 0,
                height: 0,
            },
            PresentCrop {
                source_y: 100,
                height: 1,
            },
            PresentCrop {
                source_y: 99,
                height: 2,
            },
            PresentCrop {
                source_y: u32::MAX,
                height: u32::MAX,
            },
        ] {
            assert_eq!(
                normalize_present_crop(malformed, 100, 64),
                None,
                "malformed logical crop {malformed:?} must be rejected"
            );
        }
        assert_eq!(
            normalize_present_crop(
                PresentCrop {
                    source_y: 70,
                    height: 10,
                },
                100,
                64,
            ),
            None,
            "a logically valid interval wholly below the resident prefix is empty"
        );
    }

    #[test]
    fn visible_source_scissor_preserves_signed_odd_crop_intersections() {
        let uniform = present_blit_uniform(
            false,
            None,
            Some(PresentCrop {
                source_y: 1,
                height: 6,
            }),
            10,
            11,
            7,
            8,
            0,
        );
        if cfg!(target_os = "linux") {
            assert_eq!(uniform.content_off, [-2.0, 0.0]);
            assert_eq!(visible_source_scissor(&uniform, 7, 8), Some((0, 1, 7, 6)));
        } else {
            assert_eq!(uniform.content_off, [-2.0, -2.0]);
            assert_eq!(visible_source_scissor(&uniform, 7, 8), Some((0, 0, 7, 5)));
        }

        let wholly_above = present_blit_uniform(
            false,
            None,
            Some(PresentCrop {
                source_y: 0,
                height: 1,
            }),
            10,
            11,
            1,
            1,
            0,
        );
        if cfg!(target_os = "linux") {
            assert_eq!(
                visible_source_scissor(&wholly_above, 1, 1),
                Some((0, 0, 1, 1)),
                "the pinned top row stays on screen"
            );
        } else {
            assert_eq!(
                visible_source_scissor(&wholly_above, 1, 1),
                None,
                "a fully clipped crown must not issue a zero/underflowed scissor"
            );
        }
    }

    #[test]
    fn crown_scissor_clips_both_scroll_directions_to_independent_destination_bounds() {
        let mut uniform = present_blit_uniform(false, None, None, 40, 60, 40, 60, 0);
        let up = BandShift {
            src_y: 13,
            dst_y: 10,
            moved: 17,
        };
        let down = BandShift {
            src_y: 10,
            dst_y: 13,
            moved: 17,
        };
        assert_eq!(
            crown_band_scissor(&uniform, None, 40, 60),
            Some((0, 0, 40, 60))
        );
        assert_eq!(
            crown_band_scissor(&uniform, Some(up), 40, 60),
            Some((0, 10, 40, 17))
        );
        assert_eq!(
            crown_band_scissor(&uniform, Some(down), 40, 60),
            Some((0, 13, 40, 17))
        );

        uniform.visible_y = 15.0;
        uniform.visible_h = 5.0;
        for shift in [up, down] {
            assert_eq!(
                crown_band_scissor(&uniform, Some(shift), 40, 60),
                Some((0, 15, 40, 5))
            );
        }
        uniform.visible_y = 30.0;
        uniform.visible_h = 10.0;
        for shift in [up, down] {
            assert_eq!(crown_band_scissor(&uniform, Some(shift), 40, 60), None);
        }

        uniform.content_off = [-3.0, -12.0];
        uniform.visible_y = 0.0;
        uniform.visible_h = 60.0;
        assert_eq!(
            crown_band_scissor(&uniform, Some(up), 30, 30),
            Some((0, 0, 30, 15))
        );
        assert_eq!(
            crown_band_scissor(&uniform, Some(down), 30, 30),
            Some((0, 1, 30, 17))
        );
        assert_eq!(
            crown_band_scissor(&uniform, Some(up), 30, 3),
            Some((0, 0, 30, 3))
        );
        assert_eq!(
            crown_band_scissor(&uniform, Some(down), 30, 3),
            Some((0, 1, 30, 2))
        );
    }

    #[test]
    fn crown_band_scissor_and_present_uniforms_track_fractional_scroll_band() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.set_hdr_glow(true);
        let (rows, cols) = (6usize, 12usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"hello");
        let mut input = term.cell_frame(rows, cols);
        input.grid_top_row = 1;
        input.grid_bot_row = rows - 1;
        input.cursor_glow_add.push(GlowQuad {
            x: 8,
            y: 24,
            w: 8,
            h: 16,
            color: 0xffffff,
            alpha: 0,
            color2: 0xffffff,
            alpha2: 0,
            row: 2,
        });
        let (fw, fh) = gpu.frame_size(rows, cols);
        let (fw, fh) = (fw as u32, fh as u32);
        let (dw, dh) = (fw + 10, fh + 6);
        let mut win = WindowGpu::new();
        win.edr_max = 2.0;

        input.scroll_frac_px = 0;
        let base = gpu.present_uniforms(
            &mut win,
            &input,
            false,
            None,
            None,
            (fw, fh),
            (dw, dh),
            true,
            false,
            false,
        );
        let base_crown = base.crown.expect("HDR present with glow emits a crown");
        assert_eq!(base_crown.content_off, base.want.content_off);

        input.scroll_frac_px = 3;
        let shifted = gpu.present_uniforms(
            &mut win,
            &input,
            false,
            None,
            None,
            (fw, fh),
            (dw, dh),
            true,
            false,
            false,
        );
        let shifted_crown = shifted.crown.expect("HDR present with glow emits a crown");
        assert_eq!(
            shifted_crown.content_off,
            [base.want.content_off[0], base.want.content_off[1] - 3.0],
            "crown placement must translate with the sub-row scroll band"
        );
        let (band_y0, band_y1) = aterm_render::scroll_translate::grid_band_px(
            gpu.cpu.grid_top(),
            gpu.cell_size().1,
            input.grid_top_row,
            input.grid_bot_row,
            fh as usize,
        );
        let expected_shift =
            band_shift_ops(band_y0, band_y1, 3).expect("nonzero frac inside non-empty band");
        assert_eq!(
            shifted.crown_scissor,
            crown_band_scissor(&shifted.want, Some(expected_shift), dw, dh),
            "crown scissor must clip to the moved grid band"
        );
    }

    #[test]
    fn atlas_texel_bytes_match_cpu_glyph_bytes_exactly() {
        let Some(mut cpu) = Renderer::from_system(18.0, Theme::default()) else {
            aterm_spec::host::could_not_run("no system monospace font");
        };

        cpu.debug_block_on_lazy_fallbacks();
        let keys: Vec<GlyphKey> = "RXab\u{65E5}\u{672C}"
            .chars()
            .map(|c| cpu.glyph_key(c))
            .collect();
        assert_atlas_holds_the_cpu_bytes(&mut cpu, keys, 5);
    }

    #[test]
    fn fitted_fallback_styles_reach_the_atlas_as_the_cpu_fit_them() {
        const PRIMARY: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-render/assets/bundled/CascadiaMonoPL-Regular.ttf"
        );
        let mut cpu = Renderer::from_system_with_family(Some(PRIMARY), 18.0, Theme::default())
            .unwrap_or_else(|| panic!("{PRIMARY} does not load"));
        cpu.debug_block_on_lazy_fallbacks();
        let warning = cpu.glyph_key('\u{26A0}');
        if !matches!(
            warning.source,
            FaceId::Fallback
                | FaceId::SymbolFallback
                | FaceId::RuntimeFallback
                | FaceId::DisplayMix
        ) || warning.glyph_class != GlyphClass::Mono
        {
            aterm_spec::host::could_not_run(format!(
                "this host serves U+26A0 from no mono fallback face ({warning:?})"
            ));
        }
        let (cell_w, cell_h) = cpu.cell_size();
        let mut keys = Vec::new();
        for style in [
            StyleBits::REGULAR,
            StyleBits::BOLD,
            StyleBits::ITALIC,
            StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
        ] {
            let key = cpu.glyph_key_styled('\u{26A0}', style);
            assert_eq!(key.cell_span, 1);
            let img = cpu.glyph_image(key);
            assert!(
                img.xmin() >= 0 && img.xmin() + img.width() as i32 <= cell_w as i32,
                "{style:?} warning escaped its one-cell CPU mask before atlas packing"
            );
            assert!(img.height() <= cell_h);
            assert_eq!(img.advance(), cell_w as f32);
            keys.push(key);
        }
        assert_atlas_holds_the_cpu_bytes(&mut cpu, keys, 1);
    }

    fn assert_atlas_holds_the_cpu_bytes(
        cpu: &mut Renderer,
        mut keys: Vec<GlyphKey>,
        at_least: usize,
    ) {
        keys.sort_unstable();
        keys.dedup();
        assert!(
            keys.len() >= at_least,
            "the fixture contributes at least {at_least} distinct glyphs, got {}",
            keys.len()
        );

        let atlas = build_atlas(cpu, &keys, u32::MAX);
        for &key in &keys {
            let slot = atlas
                .map
                .get(&key)
                .expect("every requested glyph gets a slot");
            let img = cpu.glyph_image(key).clone();
            let GlyphImage::Mono {
                width,
                height,
                bytes,
                ..
            } = &img
            else {
                panic!("char keys rasterize as Mono");
            };
            if *width == 0 || *height == 0 {
                assert_eq!(
                    (slot.gw, slot.gh),
                    (0, 0),
                    "empty glyph must pack as an empty slot"
                );
                continue;
            }
            assert_eq!(
                (slot.gw as usize, slot.gh as usize),
                (*width, *height),
                "slot size differs from CPU bitmap for {:?}",
                key.chr()
            );
            for j in 0..slot.gh {
                for i in 0..slot.gw {
                    let atlas_byte =
                        atlas.data[((slot.ay + j) * atlas.width + slot.ax + i) as usize];
                    let cpu_byte = bytes[(j * slot.gw + i) as usize];
                    assert_eq!(
                        atlas_byte,
                        cpu_byte,
                        "atlas texel ({i},{j}) of {:?} differs from the CPU cache byte",
                        key.chr()
                    );
                }
            }
        }
    }

    #[test]
    fn vs15_and_bare_primary_glyphs_pack_distinct_atlas_entries() {
        let primary_bytes = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
        let mut primary = Renderer::from_bytes(primary_bytes, 18.0, Theme::default())
            .expect("fixture primary renderer");
        let bolt = '\u{26A1}';
        let bare_primary = primary.glyph_key(bolt);
        assert_eq!(bare_primary.source, FaceId::Primary);
        assert_eq!(bare_primary.glyph_class, GlyphClass::MonoGid);
        assert_eq!(bare_primary.cell_span, 0);

        let mut primary_term = Terminal::new(1, 3);
        primary_term.process("⚡\u{FE0E}".as_bytes());
        let primary_input = primary_term.cell_frame(1, 3);
        let text_primary = primary.resolve_cell_key_for_span(None, &primary_input.cells[0][0], 1);
        assert_eq!(text_primary.source, bare_primary.source);
        assert_eq!(text_primary.glyph_class, bare_primary.glyph_class);
        assert_eq!(text_primary.ch_or_id, bare_primary.ch_or_id);
        assert_eq!(text_primary.cell_span, 1);
        assert_ne!(text_primary, bare_primary);

        let primary_atlas = build_atlas(&mut primary, &[bare_primary, text_primary], u32::MAX);
        let bare_slot = primary_atlas
            .map
            .get(&bare_primary)
            .expect("bare primary key reaches mono atlas");
        let text_slot = primary_atlas
            .map
            .get(&text_primary)
            .expect("VS15 primary key reaches mono atlas");
        assert_ne!(
            (bare_slot.ax, bare_slot.ay),
            (text_slot.ax, text_slot.ay),
            "bare and VS15 primary rasters must occupy distinct atlas entries"
        );
    }

    #[test]
    fn atlas_place_degrades_glyph_wider_than_atlas() {
        let px_q = GlyphKey::quantize_px(18.0);
        let ow = (ATLAS_WIDTH as usize) * 2 + 100;
        let overwide = GlyphImage::Mono {
            width: ow,
            height: 10,
            xmin: 3,
            ymin: 7,
            advance: 12.0,
            bytes: vec![0xAB; ow * 10],
        };

        let mut mono = Atlas {
            kind: AtlasKind::Mono,
            width: ATLAS_WIDTH,
            height: 1,
            data: vec![0u8; ATLAS_WIDTH as usize],
            map: FxHashMap::default(),
            px: 0,
            py: 0,
            shelf_h: 0,
            write_gen: 0,
        };
        let key = GlyphKey::mono_char(
            aterm_render::FaceId::Primary,
            'W',
            aterm_render::StyleBits::REGULAR,
            px_q,
        );
        assert!(
            mono.place(key, &overwide).is_none(),
            "an over-wide glyph must not claim a packed band"
        );
        assert_eq!(
            (mono.px, mono.py, mono.shelf_h),
            (0, 0, 0),
            "cursor unmoved"
        );
        let slot = *mono.map.get(&key).expect("Mono records every key");
        assert_eq!((slot.gw, slot.gh), (0, 0), "degraded slot is zero-sized");
        assert_eq!(
            (slot.xmin, slot.ymin),
            (3, 7),
            "placement offsets preserved"
        );
        let before = mono.data.clone();
        mono.blit(&overwide, &slot);
        assert_eq!(mono.data, before, "zero-slot blit writes nothing");

        let mut color = Atlas {
            kind: AtlasKind::Color,
            width: ATLAS_WIDTH,
            height: 1,
            data: vec![0u8; (ATLAS_WIDTH * 4) as usize],
            map: FxHashMap::default(),
            px: 0,
            py: 0,
            shelf_h: 0,
            write_gen: 0,
        };
        let overwide_rgba = GlyphImage::Rgba {
            width: ow,
            height: 10,
            xmin: 0,
            ymin: 0,
            advance: 12.0,
            bytes: vec![0u8; ow * 10 * 4],
        };
        let ckey = GlyphKey::rgba_char(aterm_render::FaceId::Primary, '\u{1F600}', px_q);
        assert!(
            color.place(ckey, &overwide_rgba).is_none(),
            "an over-wide colour glyph must not claim a packed band"
        );
        assert_eq!(
            (color.px, color.py, color.shelf_h),
            (0, 0, 0),
            "cursor unmoved"
        );
        assert!(
            color.map.is_empty(),
            "Color skips an over-wide glyph entirely (no slot)"
        );
    }

    #[test]
    fn set_face_preserves_text_shaping() {
        let theme = Theme::default();
        let mut gpu = crate::renderer::gpu_renderer_for_test(Some("Menlo"), 16.0, theme);
        gpu.set_text_shaping(aterm_render::TextShapingConfig {
            ligature_mode: aterm_types::text_shaping::LigatureMode::Disabled,
            ..Default::default()
        });
        let Some(face) = Renderer::from_system(24.0, theme) else {
            aterm_spec::host::could_not_run("no system monospace font");
        };
        gpu.set_face(face, theme);
        assert_eq!(
            gpu.cpu.text_shaping().ligature_mode,
            aterm_types::text_shaping::LigatureMode::Disabled,
            "shaping must survive the face rebuild, not reset to Enabled"
        );
    }

    /// A prepared face draws the weight it was prepared at: a config reload of
    /// `font_weight`, and the headless build's deferred GPU, install a face that already
    /// carries the config's variation requests. Before, installing it put back the
    /// requests of the face it replaced, so a weight changed in Settings (or a headless
    /// instance's configured weight) never reached the GPU's glyphs.
    #[test]
    fn a_prepared_face_keeps_the_weight_it_was_prepared_at() {
        let theme = Theme::default();
        let mut gpu = crate::renderer::gpu_renderer_for_test(Some("Menlo"), 16.0, theme);
        assert!(gpu.cpu.font_variation_requests().is_empty());
        let Some(mut prepared) = Renderer::from_system(16.0, theme) else {
            aterm_spec::host::could_not_run("no system monospace font");
        };
        let weight = [(aterm_render::variation::WGHT_TAG, 600.0)];
        let _ = prepared.set_font_variations(&weight, 0.0);
        gpu.install_prepared_font(None, prepared, theme);
        assert_eq!(gpu.cpu.font_variation_requests(), weight);
    }

    #[test]
    fn deco_atlas_drops_on_oversized_cell_height_no_abort() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        let (cw, ch) = gpu.cell_size();
        gpu.ensure_deco_atlas(cw, ch);
        assert!(
            gpu.deco_atlas.is_some(),
            "a realistic {cw}x{ch} cell must build the deco atlas (guard must not over-trigger)"
        );
        gpu.ensure_deco_atlas(cw, 10_000);
        assert!(
            gpu.deco_atlas.is_none(),
            "an oversized cell height must DROP the deco atlas (graceful), not abort"
        );
    }

    #[test]
    fn present_prev_is_per_window_no_cross_window_leak() {
        let theme = Theme::default();
        let px = 18.0;
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, px, theme);

        let (rows, cols) = (4usize, 10usize);
        let frame = |bytes: &[u8]| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(b"\x1b[?25l");
            term.process(bytes);
            term.cell_frame(rows, cols)
        };
        let a_frames = [
            frame(b"AAAA"),
            frame(b"AAAA\r\nbbbb"),
            frame(b"AAAA\r\ncccc"),
        ];
        let b_frames = [
            frame(b"ZZZZ"),
            frame(b"ZZZZ\r\nyyyy"),
            frame(b"ZZZZ\r\nxxxx"),
        ];

        let mut win_a = WindowGpu::new();
        let mut win_b = WindowGpu::new();

        let mut last_a = None;
        let mut last_b = None;
        for i in 0..a_frames.len() {
            last_a = Some(gpu.present_input_readback(&mut win_a, &a_frames[i]));
            last_b = Some(gpu.present_input_readback(&mut win_b, &b_frames[i]));
        }
        let got_a = last_a.expect("at least one frame driven");
        let got_b = last_b.expect("at least one frame driven");

        assert!(
            gpu.scissor_taken() > 0,
            "scissor path must be exercised by the changed frames"
        );

        let mut ref_win_a = WindowGpu::new();
        let mut ref_win_b = WindowGpu::new();
        let want_a = gpu.render_input(&mut ref_win_a, &a_frames[a_frames.len() - 1], None);
        let want_b = gpu.render_input(&mut ref_win_b, &b_frames[b_frames.len() - 1], None);

        assert_eq!(
            (got_a.width, got_a.height),
            (want_a.width, want_a.height),
            "window A readback dims must match the reference"
        );
        assert_eq!(
            got_a.pixels, want_a.pixels,
            "window A's interleaved scissored readback must be byte-identical to a fresh full \
             render — a mismatch means window B's prior frame leaked into window A"
        );
        assert_eq!(
            (got_b.width, got_b.height),
            (want_b.width, want_b.height),
            "window B readback dims must match the reference"
        );
        assert_eq!(
            got_b.pixels, want_b.pixels,
            "window B's interleaved scissored readback must be byte-identical to a fresh full \
             render — a mismatch means window A's prior frame leaked into window B"
        );
    }

    #[test]
    fn asymmetric_top_pad_change_invalidates_gpu_present_and_readback_caches() {
        let model = aterm_models_render::asymmetric_pad_layout_model();
        let picked = model
            .successors("PickLayout", &model.init_state())
            .into_iter()
            .find(|state| {
                state["pad"] == 3
                    && state["head"] == 1
                    && state["initial_request"] == 3
                    && state["changed_request"] == 0
            })
            .expect("bounded GPU layout fixture");
        let initial = model.successors("ApplyInitialTop", &picked)[0].clone();
        let primed = model.successors("PrimeLayoutCache", &initial)[0].clone();
        let changed = model.successors("ApplyChangedTop", &primed)[0].clone();
        let decided = model.successors("RenderWithLayoutCache", &changed)[0].clone();
        assert_eq!((decided["cache_hit"], decided["full_repaint"]), (0, 1));

        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.set_pad(picked["pad"] as usize);
        gpu.set_head(picked["head"] as usize);
        gpu.set_pad_top(picked["initial_request"] as usize);
        assert_eq!(gpu.pad_top(), initial["pad_top"] as usize);
        gpu.set_pad(picked["pad"] as usize);
        assert_eq!(
            gpu.pad_top(),
            initial["pad_top"] as usize,
            "the GPU delegate must preserve an asymmetric origin on same-value set_pad"
        );
        let (rows, cols) = (4usize, 12usize);
        let frame_size = gpu.frame_size(rows, cols);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lgrid-origin");
        let input = term.cell_frame(rows, cols);
        let mut win = WindowGpu::new();

        let first = gpu.present_input_readback(&mut win, &input);
        let initial_projection = gpu.project_asymmetric_pad_layout(&win);
        assert_eq!(
            initial_projection.present_cached_grid_top,
            Some(primed["cached_grid_top"] as usize)
        );
        assert_eq!(initial_projection.grid_top, initial["grid_top"] as usize);
        let full_before = gpu.full_repaints();
        gpu.set_pad_top(picked["changed_request"] as usize);
        assert_eq!(gpu.frame_size(rows, cols), frame_size);
        let stale_present = gpu.project_asymmetric_pad_layout(&win);
        assert_eq!(stale_present.grid_top, changed["grid_top"] as usize);
        assert_eq!(
            stale_present.present_cached_grid_top,
            Some(changed["cached_grid_top"] as usize)
        );
        assert_ne!(
            stale_present.present_cached_grid_top,
            Some(stale_present.grid_top)
        );

        let dimension_only_mutant_hit = gpu.frame_size(rows, cols) == frame_size;
        assert!(dimension_only_mutant_hit);
        assert_ne!(
            usize::from(dimension_only_mutant_hit),
            decided["cache_hit"] as usize
        );

        let moved = gpu.present_input_readback(&mut win, &input);
        assert!(
            gpu.full_repaints() > full_before,
            "grid-origin change must force a full GPU repaint"
        );
        let moved_projection = gpu.project_asymmetric_pad_layout(&win);
        assert_eq!(
            moved_projection.present_cached_grid_top,
            Some(changed["grid_top"] as usize)
        );
        let mut reference = WindowGpu::new();
        let fresh = gpu.render_input(&mut reference, &input, None);
        assert_eq!(moved.pixels, fresh.pixels);
        assert_ne!(first.pixels, moved.pixels, "the visible grid must move");

        let mut gate_win = WindowGpu::new();
        gpu.set_pad_top(picked["initial_request"] as usize);
        let gated_first = gpu
            .render_input_cached(&mut gate_win, &input)
            .pixels()
            .to_vec();
        let gate_primed = gpu.project_asymmetric_pad_layout(&gate_win);
        assert_eq!(
            gate_primed.gate_cached_grid_top,
            Some(primed["cached_grid_top"] as usize)
        );
        let hits_before = gpu.gate_hits();
        let _ = gpu.render_input_cached(&mut gate_win, &input);
        assert_eq!(gpu.gate_hits(), hits_before + 1, "identical layout may hit");

        gpu.set_pad_top(picked["changed_request"] as usize);
        let stale_gate = gpu.project_asymmetric_pad_layout(&gate_win);
        assert_eq!(stale_gate.grid_top, changed["grid_top"] as usize);
        assert_eq!(
            stale_gate.gate_cached_grid_top,
            Some(changed["cached_grid_top"] as usize)
        );
        let misses_before = gpu.gate_misses();
        let gated_moved = gpu
            .render_input_cached(&mut gate_win, &input)
            .pixels()
            .to_vec();
        assert_eq!(
            gpu.gate_misses(),
            misses_before + 1,
            "origin change must miss the readback gate"
        );
        assert_eq!(gpu.gate_hits(), hits_before + 1);
        assert_eq!(
            gpu.project_asymmetric_pad_layout(&gate_win)
                .gate_cached_grid_top,
            Some(changed["grid_top"] as usize)
        );
        let mut gate_reference = WindowGpu::new();
        let gate_fresh = gpu.render_input(&mut gate_reference, &input, None);
        assert_eq!(gated_moved, gate_fresh.pixels);
        assert_ne!(gated_first, gated_moved);
    }

    #[test]
    fn bloom_glow_frames_scissor_and_match_full_render() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        assert!(gpu.bloom_enabled(), "precondition: bloom defaults ON");
        gpu.set_shimmer_phase_for_test(Some(0.33));
        gpu.set_pad(14);
        let pad = gpu.pad();
        let (cw, ch) = gpu.cell_size();
        let (rows, cols) = (10usize, 12usize);

        let frame = |bytes: &[u8], glow: Vec<GlowQuad>| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(b"\x1b[?25l");
            term.process(bytes);
            let mut input = term.cell_frame(rows, cols);
            input.cursor_glow_add = glow;
            input
        };
        let quad = |row: usize, x: usize, y: usize, h: usize| GlowQuad {
            row: row as u16,
            x: (pad + x) as u16,
            y: (pad + row * ch + y) as u16,
            w: cw as u16,
            h: h as u16,
            color: 0x0030_50A0, // premultiplied ADDITIVE light (alpha 0)
            alpha: 0,
            color2: 0x0030_50A0,
            alpha2: 0,
        };
        let inner = |row: usize, dx: usize| quad(row, 2 * cw + dx, 2, ch - 4);
        let seq = [
            (None, frame(b"AAAA", Vec::new())),
            (None, frame(b"AAAA\r\nbbbb", vec![inner(4, 0)])),
            (
                Some(8.0),
                frame(b"AAAA\r\nbbbb", vec![inner(4, 3), inner(5, 1)]),
            ),
            (None, frame(b"AAAA\r\nbbbb", Vec::new())),
            (None, frame(b"AAAA\r\ncccc", vec![quad(0, cw, 1, ch - 2)])),
            (None, frame(b"AAAA\r\ncccc", Vec::new())),
        ];
        let mut win = WindowGpu::new();
        let mut scissored = 0;
        for (i, (radius, input)) in seq.iter().enumerate() {
            if let Some(radius) = radius {
                gpu.set_bloom_params(BLOOM_STRENGTH, *radius);
            }
            let before = gpu.scissor_taken();
            let got = gpu.present_input_readback(&mut win, input);
            scissored += gpu.scissor_taken() - before;
            let mut ref_win = WindowGpu::new();
            let want = gpu.render_input(&mut ref_win, input, None);
            assert_eq!(
                (got.width, got.height, got.pixels == want.pixels),
                (want.width, want.height, true),
                "frame {i}: the scissored bloom present must be byte-identical to \
                 a fresh full render (halo ghost, accumulation or old-radius fringe?)"
            );
        }
        assert_eq!(
            scissored as usize,
            seq.len() - 1,
            "every frame after the first must keep the scissored path, not force Full \
             (full_repaints={})",
            gpu.full_repaints(),
        );
    }

    /// A shrunk window keeps its GL bloom half texture, and the halo drawn through that roomy
    /// storage matches a never-resized window's out to the frame's edge.
    #[cfg(gl_arm)]
    #[test]
    fn a_bloom_through_kept_storage_matches_one_never_resized() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        assert!(gpu.bloom_enabled(), "precondition: bloom defaults ON");
        let pad = gpu.pad();
        let (cw, ch) = gpu.cell_size();
        let frame = |rows: usize, cols: usize| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(b"\x1b[?25lAAAA\r\nbbbb");
            let mut input = term.cell_frame(rows, cols);
            // In the last column, so the halo reaches the half texture's last texels in use.
            input.cursor_glow_add = vec![GlowQuad {
                row: 1,
                x: (pad + (cols - 1) * cw) as u16,
                y: (pad + ch + 2) as u16,
                w: cw as u16,
                h: (ch - 4) as u16,
                color: 0x0030_50A0,
                alpha: 0,
                color2: 0x0030_50A0,
                alpha2: 0,
            }];
            input
        };
        let (large, small) = (frame(14, 20), frame(12, 18));
        let mut shrunk = WindowGpu::new();
        let _ = gpu.present_input_readback(&mut shrunk, &large);
        let got = gpu.present_input_readback(&mut shrunk, &small);
        let half = shrunk
            .gl_bloom_half
            .as_ref()
            .expect("a bloom frame on the GL arm draws its half texture");
        assert!(
            half.roomy(),
            "the shrink kept the larger frame's storage: {half:?}"
        );
        let want = gpu.present_input_readback(&mut WindowGpu::new(), &small);
        assert_eq!((got.width, got.height), (want.width, want.height));
        assert_eq!(
            got.pixels, want.pixels,
            "the halo through kept storage must be the never-resized window's"
        );
    }

    #[test]
    fn tray_upload_skips_unchanged_card_bytes() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        let (rows, cols) = (4usize, 10usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lhello");
        let input = term.cell_frame(rows, cols);
        let mut win = WindowGpu::new();

        let (pw, ph) = (32u32, 16u32);
        let red: Vec<u8> = [200, 40, 40, 255].repeat((pw * ph) as usize);
        let green: Vec<u8> = [40, 200, 40, 255].repeat((pw * ph) as usize);
        fn tray(rgba: &[u8], pw: u32, ph: u32) -> TrayQuad<'_> {
            TrayQuad {
                rgba,
                pw,
                ph,
                dx: 4,
                dy: 4,
                scrim: None,
            }
        }

        let first = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph)));
        assert_eq!(gpu.tray_uploads(), 1, "first card present must upload");

        let second = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph)));
        assert_eq!(
            gpu.tray_uploads(),
            1,
            "identical card bytes must skip the write_texture re-upload"
        );
        assert_eq!(
            first.pixels, second.pixels,
            "the skipped upload must not change the composited output"
        );

        let third = gpu.render_input(&mut win, &input, Some(tray(&green, pw, ph)));
        assert_eq!(gpu.tray_uploads(), 2, "changed card bytes must upload");
        assert_ne!(
            second.pixels, third.pixels,
            "the new card must actually reach the frame"
        );

        let swapped = TrayQuad {
            rgba: &green,
            pw: ph,
            ph: pw,
            dx: 4,
            dy: 4,
            scrim: None,
        };
        let _ = gpu.render_input(&mut win, &input, Some(swapped));
        assert_eq!(gpu.tray_uploads(), 3, "a recreate must always upload");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_tray_card_drawn_and_waited_inline_records_no_reader() {
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.arm_metal_for_test();
        let (rows, cols) = (4usize, 10usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lhello");
        let input = term.cell_frame(rows, cols);
        let mut win = WindowGpu::new();
        let (pw, ph) = (32u32, 16u32);
        let cards: Vec<Vec<u8>> = [[200, 40, 40, 255], [40, 200, 40, 255], [40, 40, 200, 255]]
            .iter()
            .map(|rgba| rgba.repeat((pw * ph) as usize))
            .collect();
        fn tray(rgba: &[u8], pw: u32, ph: u32) -> TrayQuad<'_> {
            TrayQuad {
                rgba,
                pw,
                ph,
                dx: 4,
                dy: 4,
                scrim: None,
            }
        }
        let readers = |win: &WindowGpu| {
            let card = win.metal_tray_card.as_ref().expect("the armed card");
            (0..crate::tray_card_ring::TRAY_CARD_SLOTS)
                .filter_map(|slot| card.ring.slot(slot))
                .map(|slot| slot.last_read)
                .collect::<Vec<_>>()
        };
        for (i, card) in cards.iter().cycle().take(4).enumerate() {
            let _ = gpu.render_input(&mut win, &input, Some(tray(card, pw, ph)));
            assert!(gpu.metal_render_armed(), "the frame took the Metal arm");
            assert!(
                readers(&win).iter().all(Option::is_none),
                "bake {i}: {:?}",
                readers(&win)
            );
        }
        assert_eq!(
            readers(&win).len(),
            2,
            "the ring rotated through both slots"
        );
        for (i, card) in cards.iter().take(2).enumerate() {
            assert!(gpu.present_virtual(&mut win, &input, false, None, Some(tray(card, pw, ph))));
            assert!(
                readers(&win).iter().all(Option::is_none),
                "virtual present {i}: {:?}",
                readers(&win)
            );
        }
    }

    #[test]
    fn fractional_scroll_keeps_tray_pixels_pinned() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        let (rows, cols) = (6usize, 12usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lrow zero\r\nrow one\r\nrow two\r\nrow three");
        let mut input = term.cell_frame(rows, cols);
        input.grid_top_row = 1;
        input.grid_bot_row = rows - 1;
        let (cw, ch) = gpu.cell_size();
        let grid_top = gpu.cpu.grid_top();
        let (pw, ph) = ((cw * 3) as u32, ch.saturating_sub(4).max(4) as u32);
        let (dx, dy) = (cw as u32, (grid_top + ch * 2 + 2) as u32);
        let magenta: Vec<u8> = [220, 30, 180, 255].repeat((pw * ph) as usize);
        let tray = || TrayQuad {
            rgba: &magenta,
            pw,
            ph,
            dx,
            dy,
            scrim: None,
        };

        input.scroll_frac_px = 0;
        let mut zero_win = WindowGpu::new();
        let zero = gpu.render_input(&mut zero_win, &input, Some(tray()));
        input.scroll_frac_px = 3;
        let mut shifted_win = WindowGpu::new();
        let shifted = gpu.render_input(&mut shifted_win, &input, Some(tray()));
        assert_eq!((zero.width, zero.height), (shifted.width, shifted.height));

        for y in dy as usize..(dy + ph) as usize {
            let x0 = y * zero.width + dx as usize;
            let x1 = x0 + pw as usize;
            assert_eq!(
                &shifted.pixels[x0..x1],
                &zero.pixels[x0..x1],
                "tray row {y} moved with the terminal scroll band"
            );
        }
        assert_ne!(
            shifted.pixels, zero.pixels,
            "negative control: terminal pixels outside the pinned tray must move"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_failed_first_frame_warm_leaves_the_inline_mint_and_its_failure_route() {
        let dev = crate::metal::ffi::Device::preferred()
            .unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"));
        static LATCH: crate::metal::warm::WarmLatch =
            crate::metal::warm::WarmLatch::new(crate::metal::warm::DEADLINE);
        let renderer = || {
            let mut g = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
            g.arm_metal_for_test();
            g.set_first_frame_latch_for_test(&LATCH);
            g
        };
        let mut gpu = renderer();
        let mut warm = LATCH.begin().expect("the latch's first warm");
        let failed = warm.run_on(
            &dev,
            crate::metal::warm::FirstFrameKnobs::default(),
            |_, _| Err("injected warm failure".to_owned()),
        );
        assert_eq!(failed, Err("injected warm failure".to_owned()));
        drop(warm);
        assert!(
            gpu.metal_arm_ready(),
            "the mint after a failed warm compiles inline: {}",
            gpu.metal_start_error()
        );
        assert!(gpu.metal_render_armed());

        let mut gpu = renderer();
        INJECTED_MINT_FAILURE.with(|f| f.set(Some("injected mint failure")));
        assert!(!gpu.metal_arm_ready(), "a failed mint is not ready");
        assert!(!gpu.metal_armed, "a failed mint disarms");
        assert_eq!(
            gpu.metal_start_error(),
            "the Metal renderer could not start: injected mint failure"
        );
    }

    /// A small text frame lit by one cursor-aurora quad: Unorm-law light for the route to place.
    #[cfg(target_os = "macos")]
    fn glow_over_text(gpu: &GpuRenderer) -> RenderInput {
        let (rows, cols) = (6usize, 24usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"glow over text");
        let mut input = term.cell_frame(rows, cols);
        let (cw, ch) = gpu.cpu.cell_size();
        input.cursor_glow_add.push(GlowQuad {
            row: 0,
            x: 0,
            y: 0,
            w: (4 * cw) as u16,
            h: ch as u16,
            color: 0x0040_6080,
            alpha: 0,
            color2: 0x0040_6080,
            alpha2: 0,
        });
        input
    }

    /// One [`glow_over_text`] frame on a fresh armed renderer: its shape and the fetch twins built.
    #[cfg(target_os = "macos")]
    fn glow_frame_on(setup: impl FnOnce(&mut GpuRenderer)) -> (FrameShape, usize) {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.arm_metal_for_test();
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
        setup(&mut gpu);
        let input = glow_over_text(&gpu);
        let mut win = WindowGpu::new();
        let _ = gpu.render_input(&mut win, &input, None);
        assert!(gpu.metal_render_armed(), "the frame minted the Metal arm");
        let twins = gpu
            .metal_built_for_test()
            .into_iter()
            .filter(|item| matches!(item, crate::metal::warm::WarmItem::Twin(_)))
            .count();
        (gpu.frame_shape(), twins)
    }

    /// A device whose fetch verdict is anything but Passed draws its Unorm-law light on the split
    /// whatever route the renderer asks for, building no twin.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_device_that_did_not_pass_the_probe_draws_its_unorm_light_on_the_split() {
        let dev = crate::metal::ffi::Device::preferred()
            .unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"));
        let failed = FetchProbe::Failed {
            stage: crate::FetchStage::InPass,
            channel: 2,
            want: 9,
            got: 10,
        };
        for verdict in [failed, FetchProbe::Refused, FetchProbe::Unsupported] {
            let (shape, twins) = glow_frame_on(|gpu| {
                gpu.set_unorm_route(UnormRoute::Fetch);
                gpu.set_fetch_probe_seam(Some(verdict));
            });
            assert_eq!(shape.route, UnormRoute::Split, "{verdict:?}");
            assert!(
                shape.unorm_draws > 0 && shape.passes > 1,
                "{verdict:?}: the light draws on the Unorm view, {} passes",
                shape.passes
            );
            assert_eq!(twins, 0, "{verdict:?}: no twin is built");
        }
        if !crate::metal::fetch::capable(&dev) {
            aterm_spec::host::could_not_run("the device has no framebuffer fetch");
        }
        let (shape, twins) = glow_frame_on(|gpu| {
            gpu.set_unorm_route(UnormRoute::Fetch);
            gpu.set_fetch_probe_seam(Some(FetchProbe::Passed));
        });
        assert_eq!(
            (shape.route, shape.passes),
            (UnormRoute::Fetch, 1),
            "the same frame under a passed verdict takes the fetch: the verdict kept the split"
        );
        assert!(twins > 0);
    }

    /// An unconfigured renderer routes Unorm-law light through fetch, every Apple GPU has fetch,
    /// and on a device that passes its probe the default glow frame is one pass.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_default_route_is_the_fetch_and_this_device_takes_it() {
        use std::ffi::OsStr;
        assert_eq!(UnormRoute::initial(None), UnormRoute::Fetch);
        assert_eq!(
            UnormRoute::initial(Some(OsStr::new("split"))),
            UnormRoute::Split
        );
        assert_eq!(
            UnormRoute::initial(Some(OsStr::new("fetch"))),
            UnormRoute::Fetch
        );
        let dev = crate::metal::ffi::Device::preferred()
            .unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"));
        if cfg!(target_arch = "aarch64") {
            assert!(
                crate::metal::fetch::capable(&dev),
                "{}: an Apple GPU has framebuffer fetch",
                dev.name()
            );
        } else if !crate::metal::fetch::capable(&dev) {
            aterm_spec::host::could_not_run("the device has no framebuffer fetch");
        }
        if aterm_types::dev_seam!("ATERM_GPU_UNORM_ROUTE").is_some() {
            aterm_spec::host::could_not_run("ATERM_GPU_UNORM_ROUTE chooses the route here");
        }
        let (shape, twins) = glow_frame_on(|_| {});
        assert_eq!(
            (shape.route, shape.passes),
            (UnormRoute::Fetch, 1),
            "{}: the default glow frame",
            dev.name()
        );
        assert!(twins > 0, "it drew through a twin");
    }

    /// An armed renderer with every present pass on (bloom, shimmer, both crowns, a window on
    /// screen that may present f16), routing Unorm-law light through framebuffer fetch.
    #[cfg(target_os = "macos")]
    fn every_pass_on() -> GpuRenderer {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.arm_metal_for_test();
        gpu.metal_on_screen = true;
        gpu.set_unorm_route(UnormRoute::Fetch);
        gpu.set_bloom(true);
        gpu.set_bloom_params(0.85, 2.2);
        gpu.set_shimmer(true);
        gpu.set_hdr_glow(true);
        gpu.set_sdr_glow_boost(0.25);
        gpu
    }

    /// A default window's first frames on `gpu`, presented to a fresh `format` swapchain on a
    /// screen of `edr` headroom: a blank grid, the first prompt (cursor aurora and a Claude card's
    /// fx layer), then the prompt with an emoji.
    #[cfg(target_os = "macos")]
    fn present_first_frames(
        gpu: &mut GpuRenderer,
        format: crate::device_layer::TexelFormat,
        edr: f32,
    ) {
        let (rows, cols) = (24usize, 80usize);
        let mut win = WindowGpu::new();
        win.set_edr_max(edr);
        let blank = Terminal::new(rows as u16, cols as u16).cell_frame(rows, cols);
        let (cw, ch) = gpu.cpu.cell_size();
        let card = aterm_core::render::FxLayer::new(
            1,
            std::sync::Arc::new(aterm_core::render::FxContent::new(
                vec![aterm_core::render::FxItem::Rect(
                    aterm_core::render::FxRect {
                        x: 0,
                        y: (2 * ch) as i32,
                        w: (20 * cw) as u16,
                        h: 2,
                        ink: 0,
                    },
                )],
                vec![
                    aterm_core::render::FxInk::add(0x0020_4060),
                    aterm_core::render::FxInk::add(0x0060_4020),
                ],
            )),
            aterm_core::render::FxSlot::Under,
            aterm_core::render::FxSpace::Window,
            aterm_core::render::FxBlend::Exact,
        );
        let prompt = |text: &str| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(text.as_bytes());
            let mut input = term.cell_frame(rows, cols);
            input.cursor_glow_add.push(aterm_render::GlowQuad {
                row: 0,
                x: (14 * cw) as u16,
                y: 0,
                w: (2 * cw) as u16,
                h: ch as u16,
                color: 0x0040_6080,
                alpha: 0,
                color2: 0x0040_6080,
                alpha2: 0,
            });
            input.fx_layers.push(card.clone());
            input
        };
        let (fw, fh) = gpu.frame_size(rows, cols);
        for input in [&blank, &prompt("user@host ~ % "), &prompt("\u{1f680} ~ % ")] {
            gpu.metal_present_standalone_for_test(
                &mut win,
                input,
                (fw as u32, fh as u32),
                (false, true),
                format,
                |_, _| Ok(()),
            )
            .expect("the armed present");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_first_frame_warm_builds_what_a_default_windows_first_frames_build() {
        use crate::device_layer::TexelFormat;
        use crate::metal::warm::{FirstFrameKnobs, WarmItem, first_frame_rows, first_frame_twins};
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        let mut built: Vec<WarmItem> = Vec::new();
        let mut fetch = false;
        for (format, edr) in [
            (TexelFormat::Bgra8Unorm, 1.0),
            (TexelFormat::Rgba16Float, 2.0),
        ] {
            let mut gpu = every_pass_on();
            present_first_frames(&mut gpu, format, edr);
            for item in gpu.metal_built_for_test() {
                if !built.contains(&item) {
                    built.push(item);
                }
            }
            fetch = gpu.fetch_probe() == Some(FetchProbe::Passed);
        }
        let knobs = FirstFrameKnobs {
            aurora: true,
            bloom: true,
            hdr_glow: true,
            sdr_glow: true,
            fx: true,
        };
        let mut warmed: Vec<WarmItem> = Vec::new();
        for (row, present) in first_frame_rows(knobs, fetch) {
            for item in [
                WarmItem::Library(row.spec().library),
                WarmItem::pipeline(row, present),
            ] {
                if !warmed.contains(&item) {
                    warmed.push(item);
                }
            }
        }
        warmed.extend(
            first_frame_twins(knobs, fetch)
                .into_iter()
                .map(WarmItem::Twin),
        );
        let missed: Vec<&WarmItem> = built.iter().filter(|i| !warmed.contains(i)).collect();
        let idle: Vec<&WarmItem> = warmed.iter().filter(|i| !built.contains(i)).collect();
        assert!(
            missed.is_empty() && idle.is_empty(),
            "the first frames built {built:?}\nthe warm builds {warmed:?}\nmissed by the \
             warm: {missed:?}\nbuilt by the warm for nothing: {idle:?}"
        );
    }

    /// A warm asked for before the arm minted starts from the mint and hands over every row and
    /// twin, so a default window's first frames build no pipeline on the render thread; without
    /// the warm they build their own.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_warmed_arm_builds_no_pipeline_on_the_render_thread() {
        use crate::device_layer::TexelFormat;
        use crate::metal::warm::{PresentKnobs, bindable};
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);
        let on = PresentKnobs {
            bloom: true,
            shimmer: true,
            hdr_glow: true,
            sdr_glow: true,
        };
        for (format, edr) in [
            (TexelFormat::Bgra8Unorm, 1.0),
            (TexelFormat::Rgba16Float, 2.0),
        ] {
            let mut gpu = every_pass_on();
            gpu.warm_effect_pipelines();
            assert!(
                gpu.metal_arm_ready(),
                "the mint: {}",
                gpu.metal_start_error()
            );
            let twins = gpu.fetch_probe().is_none_or(|v| v == FetchProbe::Passed);
            assert_eq!(
                gpu.metal_built_for_test(),
                vec![crate::metal::warm::WarmItem::Library(ShaderLibrary::Cell)],
                "the mint built nothing"
            );
            assert_eq!(
                gpu.await_pipeline_warm_for_test(HANG),
                Vec::new(),
                "builds the warm failed"
            );
            present_first_frames(&mut gpu, format, edr);
            let held = gpu.metal_built_for_test();
            let untaken: Vec<_> = bindable(on, twins)
                .into_iter()
                .filter(|item| !held.contains(item))
                .collect();
            assert!(
                untaken.is_empty(),
                "the frames' first miss took every build the warm made: {untaken:?} untaken"
            );
            assert_eq!(
                gpu.metal_built_here_for_test(),
                Vec::new(),
                "{format:?}: built on the render thread after the warm"
            );

            let mut cold = every_pass_on();
            present_first_frames(&mut cold, format, edr);
            assert!(
                !cold.metal_built_here_for_test().is_empty(),
                "{format:?}: without the warm the frames build their own"
            );
        }
    }

    /// A pass whose switch was off when the warm ran is warmed when the switch turns it on, and
    /// only then.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pass_switched_on_after_the_warm_is_warmed_then() {
        use crate::metal::ffi::PixelFormat;
        use crate::metal::warm::WarmItem;
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);
        let shimmer = WarmItem::pipeline(Pipeline::Shimmer, PixelFormat::Rgba8Unorm);
        let mut gpu = every_pass_on();
        gpu.set_shimmer(false);
        gpu.warm_effect_pipelines();
        assert!(
            gpu.metal_arm_ready(),
            "the mint: {}",
            gpu.metal_start_error()
        );
        assert!(
            !gpu.metal_warm_owed_for_test().is_empty()
                && !gpu.metal_warm_owed_for_test().contains(&shimmer),
            "the warm left the shimmer it found off"
        );
        gpu.set_shimmer(false);
        assert!(!gpu.metal_warm_owed_for_test().contains(&shimmer));
        gpu.set_shimmer(true);
        assert!(
            gpu.metal_warm_owed_for_test().contains(&shimmer),
            "turning the shimmer on warms it"
        );
        assert_eq!(gpu.await_pipeline_warm_for_test(HANG), Vec::new());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn armed_present_composes_the_stale_rect_and_matches_a_full_copy() {
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.arm_metal_for_test();
        let (rows, cols) = (6usize, 12usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lrow zero\r\nrow one\r\nrow two\r\nrow three");
        let glide_input = |term: &mut Terminal| {
            let mut input = term.cell_frame(rows, cols);
            input.grid_top_row = 1;
            input.grid_bot_row = rows - 1;
            input.scroll_frac_px = 3;
            input
        };
        let mut input = glide_input(&mut term);
        let mut win = WindowGpu::new();
        input.scroll_frac_px = 0;
        let first = gpu.render_input(&mut win, &input, None);
        assert!(
            gpu.metal_render_armed(),
            "the first frame must mint the arm"
        );
        let (fw, fh) = (first.width as u32, first.height as u32);
        let dest = (fw + 9, fh + 7);
        input.scroll_frac_px = 3;

        let primed = gpu
            .metal_present_bytes_with_scope_for_test(&mut win, &input, dest, false, true)
            .expect("armed present");
        assert_eq!(
            win.metal_last_compose_rect,
            Some([0, 0, fw, fh]),
            "the first sync into a fresh copy is the full frame"
        );
        let mut flat = input.clone();
        flat.scroll_frac_px = 0;
        let flat = gpu
            .metal_present_bytes_with_scope_for_test(
                &mut WindowGpu::new(),
                &flat,
                dest,
                false,
                true,
            )
            .expect("armed present");
        assert_ne!(
            primed, flat,
            "a frac 3 present must move terminal pixels on glass"
        );

        term.process(b"\x1b[3;1HX");
        let changed = glide_input(&mut term);
        let scissors_before = gpu.scissor_taken();
        let incremental = gpu
            .metal_present_bytes_with_scope_for_test(&mut win, &changed, dest, false, false)
            .expect("armed present");
        assert!(
            gpu.scissor_taken() > scissors_before,
            "non-vacuity: the second armed encode must take the scissor path"
        );
        let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
            gpu.cpu.grid_top(),
            gpu.cell_size().1,
            1,
            rows - 1,
            fh as usize,
        );
        let band = [0, y0 as u32, fw, y1 as u32];
        assert!(
            band[3] - band[1] < fh,
            "fixture: the band {band:?} must be smaller than the {fw}x{fh} frame"
        );
        assert_eq!(
            win.metal_last_compose_rect,
            Some(band),
            "the compose must re-copy the stale union (the previous shift's band, \
             which contains the dirty row), never the whole {fw}x{fh} frame"
        );
        assert_ne!(
            incremental, primed,
            "negative control: the changed cell must reach the glass"
        );

        let mut fresh = WindowGpu::new();
        let oracle = gpu
            .metal_present_bytes_with_scope_for_test(&mut fresh, &changed, dest, false, true)
            .expect("armed present");
        assert_eq!(
            fresh.metal_last_compose_rect,
            Some([0, 0, fw, fh]),
            "fixture: the oracle is a full-frame compose"
        );
        assert_eq!(
            incremental, oracle,
            "the stale-rect compose must present the full-copy bytes"
        );

        assert_eq!(
            win.offscreen_dirty_since_sync,
            Some([0, 0, 0, 0]),
            "fixture: the compose just synced the copy"
        );
        gpu.shift_offscreen_rows(&mut win, 1, rows);
        assert_eq!(
            win.offscreen_dirty_since_sync, None,
            "the armed E7 shift must invalidate the incremental sync"
        );
    }

    #[test]
    fn hdr_upgrade_publishes_linear_capture_metadata_with_safe_defaults() {
        use crate::video_tap::CaptureColorSpace;

        let mut win = WindowGpu::new();
        win.set_capture_color_space(CaptureColorSpace::Srgb);
        win.set_sdr_white_scale(3.0);
        win.set_edr_max(4.0);
        win.apply_hdr_surface_upgrade();
        assert_eq!(
            win.capture_color_space(),
            CaptureColorSpace::ExtendedLinearSrgb
        );
        assert_eq!(win.sdr_white_scale(), 1.0);
        assert_eq!(win.edr_max(), 0.0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_screen_upgrade_moves_the_layer_before_it_promotes_the_metadata() {
        use crate::device_layer::TexelFormat;
        use crate::video_tap::CaptureColorSpace;

        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let parent = crate::metal::test_support::plain_calayer();
        let mut gpu =
            crate::renderer::gpu_renderer_for_test(None, 18.0, aterm_render::Theme::default());
        gpu.set_hdr_glow(true);
        let mut win = WindowGpu::new();
        let mut surf = gpu
            .attach_metal_window_surface(&parent, TexelFormat::Bgra8Unorm, 64, 48)
            .expect("an 8-bit attach under a stand-in parent layer");
        let linear =
            |win: &WindowGpu| win.capture_color_space == CaptureColorSpace::ExtendedLinearSrgb;
        let layer = |surf: &super::GpuSurface| {
            surf.metal
                .as_ref()
                .expect("the attach armed a swapchain")
                .config()
                .format
        };
        let held_8bit = |surf: &super::GpuSurface, why: &str| {
            assert_eq!(
                surf.neutral.format,
                TexelFormat::Bgra8Unorm,
                "{why}: retained"
            );
            assert_eq!(
                layer(surf),
                TexelFormat::Bgra8Unorm.metal(),
                "{why}: live layer"
            );
        };

        for potential in [Some(1.0), None] {
            assert!(
                !gpu.upgrade_surface_on_screen(&mut win, &mut surf, || potential),
                "{potential:?} is no evidence of an EDR screen"
            );
            held_8bit(&surf, "no EDR screen");
            assert!(!linear(&win), "no EDR screen promotes no metadata");
        }

        let arm = gpu.metal_arm.take();
        assert!(!gpu.upgrade_surface_on_screen(&mut win, &mut surf, || Some(2.0)));
        held_8bit(&surf, "a refused flip");
        assert!(!linear(&win), "a refused flip promotes no metadata");
        gpu.metal_arm = arm;

        assert!(
            gpu.upgrade_surface_on_screen(&mut win, &mut surf, || Some(2.0)),
            "an EDR screen upgrades an opted-in 8-bit window"
        );
        assert_eq!(surf.neutral.format, TexelFormat::Rgba16Float);
        assert_eq!(
            layer(&surf),
            TexelFormat::Rgba16Float.metal(),
            "the live layer itself moved, not only the retained desire"
        );
        assert!(linear(&win), "the metadata follows the flipped layer");

        let mut read = false;
        assert!(!gpu.upgrade_surface_on_screen(&mut win, &mut surf, || {
            read = true;
            Some(1.0)
        }));
        assert!(!read, "an f16 surface never reads the screen");
        assert_eq!(
            layer(&surf),
            TexelFormat::Rgba16Float.metal(),
            "one direction only"
        );
    }
}

impl Rasterizer for GpuRenderer {
    fn cell_size(&self) -> (usize, usize) {
        GpuRenderer::cell_size(self)
    }
    fn render_input(&mut self, input: &RenderInput) -> Frame {
        let mut win = WindowGpu::new();
        GpuRenderer::render_input(self, &mut win, input, None)
    }
    fn set_cursor_blink_phase(&mut self, on: bool) {
        GpuRenderer::set_cursor_blink_phase(self, on)
    }
    fn set_cursor_style_override(&mut self, style: Option<CursorStyle>) {
        GpuRenderer::set_cursor_style_override(self, style)
    }
}

/// A window a GPU backend can attach a swapchain to.
#[cfg(target_os = "macos")]
pub trait WindowTarget: aterm_objc::HasNsView + Send + Sync + 'static {}

#[cfg(target_os = "macos")]
impl<T> WindowTarget for T where T: aterm_objc::HasNsView + Send + Sync + 'static {}

/// A window a GPU backend can attach a swapchain to.
#[cfg(any(target_os = "linux", windows))]
pub trait WindowTarget:
    aterm_types::native_window::HasNativeWindow + Send + Sync + 'static
{
}

#[cfg(any(target_os = "linux", windows))]
impl<T> WindowTarget for T where
    T: aterm_types::native_window::HasNativeWindow + Send + Sync + 'static
{
}

/// The frontend's own presenter for one window, which the GL arm posts frames through instead of
/// a window surface where the window system's GL stalls it (X11 on NVIDIA's GLX under a
/// compositor, where every resize of a `GLXWindow` stalls the X server and the GPU for every
/// client). Handed to [`GpuRenderer::create_window_surface_with_sink`].
#[cfg(target_os = "linux")]
pub trait FrameSink: Send {
    /// Get ready to post. `Err` says why it cannot, and the window is posted
    /// through its window surface instead.
    fn attach(&mut self) -> Result<(), String>;

    /// Post one `width` x `height` frame: size the store, wait until the window system has read
    /// the last post, have `draw` write a rectangle of the frame into the store (`0x00RRGGBB`
    /// words, rows top first, `width` words a row), and post that rectangle. `damage`
    /// (`[x0, y0, x1, y1]`, half-open; `None` for all of it) is the part that differs from the
    /// last post; the sink widens it to the whole frame wherever its store does not hold the last
    /// post (the first post, a new size) and hands `draw` the rectangle it settled on. An empty
    /// one posts nothing; `draw`'s `Err` posts nothing and is returned.
    fn post(
        &mut self,
        width: u32,
        height: u32,
        damage: Option<[u32; 4]>,
        draw: &mut FrameDraw<'_>,
    ) -> Result<(), String>;
}

/// A [`FrameSink::post`]'s draw: write the rectangle (`[x0, y0, x1, y1]`,
/// half-open) of the frame into the store's words.
#[cfg(target_os = "linux")]
pub type FrameDraw<'a> = dyn FnMut(&mut [u32], [u32; 4]) -> Result<(), String> + 'a;

#[cfg(test)]
mod device_lost_tests {

    #[cfg(target_os = "macos")]
    #[test]
    fn a_wired_metal_loss_latch_feeds_device_lost() {
        use crate::metal::loss::{CbOutcome, LossLatch};
        let gpu =
            crate::renderer::gpu_renderer_for_test(None, 18.0, aterm_render::Theme::default());
        assert!(!gpu.device_lost(), "a fresh context is healthy");
        let latch = gpu.ctx.metal_loss_latch_or_wire();
        assert!(
            !gpu.device_lost(),
            "a wired but healthy latch reports nothing"
        );
        latch.record(&CbOutcome::Lost {
            code: None,
            name: "injected PageFault stand-in",
        });
        assert!(
            gpu.device_lost(),
            "the injected Metal loss must reach the frontend's existing poll"
        );
        assert!(
            !gpu.ctx
                .wire_metal_loss_latch(std::sync::Arc::new(LossLatch::new())),
            "a second wire refuses — one loss domain per context lifetime"
        );
    }
}

#[cfg(all(target_os = "macos", test))]
mod atlas_upload_sync_tests {
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

    fn atlas_state(
        live: &MetalArmLive,
        uploaded_h: i64,
        presented: i64,
    ) -> aterm_spec::interp::State {
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
            std::sync::Arc::new(LossLatch::new()),
            &crate::metal::warm::FIRST_FRAME,
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
            std::sync::Arc::new(LossLatch::new()),
            &crate::metal::warm::FIRST_FRAME,
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
}

#[cfg(all(test, target_os = "macos"))]
mod fx_hazard_tests {
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
        if crate::metal::ffi::Device::preferred().is_none() {
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
}

#[cfg(test)]
mod acquire_wait_failure_tests {
    use super::{SurfacePresentFailure, WindowGpu};

    #[test]
    fn a_refused_acquire_is_measured_and_published_once() {
        let mut window = WindowGpu::new();
        window.begin_surface_present();
        let mut invoked = false;
        let result = window.measure_surface_acquire(|| {
            invoked = true;
            Err::<(), _>(SurfacePresentFailure::Timeout)
        });
        assert!(invoked);
        assert!(matches!(result, Err(SurfacePresentFailure::Timeout)));
        assert_eq!(
            window.take_acquire_wait_sample_ns(),
            Some(window.last_acquire_wait_ns())
        );
        assert_eq!(
            window.take_acquire_wait_sample_ns(),
            None,
            "no duplicate publication"
        );

        window.begin_surface_present();
        assert_eq!(window.take_acquire_wait_sample_ns(), None);
        assert!(matches!(
            window.measure_surface_acquire(|| Ok::<_, SurfacePresentFailure>(7)),
            Ok(7)
        ));
        assert!(window.take_acquire_wait_sample_ns().is_some());
    }

    #[test]
    fn a_new_attempt_discards_an_unconsumed_old_sample() {
        let mut window = WindowGpu::new();
        window.begin_surface_present();
        window.measure_surface_acquire(|| ());
        window.begin_surface_present();
        assert_eq!(window.take_acquire_wait_sample_ns(), None);
    }
}

#[cfg(all(target_os = "macos", test))]
mod gpu_park_accounting_tests {
    use super::WindowGpu;

    #[test]
    fn a_gpu_park_accumulates_per_attempt_and_publishes_exactly_once() {
        let mut window = WindowGpu::new();
        window.begin_surface_present();
        assert_eq!(
            window.take_gpu_park_sample_ns(),
            None,
            "an attempt that never parked must publish no sample"
        );

        window.begin_surface_present();
        window.add_gpu_park(3_000_000);
        window.add_gpu_park(7_000_000);
        assert_eq!(window.last_gpu_park_ns(), 10_000_000);
        assert_eq!(window.take_gpu_park_sample_ns(), Some(10_000_000));
        assert_eq!(
            window.take_gpu_park_sample_ns(),
            None,
            "no duplicate publication"
        );

        window.begin_surface_present();
        assert_eq!(window.take_gpu_park_sample_ns(), None);
        window.add_gpu_park(250_000);
        assert_eq!(window.take_gpu_park_sample_ns(), Some(250_000));
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "window_atlas_tests.rs"]
mod window_atlas_tests;

#[cfg(gl_arm)]
#[path = "gl_arm.rs"]
mod gl_arm;
#[cfg(gl_arm)]
#[path = "gl_present.rs"]
mod gl_present;

// The D3D11 arm's renderer side: its residency and encode tail
// (docs/AUDIT-third-party-2026-10-15.md GW.5).
#[cfg(d3d11_arm)]
#[path = "d3d11_arm.rs"]
mod d3d11_arm;
#[cfg(d3d11_arm)]
#[path = "d3d11_present.rs"]
mod d3d11_present;
#[cfg(d3d11_arm)]
#[path = "d3d11_window.rs"]
mod d3d11_window;
// The shipped Windows build's window presents on real windows, against the
// arm's own headless present (the desktop session only).
#[cfg(all(test, d3d11_arm))]
#[path = "d3d11_window_tests.rs"]
mod d3d11_window_tests;

// The frames the arm differentials and the frame records draw.
#[cfg(all(test, any(target_os = "macos", target_os = "linux", windows)))]
#[path = "replay_fixtures_tests.rs"]
mod replay_fixtures;

// Each arm's frames against the digests wgpu drew on the same adapter: Metal
// on this Mac's GPU, GL on the Linux lane's Mesa, D3D11 on WARP
// (docs/AUDIT-third-party-2026-10-15.md GW.5 and GW.6).
#[cfg(all(test, any(target_os = "macos", target_os = "linux", windows)))]
#[path = "frame_record_tests.rs"]
mod frame_record_tests;

// Each first-party arm's headless presents on their own: a changed input
// reaches the destination, and a lost device drops the present.
#[cfg(all(test, d3d11_arm))]
#[path = "d3d11_present_tests.rs"]
mod d3d11_present_tests;
#[cfg(all(test, target_os = "linux"))]
#[path = "gl_present_tests.rs"]
mod gl_present_tests;

// The D3D11 arm's removal latch where a frame writes resident state before
// it draws, in both Windows builds.
#[cfg(all(test, d3d11_arm))]
#[path = "d3d11_removal_tests.rs"]
mod d3d11_removal_tests;

#[path = "fx_gpu.rs"]
pub(crate) mod fx_gpu;
