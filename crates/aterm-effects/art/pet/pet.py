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

S = 4.0               # outline half-width added to every coat part


@dataclass
class Pose:
    """One authored frame of the pet. Angles are degrees, 0 = straight down,
    positive swings toward +x (the direction the cat faces)."""
    ident: str
    note: str

    # torso
    bx: float = 90.0
    by: float = 64.0
    brx: float = 40.0
    bry: float = 23.0
    brot: float = 0.0        # body tilt

    # head
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
    eyes: str = "open"       # open | happy | closed | squint | wide | wink
    mouth: str = "smile"     # smile | open | flat | fang
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


# ── attachment points, derived from the torso ──────────────────────────────

FAR_OFFSET = (-7.0, -2.0)   # the off-side pair sits back and up: instant 3/4 read


def hip(p: Pose, far=False):
    dx, dy = FAR_OFFSET if far else (0.0, 0.0)
    return rot_about(p.bx - p.brx * 0.60 + dx, p.by + p.bry * 0.50 + dy,
                     p.bx, p.by, p.brot)


def shoulder(p: Pose, far=False):
    dx, dy = FAR_OFFSET if far else (0.0, 0.0)
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
    # a standing torso: the chest lifts toward the head, the belly tucks, the
    # rump stays round — the arch is what stops it reading as a sausage.
    spokes = [(0, 1.00), (42, 1.10), (78, 1.02), (112, 0.92), (150, 0.85),
              (180, 0.84), (212, 0.90), (250, 1.00), (285, 1.05), (322, 1.03)]
    core = blob(p.bx, p.by, [(a, r * p.brx) for a, r in spokes],
                rot=math.radians(p.brot), sy=p.bry / p.brx)
    out = blob(p.bx, p.by, [(a, r * (p.brx + S)) for a, r in spokes],
               rot=math.radians(p.brot), sy=(p.bry + S) / (p.brx + S))
    return [core], [out]


def haunch_paths(p: Pose):
    if p.haunch_at:
        cx, cy, r = p.haunch_at
        rx, ry = r * 1.02, r
    else:
        hx, hy = hip(p)
        cx, cy = hx - 2.0, hy - p.bry * 0.42
        r = p.bry * 0.86
        rx, ry = r * 1.05, r
    return [ellipse(cx, cy, rx, ry)], [ellipse(cx, cy, rx + S, ry + S)]


def head_spokes(p: Pose):
    # egg-ish 3/4 head: full cheeks, a slightly narrower crown, chin tapering
    return [(0, 0.94), (35, 0.99), (70, 1.03), (100, 1.02), (135, 0.94),
            (168, 0.88), (200, 0.94), (232, 1.02), (262, 1.03), (295, 1.00),
            (325, 0.96)]


def head_paths(p: Pose):
    sp = head_spokes(p)
    core = blob(p.hx, p.hy, [(a, r * p.hr) for a, r in sp],
                rot=math.radians(p.hrot), sx=p.hsx, sy=p.hsy)
    out = blob(p.hx, p.hy, [(a, r * (p.hr + S)) for a, r in sp],
               rot=math.radians(p.hrot), sx=p.hsx, sy=p.hsy)
    return [core], [out]


def ear_tri(p: Pose, side, inset=0.0, flat_scale=1.0):
    """side = +1 near (toward +x), -1 far."""
    base_a = 30.0 * side + (14.0 if side > 0 else -6.0)
    sweep = (p.ear_near if side > 0 else p.ear_far)
    pin = p.ear_flat
    # base sits on the skull, tip pushed outward and (when pinned) backward
    th = math.radians(base_a - 90.0 + p.hrot)
    bx = p.hx + math.cos(th) * (p.hr - 4.0) * p.hsx
    by = p.hy + math.sin(th) * (p.hr - 4.0) * p.hsy
    w = (14.0 - inset * 0.55) * flat_scale
    h = (20.5 - inset) * flat_scale
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


def ears_paths(p: Pose):
    coat, out = [], []
    for side in (-1, 1):
        out.append(ear_tri(p, side, inset=-S * 0.9))
        coat.append(ear_tri(p, side))
    return coat, out


def inner_ear_paths(p: Pose):
    return [ear_tri(p, -1, inset=9.0), ear_tri(p, 1, inset=9.0)]


def tail_pts(p: Pose):
    root = rump(p)
    bones = [(p.tail_len, a) for a in p.tail]
    return chain(root, bones)


def tail_paths(p: Pose):
    pts = tail_pts(p)
    t = p.tail_thick
    radii = [t, t * 0.92, t * 0.84, t * 0.76, t * 0.66]
    core = limb(pts, radii, smooth=True)
    out = limb(pts, [r + S for r in radii], smooth=True)
    return [core], [out]


def legs(p: Pose, near: bool):
    specs = [(shoulder(p, not near), p.fl_near if near else p.fl_far)]
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


def muzzle_paths(p: Pose):
    cx, cy = face_anchor(p, 0.08, 0.38)
    return [blob(cx, cy, [(0, 13.5), (50, 15.5), (95, 17.0), (140, 14.5),
                          (180, 12.0), (220, 14.5), (265, 17.0), (310, 15.5)])]


def eye_centres(p: Pose):
    near = face_anchor(p, 0.46, 0.02)
    far = face_anchor(p, -0.33, 0.04)
    return near, far


def eye_paths(p: Pose):
    near, far = eye_centres(p)
    rn, rf = 8.6, 7.6
    if p.eyes == "closed":
        return [_lid(near, rn, 1.0), _lid(far, rf, 1.0)]
    if p.eyes == "happy":
        return [_arc_eye(near, rn), _arc_eye(far, rf)]
    if p.eyes == "squint":
        return [_lid(near, rn, 0.55), _lid(far, rf, 0.55)]
    if p.eyes == "wink":
        return [_arc_eye(near, rn), ellipse(far[0], far[1], rf, rf * 1.06)]
    k = 1.22 if p.eyes == "wide" else 1.06
    return [ellipse(near[0], near[1], rn, rn * k),
            ellipse(far[0], far[1], rf, rf * k)]


def _lid(c, r, thick):
    """A closed sleeping lid: shallow downward arc with body."""
    x, y = c
    w = r * 1.15
    t = 2.6 * thick
    return catmull_closed([(x - w, y - 1.5), (x, y + r * 0.52), (x + w, y - 1.5),
                           (x, y + r * 0.52 - t)], tension=0.16)


def _arc_eye(c, r):
    """A happy ^ eye."""
    x, y = c
    w = r * 1.18
    t = 3.0
    return catmull_closed([(x - w, y + r * 0.42), (x, y - r * 0.46), (x + w, y + r * 0.42),
                           (x, y - r * 0.46 + t)], tension=0.14)


def iris_paths(p: Pose):
    if p.eyes in ("closed", "happy", "squint"):
        return []
    near, far = eye_centres(p)
    out = [ellipse(near[0], near[1], 6.2, 6.8)]
    if p.eyes != "wink":
        out.append(ellipse(far[0], far[1], 5.4, 6.0))
    return out


def pupil_paths(p: Pose):
    if p.eyes in ("closed", "happy", "squint"):
        return []
    near, far = eye_centres(p)
    gx, gy = p.gaze
    k = 1.9 if p.eyes == "wide" else 2.5
    out = [ellipse(near[0] + gx, near[1] + gy, 2.9, 6.4 / k * 2.5 * 0.62)]
    if p.eyes != "wink":
        out.append(ellipse(far[0] + gx * 0.85, far[1] + gy, 2.5, 5.6 / k * 2.5 * 0.62))
    return out


def catchlight_paths(p: Pose):
    if p.eyes in ("closed", "happy", "squint"):
        return []
    near, far = eye_centres(p)
    out = [ellipse(near[0] - 2.6, near[1] - 3.4, 2.5, 2.5)]
    if p.eyes != "wink":
        out.append(ellipse(far[0] - 2.4, far[1] - 3.2, 2.2, 2.2))
    return out


def nose_paths(p: Pose):
    cx, cy = face_anchor(p, 0.08, 0.26)
    return [catmull_closed([(cx - 5.2, cy - 2.6), (cx + 5.2, cy - 2.6),
                            (cx, cy + 5.0)], tension=0.22)]


def mouth_paths(p: Pose):
    cx, cy = face_anchor(p, 0.08, 0.26)
    if p.mouth == "open":
        return [blob(cx, cy + 8.0, [(0, 5.4), (60, 6.8), (120, 7.0), (180, 6.8),
                                    (240, 7.0), (300, 6.8)])]
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
    out = []
    for side, base_dx, scale in ((1, 0.42, 1.0), (-1, -0.80, 0.46)):
        for dy, sweep, ln0 in ((-0.03, -13.0, 27.0), (0.11, 7.0, 29.0)):
            ln = ln0 * scale
            bx, by = face_anchor(p, base_dx, 0.30 + dy)
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
    a = face_anchor(p, 0.62, 0.24)
    b = face_anchor(p, -0.52, 0.26)
    return [ellipse(a[0], a[1], 6.4, 4.4), ellipse(b[0], b[1], 5.6, 4.0)]


def pattern_paths(p: Pose):
    """Tabby: three back bars plus two tail rings, riding the torso."""
    out = []
    if not p.curl:
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
        ("nose", NOSE, "fixed", nose_paths(p)),
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
