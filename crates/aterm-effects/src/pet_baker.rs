// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow-kitty **pet**'s bake path: one authored full-body pose glyph
//! ([`crate::pet_glyphs_gen::PET_GLYPHS`]) rasterized into one EXACT-SIZE
//! straight-alpha RGBA8 tile, cached in a small LRU, and handed to the shared
//! cat atlas through
//! [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile).
//!
//! This is the decision twin of the cat path in [`crate::cat_baker`] — same
//! rasterizer ([`fill_path_fixed`]), same fill resolution ([`resolve_layer`]
//! over [`ResolvedFills`]), same structural budgets — with exactly three
//! deliberate differences, all of which delete work rather than add it:
//!
//! * **No patch strip.** A cat tile is baked `w + PATCH_STRIP` texels wide so
//!   the live gaze stage has somewhere to sample a pupil + catch-light from
//!   while the body quad samples only `[0, w)`. The pet has **no live gaze
//!   quads**: its eyes are authored into each pose (a crouch stares at the
//!   landing, a groom shuts them, a startle blows them wide), so the reserved
//!   strip would be four columns of transparent texels carried through every
//!   blit, every atlas upload, and every LRU byte budget for nothing. A pet
//!   tile is therefore exactly `w · h · 4` bytes with the art filling all of
//!   it — which is also precisely the shape `host_tile` wants.
//! * **No [`EyesFrame`](crate::cat_baker::EyesFrame) axis.** The cat's blink /
//!   squint is a bake-time squash of the eye family toward a shared eye line,
//!   because ONE authored head has to cover every mood. The pet roster instead
//!   *authors* its eye state per pose — the pose IS the expression — so an
//!   `eyes` key axis would multiply the cache by three while every one of those
//!   tiles fought the pose's own authored intent (a squashed startle is not a
//!   blink, it is a broken startle). Blinking is a POSE choice made upstream in
//!   [`crate::kitty_pet`], not a bake parameter here.
//! * **A size-aware face LOD.** The cat bakes at reveal/gallery sizes where
//!   the whole authored face survives; the pet ships at a couple of text rows,
//!   where it measurably does not. Below [`FACE_DETAIL_MIN_H`] the bake culls
//!   the sub-pixel charm ink (whiskers, catch-light, blush, tabby pattern) and
//!   collapses the eye stack to one solid dot per eye; below
//!   [`MOUTH_DETAIL_MIN_H`] the mouth goes too. Size is already a cache-key
//!   axis (`w`/`h`), so LOD needs no new key — one size never serves another
//!   size's tile. See the LOD const block for the design-review numbers.
//!
//! ## Budgets (all structural, none assumed)
//!
//! * [`MAX_SLOTS`] resident tiles — reused verbatim from the cat baker. The pet
//!   roster is ~2 dozen poses and only ONE identity (coat/iris) and ONE size are
//!   live at a time, so 32 slots hold a whole gait cycle plus its idle and
//!   startle branches without ever thrashing.
//! * [`MAX_ATLAS_BYTES`] of resident texels — the same 2 MiB ceiling the cat
//!   atlas is sized against, applied here as a *residency* bound so a large cell
//!   metric cannot turn 32 slots into 32 huge tiles. Whichever of the two bounds
//!   binds first wins; a single tile larger than the whole budget is refused
//!   outright rather than draining the cache to admit it.
//! * [`MAX_BAKES_PER_FRAME`] cold bakes per presented frame. A pet that just
//!   changed gait fills its new frames over a few ticks; the caller simply
//!   re-draws its last resolved pose until the tile lands (the same tolerance
//!   the cat entrance has).
//! * A wholesale [`PetBaker::clear`] on cell-metric change. The pixel size is
//!   already IN the key, so a stale tile can never be *mis-sampled* — but it
//!   would occupy a slot no live key can ever hit again, so it is dropped
//!   eagerly rather than waiting to age out.

use aterm_scene::vector::{FIXED_ONE, PathSeg};
use aterm_scene::{PathTransform, Tile, fill_path_fixed};

use crate::cat_baker::{
    CatColorKey, MAX_ATLAS_BYTES, MAX_BAKES_PER_FRAME, MAX_SLOTS, ResolvedFills, resolve_layer,
};
use crate::cat_glyphs_gen::{GlyphRole, Layer};
use crate::pet_glyphs_gen::{PET_GLYPHS, PetGlyphId};

// ── Size-aware face LOD (design-review #4, resolution C1's second half) ─────
//
// The chibi rig pass grew the AUTHORED face; these thresholds make the BAKE
// stop painting detail the canvas cannot hold. At ship size (cell_h 14 ⇒ a
// ~40×24 px tile at 1×, ~59×36 at 1.5× HiDPI) a whisker, a catch-light, a
// blush oval, a tabby stripe or the iris ring each cover well under one device
// pixel, and the 4×4-supersampled filler dutifully AVERAGES them into whatever
// they cross. The review measured what that does to the face's one
// load-bearing feature: a single-pixel eye of rgb(144,144,135) on a
// rgb(223,199,132) coat — the eye grayed out of existence by its own charm.
// Below these heights the face is drawn FOR the size: the charm ink is culled
// and the whole eye stack collapses to one solid dot of the authored eye ink.
//
// Both thresholds are tile HEIGHTS in px — the axis the emitter derives from
// `cell_h`, and (the roster sharing one viewbox) the axis every facial
// feature's device coverage is proportional to.

/// Tile heights below this bake the LOD face; from here up the full authored
/// face returns, because its finest strokes finally clear a device pixel. The
/// review bracketed the crossover at 56–64 px; it is pinned at the LOW end so
/// the first tile granted the full drawing is one that can actually afford
/// it, while a 2× HiDPI tile at the common cell_h 14 (h = 48) still gets the
/// LOD it visibly needs.
pub const FACE_DETAIL_MIN_H: u32 = 56;

/// Below this tile height the mouth is dropped. The review allowed either
/// "drop" or "thicken ~1.8× if kept", and keeping it was tried first: the
/// authored W-smile hangs exactly between and below the grown eye dots, in the
/// SAME dark ink, so at 24–36 px the thickened stroke bridges the pair and the
/// whole lower face fuses into one mask (walk/sit), while the purr's smile
/// smudges over its happy arcs. Dropped, the LOD face keeps the chibi grammar
/// that survives these sizes — two dark eyes, pink nose, pale muzzle — and the
/// mouth returns with the rest of the fine strokes as soon as the canvas can
/// carry it as a STROKE rather than a bridge.
pub const MOUTH_DETAIL_MIN_H: u32 = 40;

/// The vertical size, in DEVICE pixels, a collapsed OPEN eye dot grows
/// toward — the legibility floor, not an aesthetic size.
///
/// This constant is where the owner's "the kitty eyes are too big" bug
/// lived. The first size-aware law aimed every dot at 7 px — the number
/// that made the purr's thin arc pass the dark-core bar at the 36 px
/// acceptance bake — and open eyes rode the same target. On a 1× Linux
/// desktop the default cell (cell_h 14) bakes the pet 24 px tall, where the
/// authored open eye is 3.0 px — 12.4% of body height — and the 7 px target
/// drove it to the 2.2× ceiling: a 6.5 px dot, 27% of the body, twice the
/// artist's proportion. Every Retina mac dodged the bug by geometry (its 2×
/// device-pixel bake lands at h ≈ 72, above [`FACE_DETAIL_MIN_H`], where no
/// LOD runs at all), so the inflated face WAS the everyday kitty precisely
/// and only on 1× displays.
///
/// The corrected law grows an open eye only far enough to HOST its
/// guaranteed dark core: [`EYE_DASH_CORE_PX`] (3.4 px — two full rows and
/// columns at every raster phase) plus headroom for the authored curve to
/// peek around the plug. At the Linux default bake the eye lands at 4 px —
/// 16.7% of body height against the authored 12.4% — and from h ≈ 28 up the
/// authored eye already clears the target and the growth is IDENTITY: a 36
/// or 48 px bake now paints the open eyes exactly as drawn. Measured on the
/// probe sweep (stand/sit/walk rosters, both grounds): before, 27% of body
/// height at h = 24 and 19.4% at h = 36; after, 16.7% and the authored
/// 12.3–12.4%.
pub const EYE_DOT_TARGET_H_PX: f32 = 4.0;

/// The vertical target for SHALLOW eye subpaths — the happy arcs and closed
/// lids. Deliberately EQUAL to [`EYE_DASH_CORE_PX`]: an arc is a thin band
/// whose dark rows the raster grid rations, and the band needs to grow
/// exactly until the guaranteed core plug fits inside it — any further and
/// the lid stops reading as a lid. The old 7 px target pushed every arc to
/// its 2.8× ceiling at every LOD size, painting the purr's 2.0 px squint
/// 5.7 px tall at a 36 px bake; at this target the same squint carries 3.4 px
/// — two guaranteed dark rows with the chevron's wings still showing — and
/// the sleeping faces keep their lids readable as LIDS, clearly shorter than
/// the 4 px open dot beside a purr's smile.
pub const EYE_DOT_TARGET_H_PX_ARC: f32 = 3.4;

/// Ceiling on a shallow subpath's HORIZONTAL growth. The old law grew arcs
/// uniformly, and the width is where the sunglasses came from: at the 2.8×
/// vertical ceiling the purr's pair painted 9.3 + 8.2 px of dark band across
/// a 24 px cat — the whole muzzle top fused into one mask, and the sleeper
/// "wearing sunglasses" was the owner-visible symptom. An arc's WIDTH already
/// carries its expression at the authored proportion (13.8% of body width,
/// the widest feature on the face); what rasterization starves is only its
/// interior rows, which the vertical target and the core plug restore. 1.6×
/// is enough spare width that the plug plus wings read as one dash while the
/// dash stays clearly WIDER than the open dot it must never be confused with
/// (a lid that reads as an open eye is a sleeping cat that reads awake — the
/// `sleep_0` bug of the first LOD pass).
pub const EYE_DOT_ARC_KX_MAX: f32 = 1.6;

/// Ceiling on the size-aware growth of an open eye. With the target at
/// [`EYE_DOT_TARGET_H_PX`] this only binds below h ≈ 15 — cells no live
/// metric produces — but it stays: uncapped, a degenerate 1 px feature would
/// grow 4× and paint an eye the size of the muzzle, so the LOD accepts a
/// sub-bar core at absurd sizes rather than inventing a new face.
pub const EYE_DOT_SCALE_MAX: f32 = 2.2;

/// The growth ceiling for SHALLOW eye subpaths. A round eye is solid ink, so
/// growth only widens an already-solid core; an arc is a thin BAND, and what
/// survives rasterization is the band's interior rows — the roster's 1.2 px
/// arcs at the Linux-default 24 px bake need 2.8× to reach the
/// [`EYE_DOT_TARGET_H_PX_ARC`] band, where a round eye never needs more than
/// ~1.5×. Growth alone cannot make the band's dark rows a guarantee, though:
/// the chevron's wings SLOPE, so away from the centre column the
/// fully-covered rows thin out and a 2×2 core stays phase-dependent at any
/// sane ceiling — which is what [`EYE_DASH_CORE_PX`] is for.
pub const EYE_DOT_SCALE_MAX_SHALLOW: f32 = 2.8;

/// Side, in device pixels, of the solid CORE PLUG every LOD eye gets: a
/// small square of the eye ink filled at the subpath's centre, clamped to
/// the grown dot's own footprint. 3.4 px covers two full pixel rows and two
/// full columns at every sub-pixel phase (2 px for the bar, plus one
/// boundary crossing, plus anti-aliasing slack).
///
/// Originally the plug patched only the shallow arcs and the compressed far
/// cores, because the open eyes' 7 px growth target made their cores huge by
/// brute force. With open growth pulled back to the authored proportion
/// (see [`EYE_DOT_TARGET_H_PX`]) the plug is the guarantee for EVERY eye:
/// a 4 px solid ellipse's fully-covered pixels are phase-dependent, a 3.4 px
/// solid square's are not. Inside a 4 px round dot the square's corners poke
/// ~0.3 px past the curve — under half an anti-aliased pixel, invisible —
/// and inside anything larger it vanishes entirely. Two clamps keep it
/// honest: the footprint clamp (the plug may not exceed the grown dot, so a
/// degenerate size gets a sub-bar core rather than a square eye) and the
/// pair-daylight clamp (the plug may not close the [`EYE_DOT_GAP_PX`] gap
/// its own ellipse was throttled to keep — plugs painted at authored centres
/// would otherwise fuse exactly the close-set pairs the guard exists for).
pub const EYE_DASH_CORE_PX: f32 = 3.4;

/// Aspect (bbox height ÷ width, measured in DEVICE pixels — the frame where
/// the artist's proportions hold; see the shallowness note in
/// [`paint_eye_dots`]) below which an eye subpath counts as shallow: the
/// roster's arcs and lids sit at 0.38–0.52, the open/wide ellipses at
/// 0.95–1.25 — 0.6 splits the populations with margin on both sides.
pub const EYE_DOT_SHALLOW_ASPECT: f32 = 0.6;

/// The 3/4 far-eye's LOD fate — COMPRESSED, not culled: below
/// [`FACE_DETAIL_MIN_H`], an eye subpath authored narrower than this fraction
/// of its widest sibling is painted as a compressed core (vertically full,
/// horizontally held near its authored foreshortened width and floored at
/// [`EYE_DOT_FAR_CORE_MIN_W_FRAC`]) instead of being grown like the near eye.
///
/// The first LOD pass CULLED these eyes outright, on the theory that one dot
/// plus the yaw's offset muzzle is how a turned head reads at sprite size.
/// The owner ruled on the shipped pixels: it reads as a ONE-EYED cat —
/// "that's not cute that's creepy" — so every pose that authors two eyes now
/// renders two eye cores at every size. The compression is what the cull was
/// actually protecting against: growing BOTH eyes toward the device target
/// left two near-equal dark dots a device pixel apart, and the pair read as
/// one ink smear. A full near dot beside a deliberately narrow far core keeps
/// the turned-head asymmetry without deleting the second eye.
///
/// Keyed on the AUTHORED width ratio. The roster's populations: the
/// frontal/rest pairs at 0.88–1.0 (grown whole), the full-yaw locomotion
/// pairs at 0.62 and the peek's deep-3/4 pair at 0.67 (compressed). 0.70
/// splits the populations with ~0.03 of margin below and ~0.18 above — never
/// a quantization coin flip (authored ratios quantize within ~0.002).
pub const EYE_DOT_FAR_COMPRESS_RATIO: f32 = 0.70;

/// The only ratio that still culls: an eye subpath authored narrower than
/// this fraction of its widest sibling is treated as GENUINELY OCCLUDED — a
/// sliver the rig leaves while an eye passes behind the head's profile — and
/// paints nothing at LOD sizes. Deliberately far below every live ratio (the
/// roster's minimum is 0.62, see the populations above): no shipped pose
/// loses an eye to this gate, and per the owner's one-eyed-is-creepy ruling
/// none may. It exists so future art CAN author a true behind-the-profile
/// pass without the baker inventing a floating dot for the leftover sliver.
pub const EYE_DOT_FAR_OCCLUDED_RATIO: f32 = 0.40;

/// Width floor for the compressed far-eye core, as a fraction of the tile
/// HEIGHT (the axis every facial feature's device coverage is proportional
/// to): 2 device px at the 1× ship tile (cell_h 14 ⇒ h = 24), scaling with
/// the DPR so every HiDPI rendering of the same cell keeps the same face. At
/// the 36 px acceptance size it floors the painted core at 3 px — the width
/// at which the core's [`EYE_DASH_CORE_PX`]-style plug always spans two full
/// pixel columns (⌊x+3⌋−⌈x⌉ ≥ 2 at every sub-pixel phase), which is what
/// makes the far eye's ≥2×2 dark-core bar a GUARANTEE rather than a bet on
/// where the head lands on the pixel grid. The floor may exceed what the
/// [`EYE_DOT_GAP_PX`] daylight guard would allow, which is why the compressed
/// core grows about its INNER edge, away from the near eye (see
/// [`paint_eye_dots`]) — the floor and the daylight guarantee never fight.
pub const EYE_DOT_FAR_CORE_MIN_W_FRAC: f32 = 2.0 / 24.0;

/// Horizontal daylight, in DEVICE pixels, an eye dot must keep from each
/// sibling dot. The chibi pass authored the pair nearly touching (≈6.5 art
/// units — barely one device pixel at ship size), so unguarded growth would
/// fuse the eyes into one dark mask: strictly worse than the gray it
/// replaces, two eyes that read as none. Device pixels rather than a frame
/// fraction because fusion is a raster phenomenon — the same authored gap is
/// generous at 96 px and gone at 40.
///
/// The guard cuts BELOW authored width when it must. At the Linux-default
/// 24 px bake the frontal pairs' authored gap is itself sub-pixel (0.9 px),
/// so even zero growth leaves the pair bridged by each other's anti-aliasing
/// — daylight outranks authored width there, and each dot narrows a little
/// (floored at [`EYE_DOT_FAR_CORE_MIN_W_FRAC`], so an eye can thin but never
/// thin AWAY) until the full gap opens. Two slightly-narrow eyes with a
/// clear coat column between them read as a face; two authored-width eyes
/// sharing one smudged bridge pixel read as a mask.
pub const EYE_DOT_GAP_PX: f32 = 1.3;

/// Ceiling on how far an OPEN eye's painted height may outrun its painted
/// width — grown h ≤ this × grown w. The daylight guard narrows a close-set
/// pair while the vertical target still wants its device pixels, and without
/// a coupling the Linux-default frontal faces painted 2.3 × 4.0 px eyes:
/// tall bars, the startled-mask look again by another route. An open eye is
/// authored round (bbox aspects 0.95–1.25); this cap lets rasterization win
/// a little height (a 2-wide dot may still be 2.8 tall) but keeps the dot
/// recognizably a DOT, trading guaranteed-core rows for roundness only where
/// the daylight guard has already judged width the scarcer resource. Arcs
/// and compressed far cores are exempt: their whole point is a non-round
/// aspect.
pub const EYE_DOT_ROUND_ASPECT_MAX: f32 = 1.4;

/// Bounding box of one fixed-point subpath in the glyph's normalized 0..1
/// frame, `(x0, y0, x1, y1)` — control points included, the same generous
/// reading the cat baker's eye-line probe uses (a Bézier never escapes its
/// control hull, so the box can only err wide, never lie small). `None` for a
/// subpath with no coordinates.
fn subpath_bounds(path: &[PathSeg]) -> Option<(f32, f32, f32, f32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u16::MAX, u16::MAX, 0u16, 0u16);
    let mut seen = false;
    let mut take = |x: u16, y: u16| {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
        seen = true;
    };
    for seg in path {
        match *seg {
            PathSeg::Move(x, y) | PathSeg::Line(x, y) => take(x, y),
            PathSeg::Cubic(ax, ay, bx, by, x, y) => {
                take(ax, ay);
                take(bx, by);
                take(x, y);
            }
            PathSeg::Close => {}
        }
    }
    let norm = |v: u16| f32::from(v) / f32::from(FIXED_ONE);
    seen.then(|| (norm(x0), norm(y0), norm(x1), norm(y1)))
}

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

/// The LOD eye pass: each subpath of the `Eye` layer — one authored eye — is
/// refilled solid as its own dot, grown about its own centre ([`zoom_about`],
/// so neither eye drifts) toward the legibility target for its SHAPE:
/// [`EYE_DOT_TARGET_H_PX`] of device height for an open round eye (capped at
/// [`EYE_DOT_SCALE_MAX`]) or [`EYE_DOT_TARGET_H_PX_ARC`] for a shallow
/// arc/lid dash (capped at [`EYE_DOT_SCALE_MAX_SHALLOW`] vertically and
/// [`EYE_DOT_ARC_KX_MAX`] horizontally — the dash thickens but never widens
/// into a mask). Horizontal growth is clamped per neighbouring dot so the
/// pair keeps [`EYE_DOT_GAP_PX`] of daylight — cutting below authored width
/// when the authored gap is itself sub-pixel — and an open eye's height is
/// then held within [`EYE_DOT_ROUND_ASPECT_MAX`] of whatever width survived,
/// so the dot stays a dot. A foreshortened 3/4 far eye (see
/// [`EYE_DOT_FAR_COMPRESS_RATIO`]) keeps the full vertical growth but is
/// painted as a NARROW core — the near-eye horizontal treatment scaled down
/// by the pose's own foreshortening ratio, floored at
/// [`EYE_DOT_FAR_CORE_MIN_W_FRAC`] of the tile height, and anchored at its
/// inner edge so the floor grows the core AWAY from the near eye instead of
/// into the pair's daylight. EVERY dot then gets its guaranteed core: a
/// solid [`EYE_DASH_CORE_PX`] plug at its centre, clamped inside its own
/// footprint and outside the pair's daylight, so the ≥2×2 dark core is a
/// guarantee instead of a phase accident. Painted with the layer's own
/// resolved ink: the iris ring, pupil and catch-light above it were culled,
/// so what remains IS the solid dot-per-eye the review asked for — at the
/// artist's proportion, per the owner's eyes-too-big call.
fn paint_eye_dots(
    tile: &mut Tile,
    layer: &Layer,
    col: (f32, f32, f32),
    alpha: f32,
    xform: PathTransform,
) {
    let bounds: Vec<Option<(f32, f32, f32, f32)>> =
        layer.paths.iter().map(|p| subpath_bounds(p)).collect();
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
        // A genuinely occluded sliver paints nothing — no live pose authors
        // one (see [`EYE_DOT_FAR_OCCLUDED_RATIO`]); every VISIBLE authored
        // eye below renders a core, per the owner's one-eyed-is-creepy call.
        if occluded(&bounds[i]) {
            continue;
        }
        let compressed = ratio(&bounds[i]) < EYE_DOT_FAR_COMPRESS_RATIO;
        let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        // Size-aware growth: how far this dot must grow for its authored
        // height to reach its shape's device-pixel target (scale_y IS the
        // tile height in the normalized fit). A shallow subpath — an arc or
        // a lid, a thin BAND rather than solid ink — aims at the band target
        // with the higher ceiling: its dark core lives in the band's
        // interior rows, and it starts from a fraction of the open eye's
        // height. Shallowness is judged in DEVICE pixels, deliberately: the
        // glyph's normalized frame maps each viewbox axis to 0..1
        // separately, so a normalized aspect is warped by the viewbox's own
        // shape, while the aspect-preserving tile restores the artist's
        // proportions — the arcs measure 0.41 there and the open eyes
        // 0.95+, exactly the populations [`EYE_DOT_SHALLOW_ASPECT`] was
        // drawn between.
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
        // The daylight floor, converted from device px into the normalized
        // frame this bake maps at (scale_x IS the tile width). Horizontal
        // growth starts from the vertical factor — capped for an arc, whose
        // width already carries its expression (see [`EYE_DOT_ARC_KX_MAX`]).
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
            // An occluded sibling paints nothing — it cannot fuse with
            // anyone, so it must not throttle this dot's growth either. A
            // COMPRESSED sibling still paints, so it stays in the guard.
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
        // The guard may cut BELOW authored width: at the Linux-default bake
        // the frontal pairs' authored gap is itself sub-pixel, and daylight
        // outranks authored width (see [`EYE_DOT_GAP_PX`]). Floored at the
        // guaranteed-core width so an eye can thin but never thin away.
        let kx_floor =
            (EYE_DOT_FAR_CORE_MIN_W_FRAC * xform.scale_y / w_px.max(f32::EPSILON)).min(1.0);
        let kx = kx.clamp(kx_floor, ky);
        // A round eye stays round: when the daylight guard has narrowed the
        // dot, its height follows the width down (see
        // [`EYE_DOT_ROUND_ASPECT_MAX`]) instead of painting a startled bar.
        // Never below authored height, and never for the shapes whose whole
        // point is a non-round aspect.
        let ky = if shallow || compressed {
            ky
        } else {
            ky.min((EYE_DOT_ROUND_ASPECT_MAX * w_px * kx / h_px.max(f32::EPSILON)).max(1.0))
        };
        // The compressed far core: the near-eye horizontal treatment scaled
        // DOWN by the authored foreshortening ratio (so the turned head keeps
        // its asymmetry even after `ky` rounds both eyes up), floored at the
        // guaranteed-core width. The floor may exceed what the daylight guard
        // above allowed, so the growth is anchored at the eye's INNER edge —
        // the one facing the near eye — and extends OUTWARD: the authored
        // daylight between the pair survives whatever the floor asks for.
        let (kx, anchor_x) = if compressed {
            let floor_k = EYE_DOT_FAR_CORE_MIN_W_FRAC * xform.scale_y / w_px.max(f32::EPSILON);
            let inner = if near_cx >= cx { x1 } else { x0 };
            ((kx * ratio(&bounds[i])).max(floor_k), inner)
        } else {
            (kx, cx)
        };
        fill_path_fixed(
            tile,
            std::slice::from_ref(path),
            col,
            alpha,
            zoom_about(xform, anchor_x, cy, kx, ky),
        );
        // Where the dot's centre landed after the zoom — identical to `cx`
        // for a centre-anchored dot, shifted outward by the sub-pixel the
        // floor added for an inner-edge-anchored compressed core. The plug
        // must sit on the PAINTED dot, not the authored one.
        let gx = anchor_x + (cx - anchor_x) * kx;
        // Every dot gets its guaranteed core (see [`EYE_DASH_CORE_PX`]): a
        // solid square of the same ink at the dot's centre, clamped inside
        // the grown footprint so it reads as stroke weight, never as a
        // second feature.
        {
            // The pair-daylight clamp: plugs sit at (nearly) authored
            // centres, so two full-width plugs on a close-set pair would
            // meet in the middle and fuse exactly the gap the ellipse
            // growth above was throttled to keep. Both siblings compute the
            // same centre distance, so both shrink to the same ceiling and
            // the [`EYE_DOT_GAP_PX`] daylight survives symmetric.
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
            // …floored at the guaranteed-core width, the same precedence the
            // compressed far core already established: when a pose authors
            // its pair closer than daylight-plus-two-cores can share (the
            // land's braced squint), the core outranks the daylight — a
            // sub-pixel gap on a transient face is recoverable, an invisible
            // eye is not.
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
        }
    }
}

/// Bake-cache key for one pet tile: everything that can change a texel, and
/// nothing that cannot.
///
/// All-integer by construction — the [`BakeKey`](crate::cat_baker::BakeKeyV4)
/// discipline. The resolved coat/iris/outline colours are `f32` triples, so
/// they are re-derived from these ramp INDICES at bake time
/// ([`PetBakeKey::fills`]) and never stored in a hashed key; a float in an
/// `Eq`/`Hash` type is how a cache silently stops hitting (or starts colliding)
/// on a denormal.
///
/// Note what is deliberately absent: no `eyes` axis (the pose authors its own
/// eye state, see the module docs) and no age/scale axis (the exact `w × h`
/// already encodes every caller-side scaling decision, so a second axis could
/// only fragment the cache).
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
    /// keys host-authored tiles by an opaque id rather than by a cat key.
    ///
    /// FNV-1a over the key's own integer fields, deliberately NOT
    /// `DefaultHasher`: the atlas slot must stay the same tile across process
    /// restarts and across std releases, and `DefaultHasher`'s algorithm is
    /// explicitly not part of std's stable contract. The high bit is set so a
    /// pet id can never collide with a small hand-assigned host id (the kitty
    /// cursor's, for instance) in the shared atlas.
    #[must_use]
    pub fn host_id(&self) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h = FNV_OFFSET;
        let mut eat = |b: u64| {
            h ^= b;
            h = h.wrapping_mul(FNV_PRIME);
        };
        eat(self.pose as u64);
        eat(u64::from(self.coat));
        eat(u64::from(self.iris));
        eat(u64::from(self.colors.accent));
        eat(u64::from(self.colors.background));
        eat(u64::from(self.w));
        eat(u64::from(self.h));
        h | (1 << 63)
    }
}

/// Rasterize one authored pose into a fresh `w × h` [`Tile`], painter order =
/// layer order, each layer's colour resolved by [`resolve_layer`] and its
/// fixed-point `0..=FIXED_ONE` frame mapped onto the tile by
/// [`PathTransform::fit`].
///
/// Deterministic: the const integer drawlist plus the fixed-point filler give
/// byte-identical output for identical arguments, on any host, forever — which
/// is what makes the LRU above safe to treat as a pure function of its key.
///
/// The one thing painted that is not a plain layer fill is the MIXED-background
/// halo: when [`ResolvedFills::outline_underlay`] is `Some` (background band 4
/// — the pet's footprint spans both sides of the light/dark crossover, which a
/// roaming full-body pet does constantly), the silhouette and whisker layers get
/// a compact pale keyline painted underneath the dark one. Neither ink alone
/// clears 3:1 at every luminance a mixed footprint contains; the PAIR does. It
/// costs four extra fills on a cache miss and nothing at all in steady state.
///
/// Below [`FACE_DETAIL_MIN_H`] the walk applies the face LOD (see the const
/// block): the whisker, catch-light, blush, pattern, iris and pupil layers —
/// identified by their authored ROLES, never by drawlist position — are
/// skipped outright, and each `Eye` subpath is refilled as one solid grown dot
/// ([`paint_eye_dots`]; the 3/4 rig's foreshortened far eye survives as a
/// compressed narrow core, see [`EYE_DOT_FAR_COMPRESS_RATIO`] — every pose
/// that authors two eyes renders two). Below [`MOUTH_DETAIL_MIN_H`] the mouth is culled
/// (see that const for why thickening lost). All of it is keyed off `h` and
/// `w` alone, both already in the cache key, so determinism is untouched.
#[must_use]
pub fn bake_pose(pose: PetGlyphId, fills: &ResolvedFills, w: u32, h: u32) -> Tile {
    let mut tile = Tile::new(w, h);
    if w == 0 || h == 0 {
        return tile;
    }
    let xform = PathTransform::fit(w, h);
    let face_lod = h < FACE_DETAIL_MIN_H;
    for layer in PET_GLYPHS[pose as usize].layers {
        // The review's cull list, by authored role: whisker/blush/pattern are
        // sub-pixel charm at LOD sizes, and iris/catch-light/pupil are the eye
        // stack the solid dot replaces.
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
            paint_eye_dots(&mut tile, layer, col, alpha, xform);
            continue;
        }
        if h < MOUTH_DETAIL_MIN_H && layer.role == GlyphRole::Mouth {
            continue;
        }
        if matches!(layer.role, GlyphRole::Outline | GlyphRole::Whisker)
            && let Some(underlay) = fills.outline_underlay
        {
            // Halo radius tracks the tile scale but is clamped to a sub-pixel
            // band: any wider and the pale ring reads as a second outline
            // instead of a legibility backstop.
            let halo = (xform.scale_x.min(xform.scale_y) * 0.04).clamp(0.75, 1.25);
            for (dx, dy) in [(halo, 0.0), (-halo, 0.0), (0.0, halo), (0.0, -halo)] {
                fill_path_fixed(
                    &mut tile,
                    layer.paths,
                    underlay,
                    alpha,
                    PathTransform {
                        dx: xform.dx + dx,
                        dy: xform.dy + dy,
                        ..xform
                    },
                );
            }
        }
        fill_path_fixed(&mut tile, layer.paths, col, alpha, xform);
    }
    tile
}

/// One resident tile: its key, its texels, and the frame it was last touched on.
struct PetSlot {
    key: PetBakeKey,
    tile: Tile,
    last_used: u64,
}

/// Host-side LRU of exact-size pet tiles (see the module docs for the budgets
/// and for why there is neither a patch strip nor an eyes axis).
///
/// `Default` is a valid empty baker: the first
/// [`begin_frame`](PetBaker::begin_frame) adopts the cell metrics.
#[derive(Default)]
pub struct PetBaker {
    slots: Vec<PetSlot>,
    /// Sum of `slots[..].tile.pixels().len()`, kept incrementally so the
    /// [`MAX_ATLAS_BYTES`] residency bound costs O(1) per admission instead of
    /// a walk.
    bytes: usize,
    cell_w: u16,
    cell_h: u16,
    /// LRU clock, advanced once per [`begin_frame`](PetBaker::begin_frame).
    clock: u64,
    /// Cold bakes still allowed this frame.
    bakes_left: u32,
    /// Monotonic; bumped on every bake and every wholesale clear, so a host that
    /// folds it into a frame fingerprint re-uploads after either.
    version: u64,
}

impl PetBaker {
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
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the cache holds no tiles.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Resident texel bytes (`≤ MAX_ATLAS_BYTES`). Diagnostic: it is how a test
    /// proves the residency bound is enforced rather than merely intended.
    #[doc(hidden)]
    pub fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// The authored aspect (width ÷ height) of a pose, from its asset viewbox.
    ///
    /// Callers size the destination box from THIS rather than from the cell
    /// grid: the pet roster's poses are not all the same shape (a stretch is
    /// long and low, a sit is tall and narrow), and fitting them all into one
    /// box would squash the art the animator drew. Clamped away from zero so a
    /// degenerate asset cannot produce a NaN/∞ destination rect.
    #[must_use]
    pub fn aspect(pose: PetGlyphId) -> f32 {
        (f32::from(PET_GLYPHS[pose as usize].aspect_x1000) / 1000.0).max(0.001)
    }

    /// Look `key` up, baking on a miss when the per-frame budget allows.
    ///
    /// Returns straight-alpha RGBA8, exactly `key.w · key.h · 4` bytes, rows
    /// top-down — ready to hand straight to
    /// [`CatBaker::host_tile`](crate::cat_baker::CatBaker::host_tile) with
    /// [`PetBakeKey::host_id`].
    ///
    /// `None` means "not this frame": the cold-bake budget is spent, or the key
    /// is degenerate (zero-sized, or a single tile larger than the entire
    /// residency budget). This is why the return is an `Option<&[u8]>` and not
    /// the bare `&[u8]` the shape sketch suggested — a deferred bake and an
    /// all-transparent tile are different facts, and a caller that cannot tell
    /// them apart will happily blit a hole into the scene and then never retry.
    /// The correct response to `None` is to keep drawing the previously resolved
    /// pose and ask again next frame.
    pub fn tile(&mut self, key: &PetBakeKey) -> Option<&[u8]> {
        if key.w == 0 || key.h == 0 {
            return None;
        }
        let want = usize::from(key.w) * usize::from(key.h) * 4;
        // A tile that alone exceeds the residency budget is refused rather than
        // admitted by evicting everything else — otherwise one absurd cell
        // metric turns the cache into a one-entry miss machine.
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
        let tile = key.bake();
        // Make room BEFORE inserting, so the pushed slot's index is final and
        // the tile we are about to hand out can never be the one evicted.
        while !self.slots.is_empty()
            && (self.slots.len() >= MAX_SLOTS || self.bytes + want > MAX_ATLAS_BYTES)
        {
            self.evict_lru();
        }
        self.bytes += tile.pixels().len();
        self.slots.push(PetSlot {
            key: *key,
            tile,
            last_used: self.clock,
        });
        self.version = self.version.wrapping_add(1);
        Some(self.slots[self.slots.len() - 1].tile.pixels())
    }

    /// Drop the least-recently-used slot, lowest index breaking ties — the
    /// tiebreak is what makes eviction (and therefore the whole cache) a
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pet_glyphs_gen::PET_GLYPH_IDS;

    /// Find a pose by a substring of its authored asset id. Tests address the
    /// roster through the ASSET vocabulary, not through generated variant
    /// spellings, so a codegen rename cannot silently retarget a test.
    /// The CAT pose named `needle` — `pose("walk_0")` is `pet_walk_0`.
    ///
    /// EXACT, not `contains`, and that is not tidiness. The roster carries two
    /// species since the dog landed, it is ordered by id, and `pet_dog_walk_0`
    /// sorts before `pet_walk_0` — so a substring match silently handed every
    /// one of these tests the DOG. That is a false green in the worst
    /// direction: `face_lod_culls_charm_ink_at_ship_size` asserts a `Pattern`
    /// layer exists, dogs have no tabby and therefore no `Pattern`, and the
    /// test failed with "walk pose authors all three charm layers" — a
    /// complaint about the cat that was really a lookup finding a dog. Pin the
    /// species in the id and the question the test asks stays the one it means.
    /// For a dog pose, ask for one: `pose_id("pet_dog_walk_0")`.
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

    /// Visual-QA dump for art passes — `#[ignore]`d, so it never runs in CI.
    /// The LOD's acceptance bars are pixel measurements, but a bar can pass
    /// while the face reads wrong (the 3/4 far-eye cull exists because two
    /// PASSING dots read as a smear); every rig change wants human eyes on
    /// the actual ship-size bake, not just on the 160 px authored art.
    ///
    /// ```sh
    /// PET_LOD_DUMP_DIR=/tmp/lod cargo test -p aterm-effects --lib \
    ///     dump_lod_tiles_for_review -- --ignored
    /// ```
    ///
    /// Writes `<pose>_<h>_<ground>.ppm` (P6) tiles at 24/36/48 px, composited
    /// on both grounds.
    #[test]
    #[ignore = "visual QA dump; set PET_LOD_DUMP_DIR and run explicitly"]
    fn dump_lod_tiles_for_review() {
        let dir = std::env::var("PET_LOD_DUMP_DIR").expect("set PET_LOD_DUMP_DIR");
        for needle in [
            "walk_0",
            "walk_1",
            "run_1",
            "stand",
            "sit",
            "purr",
            "loaf",
            "sit_front",
            "peek_shoulder",
            // The sleeper earns a slot: it is the pose a long-idle terminal
            // parks on, so it is the one the owner sees most, and its face is
            // the one that went out wrong (a fat `closed` lid baking to a dark
            // bar that read as an OPEN eye). Its two LOD regimes both matter
            // and both live in this sweep — 48 px keeps the mouth while
            // collapsing the eyes, 24/36 drop both.
            "sleep_0",
        ] {
            let p = pose(needle);
            for h in [24u32, 36, 48] {
                let w = (h as f32 * PetBaker::aspect(p)).round() as u32;
                for dark_bg in [false, true] {
                    let fills = ResolvedFills::from_indices(9, 4, dark_bg);
                    let tile = bake_pose(p, &fills, w, h);
                    let px = tile.pixels();
                    let bg: [u8; 3] = if dark_bg {
                        [40, 40, 48]
                    } else {
                        [246, 246, 240]
                    };
                    let mut out = format!("P6 {w} {h} 255\n").into_bytes();
                    // `as_chunks::<4>()` over `chunks_exact(4)`: the pixel is
                    // RGBA and the const generic says so in the TYPE, so `c[3]`
                    // below is a proven-in-bounds field read rather than an
                    // index into a slice whose length the compiler only knows
                    // at run time. The `.1` remainder is empty by construction
                    // (a tile's buffer is 4 bytes per pixel) and dropping it
                    // keeps the old loop's exact behaviour.
                    for c in px.as_chunks::<4>().0 {
                        let a = c[3] as u32;
                        for i in 0..3 {
                            out.push(((c[i] as u32 * a + bg[i] as u32 * (255 - a)) / 255) as u8);
                        }
                    }
                    let ground = if dark_bg { "dark" } else { "light" };
                    std::fs::write(format!("{dir}/{needle}_{h}_{ground}.ppm"), out).unwrap();
                }
            }
        }
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

    /// EXACT-SIZE, no patch strip: the tile is `w · h · 4` bytes, full stop.
    /// (A cat tile of the same art box would be `(w + PATCH_STRIP) · h · 4`.)
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

    /// Review #4's acceptance bar for the face LOD, extended by the owner's
    /// two-eyes ruling ("this new kitty only has one eye … that's not cute
    /// that's creepy"): at ship size (cell_h 14 ⇒ a 36 px-tall tile at 1.5×
    /// HiDPI), for EVERY live pose on BOTH grounds, every authored eye
    /// subpath must survive as its own ≥2×2 block of DARK, fully-opaque ink
    /// near its authored centre — the turned heads' compressed far eye
    /// included (see [`EYE_DOT_FAR_COMPRESS_RATIO`]; nothing in the live
    /// roster may fall under the occlusion gate). The per-pose CORE COUNT
    /// equals the authored eye-subpath count, so a one-eyed render of a
    /// two-eyed pose fails; the cores of a pair authored with daylight must
    /// stay distinct AND keep at least one non-dark texel between them, so a
    /// pair fused into one ink mask fails too. Before the LOD the same probe
    /// found single blended pixels around rgb(144,144,135): the stack's own
    /// charm had averaged the eye gray. "Dark" is every channel under 64 —
    /// the authored eye ink is rgb(36,31,41).
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
                    let bounds: Vec<(f32, f32, f32, f32)> = layer
                        .paths
                        .iter()
                        .filter_map(|p| subpath_bounds(p))
                        .collect();
                    let max_w_n = bounds.iter().map(|b| b.2 - b.0).fold(0.0, f32::max);
                    // Every authored subpath is a VISIBLE eye that must
                    // render: the occlusion gate stays unused by the live
                    // roster, per the owner's ruling.
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

    /// The LOD deletes the charm ink rather than merely shrinking it: at ship
    /// size the baked walk tile contains not one fully-covered texel of
    /// catch-light white, blush pink or tabby-pattern brown, while a
    /// desk-size bake (above [`FACE_DETAIL_MIN_H`]) keeps all three. The iris
    /// ring and pupil need no separate probe — the core test above proves the
    /// dot they collapse into is PURE eye ink.
    #[test]
    fn face_lod_culls_charm_ink_at_ship_size() {
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
        for role in [GlyphRole::CatchLight, GlyphRole::Blush, GlyphRole::Pattern] {
            let layer = PET_GLYPHS[p as usize]
                .layers
                .iter()
                .find(|l| l.role == role)
                .expect("walk pose authors all three charm layers");
            let (col, _) = resolve_layer(layer, &fills);
            let byte = |c: f32| (c * 255.0 + 0.5) as u8;
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

    /// The eye-SIZE law, pinned at the sizes 1× desktops actually bake — the
    /// owner's "the kitty eyes are too big" bug, measured. The default Linux
    /// cell (cell_h 14) bakes the pet 24 px tall, and the old 7 px growth
    /// target painted the authored 3.0 px open eye 6.5 px — 27% of body
    /// height against the artist's 12.4%, twice the drawn proportion — while
    /// the arcs' uniform 2.8× growth painted the purr/sleep pair as one
    /// 17 px sunglasses band. For EVERY live pose on BOTH grounds, at the
    /// Linux-default 24 px bake and the 1.5×-HiDPI 36 px bake, each authored
    /// eye's dark ink component must now stay inside its law-derived box:
    ///
    /// * an OPEN eye: no taller than the larger of its authored height and
    ///   [`EYE_DOT_TARGET_H_PX`], no wider than the larger of its authored
    ///   width and [`EYE_DASH_CORE_PX`] (the guaranteed-core plug may pad a
    ///   narrow eye) — plus 1.5 px of anti-aliasing slack each;
    /// * a SHALLOW arc/lid: height against [`EYE_DOT_TARGET_H_PX_ARC`],
    ///   width against [`EYE_DOT_ARC_KX_MAX`] × authored — the dash may
    ///   thicken but never widen into a mask, and a pair fused across its
    ///   daylight shows up here as one component twice its lawful width;
    ///
    /// and must still EXIST — at least a few fully-dark texels per eye, the
    /// legibility floor the growth exists for. Subpaths whose authored
    /// x-ranges overlap are one eye drawn in multiple strokes and are
    /// measured as one. Components are measured by flood fill inside a
    /// window sized from the bound itself, so an over-grown eye cannot
    /// escape measurement by outgrowing the probe.
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
                        // Cluster subpaths that share x-range: the baker's
                        // own daylight guard treats x-overlap as "one eye,
                        // nothing to guard" (the roll's tilted face stacks
                        // its two arcs diagonally), so the test measures the
                        // same union the law paints unguarded.
                        let mut clusters: Vec<((f32, f32, f32, f32), usize)> = Vec::new();
                        for b in layer.paths.iter().filter_map(|p| subpath_bounds(p)) {
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
                                // A multi-stroke union: each member grows by
                                // at most the global ceilings about its own
                                // centre inside the union, so the union is
                                // bounded loosely — tight pinning belongs to
                                // the single-stroke eyes every everyday face
                                // is made of.
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
                            // The legibility floor scales with what the size
                            // can guarantee: at 36 the full core plug fits
                            // every pose (≥2×2 fully dark), at the 24 px
                            // default the land's sub-pixel braced squint
                            // (0.8 px authored) carries one fully-dark texel
                            // with its anti-aliased halo doing the visible
                            // rest (checked on the capture) — a fleck, never
                            // nothing.
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

    /// The Retina identity path, pinned. A 2× mac default (cell_h ≈ 42
    /// device px ⇒ an ART_ROWS · 42 ≈ 71 px tile) bakes at or above
    /// [`FACE_DETAIL_MIN_H`], where the LOD never runs and the full authored
    /// face — charm ink included — is painted verbatim: the eye-size law is
    /// a 1×/1.5× repair and must not move a single Retina texel. Proven at
    /// the boundary: the first full-face height keeps the catch-light, one
    /// pixel below it the LOD culls it.
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
        let has_ink = |h: u32| {
            let w = (h as f32 * PetBaker::aspect(p)).round() as u32;
            bake_pose(p, &fills, w, h)
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] == 255 && px[..3] == ink)
        };
        assert!(
            has_ink(72),
            "a 72 px bake (the 2x Retina default) must paint the full \
             authored face, catch-light included"
        );
        assert!(
            has_ink(FACE_DETAIL_MIN_H),
            "the first full-face height lost its catch-light — the LOD \
             leaked above its threshold"
        );
        assert!(
            !has_ink(FACE_DETAIL_MIN_H - 1),
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
