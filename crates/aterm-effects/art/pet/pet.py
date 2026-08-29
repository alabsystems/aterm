#!/usr/bin/env python3
"""The aterm pet kitty: one rig, many poses, emitted as glyph-asset TOML."""

import math
from dataclasses import dataclass, field, replace

from rig import (GROUND, GROUND_INK, OX, VB_H, VB_W, blob, catmull_closed,
                 chain, ellipse, fmt, limb, rot_about)

# ── ink ────────────────────────────────────────────────────────────────────
INK = "#2B2530"       # outline
COAT = "#D9A273"      # recolorable coat
PATTERN = "#A9754C"   # tabby, fixed
INNER_EAR = "#EFA3AE"
MUZZLE = "#F7E9DE"
EYE = "#241F29"
IRIS = "#7FA88E"
PUPIL = "#1A161E"
LIGHT = "#FFFFFF"
NOSE = "#B0637A"
BLUSH = "#F2A9B4"

# ── the DOG skin ───────────────────────────────────────────────────────────
# The pet dog is not a second rig. It is the SAME rig — same torso, same limb
# chains, same gait, same registration — wearing a different head. Owner,
# 2026-08-11: "make a dog like the walking cat … you can use the same code".
# So every pose in `poses.py` is emitted twice, and the species switch below
# changes only the four things that actually separate a dog's head from a
# cat's at 16 px: the ears hang instead of standing, the muzzle projects into
# a snout, the nose is a big dark button on the end of it, and there are no
# whiskers. The tabby goes with them — bars belong to a tabby, not a dog.
#
# Every branch is keyed off `Pose.species`, which DEFAULTS to "cat", so the
# 29 checked-in cat TOMLs regenerate byte-identical. That is the invariant to
# preserve when touching anything in this file: `pet_glyphs_gen_matches_assets`
# is what notices if it breaks.
DOG_NOSE = "#2B2530"  # a dog's nose is a dark button, not a cat's pink triangle

S = 4.0               # outline half-width added to every coat part


@dataclass
class Pose:
    """One authored frame of the pet. Angles are degrees, 0 = straight down,
    positive swings toward +x (the direction the cat faces)."""
    ident: str
    note: str

    # Which animal wears this pose. "cat" (the default) reproduces the
    # original rig exactly; "dog" swaps the head skin — see the DOG SKIN note
    # at the top of this file. Body, limbs, gait and registration are shared,
    # which is the whole point: the dog walks with the cat's walk.
    species: str = "cat"

    # torso
    bx: float = 90.0
    by: float = 64.0
    brx: float = 40.0
    bry: float = 23.0
    brot: float = 0.0        # body tilt

    # head — the artist's proportions: a real cat's head on a real cat's body.
    # Ship-size legibility is the BAKE's job (pet_baker's face LOD), not the
    # rig's; the chibi detour proved a balloon head just breaks every pose.
    hx: float = 150.0
    hy: float = 41.0
    hr: float = 29.0
    hrot: float = 0.0
    hsx: float = 1.0
    hsy: float = 1.0

    # ears: (near, far) sweep away from vertical; negative = flattened
    ear_near: float = 0.0
    ear_far: float = 0.0
    ear_flat: float = 0.0    # 0 = perked, 1 = pinned back

    # legs: each is (thigh_angle, shin_angle, thigh_len, shin_len)
    fl_near: tuple = (0.0, 0.0, 22.0, 16.0)
    fl_far: tuple = (0.0, 0.0, 22.0, 16.0)
    hl_near: tuple = (0.0, 0.0, 22.0, 16.0)
    hl_far: tuple = (0.0, 0.0, 22.0, 16.0)

    # tail: absolute angles of a 4-bone chain from the rump, and its taper
    tail: tuple = (-112.0, -140.0, -172.0, -206.0)
    tail_len: float = 13.5
    tail_thick: float = 8.5

    # face
    eyes: str = "open"       # open | happy | closed | squint | halflid | wide | wink
    mouth: str = "smile"     # smile | open | flat | oof | yawn | none
    blush: bool = True
    gaze: tuple = (0.0, 0.0)

    # extras
    curl: bool = False       # whole-body curl (sleep) replaces torso+legs
    show_far_legs: bool = True
    hide_legs: bool = False  # the curl tucks every paw out of sight
    hide_hind: bool = False  # a sit puts the haunch on the ground instead
    haunch_at: tuple = ()    # (cx, cy, r) override — the seated rump
    # Vertical registration. The emitter stands the sprite's BOTTOM EDGE on the
    # text row's baseline, so where a pose's lowest INK sits inside the box is
    # literally how high off the line the cat looks. Every weight-bearing pose is
    # therefore planted on one ground line by construction (below), and only the
    # poses that are genuinely off the floor float — by an authored amount, not
    # by whatever their limb angles happened to reach.
    airborne: float = 0.0    # viewbox units of clearance under the paws
    fl_root: tuple = ()      # forelimb attach override (x, y)
    tail_root: tuple = ()    # tail attach override (x, y)
    # Face-on mode: the whole drawing turns to address the viewer. The 3/4
    # asymmetries (off-side eye smaller, far ear swept back, one whisker fan
    # shortened, the travel-direction torso arch) all collapse to left-right
    # symmetry, and both forelegs paint IN FRONT of the chest.
    front: bool = False
    # The off-side whisker fan. On most heads it pokes past the far cheek onto
    # AIR and reads as whiskers; when a settled pose parks body mass BEHIND the
    # far cheek (the loaf's dome, the peek's shoulder), the same strokes paint
    # ON the coat and read as scratches — so those poses turn the fan off.
    whisker_far: bool = True
    # The NEAR whisker fan. Its roots sit ~3.6 viewbox units under the near
    # eye's lower edge — under a pixel apart once the pose is baked at the
    # ship's 34 px art height, so on a head that also tips the eye down toward
    # them (the sleeping loaf's `hrot`) the fan, the eye and the head outline
    # rasterise into ONE black clot where the eye should be. A pose that
    # cannot afford that spends the near fan: at 34 px the ears, the loaf and
    # the closed arcs carry the cat, and no viewer misses four strokes.
    whisker_near: bool = True
    # Where the tabby back bars ride. "torso" is the default topline; a pose
    # whose head covers the torso's top (the peek's over-the-shoulder seat)
    # moves them to the haunch's crown instead of wearing them on its cheek.
    bar_site: str = "torso"
    # Head yaw: 0 = the rest read (a nearly frontal face, eye contact — the
    # point of a settled pose), 1 = the full 3/4 locomotion head, where the
    # face turns WITH the body: far eye foreshortened to ~60% width and pulled
    # toward the leading edge, muzzle/nose pushed ~0.22 head radii into the
    # travel direction, near ear rotated out while the far ear narrows behind,
    # whiskers asymmetric, the skull egg-shaped toward the muzzle. The art is
    # authored facing right and the engine mirrors the whole sprite
    # (kitty_pet's flip_x), so the yaw flips with the body for free.
    yaw: float = 0.0
    # ── two art-director fixes that used to live ONLY in the emitted TOML ────
    #
    # Both land from 8cf4b4a4 ("final kitty pass"), and both were hand-edited
    # into the GENERATED files instead of into this rig — so poses.py and the
    # checked-in art disagreed, and any full regen silently reverted them. That
    # trap has now cost something real: the dog roster was minted from the
    # unfixed rig, so `pet_dog_bat` shipped the identical hole. As rig fields
    # BOTH species get the fix and a regen reproduces the art byte-for-byte.
    #
    # (dx, dy, sx, sy) applied to the FAR eye only. `pet_leap_descend` read
    # ONE-EYED at ship size: authored at its rise sibling's size, the far eye
    # drowned against the muzzle boundary. Grown ~25% and nudged clear it sits
    # at 0.77 of the near eye's width, clearing the face LOD's 0.70 compression
    # threshold outright. It rides `eye_centres`/`eye_scales`, so the iris,
    # pupil and catch-light follow the eye they belong to — the re-anchoring
    # the hand fix had to do three layers at a time, by hand.
    far_eye: tuple = ()
    # (cx, cy, rx, ry) of a coat blob painted with the coat, under the face.
    # `pet_bat` had a D-shaped HOLE: the raised hind leg's loop encloses a
    # counter, and a counter inside a coat-filled silhouette shows the terminal
    # background THROUGH the animal — a hole, not a marking. Filling it in the
    # coat leaves the leg reading by its own outline. Authored in
    # pre-registration coordinates, like every other geometry in this file.
    belly: tuple = ()


def lerp(a, b, t):
    return a + (b - a) * t


# ── attachment points, derived from the torso ──────────────────────────────

FAR_OFFSET = (-7.0, -2.0)   # the off-side pair sits back and up: instant 3/4 read
FRONT_OFFSET = (-24.0, 0.0)  # face-on: the off-side foreleg mirrors across the chest
                             # (24 units apart: any closer and the two mitts'
                             # outlines fuse into one two-toed paw)


def hip(p: Pose, far=False):
    dx, dy = FAR_OFFSET if far else (0.0, 0.0)
    return rot_about(p.bx - p.brx * 0.60 + dx, p.by + p.bry * 0.50 + dy,
                     p.bx, p.by, p.brot)


def shoulder(p: Pose, far=False):
    dx, dy = ((FRONT_OFFSET if p.front else FAR_OFFSET) if far else (0.0, 0.0))
    if p.fl_root:
        return (p.fl_root[0] + dx, p.fl_root[1] + dy)
    return rot_about(p.bx + p.brx * 0.62 + dx, p.by + p.bry * 0.48 + dy,
                     p.bx, p.by, p.brot)


def rump(p: Pose):
    if p.tail_root:
        return p.tail_root
    return rot_about(p.bx - p.brx * 1.00, p.by - p.bry * 0.20, p.bx, p.by, p.brot)


def leg_pts(root, spec):
    thigh_a, shin_a, thigh_l, shin_l = spec
    return chain(root, [(thigh_l, thigh_a), (shin_l, shin_a)])


# ── part builders: each returns (coat_paths, outline_paths) ────────────────

def torso_paths(p: Pose):
    if p.curl:
        # a settled loaf: wide, low, softly domed
        # a sleeping loaf: domed back, flat underside, the rump a touch fuller
        spokes = [(0, 1.02), (38, 1.04), (72, 0.98), (108, 0.90), (145, 0.84),
                  (180, 0.80), (215, 0.86), (250, 0.96), (288, 1.06), (324, 1.05)]
        core = blob(p.bx, p.by, [(a, r * p.brx) for a, r in spokes],
                    rot=math.radians(p.brot), sy=p.bry / p.brx)
        out = blob(p.bx, p.by, [(a, r * (p.brx + S)) for a, r in spokes],
                   rot=math.radians(p.brot), sy=(p.bry + S) / (p.brx + S))
        return [core], [out]
    if p.front:
        # face-on seated chest: an upright pear, left-right symmetric — wide
        # over the hips, tapering under the chin, no travel direction at all.
        # The lower spokes stay INSIDE the seated haunch so the forepaws can
        # break the bottom silhouette instead of drowning in it.
        spokes = [(0, 0.96), (38, 1.00), (72, 1.04), (106, 1.08), (142, 0.98),
                  (180, 0.92), (218, 0.98), (254, 1.08), (288, 1.04), (322, 1.00)]
    else:
        # a standing torso: the chest lifts toward the head, the belly tucks,
        # the rump stays round — the arch is what stops it reading as a sausage.
        spokes = [(0, 1.00), (42, 1.10), (78, 1.02), (112, 0.92), (150, 0.85),
                  (180, 0.84), (212, 0.90), (250, 1.00), (285, 1.05), (322, 1.03)]
    core = blob(p.bx, p.by, [(a, r * p.brx) for a, r in spokes],
                rot=math.radians(p.brot), sy=p.bry / p.brx)
    out = blob(p.bx, p.by, [(a, r * (p.brx + S)) for a, r in spokes],
               rot=math.radians(p.brot), sy=(p.bry + S) / (p.brx + S))
    return [core], [out]


def haunch_paths(p: Pose):
    if p.haunch_at:
        # (cx, cy, r) — the round seated rump — or (cx, cy, rx, ry, rot_deg)
        # for a pose that needs the hip mass ORIENTED (the peek's rump swings
        # away from the viewer, and a circle has no away to swing)
        if len(p.haunch_at) == 5:
            cx, cy, rx, ry, rot = p.haunch_at
            th = math.radians(rot)
            return ([ellipse(cx, cy, rx, ry, th)],
                    [ellipse(cx, cy, rx + S, ry + S, th)])
        cx, cy, r = p.haunch_at
        rx, ry = r * 1.02, r
    else:
        hx, hy = hip(p)
        cx, cy = hx - 2.0, hy - p.bry * 0.42
        r = p.bry * 0.86
        rx, ry = r * 1.05, r
    return [ellipse(cx, cy, rx, ry)], [ellipse(cx, cy, rx + S, ry + S)]


def head_spokes(p: Pose):
    if p.front:
        # face-on: symmetric about the vertical — full cheeks on both sides,
        # the chin tapering dead centre
        return [(0, 0.96), (36, 1.00), (72, 1.03), (108, 1.00), (144, 0.92),
                (180, 0.88), (216, 0.92), (252, 1.00), (288, 1.03), (324, 1.00)]
    # rest: an even egg. Yawed, the skull leans into the travel direction —
    # the muzzle-side spokes swell and the back of the head shaves down, so
    # the silhouette itself says "turned", not just the features on it.
    rest = [(0, 0.94), (35, 0.99), (70, 1.03), (100, 1.02), (135, 0.94),
            (168, 0.88), (200, 0.94), (232, 1.02), (262, 1.03), (295, 1.00),
            (325, 0.96)]
    yawed = [(0, 1.03), (35, 1.05), (70, 1.03), (100, 1.00), (135, 0.90),
             (168, 0.85), (200, 0.92), (232, 1.01), (262, 1.03), (295, 1.00),
             (325, 1.01)]
    return [(a, lerp(r0, r1, p.yaw)) for (a, r0), (_, r1) in zip(rest, yawed)]


def head_paths(p: Pose):
    sp = head_spokes(p)
    core = blob(p.hx, p.hy, [(a, r * p.hr) for a, r in sp],
                rot=math.radians(p.hrot), sx=p.hsx, sy=p.hsy)
    out = blob(p.hx, p.hy, [(a, r * (p.hr + S)) for a, r in sp],
               rot=math.radians(p.hrot), sx=p.hsx, sy=p.hsy)
    return [core], [out]


def ear_tri(p: Pose, side, inset=0.0, flat_scale=1.0):
    """side = +1 near (toward +x), -1 far."""
    if p.front:
        base_a = 36.0 * side          # face-on: the pair sits symmetric
    else:
        base_a = 30.0 * side + (14.0 if side > 0 else -6.0)
        # yawed, the near ear rotates OUT with the turning skull and the far
        # ear trails behind the crown
        base_a += (4.0 if side > 0 else -6.0) * p.yaw
    sweep = (p.ear_near if side > 0 else p.ear_far)
    pin = p.ear_flat
    # base sits on the skull, tip pushed outward and (when pinned) backward
    th = math.radians(base_a - 90.0 + p.hrot)
    bx = p.hx + math.cos(th) * (p.hr - 4.0) * p.hsx
    by = p.hy + math.sin(th) * (p.hr - 4.0) * p.hsy
    # the far ear foreshortens with the yaw: narrower, and a step shorter
    ear_k = 1.0 if (p.front or side > 0) else lerp(1.0, 0.80, p.yaw)
    w = (14.0 - inset * 0.55) * flat_scale * ear_k
    h = (20.5 - inset) * flat_scale * (1.0 if ear_k == 1.0 else lerp(1.0, 0.92, p.yaw))
    tip_a = base_a + sweep + pin * (-62.0 if side > 0 else -50.0)
    tth = math.radians(tip_a - 90.0 + p.hrot)
    tx = bx + math.cos(tth) * h
    ty = by + math.sin(tth) * h
    # base corners perpendicular to the base->tip axis
    px, py = -math.sin(tth), math.cos(tth)
    a = (bx + px * w * 0.5, by + py * w * 0.5)
    b = (bx - px * w * 0.5, by - py * w * 0.5)
    mid_a = ((a[0] + tx) * 0.5 + px * 1.5, (a[1] + ty) * 0.5 + py * 1.5)
    mid_b = ((b[0] + tx) * 0.5 - px * 1.5, (b[1] + ty) * 0.5 - py * 1.5)
    return catmull_closed([a, mid_a, (tx, ty), mid_b, b], tension=0.10)


def ear_drop(p: Pose, side, inset=0.0, flat_scale=1.0):
    """The DOG ear: a long rounded lobe HANGING down the side of the skull.

    Same contract as [`ear_tri`] — `side` = +1 near / -1 far, larger `inset`
    shrinks the lobe (the outline pass insets negative, the inner-ear pass
    insets positive) — so the three ear passes below are species-agnostic.

    The cat's ear is a triangle whose tip pushes AWAY from the skull; a dog's
    swings down past the jaw instead, which is the single cue that carries the
    species at 16 px. The pose's authored `ear_near`/`ear_far` sweep still
    tilts the lobe (a perked pose lifts it) and `ear_flat` still pins it, but
    pinning a hanging ear presses it BACK along the neck rather than flattening
    it onto the crown, so both sides rotate the same way.
    """
    if p.front:
        base_a = 62.0 * side
    else:
        base_a = 58.0 * side + (10.0 if side > 0 else -4.0)
        base_a += (4.0 if side > 0 else -6.0) * p.yaw
    sweep = (p.ear_near if side > 0 else p.ear_far)
    # The lobe roots slightly deeper into the skull than the cat's triangle:
    # a hanging ear is attached along a seam, not balanced on a point.
    th = math.radians(base_a - 90.0 + p.hrot)
    bx = p.hx + math.cos(th) * (p.hr - 6.0) * p.hsx
    by = p.hy + math.sin(th) * (p.hr - 6.0) * p.hsy
    # the far ear foreshortens with the yaw, exactly like the cat's
    ear_k = 1.0 if (p.front or side > 0) else lerp(1.0, 0.82, p.yaw)
    w = (15.5 - inset * 0.55) * flat_scale * ear_k
    h = (27.0 - inset) * flat_scale * (1.0 if ear_k == 1.0 else lerp(1.0, 0.90, p.yaw))
    # DROOP is what makes it a dog: ~82 degrees of swing past the ear's root
    # angle lands the tip below the jaw on both sides.
    tip_a = base_a + side * 82.0 + sweep + p.ear_flat * 26.0
    tth = math.radians(tip_a - 90.0 + p.hrot)
    dx, dy = math.cos(tth), math.sin(tth)
    px, py = -math.sin(tth), math.cos(tth)

    def at(along, across):
        return (bx + dx * h * along + px * w * across,
                by + dy * h * along + py * w * across)

    # A teardrop hanging from its narrow end: the root is the width of the
    # seam, the belly swells below it, and the tip rounds off.
    return catmull_closed([
        at(0.0, 0.46), at(0.42, 0.60), at(0.82, 0.40), at(1.0, 0.0),
        at(0.82, -0.40), at(0.42, -0.52), at(0.0, -0.46),
    ], tension=0.12)


def ear_shape(p: Pose, side, inset=0.0, flat_scale=1.0):
    """Dispatch one ear to its species' builder."""
    if p.species == "dog":
        return ear_drop(p, side, inset=inset, flat_scale=flat_scale)
    return ear_tri(p, side, inset=inset, flat_scale=flat_scale)


def ears_paths(p: Pose):
    coat, out = [], []
    for side in (-1, 1):
        out.append(ear_shape(p, side, inset=-S * 0.9))
        coat.append(ear_shape(p, side))
    return coat, out


def inner_ear_paths(p: Pose):
    # The dog's inner lobe insets harder: on a hanging ear only the upper
    # inside shows, so a cat-sized inner shape would paint pink over the whole
    # flap and read as a tongue.
    inset = 13.0 if p.species == "dog" else 9.0
    return [ear_shape(p, -1, inset=inset), ear_shape(p, 1, inset=inset)]


def tail_pts(p: Pose):
    root = rump(p)
    bones = [(p.tail_len, a) for a in p.tail]
    return chain(root, bones)


def tail_paths(p: Pose):
    pts = tail_pts(p)
    # A dog's tail is a thicker, blunter club than a cat's tapering whip — and
    # with the tabby rings gone it is the tail's OUTLINE that has to carry it.
    t = p.tail_thick * (1.18 if p.species == "dog" else 1.0)
    radii = [t, t * 0.92, t * 0.84, t * 0.76, t * 0.66]
    core = limb(pts, radii, smooth=True)
    out = limb(pts, [r + S for r in radii], smooth=True)
    return [core], [out]


def legs(p: Pose, near: bool):
    specs = [(shoulder(p, not near), p.fl_near if near else p.fl_far)]
    if near and p.front:
        # face-on both forelegs stand IN FRONT of the chest, side by side —
        # the far-pass slot would bury one behind the torso, so it joins the
        # near pass (off-side first, equal width: the pair is symmetric)
        specs.insert(0, (shoulder(p, True), p.fl_far))
    if not p.hide_hind:
        specs.append((hip(p, not near), p.hl_near if near else p.hl_far))
    coat, out = [], []
    for root, spec in specs:
        pts = leg_pts(root, spec)
        w = 8.2 if near else 7.2
        # the paw flares back out so the foot reads as a foot at 16 px
        radii = [w, w * 0.76, w * 0.94]
        coat.append(limb(pts, radii))
        out.append(limb(pts, [r + S for r in radii]))
    return coat, out


# ── face ───────────────────────────────────────────────────────────────────

def face_anchor(p: Pose, dx, dy):
    """Place a face feature in head-local units (x right, y down, 1 = radius)."""
    x = p.hx + dx * p.hr * p.hsx
    y = p.hy + dy * p.hr * p.hsy
    return rot_about(x, y, p.hx, p.hy, p.hrot)


def muzzle_dx(p: Pose):
    """The muzzle group's x offset: at rest it hints toward the near cheek;
    yawed it pushes hard toward the travel direction (~0.22 head radii — the
    single strongest 'the face turned' cue); face-on it sits dead centre."""
    return 0.0 if p.front else lerp(0.08, 0.22, p.yaw)


def muzzle_paths(p: Pose):
    if p.species == "dog":
        return dog_muzzle_paths(p)
    cx, cy = face_anchor(p, muzzle_dx(p), 0.38)
    return [blob(cx, cy, [(0, 13.5), (50, 15.5), (95, 17.0), (140, 14.5),
                          (180, 12.0), (220, 14.5), (265, 17.0), (310, 15.5)])]


def dog_snout_dx(p: Pose):
    """How far the DOG's muzzle group sits ahead of the cat's.

    MODEST ON PURPOSE. The muzzle layer carries no outline of its own — it is
    a cream patch painted ON the head's coat — so anything it does past the
    skull's edge reads as a sticker stuck to the face rather than a snout. The
    projection therefore has a hard budget: `dx * hr` plus the muzzle's +x
    spoke must stay inside `hr`. What actually sells the snout at ship size is
    the muzzle's SHAPE (long toward +x, shallow above) and the dark nose on
    its leading edge, not raw offset.
    """
    return muzzle_dx(p) + (0.0 if p.front else lerp(0.08, 0.12, p.yaw))


def dog_muzzle_paths(p: Pose):
    """The snout: the cat's round muzzle drawn out along the facing axis.

    Blob angles run clockwise from 12 o'clock, so 90 is the +x (facing) side —
    that is the spoke that grows, while the 0 (crown) spoke shrinks. Shallower
    on top and longer ahead is the whole difference between a cat's round
    cheek-pad and a dog's muzzle; keeping the total mass similar is what keeps
    it inside the skull.
    """
    if p.front:
        cx, cy = face_anchor(p, dog_snout_dx(p), 0.46)
        return [blob(cx, cy, [(0, 11.0), (45, 14.0), (90, 15.5), (135, 16.5),
                              (180, 17.0), (225, 16.5), (270, 15.5), (315, 14.0)])]
    cx, cy = face_anchor(p, dog_snout_dx(p), 0.44)
    return [blob(cx, cy, [(0, 11.0), (45, 15.5), (90, 20.0), (135, 17.0),
                          (180, 13.5), (225, 12.0), (270, 11.0), (315, 10.0)])]


def eye_centres(p: Pose):
    # the artist's placement: the eye line rides the skull's equator
    if p.front:
        return face_anchor(p, 0.42, 0.04), face_anchor(p, -0.42, 0.04)
    # yawed, both eyes slide toward the leading edge — the far eye crosses
    # most of the way to the nose bridge, which is what "the face turned"
    # actually looks like (a frontal pair merely SHRUNK still reads frontal).
    # The pair also rides a step HIGHER: the muzzle group leans down-forward
    # on a turned head, and the lift keeps the nose out of the near eye.
    near = face_anchor(p, lerp(0.46, 0.48, p.yaw), lerp(0.02, -0.04, p.yaw))
    far = face_anchor(p, lerp(-0.33, -0.16, p.yaw), lerp(0.04, -0.01, p.yaw))
    if p.far_eye:
        dx, dy, _, _ = p.far_eye
        far = (far[0] + dx, far[1] + dy)
    return near, far


def eye_scales(p: Pose):
    """(near_r, far_rx, far_ry): the far eye foreshortens in WIDTH with the
    yaw (to ~62% of the near eye at full turn) and only mildly in height —
    a turning eyeball loses azimuth, not elevation."""
    rn = 8.6
    if p.front:
        return rn, rn, rn
    fw = lerp(0.884, 0.62, p.yaw)   # 7.6/8.6 at rest
    fh = lerp(0.884, 0.80, p.yaw)
    if p.far_eye:
        _, _, sx, sy = p.far_eye
        return rn, rn * fw * sx, rn * fh * sy
    return rn, rn * fw, rn * fh


def eye_paths(p: Pose):
    near, far = eye_centres(p)
    # the artist's eye — ship-size legibility comes from the bake's face LOD.
    # Face-on the pair is equal; in 3/4 the off-side eye foreshortens.
    rn, rfx, rfy = eye_scales(p)
    if p.eyes == "closed":
        return [_lid(near, rn, 1.0), _lid(far, rfx, 1.0)]
    if p.eyes == "happy":
        return [_arc_eye(near, rn), _arc_eye(far, rfx)]
    if p.eyes == "squint":
        return [_lid(near, rn, 0.55), _lid(far, rfx, 0.55)]
    if p.eyes == "halflid":
        return [_half(near, rn, 1), _half(far, rfx, -1)]
    if p.eyes == "wink":
        # The NEAR eye is the closed one (an arc), the FAR eye stays open —
        # and the iris/pupil/catch-light stack below follows the same split:
        # far eye only. A stack painted on the near side would land on the
        # closed arc and leave the open eye a solid black dot.
        return [_arc_eye(near, rn), ellipse(far[0], far[1], rfx, rfy * 1.06)]
    k = 1.22 if p.eyes == "wide" else 1.06
    return [ellipse(near[0], near[1], rn, rn * k),
            ellipse(far[0], far[1], rfx, rfy * k)]


def _lid(c, r, thick):
    """A closed sleeping lid: shallow downward arc with body.

    Fat on purpose — a thin lid rasterizes to NOTHING at terminal sizes, and a
    closed eye that renders zero pixels reads as no face at all. (The chibi
    pass's 7.0 rescaled to the artist's eye: same ink share of the aperture.)"""
    x, y = c
    w = r * 1.15
    t = 4.8 * thick
    return catmull_closed([(x - w, y - 1.5), (x, y + r * 0.52), (x + w, y - 1.5),
                           (x, y + r * 0.52 - t)], tension=0.16)


def _half(c, r, sgn):
    """A half-lidded eye: a slim lens whose OUTER corner (`sgn` = the side it
    faces, +1 near / -1 far) droops a step lower — drowsy contentment. The
    squint's pinched arc is a WINCE (a settled loaf wearing it looks like it
    regrets settling); a level flat-topped block, with a whisker running
    through at the same height, reads as sunglasses."""
    x, y = c
    # The lens rides the UPPER half of the eye aperture — where a half-closed
    # lid physically sits, and clear of the whisker roots at cheek height.
    yy = y - r * 0.14
    w = r * 0.90
    li = r * (0.16 if sgn < 0 else 0.0)   # left corner drop
    ri = r * (0.16 if sgn > 0 else 0.0)   # right corner drop
    return catmull_closed([(x - w, yy + li), (x, yy + r * 0.10), (x + w, yy + ri),
                           (x + w * 0.55, yy + ri + r * 0.30), (x, yy + r * 0.52),
                           (x - w * 0.55, yy + li + r * 0.30)], tension=0.10)


def _arc_eye(c, r):
    """A happy ^ eye — a fat arc, so the purr and the groom keep a face at 1x.
    (8.5 on the chibi eye, rescaled to the artist's eye at the same ratio.)"""
    x, y = c
    w = r * 1.18
    t = 5.8
    return catmull_closed([(x - w, y + r * 0.42), (x, y - r * 0.46), (x + w, y + r * 0.42),
                           (x, y - r * 0.46 + t)], tension=0.14)


def iris_paths(p: Pose):
    if p.eyes in ("closed", "happy", "squint", "halflid"):
        return []
    near, far = eye_centres(p)
    _, rfx, rfy = eye_scales(p)
    # On a wink the NEAR eye is the closed one (`eye_paths`), so the iris —
    # and the pupil and catch-light that ride on it — goes to the FAR (open)
    # eye only. An earlier branch had this backwards: the stack sat on the
    # closed arc and the open eye rendered as a solid black dot.
    out = [] if p.eyes == "wink" else [ellipse(near[0], near[1], 6.2, 6.8)]
    frx, fry = (6.2, 6.8) if p.front else (rfx * 0.71, rfy * 0.79)
    out.append(ellipse(far[0], far[1], frx, fry))
    return out


def pupil_paths(p: Pose):
    if p.eyes in ("closed", "happy", "squint", "halflid"):
        return []
    near, far = eye_centres(p)
    _, rfx, rfy = eye_scales(p)
    gx, gy = p.gaze
    k = 1.9 if p.eyes == "wide" else 2.5
    out = [] if p.eyes == "wink" else [ellipse(near[0] + gx, near[1] + gy, 2.9, 6.4 / k * 2.5 * 0.62)]
    frx, fh = (2.9, 6.4) if p.front else (rfx * 0.33, rfy * 0.74)
    fgx = gx if p.front else gx * 0.85
    out.append(ellipse(far[0] + fgx, far[1] + gy, frx, fh / k * 2.5 * 0.62))
    return out


def catchlight_paths(p: Pose):
    if p.eyes in ("closed", "happy", "squint", "halflid"):
        return []
    near, far = eye_centres(p)
    _, rfx, rfy = eye_scales(p)
    out = [] if p.eyes == "wink" else [ellipse(near[0] - 2.6, near[1] - 3.4, 2.5, 2.5)]
    if p.front:
        out.append(ellipse(far[0] - 2.6, far[1] - 3.4, 2.5, 2.5))
    else:
        out.append(ellipse(far[0] - rfx * 0.32, far[1] - rfy * 0.42,
                           rfx * 0.29, rfx * 0.29))
    return out


def nose_paths(p: Pose):
    if p.species == "dog":
        # A dark button on the END of the snout, not a triangle on its top.
        # Rounded and deliberately oversized: it is the only dark mass on the
        # face besides the eyes, so it is what survives the face LOD and tells
        # you which end of the animal you are looking at.
        # BELOW THE EYE LINE, ALWAYS. The nose is the one face layer painted
        # AFTER the eyes, so it is the only one that can collide with them —
        # and the eyes reach ~0.32 head radii below centre, so the nose rides
        # at 0.58 where it clears them outright and still sits well inside the
        # muzzle it belongs to.
        if p.front:
            cx, cy = face_anchor(p, dog_snout_dx(p), 0.56)
        else:
            cx, cy = face_anchor(p, dog_snout_dx(p) + lerp(0.28, 0.34, p.yaw), 0.58)
        gape = []
        if p.mouth == "yawn":
            mcx, mcy = face_anchor(p, dog_snout_dx(p) + (0.0 if p.front else lerp(0.28, 0.34, p.yaw)), 0.62)
            gape.append(blob(mcx, mcy + 9.6, [(0, 7.4), (60, 9.2), (120, 9.6), (180, 9.0),
                                              (240, 9.6), (300, 9.2)]))
        return gape + [blob(cx, cy, [(0, 5.4), (55, 7.2), (110, 6.6), (180, 5.6),
                                     (250, 6.6), (305, 7.2)])]
    out = []
    if p.mouth == "yawn":
        # The yawn's GAPE is painted TWICE: here in the nose layer (nose
        # rose) as well as in `mouth_paths`. The bake (`pet_baker.rs`) culls
        # the `mouth` role below `MOUTH_DETAIL_MIN_H` = 40 px, and the ship
        # tile is 34 px tall — so a yawn authored only as a mouth has no
        # mouth at the one size that matters. The nose role is never culled;
        # at >= 40 px the dark mouth blob paints exactly over this one, so
        # nothing doubles up where both survive.
        mcx, mcy = face_anchor(p, muzzle_dx(p), 0.38)
        out.append(blob(mcx, mcy + 9.6, [(0, 7.4), (60, 9.2), (120, 9.6), (180, 9.0),
                                          (240, 9.6), (300, 9.2)]))
    # the nose slides DOWN the muzzle as the head yaws — with the eyes lifted
    # and the muzzle pushed forward, this is what keeps the pink triangle from
    # clipping the near eye's lower lid
    cx, cy = face_anchor(p, muzzle_dx(p), lerp(0.26, 0.32, 0.0 if p.front else p.yaw))
    return out + [catmull_closed([(cx - 5.2, cy - 2.6), (cx + 5.2, cy - 2.6),
                                  (cx, cy + 5.0)], tension=0.22)]


def mouth_paths(p: Pose):
    # anchored at the MUZZLE CENTRE, not its top edge — a smile hung off the
    # nose line pulls the corners up against the cheeks and reads as a grimace
    if p.species == "dog":
        # Hung off the DOG's nose, which sits lower and further forward than
        # the cat's — anchoring on the cat's line would park the smile beside
        # the snout instead of under it.
        cx, cy = face_anchor(
            p,
            dog_snout_dx(p) + (0.0 if p.front else lerp(0.28, 0.34, p.yaw)),
            # 0.62 lands the omega's top stroke exactly on the nose's lower
            # edge and its lowest ink a clear step inside the chin — further
            # down and the smile hangs off the jaw onto the coat.
            0.62,
        )
    else:
        cx, cy = face_anchor(p, muzzle_dx(p), 0.38)
    if p.mouth == "none":
        # No mouth at all. For the sleeping loaf: with ^ ^ eyes, ANY dark
        # stroke down here adds back the horizontal-bar mass that made the
        # face read awake, and at 24-36 px it lands within a pixel of the
        # nose and clots the muzzle. A bare muzzle under the arcs is the
        # cuter and the more legible read. Species-blind on purpose: the
        # dog's sleeping loaf wants the bare muzzle for the same reason.
        return []
    if p.mouth == "open":
        return [blob(cx, cy + 8.0, [(0, 5.4), (60, 6.8), (120, 7.0), (180, 6.8),
                                    (240, 7.0), (300, 6.8)])]
    if p.mouth == "yawn":
        # The pre-sleep yawn: the open blob grown ~1.4x and dropped a step,
        # so the gape reads as a yawn where the plain "open" reads as a meow.
        # Below the mouth cull this layer is gone — `nose_paths` carries the
        # same blob for the ship size.
        return [blob(cx, cy + 9.6, [(0, 7.4), (60, 9.2), (120, 9.6), (180, 9.0),
                                    (240, 9.6), (300, 9.2)])]
    if p.mouth == "oof":
        # The landing's grunt: a small round "oof", ~60% of the open mouth.
        # The full open blob smears the whole muzzle to a black clot at
        # 24-36 px (the 0.19.0 gauntlet's F6 residue); this one keeps a
        # readable muzzle at ship size while the face still says impact.
        return [blob(cx, cy + 7.4, [(0, 3.2), (60, 4.0), (120, 4.2), (180, 4.0),
                                    (240, 4.2), (300, 4.0)])]
    if p.mouth == "flat":
        return [limb([(cx - 6.0, cy + 6.0), (cx + 6.0, cy + 6.0)], [1.6, 1.6])]
    # the kawaii omega: two filled strokes hanging off the nose
    out = []
    for sgn in (-1, 1):
        out.append(limb([(cx, cy + 4.4),
                         (cx + sgn * 3.4, cy + 8.6),
                         (cx + sgn * 7.6, cy + 4.6)],
                        [1.9, 1.7, 1.3], smooth=True))
    return out


def whisker_paths(p: Pose):
    # Dogs have whiskers in life and never in drawings of dogs: the fan is a
    # CAT cue, and leaving it on undoes everything the ears and snout just did.
    if p.species == "dog":
        return []
    out = []
    if p.front:
        # face-on: rooted at the cheek edges BELOW the huge eyes and swept
        # down-and-out — the 3/4 anchors land inside the frontal eye span
        # and would rake straight across both irises
        fans = ((1, 0.68, 0.82, 0.0), (-1, -0.68, 0.82, 0.0))
        rows = ((0.14, 4.0, 26.0), (0.26, 16.0, 27.0))
    else:
        # yawed, the near fan roots slide forward with the muzzle and drop
        # below the lifted eye; the far fan both shortens and migrates in
        # toward the nose bridge — on a turned head the off-side whiskers are
        # mostly hidden by the muzzle
        fans = ((1, lerp(0.42, 0.50, p.yaw), 1.0, 0.08 * p.yaw),
                (-1, lerp(-0.80, -0.52, p.yaw), lerp(0.46, 0.30, p.yaw), 0.0))
        rows = ((-0.03, -13.0, 27.0), (0.11, 7.0, 29.0))
    if not p.whisker_far:
        fans = tuple(f for f in fans if f[0] > 0)
    if not p.whisker_near:
        fans = tuple(f for f in fans if f[0] < 0)
    for side, base_dx, scale, droot in fans:
        for dy, sweep, ln0 in rows:
            ln = ln0 * scale
            bx, by = face_anchor(p, base_dx, 0.30 + dy + droot)
            th = math.radians(sweep) if side > 0 else math.radians(180.0 - sweep)
            tx = bx + math.cos(th) * ln
            ty = by + math.sin(th) * ln
            mx = (bx + tx) / 2
            my = (by + ty) / 2 - 2.0
            out.append(limb([(bx, by), (mx, my), (tx, ty)],
                            [2.4, 1.7, 0.9], smooth=True))
    return out


def blush_paths(p: Pose):
    if not p.blush:
        return []
    # tucked just under the eyes (~0.10 head-radii below the lower lids), not
    # beside the muzzle — under-eye blush is the kawaii placement, kept from
    # the chibi pass and rescaled to the artist's eye line
    if p.front:
        a = face_anchor(p, 0.55, 0.44)
        b = face_anchor(p, -0.55, 0.44)
        return [ellipse(a[0], a[1], 5.0, 3.5), ellipse(b[0], b[1], 5.0, 3.5)]
    a = face_anchor(p, lerp(0.60, 0.64, p.yaw), 0.44)
    b = face_anchor(p, lerp(-0.48, -0.30, p.yaw), 0.46)
    return [ellipse(a[0], a[1], 5.0, 3.5),
            ellipse(b[0], b[1], lerp(4.4, 3.4, p.yaw), lerp(3.1, 2.5, p.yaw))]


def pattern_paths(p: Pose):
    """Tabby: three back bars plus two tail rings, riding the torso."""
    # The tabby is a CAT marking — bars down the back and rings on the tail
    # are the third thing (after ears and snout) that would keep saying "cat"
    # on a dog. A plain coat is also the honest read: the recolorable coat is
    # the user's chosen trail colour, and an unbroken field of it is what the
    # dog cameo's breeds already do.
    if p.species == "dog":
        return []
    out = []
    if p.front:
        # face-on, the back bars would paint across the face (pattern layers
        # after the head's coat) — so the tabby wears its classic forehead M
        # between the ears instead
        for i, t in enumerate((-0.42, 0.0, 0.42)):
            x, y = face_anchor(p, t * 0.60, -0.64)
            out.append(ellipse(x, y, 3.4 - abs(i - 1) * 0.3, 7.0))
    elif p.bar_site == "haunch" and p.haunch_at:
        # the over-the-shoulder seat: the head parks on the torso's top, so
        # the topline the bars can actually ride is the haunch's crown
        if len(p.haunch_at) == 5:
            cx, cy, hrx, hry, hrot_deg = p.haunch_at
        else:
            cx, cy, r = p.haunch_at
            hrx, hry, hrot_deg = r, r, 0.0
        for i, a in enumerate((-32.0, 0.0, 32.0)):
            th = math.radians(a)
            x0 = math.sin(th) * hrx * 0.74
            y0 = -math.cos(th) * hry * 0.74
            x, y = rot_about(cx + x0, cy + y0, cx, cy, hrot_deg)
            out.append(ellipse(x, y, 3.5 - abs(i - 1) * 0.3, 7.4,
                               math.radians(a * 0.75 + hrot_deg)))
    elif not p.curl:
        for i, t in enumerate((-0.40, -0.04, 0.32)):
            cx = p.bx + t * p.brx
            cy = p.by - p.bry * 0.56
            x, y = rot_about(cx, cy, p.bx, p.by, p.brot)
            out.append(ellipse(x, y, 3.8 - i * 0.3, 8.4, math.radians(p.brot + t * 26.0)))
    pts = tail_pts(p)
    for idx in (2, 3):
        x, y = pts[idx]
        out.append(ellipse(x, y, 5.6, 3.4))
    return out


# ── fit ────────────────────────────────────────────────────────────────────

def coords(d):
    import re
    nums = [float(t) for t in re.findall(r"-?\d+\.?\d*", d)]
    return nums[0::2], nums[1::2]


def bbox(p: Pose):
    xs, ys = [], []
    for _, _, _, paths in build(p):
        for d in paths:
            a, b = coords(d)
            xs += a
            ys += b
    return min(xs), min(ys), max(xs), max(ys)


# ── assembly ───────────────────────────────────────────────────────────────

def build(p: Pose):
    """Return the painter-ordered layer list for one pose."""
    if p.hide_legs:
        far_coat, far_out, near_coat, near_out = [], [], [], []
    else:
        far_coat, far_out = legs(p, near=False) if p.show_far_legs else ([], [])
        near_coat, near_out = legs(p, near=True)
    t_coat, t_out = torso_paths(p)
    h_coat, h_out = ([], []) if p.curl else haunch_paths(p)
    hd_coat, hd_out = head_paths(p)
    e_coat, e_out = ears_paths(p)
    tl_coat, tl_out = tail_paths(p)

    outline = tl_out + far_out + h_out + t_out + near_out + e_out + hd_out
    coat = tl_coat + far_coat + h_coat + t_coat + near_coat + e_coat + hd_coat
    if p.belly:
        # LAST in the coat list, and coat-only: it fills a counter the limbs
        # already enclose, so it must not carry an outline of its own — an
        # outlined patch would draw a seam across the belly it is hiding.
        bx, by, brx, bry = p.belly
        coat.append(ellipse(bx, by, brx, bry))

    layers = [
        ("outline", INK, "fixed", outline),
        ("coat", COAT, "coat", coat),
        ("pattern", PATTERN, "fixed", pattern_paths(p)),
        ("inner_ear", INNER_EAR, "fixed", inner_ear_paths(p)),
        ("muzzle", MUZZLE, "fixed", muzzle_paths(p)),
        ("eye", EYE, "fixed", eye_paths(p)),
        ("iris", IRIS, "iris", iris_paths(p)),
        ("detail", PUPIL, "fixed", pupil_paths(p)),
        ("catch_light", LIGHT, "fixed", catchlight_paths(p)),
        ("nose", DOG_NOSE if p.species == "dog" else NOSE, "fixed", nose_paths(p)),
        ("mouth", EYE, "fixed", mouth_paths(p)),
        ("whisker", INK, "fixed", whisker_paths(p)),
        ("blush", BLUSH, "fixed", blush_paths(p)),
    ]
    raw = [(r, f, rc, ps) for (r, f, rc, ps) in layers if ps]
    dx, dy = registration(raw, p)
    return [(r, f, rc, [translate(d, dx, dy) for d in ps]) for (r, f, rc, ps) in raw]


def registration(raw, p: Pose):
    """The pose's (dx, dy) into the viewbox.

    `dy` PLANTS THE POSE: it lands the lowest ink on [`GROUND_INK`], less the
    pose's authored `airborne` clearance. Doing this by measurement rather than
    by trusting the limb angles is what keeps the whole roster on one floor —
    a sit, a wash and a stand are built from very different chains, and hand-fit
    numbers drift the moment any of them is touched, which reads as the cat
    sinking and popping between actions.

    `dx` is a single constant for every pose, deliberately: a per-pose
    horizontal fit would slide the cat sideways between gait frames.
    """
    xs, ys = [], []
    for _, _, _, paths in raw:
        for d in paths:
            a, b = coords(d)
            xs += a
            ys += b
    return OX, GROUND_INK - p.airborne - max(ys)


def translate(d, dx, dy):
    """Offset every coordinate pair in a path string (all commands take pairs)."""
    import re
    parts = d.split()
    out = []
    i = 0
    while i < len(parts):
        t = parts[i]
        if t in ("M", "L", "C", "Z"):
            out.append(t)
            i += 1
            continue
        x = float(t) + dx
        y = float(parts[i + 1]) + dy
        out.append(fmt(x))
        out.append(fmt(y))
        i += 2
    return " ".join(out)


def sheet(poses, cols=4, ident="pet_sheet"):
    """Merge many poses into ONE asset so a single render shows the whole set."""
    rows = (len(poses) + cols - 1) // cols
    merged = {}
    order = []
    for i, p in enumerate(poses):
        dx = (i % cols) * VB_W
        dy = (i // cols) * VB_H
        for role, fill, recolor, paths in build(p):
            key = (role, fill, recolor)
            if key not in merged:
                merged[key] = []
                order.append(key)
            merged[key] += [translate(d, dx, dy) for d in paths]
    lines = [
        f'id = "{ident}"', 'kind = "special"',
        f"viewbox = [{fmt(cols * VB_W)}, {fmt(rows * VB_H)}]",
        "anchor = { eye_y = 0.34, center_x = 0.5, word_top = 1.0 }",
    ]
    # painter order must stay global, so re-sort by the canonical role order
    canon = [l[0] for l in build(poses[0])]
    order.sort(key=lambda k: canon.index(k[0]) if k[0] in canon else 99)
    for key in order:
        role, fill, recolor = key
        lines += ["", "[[layer]]", f'role = "{role}"', f'ref_fill = "{fill}"',
                  f'recolor = "{recolor}"',
                  "paths = [" + ", ".join(f'"{d}"' for d in merged[key]) + "]"]
    return "\n".join(lines) + "\n"


def emit(p: Pose, eye_y=0.34):
    lines = [
        f'id = "{p.ident}"',
        'kind = "special"',
        f"viewbox = [{fmt(VB_W)}, {fmt(VB_H)}]",
        f"anchor = {{ eye_y = {eye_y}, center_x = 0.5, word_top = 1.0 }}",
        "",
        f"# {p.note}",
    ]
    for role, fill, recolor, paths in build(p):
        lines.append("")
        lines.append("[[layer]]")
        lines.append(f'role = "{role}"')
        lines.append(f'ref_fill = "{fill}"')
        lines.append(f'recolor = "{recolor}"')
        body = ", ".join(f'"{d}"' for d in paths)
        lines.append(f"paths = [{body}]")
    return "\n".join(lines) + "\n"
