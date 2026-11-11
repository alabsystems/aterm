// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow-kitty pet's bake path: one authored full-body pose glyph ([`crate::pet_glyphs_gen::PET_GLYPHS`]) rasterized into one exact-size straight-alpha RGBA8 tile, cached in a small LRU, and handed to the shared cat atlas through [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).

use aterm_scene::vector::{FIXED_ONE, PathSeg};
use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_baker::{
    CatColorKey, MAX_ATLAS_BYTES, MAX_BAKES_PER_FRAME, MAX_SLOTS, ResolvedFills,
    paint_dark_masked_disc, paint_outline_halo, path_bounds, prepare_scratch_tile, resolve_layer,
    take_or_new_tile,
};
use crate::cat_glyphs_gen::{GlyphRole, Layer};
use crate::pet_glyphs_gen::{PET_GLYPHS, PetGlyphId};

/// Tile heights below this bake the LOD face; from here up the authored face's finest strokes clear a device pixel.
pub const FACE_DETAIL_MIN_H: u32 = 56;

/// Below this tile height the mouth is dropped: in the eye ink it would bridge the grown eye dots at 24–36 px and fuse the lower face into a mask.
pub const MOUTH_DETAIL_MIN_H: u32 = 40;

/// The height, in device pixels, a collapsed open eye dot grows toward: a legibility floor hosting the [`EYE_DASH_CORE_PX`] core plus headroom.
pub const EYE_DOT_TARGET_H_PX: f32 = 4.0;

/// The vertical target for shallow eye subpaths (happy arcs, closed lids), equal to [`EYE_DASH_CORE_PX`]: grown further, a lid stops reading as a lid.
pub const EYE_DOT_TARGET_H_PX_ARC: f32 = 3.4;

/// Ceiling on a shallow subpath's horizontal growth.
pub const EYE_DOT_ARC_KX_MAX: f32 = 1.6;

/// Ceiling on an open eye's growth: binds only below h ≈ 15, so a degenerate 1 px feature cannot grow to the size of the muzzle.
pub const EYE_DOT_SCALE_MAX: f32 = 2.2;

/// Growth ceiling for shallow eye subpaths: the roster's 1.2 px arcs at the 24 px bake need 2.8× to reach [`EYE_DOT_TARGET_H_PX_ARC`].
pub const EYE_DOT_SCALE_MAX_SHALLOW: f32 = 2.8;

/// Side, in device pixels, of the solid core plug every LOD eye gets at its centre: 3.4 px covers two full rows and columns at every sub-pixel phase, which a 4 px ellipse does not.
pub const EYE_DASH_CORE_PX: f32 = 3.4;

/// Device-pixel aspect (height ÷ width) below which an eye subpath is shallow: arcs and lids sit at 0.38–0.52, open eyes at 0.95–1.25.
pub const EYE_DOT_SHALLOW_ASPECT: f32 = 0.6;

/// Below [`FACE_DETAIL_MIN_H`], an eye subpath narrower than this fraction of its widest sibling is the 3/4 far eye: painted as a compressed core (full vertical growth, width near its foreshortened width, floored at [`EYE_DOT_FAR_CORE_MIN_W_FRAC`]) so the pair keeps the turned-head asymmetry instead of two equal dots smearing into one.
pub const EYE_DOT_FAR_COMPRESS_RATIO: f32 = 0.70;

/// An eye subpath narrower than this fraction of its widest sibling is occluded (passing behind the head's profile) and paints nothing at LOD sizes.
pub const EYE_DOT_FAR_OCCLUDED_RATIO: f32 = 0.40;

/// Width floor for the compressed far-eye core, as a fraction of tile height: 2 px at the 24 px tile, 3 px at 36 px, where the plug spans two full columns at every phase.
pub const EYE_DOT_FAR_CORE_MIN_W_FRAC: f32 = 2.0 / 24.0;

/// Horizontal daylight, in device pixels, each eye dot keeps from its siblings (fusion is a raster effect).
pub const EYE_DOT_GAP_PX: f32 = 1.3;

/// Ceiling on an open eye's painted height over width: when the daylight guard narrows a pair, height follows so the dot stays round, not a tall bar.
pub const EYE_DOT_ROUND_ASPECT_MAX: f32 = 1.4;

/// Smallest grown open-eye diameter that carries a reflection: the far eye at 34–36 px (~3 px) clears it; tiny 1× eyes keep a plain dot.
pub const EYE_REFLECTION_MIN_PX: f32 = 2.4;

/// Primary catch-light radius as a fraction of the grown eye's smaller diameter, bounded in device pixels: the floor survives 4×4 coverage at ship size, the ceiling stops a headlamp.
pub const EYE_REFLECTION_PRIMARY_FRAC: f32 = 0.13;
pub const EYE_REFLECTION_PRIMARY_MIN_PX: f32 = 0.55;
pub const EYE_REFLECTION_PRIMARY_MAX_PX: f32 = 0.82;

/// A second, quiet lower-right reflection makes a large eye read glossy; withheld below this diameter, where it would be a sub-pixel grey fleck.
pub const EYE_REFLECTION_ECHO_MIN_PX: f32 = 5.0;
pub const EYE_REFLECTION_ECHO_FRAC: f32 = 0.08;
pub const EYE_REFLECTION_ECHO_MIN_RADIUS_PX: f32 = 0.35;
pub const EYE_REFLECTION_ECHO_MAX_RADIUS_PX: f32 = 0.52;

/// `xform` with an extra `(kx, ky)` zoom about the normalized point `(cx, cy)`, which keeps its
/// device pixel: a feature grows in place instead of sliding across the face.
#[inline]
const fn zoom_about(xform: PathTransform, cx: f32, cy: f32, kx: f32, ky: f32) -> PathTransform {
    PathTransform {
        scale_x: xform.scale_x * kx,
        scale_y: xform.scale_y * ky,
        dx: xform.dx + cx * xform.scale_x * (1.0 - kx),
        dy: xform.dy + cy * xform.scale_y * (1.0 - ky),
    }
}

/// Paint the glossy finish for one grown, open LOD eye, in device pixels. Offsets and radii
/// are fractions of the grown footprint, so far and near eyes each get a glint that fits; the
/// primary at `(-0.22w, -0.23h)` with radius ≤ `0.13·min(w,h)` stays inside an elliptical eye
/// (normalized distance ≈0.64 plus radius ≤0.26), so no white fleck leaks onto the coat.
#[inline]
fn paint_eye_reflections(tile: &mut Tile, eye_mask: &Tile, cx: f32, cy: f32, w: f32, h: f32) {
    let minor = w.min(h);
    if minor < EYE_REFLECTION_MIN_PX {
        return;
    }
    let primary_r = (minor * EYE_REFLECTION_PRIMARY_FRAC)
        .clamp(EYE_REFLECTION_PRIMARY_MIN_PX, EYE_REFLECTION_PRIMARY_MAX_PX);
    paint_dark_masked_disc(
        tile,
        eye_mask,
        cx - 0.22 * w,
        cy - 0.23 * h,
        primary_r,
        (1.0, 1.0, 1.0),
        1.0,
    );

    if minor >= EYE_REFLECTION_ECHO_MIN_PX {
        let echo_r = (minor * EYE_REFLECTION_ECHO_FRAC).clamp(
            EYE_REFLECTION_ECHO_MIN_RADIUS_PX,
            EYE_REFLECTION_ECHO_MAX_RADIUS_PX,
        );
        paint_dark_masked_disc(
            tile,
            eye_mask,
            cx + 0.19 * w,
            cy + 0.20 * h,
            echo_r,
            (1.0, 0.95, 0.98),
            0.72,
        );
    }
}

const MAX_PET_EYE_PATHS: usize = 8;

#[derive(Clone, Copy)]
struct PetEyeMeta {
    paths: &'static [&'static [PathSeg]],
    bounds: [Option<(f32, f32, f32, f32)>; MAX_PET_EYE_PATHS],
    ratios: [f32; MAX_PET_EYE_PATHS],
    near_cx: f32,
}

impl PetEyeMeta {
    #[inline]
    const fn from_layers(layers: &'static [Layer]) -> Self {
        let mut eye_layer: Option<&'static Layer> = None;
        let mut i = 0;
        while i < layers.len() {
            if matches!(layers[i].role, GlyphRole::Eye) {
                assert!(eye_layer.is_none());
                eye_layer = Some(&layers[i]);
            }
            i += 1;
        }
        let mut paths: &'static [&'static [PathSeg]] = &[];
        let mut bounds = [None; MAX_PET_EYE_PATHS];
        let mut ratios = [1.0_f32; MAX_PET_EYE_PATHS];
        let mut max_w = 0.0_f32;
        let mut near_cx = 0.5_f32;
        let mut best_w: Option<f32> = None;
        if let Some(layer) = eye_layer {
            assert!(layer.paths.len() <= MAX_PET_EYE_PATHS);
            paths = layer.paths;
            let count = paths.len();
            let mut p = 0;
            while p < count {
                let b = path_bounds(paths[p]);
                bounds[p] = b;
                if let Some((x0, _, x1, _)) = b {
                    let w = x1 - x0;
                    if w > max_w {
                        max_w = w;
                    }
                    let is_ge = match best_w {
                        None => true,
                        Some(bw) => w >= bw,
                    };
                    if is_ge {
                        best_w = Some(w);
                        near_cx = (x0 + x1) * 0.5;
                    }
                }
                p += 1;
            }
            if max_w > 0.0 {
                let mut p = 0;
                while p < count {
                    if let Some((x0, _, x1, _)) = bounds[p] {
                        ratios[p] = (x1 - x0) / max_w;
                    }
                    p += 1;
                }
            }
        }
        Self {
            paths,
            bounds,
            ratios,
            near_cx,
        }
    }
}

static PET_EYE_META: [PetEyeMeta; PET_GLYPHS.len()] = {
    let mut meta = [PetEyeMeta {
        paths: &[],
        bounds: [None; MAX_PET_EYE_PATHS],
        ratios: [1.0; MAX_PET_EYE_PATHS],
        near_cx: 0.5,
    }; PET_GLYPHS.len()];
    let mut i = 0;
    while i < PET_GLYPHS.len() {
        meta[i] = PetEyeMeta::from_layers(PET_GLYPHS[i].layers);
        i += 1;
    }
    meta
};

/// The LOD eye pass: each `Eye` subpath (one authored eye) is refilled solid as its own dot,
/// grown about its centre ([`zoom_about`]) toward [`EYE_DOT_TARGET_H_PX`] (open, capped at
/// [`EYE_DOT_SCALE_MAX`]) or [`EYE_DOT_TARGET_H_PX_ARC`] (shallow, capped at
/// [`EYE_DOT_SCALE_MAX_SHALLOW`] and [`EYE_DOT_ARC_KX_MAX`] wide).
#[inline]
fn paint_eye_dots(
    tile: &mut Tile,
    eye_meta: &PetEyeMeta,
    col: (f32, f32, f32),
    alpha: f32,
    xform: PathTransform,
    reflections: bool,
    eye_mask: &mut Option<Tile>,
) {
    let bounds = &eye_meta.bounds[..eye_meta.paths.len()];
    let ratios = &eye_meta.ratios[..eye_meta.paths.len()];
    let near_cx = eye_meta.near_cx;
    let inv_sx = 1.0 / xform.scale_x.max(f32::EPSILON);
    let inv_sy = 1.0 / xform.scale_y.max(f32::EPSILON);
    let min_gap = if xform.scale_x > 1.0 {
        EYE_DOT_GAP_PX * inv_sx
    } else {
        EYE_DOT_GAP_PX
    };
    let core_min_w_px = EYE_DOT_FAR_CORE_MIN_W_FRAC * xform.scale_y;
    let half_inv_sx = 0.5 * inv_sx;
    let half_inv_sy = 0.5 * inv_sy;
    #[inline]
    const fn fx(v: f32) -> u16 {
        (v * (FIXED_ONE as f32))
            .round()
            .clamp(0.0, FIXED_ONE as f32) as u16
    }
    let mut i = 0;
    while i < eye_meta.paths.len() {
        let path = &eye_meta.paths[i];
        let Some((x0, y0, x1, y1)) = bounds[i] else {
            i += 1;
            continue;
        };
        let r_i = ratios[i];
        if r_i < EYE_DOT_FAR_OCCLUDED_RATIO {
            i += 1;
            continue;
        }
        let compressed = r_i < EYE_DOT_FAR_COMPRESS_RATIO;
        let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        let h_px = (y1 - y0) * xform.scale_y;
        let w_px = (x1 - x0) * xform.scale_x;
        let shallow = w_px > 0.0 && h_px < EYE_DOT_SHALLOW_ASPECT * w_px;
        let (cap, target) = if shallow {
            (EYE_DOT_SCALE_MAX_SHALLOW, EYE_DOT_TARGET_H_PX_ARC)
        } else {
            (EYE_DOT_SCALE_MAX, EYE_DOT_TARGET_H_PX)
        };
        let ky = if h_px > 0.0 {
            (target / h_px).clamp(1.0, cap)
        } else {
            1.0
        };
        let mut kx = if shallow {
            ky.min(EYE_DOT_ARC_KX_MAX)
        } else {
            ky
        };
        let mut allowed_w = f32::MAX;
        let w_norm = x1 - x0;
        let mut j = 0;
        while j < bounds.len() {
            if j == i || ratios[j] < EYE_DOT_FAR_OCCLUDED_RATIO {
                j += 1;
                continue;
            }
            let Some((ox0, _, ox1, _)) = bounds[j] else {
                j += 1;
                continue;
            };
            let gap = (ox0 - x1).max(x0 - ox1);
            if gap > 0.0 {
                let closing = (w_norm + ox1 - ox0) * 0.5;
                if closing > 0.0 {
                    kx = kx.min(1.0 + (gap - min_gap) / closing);
                }
                let dist = ((ox0 + ox1) * 0.5 - cx).abs() * xform.scale_x;
                allowed_w = allowed_w.min((dist - EYE_DOT_GAP_PX).max(0.0));
            }
            j += 1;
        }
        let floor_k = core_min_w_px / w_px.max(f32::EPSILON);
        let kx_floor = floor_k.min(1.0);
        let kx = kx.clamp(kx_floor, ky);
        let ky = if shallow || compressed {
            ky
        } else {
            ky.min((EYE_DOT_ROUND_ASPECT_MAX * w_px * kx / h_px.max(f32::EPSILON)).max(1.0))
        };
        let (kx, anchor_x) = if compressed {
            let inner = if near_cx >= cx { x1 } else { x0 };
            ((kx * r_i).max(floor_k), inner)
        } else {
            (kx, cx)
        };
        let dot_xform = zoom_about(xform, anchor_x, cy, kx, ky);
        let paint_gloss =
            reflections && !shallow && (w_px * kx).min(h_px * ky) >= EYE_REFLECTION_MIN_PX;
        fill_path_fixed(tile, std::slice::from_ref(path), col, alpha, dot_xform);
        if paint_gloss {
            let mask = prepare_scratch_tile(eye_mask, tile.width(), tile.height());
            fill_path_fixed(
                mask,
                std::slice::from_ref(path),
                (0.0, 0.0, 0.0),
                1.0,
                dot_xform,
            );
        }
        let gx = anchor_x + (cx - anchor_x) * kx;
        {
            let allowed_w = allowed_w.max(core_min_w_px);
            let plug_w = EYE_DASH_CORE_PX.min(w_px * kx).min(allowed_w);
            let plug_h = EYE_DASH_CORE_PX.min(h_px * ky);
            let hw = plug_w * half_inv_sx;
            let hh = plug_h * half_inv_sy;
            let (x_lo, x_hi, y_lo, y_hi) = (fx(gx - hw), fx(gx + hw), fx(cy - hh), fx(cy + hh));
            let plug = [
                PathSeg::Move(x_lo, y_lo),
                PathSeg::Line(x_hi, y_lo),
                PathSeg::Line(x_hi, y_hi),
                PathSeg::Line(x_lo, y_hi),
                PathSeg::Close,
            ];
            fill_path_fixed(tile, &[&plug[..]], col, alpha, xform);
            if paint_gloss && let Some(mask) = eye_mask.as_mut() {
                fill_path_fixed(mask, &[&plug[..]], (0.0, 0.0, 0.0), 1.0, xform);
            }
        }
        if paint_gloss && let Some(eye_mask) = eye_mask.as_ref() {
            paint_eye_reflections(
                tile,
                eye_mask,
                xform.dx + gx * xform.scale_x,
                xform.dy + cy * xform.scale_y,
                w_px * kx,
                h_px * ky,
            );
        }
        i += 1;
    }
}

/// Bake-cache key for one pet tile: everything that can change a texel, and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PetBakeKey {
    /// The authored full-body pose (gait frame, idle, startle, …).
    pub pose: PetGlyphId,
    /// [`COAT_RAMP`](crate::cat_baker::COAT_RAMP) index for `Recolor::Coat` layers.
    pub coat: u8,
    /// [`EYE_RAMP`](crate::cat_baker::EYE_RAMP) index for `Recolor::Iris` layers.
    pub iris: u8,
    /// Quantized local terminal palette (accent family + background band).
    pub colors: CatColorKey,
    /// Tile width in px, exact: no patch strip.
    pub w: u16,
    /// Tile height in px.
    pub h: u16,
}

impl PetBakeKey {
    /// Resolve this key's ramp indices and local palette into bake fills (floats appear only past this point).
    #[inline]
    #[must_use]
    pub const fn fills(&self) -> ResolvedFills {
        ResolvedFills::from_context(self.coat, self.iris, self.colors)
    }

    /// Bake the tile this key describes (the cache-miss path).
    #[inline]
    #[must_use]
    pub fn bake(&self) -> Tile {
        bake_pose(
            self.pose,
            &self.fills(),
            u32::from(self.w),
            u32::from(self.h),
        )
    }

    /// The stable `host_id` for [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
    #[inline]
    #[must_use]
    pub const fn host_id(&self) -> u64 {
        host_tile_id(&[
            self.pose as u64,
            self.coat as u64,
            self.iris as u64,
            self.colors.accent as u64,
            self.colors.background as u64,
            self.w as u64,
            self.h as u64,
        ])
    }
}

/// The shared-atlas host-tile id of one exact-size bake key's integer fields: FNV-1a (stable across std releases, unlike `DefaultHasher`) with the high bit set, so it never collides with a small hand-assigned host id.
#[inline]
pub(crate) const fn host_tile_id(words: &[u64]) -> u64 {
    let mut h = aterm_hash::FNV64_OFFSET;
    let mut i = 0;
    while i < words.len() {
        h = aterm_hash::fnv_mix_u64(h, words[i]);
        i += 1;
    }
    h | (1 << 63)
}

/// Rasterize one authored pose into a fresh `w × h` [`Tile`]: painter order = layer order, each layer's colour from [`resolve_layer`], the fixed-point frame mapped by [`PathTransform::fit`].
#[inline]
#[must_use]
pub fn bake_pose(pose: PetGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    bake_pose_with_reflections(pose, fills, w, h, true)
}

#[inline]
fn bake_pose_with_reflections(
    pose: PetGlyphId,
    fills: &ResolvedFills,
    w: u32,
    h: u32,
    reflections: bool,
) -> Tile {
    let mut tile = Tile::new(w, h);
    paint_pose_onto(&mut tile, pose, fills, (w, h), reflections, &mut None);
    tile
}

#[inline]
fn paint_pose_onto(
    tile: &mut Tile,
    pose: PetGlyphId,
    fills: &ResolvedFills,
    (w, h): (u32, u32),
    reflections: bool,
    eye_mask: &mut Option<Tile>,
) {
    if w == 0 || h == 0 {
        return;
    }
    let xform = PathTransform::fit(w, h);
    let face_lod = h < FACE_DETAIL_MIN_H;
    let mouth_lod = h < MOUTH_DETAIL_MIN_H;
    let pose_idx = pose as usize;
    let eye_meta = &PET_EYE_META[pose_idx];
    let layers = PET_GLYPHS[pose_idx].layers;
    let mut i = 0;
    while i < layers.len() {
        let layer = &layers[i];
        i += 1;
        // LOD cull by authored role: whisker/blush/pattern are sub-pixel charm;
        // iris/catch-light/pupil (`Detail`) are what the dot and its
        // device-sized reflection replace.
        if (face_lod
            && matches!(
                layer.role,
                GlyphRole::Whisker
                    | GlyphRole::Blush
                    | GlyphRole::Pattern
                    | GlyphRole::Iris
                    | GlyphRole::CatchLight
                    | GlyphRole::Detail
            ))
            || (mouth_lod && layer.role == GlyphRole::Mouth)
        {
            continue;
        }
        let (col, alpha) = resolve_layer(layer, fills);
        if face_lod && layer.role == GlyphRole::Eye {
            paint_eye_dots(tile, eye_meta, col, alpha, xform, reflections, eye_mask);
            continue;
        }
        if matches!(layer.role, GlyphRole::Outline | GlyphRole::Whisker) {
            paint_outline_halo(tile, layer, fills, alpha, xform);
        }
        fill_path_fixed(tile, layer.paths, col, alpha, xform);
    }
}

/// A bake-cache key for an [`ExactTileBaker`]: everything that can change a texel of one exact-size tile, and nothing else.
pub trait ExactTileKey: Copy + PartialEq {
    /// Tile `(w, h)` in px; the tile is exactly this size.
    fn size(&self) -> (u16, u16);
    /// Rasterize the tile this key describes (the cache-miss path).
    fn bake_tile(&self) -> Tile;
    /// Rasterize the tile this key describes, reusing `recycled` when its
    /// `(width, height)` matches [`Self::size`] and `mask_scratch` for any
    /// intermediate mask pass.
    #[inline]
    fn bake_tile_with(&self, recycled: Option<Tile>, _mask_scratch: &mut Option<Tile>) -> Tile {
        let _ = recycled;
        self.bake_tile()
    }
}

impl ExactTileKey for PetBakeKey {
    #[inline]
    fn size(&self) -> (u16, u16) {
        (self.w, self.h)
    }

    #[inline]
    fn bake_tile(&self) -> Tile {
        self.bake()
    }

    #[inline]
    fn bake_tile_with(&self, recycled: Option<Tile>, mask_scratch: &mut Option<Tile>) -> Tile {
        let (w, h) = (u32::from(self.w), u32::from(self.h));
        let mut tile = take_or_new_tile(recycled, w, h);
        paint_pose_onto(
            &mut tile,
            self.pose,
            &self.fills(),
            (w, h),
            true,
            mask_scratch,
        );
        tile
    }
}

/// One resident tile: its key, its texels, and the frame it was last touched on.
struct ExactSlot<K> {
    key: K,
    tile: Tile,
    last_used: u64,
}

/// Host-side LRU of exact-size tiles, within [`MAX_SLOTS`] tiles, [`MAX_ATLAS_BYTES`] of texels
/// and [`MAX_BAKES_PER_FRAME`] cold bakes a frame: the structure behind every exact-size roster,
/// [`PetBaker`], [`DogBaker`](crate::dog_baker::DogBaker) and
/// [`AnimalBaker`](crate::animal_baker::AnimalBaker).
pub struct ExactTileBaker<K> {
    slots: Vec<ExactSlot<K>>,
    /// One recently evicted or cleared tile buffer, kept for reuse when the next cold bake
    /// requests the same dimensions.
    tile_scratch: Option<Tile>,
    /// Reusable LOD eye-reflection mask across cold bakes.
    eye_mask_scratch: Option<Tile>,
    /// Sum of resident tile bytes, kept incrementally so the [`MAX_ATLAS_BYTES`] bound is O(1)
    /// per admission.
    bytes: usize,
    cell_w: u16,
    cell_h: u16,
    /// LRU clock, advanced once per [`begin_frame`](ExactTileBaker::begin_frame).
    clock: u64,
    /// Cold bakes still allowed this frame.
    bakes_left: u32,
    /// Bumped on every bake and every clear, so a host folding it into a frame fingerprint
    /// re-uploads after either.
    version: u64,
}

impl<K> ExactTileBaker<K> {
    /// Construct an empty exact-size tile baker with zero resident bytes and no scratch buffers.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            tile_scratch: None,
            eye_mask_scratch: None,
            bytes: 0,
            cell_w: 0,
            cell_h: 0,
            clock: 0,
            bakes_left: 0,
            version: 0,
        }
    }
}

impl<K> Default for ExactTileBaker<K> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// The pet roster's exact-size tile cache.
pub type PetBaker = ExactTileBaker<PetBakeKey>;

impl<K: ExactTileKey> ExactTileBaker<K> {
    /// Per-frame prologue: advance the LRU clock, reset the cold-bake budget, and clear on a cell-metric change.
    #[inline]
    pub fn begin_frame(&mut self, cell_w: u16, cell_h: u16) {
        self.clock = self.clock.wrapping_add(1);
        self.bakes_left = MAX_BAKES_PER_FRAME;
        if (cell_w, cell_h) != (self.cell_w, self.cell_h) {
            self.clear();
            self.cell_w = cell_w;
            self.cell_h = cell_h;
        }
    }

    /// Drop every resident tile and bump the version.
    #[inline]
    pub fn clear(&mut self) {
        self.eye_mask_scratch = None;
        if self.slots.is_empty() {
            self.tile_scratch = None;
            return;
        }
        self.tile_scratch = match self.slots.pop() {
            Some(s) => Some(s.tile),
            None => None,
        };
        self.slots.clear();
        self.bytes = 0;
        self.version = self.version.wrapping_add(1);
    }

    /// Monotonic bake/clear counter, for the frame fingerprint.
    #[inline]
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Resident tile count (`≤ MAX_SLOTS`).
    #[cfg(test)]
    pub const fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the cache holds no tiles.
    #[cfg(test)]
    pub const fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Resident texel bytes (`≤ MAX_ATLAS_BYTES`).
    #[doc(hidden)]
    #[cfg(test)]
    pub const fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// Look `key` up, baking on a miss when the per-frame budget allows.
    #[inline]
    pub fn tile(&mut self, key: &K) -> Option<&[u8]> {
        let mut i = self.slots.len();
        while i > 0 {
            i -= 1;
            if self.slots[i].key == *key {
                self.slots[i].last_used = self.clock;
                return Some(self.slots[i].tile.pixels());
            }
        }
        if self.bakes_left == 0 {
            return None;
        }
        let (w, h) = key.size();
        if w == 0 || h == 0 {
            return None;
        }
        let want = usize::from(w) * usize::from(h) * 4;
        if want > MAX_ATLAS_BYTES {
            return None;
        }
        self.bakes_left -= 1;
        let mut recycled = self.tile_scratch.take();
        while !self.slots.is_empty()
            && (self.slots.len() >= MAX_SLOTS || self.bytes + want > MAX_ATLAS_BYTES)
        {
            if let Some(evicted) = self.evict_lru() {
                match &mut recycled {
                    None => recycled = Some(evicted),
                    Some(prev) if evicted.pixels().len() > prev.pixels().len() => {
                        let smaller = std::mem::replace(prev, evicted);
                        if self.tile_scratch.is_none() {
                            self.tile_scratch = Some(smaller);
                        }
                    }
                    Some(_) => {
                        if self.tile_scratch.is_none() {
                            self.tile_scratch = Some(evicted);
                        }
                    }
                }
            }
        }
        let tile = key.bake_tile_with(recycled, &mut self.eye_mask_scratch);
        self.bytes += tile.pixels().len();
        self.slots.push(ExactSlot {
            key: *key,
            tile,
            last_used: self.clock,
        });
        self.version = self.version.wrapping_add(1);
        Some(self.slots[self.slots.len() - 1].tile.pixels())
    }

    /// Drop the least-recently-used slot, lowest index breaking ties, so eviction is a
    /// deterministic function of the request sequence.
    #[inline]
    fn evict_lru(&mut self) -> Option<Tile> {
        let slots = self.slots.as_slice();
        if slots.is_empty() {
            return None;
        }
        let mut victim = 0;
        let mut best_used = slots[0].last_used;
        let mut idx = 1;
        while idx < slots.len() {
            if slots[idx].last_used < best_used {
                victim = idx;
                best_used = slots[idx].last_used;
            }
            idx += 1;
        }
        let evicted = self.slots.remove(victim);
        self.bytes -= evicted.tile.pixels().len();
        Some(evicted.tile)
    }
}

impl PetBaker {
    /// The authored aspect (width ÷ height) of a pose, from its viewbox; callers size the destination box from it.
    #[inline]
    #[must_use]
    pub const fn aspect(pose: PetGlyphId) -> f32 {
        const POSE_ASPECTS: [f32; PET_GLYPHS.len()] = {
            let mut out = [0.0f32; PET_GLYPHS.len()];
            let mut i = 0;
            while i < PET_GLYPHS.len() {
                let a = (PET_GLYPHS[i].aspect_x1000 as f32) / 1000.0;
                out[i] = if a > 0.001 { a } else { 0.001 };
                i += 1;
            }
            out
        };
        POSE_ASPECTS[pose as usize]
    }
}

#[cfg(test)]
#[path = "pet_baker_tests.rs"]
mod tests;
