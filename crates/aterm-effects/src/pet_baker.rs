// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow-kitty pet's bake path: one authored full-body pose glyph
//! ([`crate::pet_glyphs_gen::PET_GLYPHS`]) rasterized into one exact-size straight-alpha RGBA8
//! tile, cached in a small LRU, and handed to the shared cat atlas through
//! [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! It mirrors the cat path in [`crate::cat_baker`] (same [`fill_path_fixed`], same
//! [`resolve_layer`] over [`ResolvedFills`], same budgets) with three differences:
//!
//! * No patch strip: the pet has no live gaze quads, so a tile is exactly `w · h · 4` bytes.
//! * No [`EyesFrame`](crate::cat_baker::EyesFrame) axis: the pose is the expression; blinking
//!   is a pose choice in [`crate::kitty_pet`].
//! * A size-aware face LOD: below [`FACE_DETAIL_MIN_H`] the bake culls sub-pixel charm ink and
//!   collapses each eye to one strong dot (open ones get a device-sized reflection); below
//!   [`MOUTH_DETAIL_MIN_H`] the mouth goes too. Size is already a key axis.
//!
//! ## Budgets
//!
//! * [`MAX_SLOTS`] resident tiles: one identity and one size live at a time, so 32 slots hold
//!   a gait cycle plus its idle and startle branches.
//! * [`MAX_ATLAS_BYTES`] of resident texels, so a large cell metric cannot make 32 huge tiles;
//!   a single tile over the whole budget is refused.
//! * [`MAX_BAKES_PER_FRAME`] cold bakes per presented frame; the caller keeps drawing its last
//!   pose until the new tile lands.
//! * [`PetBaker::clear`] on a cell-metric change: stale tiles cannot be mis-sampled (size is in
//!   the key) but would occupy dead slots.

use aterm_scene::vector::{FIXED_ONE, PathSeg};
use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_baker::{
    CatColorKey, MAX_ATLAS_BYTES, MAX_BAKES_PER_FRAME, MAX_SLOTS, ResolvedFills,
    paint_dark_masked_disc, paint_outline_halo, path_bounds, resolve_layer,
};
use crate::cat_glyphs_gen::{GlyphRole, Layer};
use crate::pet_glyphs_gen::{PET_GLYPHS, PetGlyphId};

// Size-aware face LOD. At ship size (cell_h 14: a ~40×24 px tile at 1×, ~59×36 at 1.5×) a
// whisker, catch-light, blush, tabby stripe or iris ring covers well under a device pixel, and
// 4×4 supersampling greys the eye out. Thresholds are tile heights in px.

/// Tile heights below this bake the LOD face; from here up the authored face's finest strokes
/// clear a device pixel. The low end of the 56–64 px crossover, so a 2× tile at cell_h 14
/// (48 px) still gets the LOD.
pub const FACE_DETAIL_MIN_H: u32 = 56;

/// Below this tile height the mouth is dropped: in the eye ink it would bridge the grown eye
/// dots at 24–36 px and fuse the lower face into a mask.
pub const MOUTH_DETAIL_MIN_H: u32 = 40;

/// The height, in device pixels, a collapsed open eye dot grows toward: a legibility floor
/// hosting the [`EYE_DASH_CORE_PX`] core plus headroom. From h ≈ 28 the authored eye already
/// clears it.
pub const EYE_DOT_TARGET_H_PX: f32 = 4.0;

/// The vertical target for shallow eye subpaths (happy arcs, closed lids), equal to
/// [`EYE_DASH_CORE_PX`]: grown further, a lid stops reading as a lid.
pub const EYE_DOT_TARGET_H_PX_ARC: f32 = 3.4;

/// Ceiling on a shallow subpath's horizontal growth. An arc's width already reads at authored
/// proportion, and wider growth fuses a pair into a sunglasses band; 1.6× keeps a lid clearly
/// wider than an open dot, so a sleeping cat does not look awake.
pub const EYE_DOT_ARC_KX_MAX: f32 = 1.6;

/// Ceiling on an open eye's growth: binds only below h ≈ 15, so a degenerate 1 px feature
/// cannot grow to the size of the muzzle.
pub const EYE_DOT_SCALE_MAX: f32 = 2.2;

/// Growth ceiling for shallow eye subpaths: the roster's 1.2 px arcs at the 24 px bake need
/// 2.8× to reach [`EYE_DOT_TARGET_H_PX_ARC`]. A sloped chevron still needs
/// [`EYE_DASH_CORE_PX`] for its 2×2 core.
pub const EYE_DOT_SCALE_MAX_SHALLOW: f32 = 2.8;

/// Side, in device pixels, of the solid core plug every LOD eye gets at its centre: 3.4 px
/// covers two full rows and columns at every sub-pixel phase, which a 4 px ellipse does not.
/// Clamped to the grown dot and to the pair daylight ([`EYE_DOT_GAP_PX`]).
pub const EYE_DASH_CORE_PX: f32 = 3.4;

/// Device-pixel aspect (height ÷ width) below which an eye subpath is shallow: arcs and lids
/// sit at 0.38–0.52, open eyes at 0.95–1.25.
pub const EYE_DOT_SHALLOW_ASPECT: f32 = 0.6;

/// Below [`FACE_DETAIL_MIN_H`], an eye subpath narrower than this fraction of its widest
/// sibling is the 3/4 far eye: painted as a compressed core (full vertical growth, width near
/// its foreshortened width, floored at [`EYE_DOT_FAR_CORE_MIN_W_FRAC`]) so the pair keeps the
/// turned-head asymmetry instead of two equal dots smearing into one. Authored ratios: frontal
/// 0.88–1.0, 3/4 yaw 0.62–0.67.
pub const EYE_DOT_FAR_COMPRESS_RATIO: f32 = 0.70;

/// An eye subpath narrower than this fraction of its widest sibling is occluded (passing
/// behind the head's profile) and paints nothing at LOD sizes. Far below every live ratio
/// (minimum 0.62), so no shipped pose loses an eye.
pub const EYE_DOT_FAR_OCCLUDED_RATIO: f32 = 0.40;

/// Width floor for the compressed far-eye core, as a fraction of tile height: 2 px at the 24 px
/// tile, 3 px at 36 px, where the plug spans two full columns at every phase. It may exceed
/// what [`EYE_DOT_GAP_PX`] allows, so the core grows about its inner edge.
pub const EYE_DOT_FAR_CORE_MIN_W_FRAC: f32 = 2.0 / 24.0;

/// Horizontal daylight, in device pixels, each eye dot keeps from its siblings (fusion is a
/// raster effect). The pair is authored about a device pixel apart at ship size, so unguarded
/// growth fuses it into a mask; the guard may narrow each dot below authored width (floored at
/// [`EYE_DOT_FAR_CORE_MIN_W_FRAC`]) to open the gap.
pub const EYE_DOT_GAP_PX: f32 = 1.3;

/// Ceiling on an open eye's painted height over width: when the daylight guard narrows a pair,
/// height follows so the dot stays round, not a tall bar. Arcs and far cores are exempt.
pub const EYE_DOT_ROUND_ASPECT_MAX: f32 = 1.4;

/// Smallest grown open-eye diameter that carries a reflection: the far eye at 34–36 px
/// (~3 px) clears it; tiny 1× eyes keep a plain dot.
pub const EYE_REFLECTION_MIN_PX: f32 = 2.4;

/// Primary catch-light radius as a fraction of the grown eye's smaller diameter, bounded in
/// device pixels: the floor survives 4×4 coverage at ship size, the ceiling stops a headlamp.
pub const EYE_REFLECTION_PRIMARY_FRAC: f32 = 0.13;
pub const EYE_REFLECTION_PRIMARY_MIN_PX: f32 = 0.55;
pub const EYE_REFLECTION_PRIMARY_MAX_PX: f32 = 0.82;

/// A second, quiet lower-right reflection makes a large eye read glossy; withheld below this
/// diameter, where it would be a sub-pixel grey fleck.
pub const EYE_REFLECTION_ECHO_MIN_PX: f32 = 5.0;
pub const EYE_REFLECTION_ECHO_FRAC: f32 = 0.08;
pub const EYE_REFLECTION_ECHO_MIN_RADIUS_PX: f32 = 0.35;
pub const EYE_REFLECTION_ECHO_MAX_RADIUS_PX: f32 = 0.52;

/// `xform` with an extra `(kx, ky)` zoom about the normalized point `(cx, cy)`, which keeps its
/// device pixel: a feature grows in place instead of sliding across the face.
#[inline]
fn zoom_about(xform: PathTransform, cx: f32, cy: f32, kx: f32, ky: f32) -> PathTransform {
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
        // Warm-white and translucent: an echo, not a second pupil.
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

/// The LOD eye pass: each `Eye` subpath (one authored eye) is refilled solid as its own dot,
/// grown about its centre ([`zoom_about`]) toward [`EYE_DOT_TARGET_H_PX`] (open, capped at
/// [`EYE_DOT_SCALE_MAX`]) or [`EYE_DOT_TARGET_H_PX_ARC`] (shallow, capped at
/// [`EYE_DOT_SCALE_MAX_SHALLOW`] and [`EYE_DOT_ARC_KX_MAX`] wide).
///
/// Width is clamped per neighbour to keep [`EYE_DOT_GAP_PX`] of daylight, and an open eye's
/// height to [`EYE_DOT_ROUND_ASPECT_MAX`] of its width. A 3/4 far eye
/// ([`EYE_DOT_FAR_COMPRESS_RATIO`]) gets full vertical growth but a narrow core anchored at its
/// inner edge. Every dot then gets an [`EYE_DASH_CORE_PX`] plug, and open dots get
/// [`paint_eye_reflections`]; arcs and lids stay pure ink.
fn paint_eye_dots(
    tile: &mut Tile,
    layer: &Layer,
    col: (f32, f32, f32),
    alpha: f32,
    xform: PathTransform,
    reflections: bool,
) {
    // Keep ordinary eye layers on the stack without making its capacity a
    // rendering limit: larger layers still paint every authored subpath.
    let mut inline_bounds = [None; 8];
    let heap_bounds = (layer.paths.len() > inline_bounds.len()).then(|| {
        layer
            .paths
            .iter()
            .map(|p| path_bounds(p))
            .collect::<Vec<_>>()
    });
    let bounds = match heap_bounds.as_deref() {
        Some(bounds) => bounds,
        None => {
            let bounds = &mut inline_bounds[..layer.paths.len()];
            for (slot, path) in bounds.iter_mut().zip(layer.paths.iter()) {
                *slot = path_bounds(path);
            }
            &*bounds
        }
    };
    let width = |b: &Option<(f32, f32, f32, f32)>| b.map_or(0.0, |(x0, _, x1, _)| x1 - x0);
    let max_w = bounds.iter().map(width).fold(0.0, f32::max);
    // Authored width over the widest sibling's: the head-yaw foreshortening
    // axis (1.0 frontal, ~0.62 full yaw).
    let ratio = |b: &Option<(f32, f32, f32, f32)>| {
        if max_w > 0.0 { width(b) / max_w } else { 1.0 }
    };
    let occluded = |b: &Option<(f32, f32, f32, f32)>| ratio(b) < EYE_DOT_FAR_OCCLUDED_RATIO;
    // The widest sibling is the near eye, which a compressed far core grows
    // away from.
    let near_cx = bounds
        .iter()
        .flatten()
        .max_by(|a, b| (a.2 - a.0).total_cmp(&(b.2 - b.0)))
        .map_or(0.5, |&(x0, _, x1, _)| (x0 + x1) * 0.5);
    let mut eye_mask: Option<Tile> = None;
    let min_gap = EYE_DOT_GAP_PX / xform.scale_x.max(1.0);
    let core_min_w_px = EYE_DOT_FAR_CORE_MIN_W_FRAC * xform.scale_y;
    let sx_eps = xform.scale_x.max(f32::EPSILON);
    let sy_eps = xform.scale_y.max(f32::EPSILON);
    for (i, path) in layer.paths.iter().enumerate() {
        let Some((x0, y0, x1, y1)) = bounds[i] else {
            continue;
        };
        let r_i = ratio(&bounds[i]);
        // An occluded sliver paints nothing; every visible eye renders a core.
        if r_i < EYE_DOT_FAR_OCCLUDED_RATIO {
            continue;
        }
        let compressed = r_i < EYE_DOT_FAR_COMPRESS_RATIO;
        let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        // Growth for the authored height to reach the shape's device target
        // (`scale_y` is the tile height). Shallowness is judged in device
        // pixels: the normalized frame warps aspects, the tile restores the
        // artist's proportions.
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
        // The daylight floor in the normalized frame (`scale_x` is the tile
        // width). Horizontal growth starts from the vertical factor.
        let mut kx = if shallow {
            ky.min(EYE_DOT_ARC_KX_MAX)
        } else {
            ky
        };
        let mut allowed_w = f32::MAX;
        for (j, other) in bounds.iter().enumerate() {
            let Some(&(ox0, _, ox1, _)) = other.as_ref() else {
                continue;
            };
            // An occluded sibling paints nothing, so it cannot fuse; a
            // compressed one still paints and stays in the guard.
            if j == i || occluded(other) {
                continue;
            }
            // Authored daylight; overlapping ranges (one eye in two strokes)
            // have nothing to guard.
            let gap = (ox0 - x1).max(x0 - ox1);
            if gap <= 0.0 {
                continue;
            }
            // Both dots growing about their centres close the gap at the rate
            // of their summed half-widths; stop while the floor remains.
            let closing = (x1 - x0 + ox1 - ox0) * 0.5;
            if closing > 0.0 {
                kx = kx.min(1.0 + (gap - min_gap) / closing);
            }
            // Pair-daylight clamp for the guaranteed core: full-width plugs at
            // authored centres would meet in a close-set pair. Both siblings
            // compute the same distance, so the gap stays symmetric.
            let dist = ((ox0 + ox1) * 0.5 - cx).abs() * xform.scale_x;
            allowed_w = allowed_w.min((dist - EYE_DOT_GAP_PX).max(0.0));
        }
        // The guard may cut below authored width, floored at the core width.
        let floor_k = core_min_w_px / w_px.max(f32::EPSILON);
        let kx_floor = floor_k.min(1.0);
        let kx = kx.clamp(kx_floor, ky);
        // A round eye stays round: height follows a guard-narrowed width down
        // (never below authored height), see [`EYE_DOT_ROUND_ASPECT_MAX`].
        let ky = if shallow || compressed {
            ky
        } else {
            ky.min((EYE_DOT_ROUND_ASPECT_MAX * w_px * kx / h_px.max(f32::EPSILON)).max(1.0))
        };
        // The compressed far core: the near-eye width scaled by the
        // foreshortening ratio, floored at the core width, and anchored at the
        // inner edge so floor growth extends away from the near eye.
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
            let mask = match eye_mask.as_mut() {
                Some(mask) => {
                    mask.pixels_mut().fill(0);
                    mask
                }
                None => eye_mask.insert(Tile::new(tile.width(), tile.height())),
            };
            fill_path_fixed(
                mask,
                std::slice::from_ref(path),
                (0.0, 0.0, 0.0),
                1.0,
                dot_xform,
            );
        }
        // The dot's centre after the zoom (shifted outward for an
        // inner-edge-anchored core); the plug sits on it.
        let gx = anchor_x + (cx - anchor_x) * kx;
        // The guaranteed core (see [`EYE_DASH_CORE_PX`]), clamped inside the
        // grown footprint.
        {
            // Floored at the core width: when a pose authors its pair closer
            // than daylight plus two cores (the land's braced squint), the core
            // wins; a sub-pixel gap is recoverable, an invisible eye is not.
            let allowed_w = allowed_w.max(core_min_w_px);
            let plug_w = EYE_DASH_CORE_PX.min(w_px * kx).min(allowed_w);
            let plug_h = EYE_DASH_CORE_PX.min(h_px * ky);
            let hw = plug_w * 0.5 / sx_eps;
            let hh = plug_h * 0.5 / sy_eps;
            let fx = |v: f32| {
                (v * f32::from(FIXED_ONE))
                    .round()
                    .clamp(0.0, f32::from(FIXED_ONE)) as u16
            };
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
    }
}

/// Bake-cache key for one pet tile: everything that can change a texel, and nothing else.
///
/// All-integer, like [`BakeKeyV4`](crate::cat_baker::BakeKeyV4): colours are re-derived from
/// ramp indices at bake time ([`PetBakeKey::fills`]). No `eyes` axis (the pose authors its
/// eyes) and no scale axis (the exact `w × h` encodes it).
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
    /// Resolve this key's ramp indices and local palette into bake fills (floats appear only
    /// past this point).
    #[must_use]
    pub fn fills(&self) -> ResolvedFills {
        ResolvedFills::from_context(self.coat, self.iris, self.colors)
    }

    /// Bake the tile this key describes (the cache-miss path).
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
    #[must_use]
    pub fn host_id(&self) -> u64 {
        host_tile_id(&[
            self.pose as u64,
            u64::from(self.coat),
            u64::from(self.iris),
            u64::from(self.colors.accent),
            u64::from(self.colors.background),
            u64::from(self.w),
            u64::from(self.h),
        ])
    }
}

/// The shared-atlas host-tile id of one exact-size bake key's integer fields: FNV-1a (stable
/// across std releases, unlike `DefaultHasher`) with the high bit set, so it never collides
/// with a small hand-assigned host id. Non-pet rosters lead `words` with a namespace salt.
#[inline]
pub(crate) fn host_tile_id(words: &[u64]) -> u64 {
    let mut h = aterm_hash::FNV64_OFFSET;
    for &word in words {
        h = aterm_hash::fnv_mix_u64(h, word);
    }
    h | (1 << 63)
}

/// Rasterize one authored pose into a fresh `w × h` [`Tile`]: painter order = layer order, each
/// layer's colour from [`resolve_layer`], the fixed-point frame mapped by
/// [`PathTransform::fit`]. Byte-identical output for identical arguments on any host.
///
/// When [`ResolvedFills::outline_underlay`] is `Some` (a footprint spanning the light/dark
/// crossover), outline and whisker layers get a pale keyline under the dark one; the pair
/// clears 3:1 where neither ink alone does.
///
/// Below [`FACE_DETAIL_MIN_H`] the face LOD culls whisker, blush, pattern, iris, catch-light
/// and pupil layers and refills each `Eye` subpath as one grown dot ([`paint_eye_dots`]);
/// below [`MOUTH_DETAIL_MIN_H`] the mouth goes too.
#[must_use]
pub fn bake_pose(pose: PetGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    bake_pose_with_reflections(pose, fills, w, h, true)
}

fn bake_pose_with_reflections(
    pose: PetGlyphId,
    fills: &ResolvedFills,
    w: u32,
    h: u32,
    reflections: bool,
) -> Tile {
    let mut tile = Tile::new(w, h);
    if w == 0 || h == 0 {
        return tile;
    }
    let xform = PathTransform::fit(w, h);
    let face_lod = h < FACE_DETAIL_MIN_H;
    let mouth_lod = h < MOUTH_DETAIL_MIN_H;
    for layer in PET_GLYPHS[pose as usize].layers {
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
            paint_eye_dots(&mut tile, layer, col, alpha, xform, reflections);
            continue;
        }
        if matches!(layer.role, GlyphRole::Outline | GlyphRole::Whisker) {
            paint_outline_halo(&mut tile, layer, fills, alpha, xform);
        }
        fill_path_fixed(&mut tile, layer.paths, col, alpha, xform);
    }
    tile
}

/// A bake-cache key for an [`ExactTileBaker`]: everything that can change a texel of one
/// exact-size tile, and nothing else.
pub trait ExactTileKey: Copy + PartialEq {
    /// Tile `(w, h)` in px; the tile is exactly this size.
    fn size(&self) -> (u16, u16);
    /// Rasterize the tile this key describes (the cache-miss path).
    fn bake_tile(&self) -> Tile;
}

impl ExactTileKey for PetBakeKey {
    fn size(&self) -> (u16, u16) {
        (self.w, self.h)
    }

    fn bake_tile(&self) -> Tile {
        self.bake()
    }
}

/// One resident tile: its key, its texels, and the frame it was last touched on.
struct ExactSlot<K> {
    key: K,
    tile: Tile,
    last_used: u64,
}

/// Host-side LRU of exact-size tiles (see the module docs for the budgets):
/// the structure behind every exact-size roster, [`PetBaker`],
/// [`DogBaker`](crate::dog_baker::DogBaker) and
/// [`AnimalBaker`](crate::animal_baker::AnimalBaker).
///
/// `Default` is a valid empty baker; the first
/// [`begin_frame`](ExactTileBaker::begin_frame) adopts the cell metrics.
pub struct ExactTileBaker<K> {
    slots: Vec<ExactSlot<K>>,
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

impl<K> Default for ExactTileBaker<K> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            bytes: 0,
            cell_w: 0,
            cell_h: 0,
            clock: 0,
            bakes_left: 0,
            version: 0,
        }
    }
}

/// The pet roster's exact-size tile cache.
pub type PetBaker = ExactTileBaker<PetBakeKey>;

impl<K: ExactTileKey> ExactTileBaker<K> {
    /// Per-frame prologue: advance the LRU clock, reset the cold-bake budget, and clear on a
    /// cell-metric change. Call once per presented frame, not per pane, or the
    /// [`MAX_BAKES_PER_FRAME`] cap multiplies by the pane count.
    pub fn begin_frame(&mut self, cell_w: u16, cell_h: u16) {
        self.clock = self.clock.wrapping_add(1);
        self.bakes_left = MAX_BAKES_PER_FRAME;
        if (cell_w, cell_h) != (self.cell_w, self.cell_h) {
            self.clear();
            self.cell_w = cell_w;
            self.cell_h = cell_h;
        }
    }

    /// Drop every resident tile and bump the version. A no-op when already empty, so a
    /// per-frame "effect is off" reset cannot make a quiescent host look dirty.
    pub fn clear(&mut self) {
        if self.slots.is_empty() {
            return;
        }
        self.slots.clear();
        self.bytes = 0;
        self.version = self.version.wrapping_add(1);
    }

    /// Monotonic bake/clear counter, for the frame fingerprint.
    #[inline]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Resident tile count (`≤ MAX_SLOTS`).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the cache holds no tiles.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Resident texel bytes (`≤ MAX_ATLAS_BYTES`).
    #[doc(hidden)]
    #[cfg(test)]
    pub fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// Look `key` up, baking on a miss when the per-frame budget allows. Returns straight-alpha
    /// RGBA8, exactly `w · h · 4` bytes, rows top-down, ready for
    /// [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
    ///
    /// `None` means "not this frame" (budget spent) or a degenerate key (zero-sized, or larger
    /// than the whole residency budget): keep drawing the previous tile and ask again.
    pub fn tile(&mut self, key: &K) -> Option<&[u8]> {
        let (w, h) = key.size();
        if w == 0 || h == 0 {
            return None;
        }
        let want = usize::from(w) * usize::from(h) * 4;
        // Refused rather than admitted by evicting everything else.
        if want > MAX_ATLAS_BYTES {
            return None;
        }
        if let Some(i) = self.slots.iter().position(|s| s.key == *key) {
            self.slots[i].last_used = self.clock;
            return Some(self.slots[i].tile.pixels());
        }
        if self.bakes_left == 0 {
            return None;
        }
        self.bakes_left -= 1;
        let tile = key.bake_tile();
        // Make room before inserting, so the tile about to be handed out is
        // never the one evicted.
        while !self.slots.is_empty()
            && (self.slots.len() >= MAX_SLOTS || self.bytes + want > MAX_ATLAS_BYTES)
        {
            self.evict_lru();
        }
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
    fn evict_lru(&mut self) {
        let Some(victim) = self
            .slots
            .iter()
            .enumerate()
            .min_by_key(|(idx, s)| (s.last_used, *idx))
            .map(|(idx, _)| idx)
        else {
            return;
        };
        self.bytes -= self.slots[victim].tile.pixels().len();
        self.slots.remove(victim);
    }
}

impl PetBaker {
    /// The authored aspect (width ÷ height) of a pose, from its viewbox; callers size the
    /// destination box from it. Clamped away from zero so a degenerate asset cannot make a NaN.
    #[inline]
    #[must_use]
    pub fn aspect(pose: PetGlyphId) -> f32 {
        (f32::from(PET_GLYPHS[pose as usize].aspect_x1000) / 1000.0).max(0.001)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pet_glyphs_gen::PET_GLYPH_IDS;

    /// The cat pose `pet_{needle}`, by exact asset id (tests address the roster by id, not
    /// generated variant names). Exact match because `pet_dog_walk_0` sorts before
    /// `pet_walk_0`; for a dog pose use `pose_id`.
    fn pose(needle: &str) -> PetGlyphId {
        pose_id(&format!("pet_{needle}"))
    }

    /// The pose with exactly this id, either species.
    fn pose_id(id: &str) -> PetGlyphId {
        PET_GLYPH_IDS
            .iter()
            .copied()
            .find(|g| PET_GLYPHS[*g as usize].id == id)
            .unwrap_or_else(|| panic!("pet roster has no pose with id `{id}`"))
    }

    /// A distinct key per `coat` index at a plausible working size.
    fn key(coat: u8) -> PetBakeKey {
        PetBakeKey {
            pose: pose("stand"),
            coat,
            iris: 3,
            colors: CatColorKey::default(),
            w: 48,
            h: 28,
        }
    }

    /// Determinism: one key ⇒ byte-identical texels, across repeated lookups,
    /// across independent bakers, and against a direct [`bake_pose`].
    #[test]
    fn same_key_bakes_byte_identical_tiles() {
        let k = key(5);
        let mut a = PetBaker::default();
        a.begin_frame(10, 20);
        let first = a.tile(&k).expect("bake").to_vec();
        let second = a.tile(&k).expect("hit").to_vec();
        assert_eq!(first, second, "a cache HIT must return the same texels");

        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        let other = b.tile(&k).expect("bake").to_vec();
        assert_eq!(first, other, "a fresh baker must bake byte-identically");

        let direct = bake_pose(k.pose, &k.fills(), u32::from(k.w), u32::from(k.h));
        assert_eq!(
            first,
            direct.pixels(),
            "the cached bake must equal the direct bake"
        );
    }

    /// A pet tile is exactly `w · h · 4` bytes, with no patch strip.
    #[test]
    fn tile_is_exact_size_with_no_patch_strip() {
        let k = key(7);
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        let px = b.tile(&k).expect("bake");
        assert_eq!(
            px.len(),
            usize::from(k.w) * usize::from(k.h) * 4,
            "pet tiles carry no reserved gaze strip"
        );
    }

    /// Exactly [`MAX_BAKES_PER_FRAME`] misses per frame; further misses defer, hits are free, and
    /// the budget resets each frame.
    #[test]
    fn bake_rate_is_capped_per_frame() {
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        for c in 0..MAX_BAKES_PER_FRAME as u8 {
            assert!(b.tile(&key(c)).is_some(), "miss {c} is within budget");
        }
        assert!(
            b.tile(&key(200)).is_none(),
            "a miss past the per-frame budget must be deferred"
        );
        assert!(b.tile(&key(0)).is_some(), "a HIT is not a bake");
        assert_eq!(b.len(), MAX_BAKES_PER_FRAME as usize);
        b.begin_frame(10, 20);
        assert!(b.tile(&key(200)).is_some(), "the budget resets each frame");
    }

    /// Filling every slot and touching key 0, the next admission evicts key 1.
    #[test]
    fn lru_holds_the_slot_ceiling_and_evicts_the_oldest() {
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        for c in 0..MAX_SLOTS as u8 {
            b.begin_frame(10, 20);
            assert!(b.tile(&key(c)).is_some(), "slot {c} bakes");
            assert!(b.len() <= MAX_SLOTS, "the ceiling is never exceeded");
        }
        assert_eq!(b.len(), MAX_SLOTS, "the cache filled to its ceiling");
        assert!(
            b.resident_bytes() <= MAX_ATLAS_BYTES,
            "residency stays inside the byte budget"
        );
        // Touch key 0 so key 1 becomes the least-recently-used.
        b.begin_frame(10, 20);
        assert!(b.tile(&key(0)).is_some(), "hit refreshes key 0");
        assert!(b.tile(&key(200)).is_some(), "a full cache still admits");
        assert_eq!(b.len(), MAX_SLOTS, "admission evicted exactly one slot");
        assert!(
            b.slots.iter().any(|s| s.key == key(0)),
            "the recently-touched key survived"
        );
        assert!(
            !b.slots.iter().any(|s| s.key == key(1)),
            "the least-recently-used key was evicted"
        );
    }

    /// A clear bumps the version and the rebake is byte-identical; clearing an empty baker is
    /// silent.
    #[test]
    fn a_cleared_baker_rebakes() {
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        let before = b.tile(&key(3)).expect("bake").to_vec();
        let v = b.version();
        b.clear();
        assert!(b.is_empty(), "clear drops every slot");
        assert_eq!(b.resident_bytes(), 0);
        assert_eq!(b.version(), v + 1, "clear bumps the version");
        b.clear();
        assert_eq!(b.version(), v + 1, "clearing an empty baker is silent");
        b.begin_frame(10, 20);
        let after = b.tile(&key(3)).expect("rebake");
        assert_eq!(before, after, "the rebake is byte-identical");
    }

    /// A cell-metric change clears the cache and bumps the version; an unchanged one does not.
    #[test]
    fn metric_change_clears_the_cache() {
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        assert!(b.tile(&key(0)).is_some());
        let v = b.version();
        b.begin_frame(12, 24);
        assert!(b.is_empty(), "a metric change clears");
        let bumped = b.version();
        assert!(bumped > v, "a metric change bumps the version");
        b.begin_frame(12, 24);
        assert_eq!(b.version(), bumped, "an unchanged metric is a no-op");
        assert!(b.is_empty());
    }

    /// A zero dimension, or one tile over the whole residency budget, is refused without
    /// spending a bake.
    #[test]
    fn degenerate_keys_are_refused_without_spending_a_bake() {
        let mut b = PetBaker::default();
        b.begin_frame(10, 20);
        for bad in [
            PetBakeKey { w: 0, ..key(0) },
            PetBakeKey { h: 0, ..key(0) },
            PetBakeKey {
                w: u16::MAX,
                h: u16::MAX,
                ..key(0)
            },
        ] {
            assert!(b.tile(&bad).is_none(), "{bad:?} must be refused");
        }
        assert!(b.is_empty(), "a refusal admits nothing");
        // The refusals did not consume the frame's bake budget.
        assert!(b.tile(&key(0)).is_some());
        assert!(b.tile(&key(1)).is_some());
        assert!(b.tile(&key(2)).is_none(), "still exactly two bakes/frame");
    }

    /// Coat, background band and pose each change the texels.
    #[test]
    fn key_axes_change_the_texels() {
        let mut b = PetBaker::default();
        let mut bake = |k: &PetBakeKey| {
            b.begin_frame(10, 20);
            b.tile(k).expect("bake").to_vec()
        };
        let base = key(2);
        let coat = bake(&PetBakeKey { coat: 14, ..base });
        let plain = bake(&base);
        assert_ne!(plain, coat, "the coat ramp must move texels");
        let dark = bake(&PetBakeKey {
            colors: CatColorKey {
                accent: 12,
                background: 3,
            },
            ..base
        });
        assert_ne!(plain, dark, "the background band must move texels");
        let other = bake(&PetBakeKey {
            pose: pose("sit"),
            ..base
        });
        assert_ne!(plain, other, "a different pose is a different tile");
        assert_eq!(b.len(), 4, "four distinct keys ⇒ four slots");
    }

    /// The `host_id` handed to the shared atlas is stable and key-separating.
    #[test]
    fn host_ids_are_stable_and_distinct() {
        let a = key(1);
        let b = key(2);
        assert_eq!(a.host_id(), key(1).host_id(), "same key ⇒ same host id");
        assert_ne!(
            a.host_id(),
            b.host_id(),
            "distinct keys ⇒ distinct host ids"
        );
        assert_ne!(
            a.host_id(),
            PetBakeKey { w: 49, ..a }.host_id(),
            "size participates in the host id"
        );
        assert!(
            (a.host_id() >> 63) == 1,
            "pet host ids live in the high half"
        );
    }

    /// At the 1.5× ship size (36 px), for every pose on both grounds, every authored eye subpath
    /// keeps its own ≥2×2 block of opaque dark ink (each channel under 64) near its centre, far
    /// eyes included, and a pair authored with daylight keeps a non-dark texel between cores.
    #[test]
    fn face_lod_keeps_a_dark_eye_core_at_ship_size() {
        const H: u32 = 36;
        let mut failures: Vec<String> = Vec::new();
        for &id in PET_GLYPH_IDS {
            let needle = PET_GLYPHS[id as usize].id;
            let w = (H as f32 * PetBaker::aspect(id)).round() as u32;
            for dark_bg in [false, true] {
                let fills = ResolvedFills::from_indices(9, 4, dark_bg);
                let tile = bake_pose(id, &fills, w, H);
                let px = tile.pixels();
                let dark = |x: i32, y: i32| -> bool {
                    if x < 0 || y < 0 || x >= w as i32 || y >= H as i32 {
                        return false;
                    }
                    let i = ((y as u32 * w + x as u32) * 4) as usize;
                    px[i + 3] == 255 && px[i..i + 3].iter().all(|&c| c < 64)
                };
                for layer in PET_GLYPHS[id as usize].layers {
                    if layer.role != GlyphRole::Eye {
                        continue;
                    }
                    let bounds: Vec<(f32, f32, f32, f32)> =
                        layer.paths.iter().filter_map(|p| path_bounds(p)).collect();
                    let max_w_n = bounds.iter().map(|b| b.2 - b.0).fold(0.0, f32::max);
                    // No live pose may hit the occlusion gate.
                    for b in &bounds {
                        assert!(
                            (b.2 - b.0) >= EYE_DOT_FAR_OCCLUDED_RATIO * max_w_n,
                            "{needle}: an authored eye sits under the \
                             occlusion gate — the roster and \
                             EYE_DOT_FAR_OCCLUDED_RATIO have drifted"
                        );
                    }
                    // One ≥2×2 dark core per authored eye, near its centre
                    // (a compressed far core shifts outward by under a pixel).
                    let mut cores: Vec<(i32, i32)> = Vec::new();
                    for b in &bounds {
                        let cx = ((b.0 + b.2) * 0.5 * w as f32).round() as i32;
                        let cy = ((b.1 + b.3) * 0.5 * H as f32).round() as i32;
                        let found = (cy - 2..=cy).find_map(|by| {
                            (cx - 2..=cx)
                                .find(|&bx| (bx..bx + 2).all(|x| (by..by + 2).all(|y| dark(x, y))))
                                .map(|bx| (bx, by))
                        });
                        match found {
                            Some(origin) => cores.push(origin),
                            None => failures.push(format!(
                                "{needle} (dark_bg={dark_bg}): the eye centred \
                                 at ({cx},{cy}) keeps no 2×2 dark core in a \
                                 {w}×{H} tile"
                            )),
                        }
                    }
                    if cores.len() < bounds.len() {
                        // Already failed above; the probes need every core.
                        continue;
                    }
                    for pi in 0..bounds.len() {
                        for pj in pi + 1..bounds.len() {
                            // Probe only pairs authored ≥1 device px apart;
                            // overlapping ranges are one eye in two strokes.
                            let gap_n =
                                (bounds[pj].0 - bounds[pi].2).max(bounds[pi].0 - bounds[pj].2);
                            if gap_n * (w as f32) < 1.0 {
                                continue;
                            }
                            let (ax, ay) = cores[pi];
                            let (bx, by) = cores[pj];
                            if (ax - bx).abs() < 2 && (ay - by).abs() < 2 {
                                failures.push(format!(
                                    "{needle} (dark_bg={dark_bg}): two authored \
                                     eyes share one dark core at ({ax},{ay}) — \
                                     the pose renders one-eyed"
                                ));
                                continue;
                            }
                            // Daylight: some texel strictly between the two
                            // blocks, near their centre rows, is not eye ink.
                            let (l, r) = if ax < bx { (ax, bx) } else { (bx, ax) };
                            let row = (ay + by) / 2 + 1;
                            if l + 2 < r
                                && !(l + 2..r).any(|x| (row - 1..=row + 1).any(|y| !dark(x, y)))
                            {
                                failures.push(format!(
                                    "{needle} (dark_bg={dark_bg}): no daylight \
                                     between the eye cores at ({ax},{ay}) and \
                                     ({bx},{by}) — the pair fused into one mask"
                                ));
                            }
                        }
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "the ship-size face regressed:\n  {}",
            failures.join("\n  ")
        );
    }

    /// The LOD deletes the authored charm stack and rebuilds only what the
    /// pixels can carry: at 36 px the walk keeps catch-light white from the
    /// device-sized reflections but has no fully covered blush pink or tabby
    /// brown. A bake above [`FACE_DETAIL_MIN_H`] keeps the authored versions of
    /// all three.
    #[test]
    fn face_lod_rebuilds_reflections_but_culls_other_charm_ink() {
        let p = pose("walk_0");
        let fills = ResolvedFills::from_indices(9, 4, false);
        let bake_at = |h: u32| {
            let w = (h as f32 * PetBaker::aspect(p)).round() as u32;
            bake_pose(p, &fills, w, h)
        };
        let small = bake_at(36);
        let large = bake_at(96);
        let has = |tile: &Tile, ink: [u8; 3]| {
            tile.pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] == 255 && px[..3] == ink)
        };
        let catch = PET_GLYPHS[p as usize]
            .layers
            .iter()
            .find(|l| l.role == GlyphRole::CatchLight)
            .expect("walk pose authors catch-lights");
        let (catch_col, _) = resolve_layer(catch, &fills);
        let byte = |c: f32| (c * 255.0 + 0.5) as u8;
        let catch_ink = [byte(catch_col.0), byte(catch_col.1), byte(catch_col.2)];
        assert!(
            has(&small, catch_ink),
            "the LOD must rebuild a crisp catch-light at shipping size"
        );
        assert!(
            has(&large, catch_ink),
            "the authored catch-light is missing above the LOD threshold"
        );

        for role in [GlyphRole::Blush, GlyphRole::Pattern] {
            let layer = PET_GLYPHS[p as usize]
                .layers
                .iter()
                .find(|l| l.role == role)
                .expect("walk pose authors both culled charm layers");
            let (col, _) = resolve_layer(layer, &fills);
            let ink = [byte(col.0), byte(col.1), byte(col.2)];
            assert!(
                !has(&small, ink),
                "{role:?} ink survives a 36 px bake — the LOD cull is not \
                 running"
            );
            assert!(
                has(&large, ink),
                "{role:?} ink is missing from a 96 px bake — the cull leaked \
                 above its threshold"
            );
        }
    }

    /// The Retina identity path: a 2× default (cell_h ≈ 42 ⇒ a ~71 px tile)
    /// bakes at or above [`FACE_DETAIL_MIN_H`], where the LOD never runs and the
    /// full authored face is painted verbatim. At the boundary the first
    /// full-face height keeps the catch-light and one pixel below it the LOD
    /// culls it.
    #[test]
    fn retina_scale_bakes_hit_the_identity_path() {
        let mac_2x_h = (crate::kitty_pet::ART_ROWS * 42.0).round() as u32;
        assert!(
            mac_2x_h >= FACE_DETAIL_MIN_H,
            "a 2x Retina default bake ({mac_2x_h} px) must clear the LOD \
             threshold ({FACE_DETAIL_MIN_H} px)"
        );
        let p = pose("stand");
        let fills = ResolvedFills::from_indices(9, 4, false);
        let catch_light = PET_GLYPHS[p as usize]
            .layers
            .iter()
            .find(|l| l.role == GlyphRole::CatchLight)
            .expect("the standing pose authors a catch-light");
        let (col, _) = resolve_layer(catch_light, &fills);
        let byte = |c: f32| (c * 255.0 + 0.5) as u8;
        let ink = [byte(col.0), byte(col.1), byte(col.2)];
        // Without the LOD gloss: its primary reflection reuses the authored
        // catch-light's white, which this colour search would mistake for it.
        let has_authored_catch_light = |h: u32| {
            let w = (h as f32 * PetBaker::aspect(p)).round() as u32;
            bake_pose_with_reflections(p, &fills, w, h, false)
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] == 255 && px[..3] == ink)
        };
        assert!(
            has_authored_catch_light(72),
            "a 72 px bake (the 2x Retina default) must paint the full \
             authored face, catch-light included"
        );
        assert!(
            has_authored_catch_light(FACE_DETAIL_MIN_H),
            "the first full-face height lost its catch-light — the LOD \
             leaked above its threshold"
        );
        assert!(
            !has_authored_catch_light(FACE_DETAIL_MIN_H - 1),
            "one pixel under the threshold must still bake the LOD face"
        );
    }
}
