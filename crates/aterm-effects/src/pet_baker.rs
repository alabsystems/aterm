// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow-kitty pet's bake path: one authored full-body pose glyph
//! ([`crate::pet_glyphs_gen::PET_GLYPHS`]) rasterized into one exact-size
//! straight-alpha RGBA8 tile, cached in a small LRU, and handed to the shared
//! cat atlas through
//! [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! It mirrors the cat path in [`crate::cat_baker`] (same rasterizer
//! [`fill_path_fixed`], same fill resolution [`resolve_layer`] over
//! [`ResolvedFills`], same structural budgets) with three differences:
//!
//! * No patch strip. Cat tiles carry `PATCH_STRIP` extra columns for the live
//!   gaze stage; the pet has no live gaze quads (its eyes are authored per
//!   pose), so a pet tile is exactly `w · h · 4` bytes, the shape `host_tile`
//!   wants.
//! * No [`EyesFrame`](crate::cat_baker::EyesFrame) axis. The pose is the
//!   expression, eyes included; blinking is a pose choice in
//!   [`crate::kitty_pet`], not a bake parameter.
//! * A size-aware face LOD. The pet ships at a couple of text rows, where the
//!   full authored face does not survive. Below [`FACE_DETAIL_MIN_H`] the bake
//!   culls sub-pixel charm ink (whiskers, authored catch-light, blush, tabby
//!   pattern) and collapses each eye to one strong dot; open eyes then get a
//!   size-aware glossy reflection. Below [`MOUTH_DETAIL_MIN_H`] the mouth goes
//!   too. Size is already a cache-key axis, so LOD needs no new key.
//!
//! ## Budgets
//!
//! * [`MAX_SLOTS`] resident tiles (the cat baker's value). The roster is about
//!   two dozen poses with one identity and one size live at a time, so 32
//!   slots hold a gait cycle plus its idle and startle branches.
//! * [`MAX_ATLAS_BYTES`] of resident texels (2 MiB), so a large cell metric
//!   cannot turn 32 slots into 32 huge tiles. A single tile larger than the
//!   whole budget is refused outright.
//! * [`MAX_BAKES_PER_FRAME`] cold bakes per presented frame; the caller keeps
//!   drawing its last resolved pose until the new tile lands.
//! * [`PetBaker::clear`] on a cell-metric change: pixel size is in the key, so
//!   stale tiles cannot be mis-sampled, but they would occupy dead slots.

use aterm_scene::vector::{FIXED_ONE, PathSeg};
use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_baker::{
    CatColorKey, MAX_ATLAS_BYTES, MAX_BAKES_PER_FRAME, MAX_SLOTS, ResolvedFills,
    paint_dark_masked_disc, paint_outline_halo, path_bounds, resolve_layer,
};
use crate::cat_glyphs_gen::{GlyphRole, Layer};
use crate::pet_glyphs_gen::{PET_GLYPHS, PetGlyphId};

// ── Size-aware face LOD ─────────────────────────────────────────────────────
//
// At ship size (cell_h 14 ⇒ a ~40×24 px tile at 1×, ~59×36 at 1.5×) a
// whisker, the authored catch-light, a blush oval, a tabby stripe or the iris
// ring each cover well under one device pixel, and 4×4 supersampling averages
// them into the eye, greying it out. Below these heights the face is drawn for
// the size: the charm stack is culled and each eye collapses to a strong dot of
// the eye ink. A reflection is then built in device pixels inside each open
// dot, clipped to the eye and never added to a happy or closed lid.
//
// Both thresholds are tile heights in px, the axis every facial feature's
// device coverage is proportional to.

/// Tile heights below this bake the LOD face; from here up the full authored
/// face returns, because its finest strokes clear a device pixel. Set at the
/// low end of the 56–64 px crossover, so a 2× tile at cell_h 14 (h = 48)
/// still gets the LOD.
pub const FACE_DETAIL_MIN_H: u32 = 56;

/// Below this tile height the mouth is dropped. A thickened mouth in the same
/// dark ink bridges the grown eye dots at 24–36 px and fuses the lower face
/// into a mask; without it the LOD face keeps two dark eyes, pink nose and pale
/// muzzle. The mouth returns once it can be a stroke rather than a bridge.
pub const MOUTH_DETAIL_MIN_H: u32 = 40;

/// The vertical size, in device pixels, a collapsed open eye dot grows toward:
/// a legibility floor, not an aesthetic size.
///
/// An open eye grows only far enough to host its guaranteed dark core
/// ([`EYE_DASH_CORE_PX`], 3.4 px) plus a little headroom for the authored curve.
/// At the 1× default bake (h = 24) the eye lands at 4 px, 16.7% of body height
/// against the authored 12.4%; from h ≈ 28 the authored eye already clears the
/// target and growth is the identity.
pub const EYE_DOT_TARGET_H_PX: f32 = 4.0;

/// The vertical target for shallow eye subpaths (happy arcs, closed lids),
/// equal to [`EYE_DASH_CORE_PX`]: an arc is a thin band that grows only until
/// the guaranteed core fits inside it; further and the lid stops reading as a
/// lid. Lids stay shorter than the 4 px open dot.
pub const EYE_DOT_TARGET_H_PX_ARC: f32 = 3.4;

/// Ceiling on a shallow subpath's horizontal growth. An arc's width already
/// carries its expression at the authored proportion; rasterization starves
/// only its interior rows, which the vertical target and core plug restore.
/// Growing width too fuses a pair into a sunglasses band. 1.6× keeps the dash
/// clearly wider than an open dot (a lid that reads as an open eye makes a
/// sleeping cat look awake).
pub const EYE_DOT_ARC_KX_MAX: f32 = 1.6;

/// Ceiling on an open eye's growth. With the target at [`EYE_DOT_TARGET_H_PX`]
/// it only binds below h ≈ 15, but keeps a degenerate 1 px feature from
/// growing into an eye the size of the muzzle.
pub const EYE_DOT_SCALE_MAX: f32 = 2.2;

/// Growth ceiling for shallow eye subpaths. An arc is a thin band whose
/// interior rows are what survive rasterization, and the roster's 1.2 px arcs
/// at the 24 px bake need 2.8× to reach [`EYE_DOT_TARGET_H_PX_ARC`] (a round eye
/// never needs more than ~1.5×). The chevron's wings slope, so growth alone
/// cannot guarantee a 2×2 core; that is [`EYE_DASH_CORE_PX`]'s job.
pub const EYE_DOT_SCALE_MAX_SHALLOW: f32 = 2.8;

/// Side, in device pixels, of the solid core plug every LOD eye gets: a square
/// of eye ink at the subpath's centre, clamped to the grown dot's footprint.
/// 3.4 px covers two full rows and columns at every sub-pixel phase (2 px plus
/// one boundary crossing plus anti-aliasing slack), where a 4 px ellipse's
/// fully covered pixels depend on phase. Inside a 4 px round dot the square's
/// corners poke ~0.3 px past the curve (invisible); in anything larger it
/// vanishes. Clamped to the grown dot (a degenerate size gets a sub-bar core,
/// not a square eye) and to the pair daylight ([`EYE_DOT_GAP_PX`]), so plugs
/// at authored centres cannot fuse close-set pairs.
pub const EYE_DASH_CORE_PX: f32 = 3.4;

/// Aspect (bbox height ÷ width, in device pixels, where the artist's
/// proportions hold; see [`paint_eye_dots`]) below which an eye subpath is
/// shallow: arcs and lids sit at 0.38–0.52, open/wide ellipses at 0.95–1.25.
pub const EYE_DOT_SHALLOW_ASPECT: f32 = 0.6;

/// The 3/4 far eye's LOD treatment: below [`FACE_DETAIL_MIN_H`], an eye
/// subpath authored narrower than this fraction of its widest sibling is
/// painted as a compressed core (full vertical growth, width held near its
/// authored foreshortened width and floored at
/// [`EYE_DOT_FAR_CORE_MIN_W_FRAC`]) rather than grown like the near eye.
///
/// Every pose that authors two eyes renders two (a one-eyed cat is not the
/// intent). Growing both eyes fully would leave two near-equal dots a pixel
/// apart that read as one smear; a full near dot beside a narrow far core
/// keeps the turned-head asymmetry.
///
/// Keyed on the authored width ratio: frontal/rest pairs sit at 0.88–1.0,
/// full-yaw locomotion pairs at 0.62, the peek's deep-3/4 pair at 0.67. 0.70
/// splits them with margin (authored ratios quantize within ~0.002).
pub const EYE_DOT_FAR_COMPRESS_RATIO: f32 = 0.70;

/// The only ratio that culls: an eye subpath narrower than this fraction of
/// its widest sibling is treated as occluded (a sliver while the eye passes
/// behind the head's profile) and paints nothing at LOD sizes. Far below every
/// live ratio (minimum 0.62), so no shipped pose loses an eye; it lets future
/// art author a behind-the-profile pass without a floating dot.
pub const EYE_DOT_FAR_OCCLUDED_RATIO: f32 = 0.40;

/// Width floor for the compressed far-eye core, as a fraction of tile height:
/// 2 device px at the 1× ship tile (h = 24), scaling with DPR. At 36 px it
/// floors the core at 3 px, the width at which its plug spans two full pixel
/// columns at every phase, so the far eye's ≥2×2 dark core is guaranteed. The
/// floor may exceed what the [`EYE_DOT_GAP_PX`] guard allows, so the compressed
/// core grows about its inner edge, away from the near eye (see
/// [`paint_eye_dots`]).
pub const EYE_DOT_FAR_CORE_MIN_W_FRAC: f32 = 2.0 / 24.0;

/// Horizontal daylight, in device pixels, each eye dot keeps from its
/// siblings. The chibi pair is authored nearly touching (about one device pixel
/// at ship size), so unguarded growth would fuse the eyes into one mask.
/// Device pixels because fusion is a raster effect.
///
/// The guard may cut below authored width: at the 24 px bake the frontal
/// pairs' authored gap is sub-pixel, so each dot narrows a little (floored at
/// [`EYE_DOT_FAR_CORE_MIN_W_FRAC`]) until the gap opens. Two slightly narrow
/// eyes with coat between them read as a face; two sharing a smudged bridge
/// pixel read as a mask.
pub const EYE_DOT_GAP_PX: f32 = 1.3;

/// Ceiling on an open eye's painted height over its painted width. When the
/// daylight guard narrows a close-set pair, height follows width down, so the
/// dot stays round (authored aspects 0.95–1.25) rather than becoming a tall
/// bar; a 2-wide dot may still be 2.8 tall. Arcs and compressed far cores are
/// exempt.
pub const EYE_DOT_ROUND_ASPECT_MAX: f32 = 1.4;

/// Smallest grown open-eye diameter that carries a reflection. The compressed
/// far eye at the 34–36 px bake (~3 px) clears it; tiny 1× footprints keep the
/// plain dark dot.
pub const EYE_REFLECTION_MIN_PX: f32 = 2.4;

/// Primary glossy catch-light radius as a fraction of the grown eye's smaller
/// diameter, with device-pixel bounds. The lower bound survives 4×4 coverage at
/// shipping size; the upper bound stops a large LOD tile growing a headlamp.
pub const EYE_REFLECTION_PRIMARY_FRAC: f32 = 0.13;
pub const EYE_REFLECTION_PRIMARY_MIN_PX: f32 = 0.55;
pub const EYE_REFLECTION_PRIMARY_MAX_PX: f32 = 0.82;

/// A second, quiet lower-right reflection makes a sufficiently large eye read
/// glossy rather than punched-out. It is withheld below this diameter: one
/// crisp highlight is cute; two sub-pixel gray flecks are noise.
pub const EYE_REFLECTION_ECHO_MIN_PX: f32 = 5.0;
pub const EYE_REFLECTION_ECHO_FRAC: f32 = 0.08;
pub const EYE_REFLECTION_ECHO_MIN_RADIUS_PX: f32 = 0.35;
pub const EYE_REFLECTION_ECHO_MAX_RADIUS_PX: f32 = 0.52;

/// `xform` with an extra `(kx, ky)` zoom about the normalized-frame point
/// `(cx, cy)`: that point maps to the same device pixel before and after, so
/// a feature grows in place instead of sliding across the face.
fn zoom_about(xform: PathTransform, cx: f32, cy: f32, kx: f32, ky: f32) -> PathTransform {
    PathTransform {
        scale_x: xform.scale_x * kx,
        scale_y: xform.scale_y * ky,
        dx: xform.dx + cx * xform.scale_x * (1.0 - kx),
        dy: xform.dy + cy * xform.scale_y * (1.0 - ky),
    }
}

/// Paint the glossy finish for one grown, open LOD eye.
///
/// Coordinates are device pixels, and offsets and radii are fractions of the
/// final grown footprint, so the compressed far eye and the full near eye
/// each get a glint that fits. With the primary at `(-0.22w, -0.23h)` and
/// radius at most `0.13·min(w,h)` the circle stays inside an elliptical eye
/// (normalized centre distance ≈0.64 plus radius ≤0.26); the lower echo has
/// more clearance. A catch-light leaking onto the coat would read as a stray
/// white fleck.
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
        // Warm-white and translucent: this is a reflection echo, not a second
        // pupil. The primary remains the one high-contrast attention point.
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

/// The LOD eye pass: each subpath of the `Eye` layer (one authored eye) is
/// refilled solid as its own dot, grown about its own centre ([`zoom_about`])
/// toward its shape's target: [`EYE_DOT_TARGET_H_PX`] for an open round eye
/// (capped at [`EYE_DOT_SCALE_MAX`]) or [`EYE_DOT_TARGET_H_PX_ARC`] for a
/// shallow arc/lid (capped at [`EYE_DOT_SCALE_MAX_SHALLOW`] vertically and
/// [`EYE_DOT_ARC_KX_MAX`] horizontally).
///
/// Horizontal growth is clamped per neighbour to keep [`EYE_DOT_GAP_PX`] of
/// daylight (cutting below authored width if needed), and an open eye's height
/// is held within [`EYE_DOT_ROUND_ASPECT_MAX`] of the surviving width. A 3/4
/// far eye ([`EYE_DOT_FAR_COMPRESS_RATIO`]) gets full vertical growth but a
/// narrow core, scaled by the pose's foreshortening ratio, floored at
/// [`EYE_DOT_FAR_CORE_MIN_W_FRAC`] of tile height, and anchored at its inner
/// edge so the floor grows away from the near eye.
///
/// Every dot then gets a solid [`EYE_DASH_CORE_PX`] plug at its centre, clamped
/// inside its footprint and outside the pair's daylight, guaranteeing a ≥2×2
/// dark core. Open dots finally get [`paint_eye_reflections`]; arcs, lids and
/// closed eyes stay pure ink.
fn paint_eye_dots(
    tile: &mut Tile,
    layer: &Layer,
    col: (f32, f32, f32),
    alpha: f32,
    xform: PathTransform,
    reflections: bool,
) {
    paint_eye_dots_with_mask_observer(tile, layer, col, alpha, xform, reflections, |_, _| {});
}

/// The LOD painter with an exact, per-authored-eye raster-mask observer. The
/// production wrapper above supplies a no-op observer; differential tests use
/// the same path to assign reflection texels to one grown eye without relying
/// on overlapping padded bounding boxes.
fn paint_eye_dots_with_mask_observer<F>(
    tile: &mut Tile,
    layer: &Layer,
    col: (f32, f32, f32),
    alpha: f32,
    xform: PathTransform,
    reflections: bool,
    mut observe: F,
) where
    F: FnMut(bool, &Tile),
{
    let bounds: Vec<Option<(f32, f32, f32, f32)>> =
        layer.paths.iter().map(|p| path_bounds(p)).collect();
    let width = |b: &Option<(f32, f32, f32, f32)>| b.map_or(0.0, |(x0, _, x1, _)| x1 - x0);
    let max_w = bounds.iter().map(width).fold(0.0, f32::max);
    // Authored width against the widest sibling — the head-yaw rig's
    // foreshortening axis (1.0 for a frontal pair, ~0.62 for a full yaw).
    let ratio = |b: &Option<(f32, f32, f32, f32)>| {
        if max_w > 0.0 { width(b) / max_w } else { 1.0 }
    };
    let occluded = |b: &Option<(f32, f32, f32, f32)>| ratio(b) < EYE_DOT_FAR_OCCLUDED_RATIO;
    // The widest sibling's centre: the near eye, which a compressed far core
    // must grow AWAY from.
    let near_cx = bounds
        .iter()
        .flatten()
        .max_by(|a, b| (a.2 - a.0).total_cmp(&(b.2 - b.0)))
        .map_or(0.5, |&(x0, _, x1, _)| (x0 + x1) * 0.5);
    for (i, path) in layer.paths.iter().enumerate() {
        let Some((x0, y0, x1, y1)) = bounds[i] else {
            continue;
        };
        // An occluded sliver paints nothing (no live pose authors one; see
        // [`EYE_DOT_FAR_OCCLUDED_RATIO`]); every visible eye renders a core.
        if occluded(&bounds[i]) {
            continue;
        }
        let compressed = ratio(&bounds[i]) < EYE_DOT_FAR_COMPRESS_RATIO;
        let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        // How far this dot must grow for its authored height to reach its
        // shape's device target (`scale_y` is the tile height in the fit). A
        // shallow subpath aims at the band target with the higher ceiling.
        // Shallowness is judged in device pixels: the normalized frame maps
        // each viewbox axis to 0..1 separately, warping aspects, while the
        // aspect-preserving tile restores the artist's proportions (arcs 0.41,
        // open eyes 0.95+).
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
        // width). Horizontal growth starts from the vertical factor, capped for
        // an arc (see [`EYE_DOT_ARC_KX_MAX`]).
        let min_gap = EYE_DOT_GAP_PX / xform.scale_x.max(1.0);
        let mut kx = if shallow {
            ky.min(EYE_DOT_ARC_KX_MAX)
        } else {
            ky
        };
        for (j, other) in bounds.iter().enumerate() {
            let Some(&(ox0, _, ox1, _)) = other.as_ref() else {
                continue;
            };
            // An occluded sibling paints nothing, so it cannot fuse and does not
            // throttle growth; a compressed sibling still paints and stays in
            // the guard.
            if j == i || occluded(other) {
                continue;
            }
            // Daylight between this eye and that one, as authored. Overlapping
            // ranges (a single eye drawn in two strokes) have nothing to guard.
            let gap = (ox0 - x1).max(x0 - ox1);
            if gap <= 0.0 {
                continue;
            }
            // Growing both dots about their own centres eats the gap at the
            // rate their half-widths sum to; stop while the floor remains.
            let closing = (x1 - x0 + ox1 - ox0) * 0.5;
            if closing > 0.0 {
                kx = kx.min(1.0 + (gap - min_gap) / closing);
            }
        }
        // The guard may cut below authored width (see [`EYE_DOT_GAP_PX`]),
        // floored at the guaranteed-core width.
        let kx_floor =
            (EYE_DOT_FAR_CORE_MIN_W_FRAC * xform.scale_y / w_px.max(f32::EPSILON)).min(1.0);
        let kx = kx.clamp(kx_floor, ky);
        // A round eye stays round: when the daylight guard narrowed the dot,
        // height follows width down (see [`EYE_DOT_ROUND_ASPECT_MAX`]). Never
        // below authored height, and not for non-round shapes.
        let ky = if shallow || compressed {
            ky
        } else {
            ky.min((EYE_DOT_ROUND_ASPECT_MAX * w_px * kx / h_px.max(f32::EPSILON)).max(1.0))
        };
        // The compressed far core: the near-eye horizontal treatment scaled by
        // the authored foreshortening ratio, floored at the guaranteed-core
        // width, and anchored at the inner edge (facing the near eye) so any
        // floor growth extends outward and the pair's daylight survives.
        let (kx, anchor_x) = if compressed {
            let floor_k = EYE_DOT_FAR_CORE_MIN_W_FRAC * xform.scale_y / w_px.max(f32::EPSILON);
            let inner = if near_cx >= cx { x1 } else { x0 };
            ((kx * ratio(&bounds[i])).max(floor_k), inner)
        } else {
            (kx, cx)
        };
        let dot_xform = zoom_about(xform, anchor_x, cy, kx, ky);
        let mut eye_mask = Tile::new(tile.width(), tile.height());
        fill_path_fixed(tile, std::slice::from_ref(path), col, alpha, dot_xform);
        fill_path_fixed(
            &mut eye_mask,
            std::slice::from_ref(path),
            (0.0, 0.0, 0.0),
            1.0,
            dot_xform,
        );
        // The dot's centre after the zoom: `cx` for a centre-anchored dot,
        // shifted outward for an inner-edge-anchored core. The plug sits on the
        // painted dot.
        let gx = anchor_x + (cx - anchor_x) * kx;
        // Every dot's guaranteed core (see [`EYE_DASH_CORE_PX`]): a solid square
        // of the same ink at the centre, clamped inside the grown footprint.
        {
            // Pair-daylight clamp: full-width plugs at authored centres would
            // meet in the middle of a close-set pair. Both siblings compute the
            // same distance and shrink to the same ceiling, keeping
            // [`EYE_DOT_GAP_PX`] symmetric.
            let mut allowed_w = f32::MAX;
            for (j, other) in bounds.iter().enumerate() {
                let Some(&(ox0, _, ox1, _)) = other.as_ref() else {
                    continue;
                };
                if j == i || occluded(other) {
                    continue;
                }
                let gap = (ox0 - x1).max(x0 - ox1);
                if gap <= 0.0 {
                    continue;
                }
                let dist = ((ox0 + ox1) * 0.5 - cx).abs() * xform.scale_x;
                allowed_w = allowed_w.min((dist - EYE_DOT_GAP_PX).max(0.0));
            }
            // Floored at the guaranteed-core width: when a pose authors its
            // pair closer than daylight plus two cores (the land's braced
            // squint), the core wins; a sub-pixel gap on a transient face is
            // recoverable, an invisible eye is not.
            let allowed_w = allowed_w.max(EYE_DOT_FAR_CORE_MIN_W_FRAC * xform.scale_y);
            let plug_w = EYE_DASH_CORE_PX.min(w_px * kx).min(allowed_w);
            let plug_h = EYE_DASH_CORE_PX.min(h_px * ky);
            let hw = plug_w * 0.5 / xform.scale_x.max(f32::EPSILON);
            let hh = plug_h * 0.5 / xform.scale_y.max(f32::EPSILON);
            let fx = |v: f32| {
                (v * f32::from(FIXED_ONE))
                    .round()
                    .clamp(0.0, f32::from(FIXED_ONE)) as u16
            };
            let plug = [
                PathSeg::Move(fx(gx - hw), fx(cy - hh)),
                PathSeg::Line(fx(gx + hw), fx(cy - hh)),
                PathSeg::Line(fx(gx + hw), fx(cy + hh)),
                PathSeg::Line(fx(gx - hw), fx(cy + hh)),
                PathSeg::Close,
            ];
            fill_path_fixed(tile, &[&plug[..]], col, alpha, xform);
            fill_path_fixed(&mut eye_mask, &[&plug[..]], (0.0, 0.0, 0.0), 1.0, xform);
        }
        observe(shallow, &eye_mask);
        if reflections && !shallow {
            paint_eye_reflections(
                tile,
                &eye_mask,
                xform.dx + gx * xform.scale_x,
                xform.dy + cy * xform.scale_y,
                w_px * kx,
                h_px * ky,
            );
        }
    }
}

/// Bake-cache key for one pet tile: everything that can change a texel, and
/// nothing else.
///
/// All-integer, like [`BakeKey`](crate::cat_baker::BakeKeyV4): the resolved
/// coat/iris/outline colours are `f32` triples, so they are re-derived from
/// ramp indices at bake time ([`PetBakeKey::fills`]) rather than hashed.
///
/// No `eyes` axis (the pose authors its eyes) and no age/scale axis (the exact
/// `w × h` already encodes every scaling decision).
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
    /// Tile width in px. The tile is EXACTLY this wide — no patch strip.
    pub w: u16,
    /// Tile height in px.
    pub h: u16,
}

impl PetBakeKey {
    /// Resolve this key's ramp indices + local palette into concrete bake fills
    /// (the key → colours step; floats appear only past this point).
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

    /// A stable `u64` identity for
    /// [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile), which
    /// keys host-authored tiles by an opaque id rather than by a cat key:
    /// `host_tile_id` over the key's own integer fields.
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

/// The shared-atlas host-tile id of one exact-size bake key's integer fields.
///
/// FNV-1a over `words`, not `DefaultHasher`, whose algorithm is not stable
/// across std releases. The high bit is set so a roster id never collides with
/// a small hand-assigned host id (such as the kitty cursor's); non-pet rosters
/// lead `words` with a namespace salt.
pub(crate) fn host_tile_id(words: &[u64]) -> u64 {
    let mut h = aterm_hash::FNV64_OFFSET;
    for &word in words {
        h = aterm_hash::fnv_mix_u64(h, word);
    }
    h | (1 << 63)
}

/// Rasterize one authored pose into a fresh `w × h` [`Tile`], painter order =
/// layer order, each layer's colour from [`resolve_layer`] and its fixed-point
/// `0..=FIXED_ONE` frame mapped by [`PathTransform::fit`].
///
/// Deterministic: the const integer drawlist and fixed-point filler give
/// byte-identical output for identical arguments on any host, so the LRU is a
/// pure function of its key.
///
/// Mixed-background halo: when [`ResolvedFills::outline_underlay`] is `Some`
/// (background band 4, a footprint spanning the light/dark crossover), the
/// silhouette and whisker layers get a pale keyline under the dark one; the
/// pair clears 3:1 where neither ink alone does. Four extra fills per miss.
///
/// Below [`FACE_DETAIL_MIN_H`] the face LOD applies: whisker, authored
/// catch-light, blush, pattern, iris and pupil layers (by authored role, not
/// drawlist position) are skipped, and each `Eye` subpath is refilled as one
/// grown dot ([`paint_eye_dots`]; the 3/4 far eye survives as a compressed
/// core), with open dots getting [`paint_eye_reflections`]. Below
/// [`MOUTH_DETAIL_MIN_H`] the mouth is culled. All keyed off `w`/`h`, already
/// in the cache key.
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
    for layer in PET_GLYPHS[pose as usize].layers {
        // The LOD cull list by authored role: whisker/blush/pattern are
        // sub-pixel charm, and iris/catch-light/pupil are the eye stack the dot
        // and device-sized reflection replace.
        if face_lod
            && matches!(
                layer.role,
                GlyphRole::Whisker
                    | GlyphRole::Blush
                    | GlyphRole::Pattern
                    | GlyphRole::Iris
                    | GlyphRole::CatchLight
                    | GlyphRole::Detail
            )
        {
            continue;
        }
        let (col, alpha) = resolve_layer(layer, fills);
        if face_lod && layer.role == GlyphRole::Eye {
            paint_eye_dots(&mut tile, layer, col, alpha, xform, reflections);
            continue;
        }
        if h < MOUTH_DETAIL_MIN_H && layer.role == GlyphRole::Mouth {
            continue;
        }
        if matches!(layer.role, GlyphRole::Outline | GlyphRole::Whisker) {
            paint_outline_halo(&mut tile, layer, fills, alpha, xform);
        }
        fill_path_fixed(&mut tile, layer.paths, col, alpha, xform);
    }
    tile
}

/// A bake-cache key for an [`ExactTileBaker`]: everything that can change a
/// texel of one exact-size tile, and nothing that cannot.
pub trait ExactTileKey: Copy + PartialEq {
    /// Tile `(w, h)` in px — the tile is EXACTLY this size, no patch strip.
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
/// [`DogBaker`](crate::dog_baker::DogBaker),
/// [`RobiBaker`](crate::robi_baker::RobiBaker) and
/// [`AnimalBaker`](crate::animal_baker::AnimalBaker).
///
/// `Default` is a valid empty baker; the first
/// [`begin_frame`](ExactTileBaker::begin_frame) adopts the cell metrics.
pub struct ExactTileBaker<K> {
    slots: Vec<ExactSlot<K>>,
    /// Sum of `slots[..].tile.pixels().len()`, kept incrementally so the
    /// [`MAX_ATLAS_BYTES`] residency bound costs O(1) per admission instead of
    /// a walk.
    bytes: usize,
    cell_w: u16,
    cell_h: u16,
    /// LRU clock, advanced once per [`begin_frame`](ExactTileBaker::begin_frame).
    clock: u64,
    /// Cold bakes still allowed this frame.
    bakes_left: u32,
    /// Monotonic; bumped on every bake and every wholesale clear, so a host that
    /// folds it into a frame fingerprint re-uploads after either.
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
    /// Per-tick prologue: advance the LRU clock, reset the per-frame cold-bake
    /// budget, and wholesale-clear on a cell-metric change.
    ///
    /// Call this exactly once per PRESENTED frame, not once per pane — the
    /// [`MAX_BAKES_PER_FRAME`] cap is a frame budget, and calling it per pane
    /// would multiply the cap by the pane count.
    pub fn begin_frame(&mut self, cell_w: u16, cell_h: u16) {
        self.clock = self.clock.wrapping_add(1);
        self.bakes_left = MAX_BAKES_PER_FRAME;
        if (cell_w, cell_h) != (self.cell_w, self.cell_h) {
            self.clear();
            self.cell_w = cell_w;
            self.cell_h = cell_h;
        }
    }

    /// Drop every resident tile and bump the version. A no-op (not even a
    /// version bump) when already empty, so a per-frame "effect is off" reset
    /// costs nothing and cannot make a quiescent host look dirty forever.
    pub fn clear(&mut self) {
        if self.slots.is_empty() {
            return;
        }
        self.slots.clear();
        self.bytes = 0;
        self.version = self.version.wrapping_add(1);
    }

    /// Monotonic bake/clear counter — fold it into the frame fingerprint.
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

    /// Resident texel bytes (`≤ MAX_ATLAS_BYTES`). Diagnostic: it is how a test
    /// proves the residency bound is enforced rather than merely intended.
    #[doc(hidden)]
    #[cfg(test)]
    pub fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// Look `key` up, baking on a miss when the per-frame budget allows.
    ///
    /// Returns straight-alpha RGBA8, exactly `w · h · 4` bytes, rows top-down,
    /// ready for [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile)
    /// with the key's own `host_id`.
    ///
    /// `None` means "not this frame": the cold-bake budget is spent, or the key
    /// is degenerate (zero-sized, or one tile larger than the whole residency
    /// budget). A deferred bake is not an all-transparent tile: keep drawing the
    /// previously resolved tile and ask again next frame.
    pub fn tile(&mut self, key: &K) -> Option<&[u8]> {
        let (w, h) = key.size();
        if w == 0 || h == 0 {
            return None;
        }
        let want = usize::from(w) * usize::from(h) * 4;
        // A tile that alone exceeds the residency budget is refused rather than
        // admitted by evicting everything else.
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
        // Make room BEFORE inserting, so the pushed slot's index is final and
        // the tile we are about to hand out can never be the one evicted.
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

    /// Drop the least-recently-used slot, lowest index breaking ties, so
    /// eviction is a deterministic function of the request sequence.
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
    /// The authored aspect (width ÷ height) of a pose, from its asset viewbox.
    /// Callers size the destination box from this, since poses differ in shape
    /// (a stretch is long and low, a sit tall and narrow). Clamped away from
    /// zero so a degenerate asset cannot produce a NaN/∞ rect.
    #[must_use]
    pub fn aspect(pose: PetGlyphId) -> f32 {
        (f32::from(PET_GLYPHS[pose as usize].aspect_x1000) / 1000.0).max(0.001)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pet_glyphs_gen::PET_GLYPH_IDS;

    /// The cat pose named `needle`: `pose("walk_0")` is `pet_walk_0`. Tests
    /// address the roster by asset id, not generated variant names, so a
    /// codegen rename cannot retarget a test.
    ///
    /// Exact match, not `contains`: the roster holds cat and dog poses sorted by
    /// id, and `pet_dog_walk_0` sorts before `pet_walk_0`, so a substring match
    /// would return the dog. For a dog pose use `pose_id("pet_dog_walk_0")`.
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

    /// The roster is non-empty and every pose has a sane authored aspect — the
    /// precondition every other test in here leans on.
    #[test]
    fn roster_is_populated_and_aspects_are_sane() {
        assert!(!PET_GLYPH_IDS.is_empty(), "pet roster must not be empty");
        assert_eq!(
            PET_GLYPH_IDS.len(),
            PET_GLYPHS.len(),
            "id table and glyph table must agree"
        );
        for &id in PET_GLYPH_IDS {
            let a = PetBaker::aspect(id);
            let authored = PET_GLYPHS[id as usize].aspect_x1000;
            assert!(
                a.is_finite() && a > 0.0,
                "{}: aspect {a} must be finite and positive",
                PET_GLYPHS[id as usize].id
            );
            assert!(authored > 0, "an authored viewbox must not be degenerate");
            assert_eq!(
                a,
                f32::from(authored) / 1000.0,
                "aspect must read the authored viewbox verbatim"
            );
            assert!(
                !PET_GLYPHS[id as usize].layers.is_empty(),
                "{}: a pose with no layers would bake an empty tile",
                PET_GLYPHS[id as usize].id
            );
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

    /// Exact size, no patch strip: the tile is `w · h · 4` bytes (a cat tile of
    /// the same art box would be `(w + PATCH_STRIP) · h · 4`).
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

    /// The cold-bake cap actually caps: exactly [`MAX_BAKES_PER_FRAME`] misses
    /// are served per frame, further misses are deferred, HITS are unaffected,
    /// and the budget resets on the next frame.
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

    /// LRU eviction holds the slot ceiling and picks the right victim: after
    /// filling every slot and touching key 0, the next admission evicts key 1.
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

    /// A cleared baker rebakes — and the clear bumps the version so the host
    /// re-uploads. An already-empty clear is silent (no spurious version bump).
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

    /// A cell-metric change wholesale-clears (a stale-size tile can never be
    /// hit again, so it must not squat on a slot).
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

    /// Degenerate keys never bake: a zero dimension, and a tile that alone
    /// exceeds the whole residency budget.
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

    /// Distinct genome/context inputs bake distinct tiles — proof the key axes
    /// are actually wired through to the fills rather than being decoration.
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
        // Distinct keys occupy distinct slots (no aliasing in the LRU).
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

    /// Face LOD acceptance: at ship size (cell_h 14 ⇒ a 36 px tile at 1.5×),
    /// for every live pose on both grounds, every authored eye subpath survives
    /// as its own ≥2×2 block of dark, fully opaque ink near its authored centre,
    /// the turned heads' compressed far eye included (no live pose may hit the
    /// occlusion gate). The per-pose core count equals the authored eye-subpath
    /// count, and a pair authored with daylight keeps distinct cores with a
    /// non-dark texel between them. "Dark" is every channel under 64 (the
    /// authored eye ink is rgb(36,31,41)).
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
                    // Every authored subpath is a visible eye that must render;
                    // the occlusion gate stays unused by the live roster.
                    for b in &bounds {
                        assert!(
                            (b.2 - b.0) >= EYE_DOT_FAR_OCCLUDED_RATIO * max_w_n,
                            "{needle}: an authored eye sits under the \
                             occlusion gate — the roster and \
                             EYE_DOT_FAR_OCCLUDED_RATIO have drifted"
                        );
                    }
                    // One ≥2×2 dark core per authored eye, near its centre
                    // (the dot grows about its own centre; the compressed far
                    // core shifts outward by under a pixel).
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
                        // The per-pose count already failed above; the
                        // distinctness probes need the full core set.
                        continue;
                    }
                    for pi in 0..bounds.len() {
                        for pj in pi + 1..bounds.len() {
                            // Overlapping authored ranges are one eye drawn
                            // in two strokes — nothing to separate. Probe
                            // only pairs the artist spaced by ≥1 device px.
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
                            // Daylight: strictly between the two blocks, on
                            // the rows around their centres, some texel must
                            // NOT be eye ink — two eyes, not one mask.
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

    /// Differential check of the reflection pass against an otherwise identical
    /// no-reflection bake: every changed texel was dark and owned by exactly one
    /// per-path grown-eye mask, every open eye gains its own pale texel, and
    /// shallow happy/closed/lidded paths gain none. The dark-core test above
    /// shows the reflected output keeps a 2×2 core.
    #[test]
    fn face_lod_reflections_are_contained_and_expression_aware() {
        const H: u32 = 36;
        let mut failures: Vec<String> = Vec::new();
        let mut exercised = 0usize;
        for &id in PET_GLYPH_IDS {
            let def = &PET_GLYPHS[id as usize];
            let w = (H as f32 * PetBaker::aspect(id)).round() as u32;
            let xform = PathTransform::fit(w, H);
            for dark_bg in [false, true] {
                let fills = ResolvedFills::from_indices(9, 4, dark_bg);
                let plain = bake_pose_with_reflections(id, &fills, w, H, false);
                let glossy = bake_pose_with_reflections(id, &fills, w, H, true);
                let mut eye_ink = Tile::new(w, H);
                let mut eye_masks: Vec<(bool, Vec<u8>)> = Vec::new();
                for layer in def.layers {
                    if layer.role != GlyphRole::Eye {
                        continue;
                    }
                    paint_eye_dots_with_mask_observer(
                        &mut eye_ink,
                        layer,
                        (0.0, 0.0, 0.0),
                        1.0,
                        xform,
                        false,
                        |shallow, mask| eye_masks.push((shallow, mask.pixels().to_vec())),
                    );
                }

                let changed: Vec<usize> = plain
                    .pixels()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(glossy.pixels().as_chunks::<4>().0)
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(i, _)| i)
                    .collect();
                for &pixel in &changed {
                    let i = pixel * 4;
                    let x = pixel % w as usize;
                    let y = pixel / w as usize;
                    let before = &plain.pixels()[i..i + 4];
                    let owners: Vec<usize> = eye_masks
                        .iter()
                        .enumerate()
                        .filter_map(|(eye, (_, mask))| (mask[i + 3] != 0).then_some(eye))
                        .collect();
                    if owners.len() != 1 || before[3] != 255 || !before[..3].iter().all(|&c| c < 96)
                    {
                        failures.push(format!(
                            "{} (dark_bg={dark_bg}): reflection texel ({x},{y}) \
                             has owners={owners:?}, before={before:?}",
                            def.id
                        ));
                    }
                }

                for (eye, (shallow, mask)) in eye_masks.iter().enumerate() {
                    let local: Vec<usize> = changed
                        .iter()
                        .copied()
                        .filter(|&pixel| mask[pixel * 4 + 3] != 0)
                        .collect();
                    if *shallow {
                        if !local.is_empty() {
                            failures.push(format!(
                                "{} (dark_bg={dark_bg}): shallow eye {eye} \
                                 gained reflection pixels {local:?}",
                                def.id
                            ));
                        }
                    } else {
                        exercised += 1;
                        if local.is_empty() {
                            failures.push(format!(
                                "{} (dark_bg={dark_bg}): open eye {eye} gained \
                                 no shipping-size reflection",
                                def.id
                            ));
                        } else if !local.iter().any(|&pixel| {
                            let i = pixel * 4;
                            glossy.pixels()[i..i + 3].iter().all(|&c| c >= 160)
                        }) {
                            failures.push(format!(
                                "{} (dark_bg={dark_bg}): open eye {eye} never \
                                 becomes visibly pale",
                                def.id
                            ));
                        }
                    }
                }
            }
        }
        assert!(exercised > 0, "the roster probe must exercise open eyes");
        assert!(
            failures.is_empty(),
            "the reflection LOD regressed:\n  {}",
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

    /// The eye-size rule at the sizes 1× desktops bake (24 px at the default
    /// cell_h 14) and at 1.5× (36 px): for every live pose on both grounds,
    /// each authored eye's dark ink component stays inside its box:
    ///
    /// * an open eye: no taller than max(authored height,
    ///   [`EYE_DOT_TARGET_H_PX`]), no wider than max(authored width,
    ///   [`EYE_DASH_CORE_PX`]), each plus 1.5 px of anti-aliasing slack;
    /// * a shallow arc/lid: height against [`EYE_DOT_TARGET_H_PX_ARC`], width
    ///   against [`EYE_DOT_ARC_KX_MAX`] × authored (a pair fused across its
    ///   daylight shows as one component twice its allowed width);
    ///
    /// and still exists (a few fully dark texels per eye). Subpaths with
    /// overlapping x-ranges are one eye in several strokes and are measured as
    /// one. Components are flood-filled inside a window sized from the bound,
    /// so an overgrown eye cannot escape measurement.
    #[test]
    fn lod_eyes_hold_the_artists_proportion_at_desktop_sizes() {
        // The Linux default, derived, not assumed: ART_ROWS · cell_h 14.
        let default_h = (crate::kitty_pet::ART_ROWS * 14.0).round() as u32;
        assert_eq!(default_h, 24, "the Linux-default bake height moved");
        let mut failures: Vec<String> = Vec::new();
        for &id in PET_GLYPH_IDS {
            let needle = PET_GLYPHS[id as usize].id;
            for h in [default_h, 36] {
                let w = (h as f32 * PetBaker::aspect(id)).round() as u32;
                for dark_bg in [false, true] {
                    let fills = ResolvedFills::from_indices(9, 4, dark_bg);
                    let tile = bake_pose(id, &fills, w, h);
                    let px = tile.pixels();
                    let dark = |x: i32, y: i32| -> bool {
                        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                            return false;
                        }
                        let i = ((y as u32 * w + x as u32) * 4) as usize;
                        px[i + 3] == 255 && px[i..i + 3].iter().all(|&c| c < 64)
                    };
                    for layer in PET_GLYPHS[id as usize].layers {
                        if layer.role != GlyphRole::Eye {
                            continue;
                        }
                        // Cluster subpaths sharing x-range, as the baker's
                        // daylight guard does (the roll's tilted face stacks
                        // its two arcs diagonally).
                        let mut clusters: Vec<((f32, f32, f32, f32), usize)> = Vec::new();
                        for b in layer.paths.iter().filter_map(|p| path_bounds(p)) {
                            if let Some((c, n)) =
                                clusters.iter_mut().find(|(c, _)| b.0 <= c.2 && c.0 <= b.2)
                            {
                                c.0 = c.0.min(b.0);
                                c.1 = c.1.min(b.1);
                                c.2 = c.2.max(b.2);
                                c.3 = c.3.max(b.3);
                                *n += 1;
                            } else {
                                clusters.push((b, 1));
                            }
                        }
                        for &((x0, y0, x1, y1), members) in &clusters {
                            let w_px = (x1 - x0) * w as f32;
                            let h_px = (y1 - y0) * h as f32;
                            let shallow = w_px > 0.0 && h_px < EYE_DOT_SHALLOW_ASPECT * w_px;
                            let (w_bound, h_bound) = if members > 1 {
                                // A multi-stroke union is bounded loosely (each
                                // member grows about its own centre); tight
                                // pinning is for single-stroke eyes.
                                (
                                    (w_px * EYE_DOT_SCALE_MAX).max(EYE_DASH_CORE_PX) + 1.5,
                                    h_px + EYE_DOT_TARGET_H_PX + 1.5,
                                )
                            } else if shallow {
                                (
                                    (w_px * EYE_DOT_ARC_KX_MAX).max(EYE_DASH_CORE_PX) + 1.5,
                                    h_px.max(EYE_DOT_TARGET_H_PX_ARC) + 1.5,
                                )
                            } else {
                                (
                                    w_px.max(EYE_DASH_CORE_PX) + 1.5,
                                    h_px.max(EYE_DOT_TARGET_H_PX) + 1.5,
                                )
                            };
                            let cx = ((x0 + x1) * 0.5 * w as f32).round() as i32;
                            let cy = ((y0 + y1) * 0.5 * h as f32).round() as i32;
                            // Seed: the dark texel nearest the authored
                            // centre (the dot may sit a rounded pixel off).
                            let seed = (0..=3)
                                .flat_map(|r| {
                                    ((cx - r)..=(cx + r)).flat_map(move |x| {
                                        ((cy - r)..=(cy + r)).map(move |y| (x, y))
                                    })
                                })
                                .find(|&(x, y)| dark(x, y));
                            let Some(seed) = seed else {
                                failures.push(format!(
                                    "{needle} h={h} (dark_bg={dark_bg}): the eye \
                                     at ({cx},{cy}) keeps NO dark ink — the dot \
                                     grew illegible"
                                ));
                                continue;
                            };
                            // Flood the component inside a bound-sized
                            // window: big enough to prove a violation,
                            // small enough to exclude the silhouette.
                            let (wx, wy) = (w_bound as i32 / 2 + 3, h_bound as i32 / 2 + 3);
                            let mut seen = std::collections::HashSet::new();
                            let mut queue = vec![seed];
                            seen.insert(seed);
                            let (mut bx0, mut by0, mut bx1, mut by1) =
                                (seed.0, seed.1, seed.0, seed.1);
                            let mut area = 0usize;
                            while let Some((x, y)) = queue.pop() {
                                area += 1;
                                bx0 = bx0.min(x);
                                by0 = by0.min(y);
                                bx1 = bx1.max(x);
                                by1 = by1.max(y);
                                for dx in -1..=1 {
                                    for dy in -1..=1 {
                                        let n = (x + dx, y + dy);
                                        if (n.0 - cx).abs() <= wx
                                            && (n.1 - cy).abs() <= wy
                                            && dark(n.0, n.1)
                                            && seen.insert(n)
                                        {
                                            queue.push(n);
                                        }
                                    }
                                }
                            }
                            let (comp_w, comp_h) = (bx1 - bx0 + 1, by1 - by0 + 1);
                            if comp_w as f32 > w_bound || comp_h as f32 > h_bound {
                                failures.push(format!(
                                    "{needle} h={h} (dark_bg={dark_bg}): the \
                                     {}px-authored eye at ({cx},{cy}) paints a \
                                     {comp_w}x{comp_h} dark component — over its \
                                     {w_bound:.1}x{h_bound:.1} lawful box \
                                     (shallow={shallow})",
                                    format_args!("{w_px:.1}x{h_px:.1}"),
                                ));
                            }
                            // The floor scales with what the size guarantees:
                            // at 36 px the full core plug fits every pose (≥2×2),
                            // at 24 px the land's sub-pixel braced squint carries
                            // one fully dark texel plus its anti-aliased halo.
                            let floor = if h >= 36 { 4 } else { 1 };
                            if area < floor {
                                failures.push(format!(
                                    "{needle} h={h} (dark_bg={dark_bg}): the eye \
                                     at ({cx},{cy}) keeps only {area} dark texels \
                                     — under the legibility floor of {floor}"
                                ));
                            }
                        }
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "the eye-size law regressed:\n  {}",
            failures.join("\n  ")
        );
    }

    /// The Retina identity path: a 2× default (cell_h ≈ 42 ⇒ a ~71 px tile)
    /// bakes at or above [`FACE_DETAIL_MIN_H`], where the LOD never runs and the
    /// full authored face is painted verbatim. At the boundary the first
    /// full-face height keeps the catch-light and one pixel below it the LOD
    /// culls it.
    #[test]
    fn retina_scale_bakes_hit_the_identity_path() {
        // The 2× mac default lands above the LOD, structurally.
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
        // Disable the LOD gloss for this probe: its primary reflection reuses
        // the authored catch-light's white, which a colour search would
        // otherwise mistake for the full-face layer.
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

    /// Smoke test: a baked standing pose is actually a CAT, not an empty box.
    /// The middle horizontal band (the body — above the feet, below the ear
    /// tips) must carry substantial opaque coverage, and the tile must contain
    /// more than one distinct colour.
    #[test]
    fn a_standing_pose_draws_a_cat() {
        let (w, h) = (128u32, 76u32);
        let stand = pose("stand");
        let fills = ResolvedFills::from_indices(9, 4, true);
        let tile = bake_pose(stand, &fills, w, h);
        let px = tile.pixels();
        assert_eq!(px.len(), (w * h * 4) as usize);

        let (lo, hi) = (h / 3, 2 * h / 3);
        let band = ((hi - lo) * w) as usize;
        let mut opaque = 0usize;
        for y in lo..hi {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                if px[i + 3] > 128 {
                    opaque += 1;
                }
            }
        }
        let coverage = opaque as f32 / band as f32;
        assert!(
            coverage > 0.20,
            "the standing pose covers only {:.1}% of its middle band — it did \
             not draw a cat",
            coverage * 100.0
        );

        // More than one colour: an outline plus a coat at minimum.
        let mut colors: Vec<u32> = px
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 200)
            .map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]))
            .collect();
        colors.sort_unstable();
        colors.dedup();
        assert!(
            colors.len() >= 3,
            "a pose should paint an outline, a coat and face ink — got {} \
             distinct opaque colours",
            colors.len()
        );
    }
}
