#!/usr/bin/env python3
"""Geometry primitives for the aterm pet-kitty rig.

Everything emits SVG path `d` strings in the pet's own viewbox frame
(208 x 128, y down), using only M / L / C / Z — the subset
`aterm_scene::vector::parse_path` accepts.
"""

import math

VB_W = 244.0
VB_H = 148.0
GROUND = 114.0          # rig-space chain target for the paw CENTRES
# Where the lowest INK of a planted pose lands in the viewbox. The emitter puts
# the sprite's bottom edge on the text baseline, so this is the cat's floor.
# Six units of slack under it absorbs the outline's outward cap.
GROUND_INK = 143.0
OX = 34.0               # rig -> viewbox offset, sized by the widest pose (leap)
K = 0.5523              # circle -> cubic magic constant


def fmt(v):
    s = f"{v:.1f}"
    return s[:-2] if s.endswith(".0") else s


def P(*nums):
    return " ".join(fmt(n) for n in nums)


# ── basic closed shapes ────────────────────────────────────────────────────

def ellipse(cx, cy, rx, ry, rot=0.0):
    """Closed ellipse as 4 cubics (6 commands)."""
    c = math.cos(rot)
    s = math.sin(rot)

    def t(x, y):
        x -= cx
        y -= cy
        return (cx + x * c - y * s, cy + x * s + y * c)

    kx, ky = rx * K, ry * K
    pts = [
        (cx, cy - ry),
        (cx + kx, cy - ry), (cx + rx, cy - ky), (cx + rx, cy),
        (cx + rx, cy + ky), (cx + kx, cy + ry), (cx, cy + ry),
        (cx - kx, cy + ry), (cx - rx, cy + ky), (cx - rx, cy),
        (cx - rx, cy - ky), (cx - kx, cy - ry), (cx, cy - ry),
    ]
    pts = [t(*p) for p in pts]
    d = [f"M {P(*pts[0])}"]
    for i in range(1, 13, 3):
        d.append(f"C {P(*pts[i], *pts[i + 1], *pts[i + 2])}")
    d.append("Z")
    return " ".join(d)


def blob(cx, cy, spokes, rot=0.0, sx=1.0, sy=1.0):
    """Closed smooth shape through `spokes` = [(angle_deg, radius), ...].

    Angles run clockwise from 12 o'clock in screen space (y down).
    Catmull-Rom through the spoke tips -> one cubic per span.
    """
    pts = []
    for a, r in spokes:
        th = math.radians(a - 90.0)
        x = math.cos(th) * r * sx
        y = math.sin(th) * r * sy
        ca, sa = math.cos(rot), math.sin(rot)
        pts.append((cx + x * ca - y * sa, cy + x * sa + y * ca))
    return catmull_closed(pts)


def catmull_closed(pts, tension=1.0 / 6.0):
    """Closed Catmull-Rom through `pts`, emitted as cubics."""
    n = len(pts)
    d = [f"M {P(*pts[0])}"]
    for i in range(n):
        p0 = pts[(i - 1) % n]
        p1 = pts[i]
        p2 = pts[(i + 1) % n]
        p3 = pts[(i + 2) % n]
        c1 = (p1[0] + (p2[0] - p0[0]) * tension, p1[1] + (p2[1] - p0[1]) * tension)
        c2 = (p2[0] - (p3[0] - p1[0]) * tension, p2[1] - (p3[1] - p1[1]) * tension)
        d.append(f"C {P(*c1, *c2, *p2)}")
    d.append("Z")
    return " ".join(d)


def catmull_open(pts, tension=1.0 / 6.0):
    """Open Catmull-Rom polyline -> list of cubic segments (as command strings)."""
    n = len(pts)
    segs = []
    for i in range(n - 1):
        p0 = pts[max(i - 1, 0)]
        p1 = pts[i]
        p2 = pts[i + 1]
        p3 = pts[min(i + 2, n - 1)]
        c1 = (p1[0] + (p2[0] - p0[0]) * tension, p1[1] + (p2[1] - p0[1]) * tension)
        c2 = (p2[0] - (p3[0] - p1[0]) * tension, p2[1] - (p3[1] - p1[1]) * tension)
        segs.append(f"C {P(*c1, *c2, *p2)}")
    return segs


# ── variable-width strokes (limbs, tail) ───────────────────────────────────

def _normals(pts):
    """Unit normal at each joint of a polyline (averaged across the joint)."""
    n = len(pts)
    segn = []
    for i in range(n - 1):
        dx = pts[i + 1][0] - pts[i][0]
        dy = pts[i + 1][1] - pts[i][1]
        ln = math.hypot(dx, dy) or 1.0
        segn.append((-dy / ln, dx / ln))
    out = []
    for i in range(n):
        if i == 0:
            nx, ny = segn[0]
        elif i == n - 1:
            nx, ny = segn[-1]
        else:
            ax, ay = segn[i - 1]
            bx, by = segn[i]
            nx, ny = ax + bx, ay + by
            ln = math.hypot(nx, ny) or 1.0
            nx, ny = nx / ln, ny / ln
            # miter compensation so a bend does not pinch
            cosb = ax * nx + ay * ny
            if cosb > 0.25:
                nx, ny = nx / cosb, ny / cosb
        out.append((nx, ny))
    return out


def limb(pts, radii, smooth=False, cap_start=True, cap_end=True):
    """Closed tapered stroke around polyline `pts` with per-joint `radii`.

    `smooth` runs the side rails through Catmull-Rom cubics (tails, curled
    legs); otherwise the rails are straight `L` segments (cheap, and correct
    for the two-bone legs).
    """
    nrm = _normals(pts)
    left = [(p[0] + nx * r, p[1] + ny * r) for p, (nx, ny), r in zip(pts, nrm, radii)]
    right = [(p[0] - nx * r, p[1] - ny * r) for p, (nx, ny), r in zip(pts, nrm, radii)]

    d = [f"M {P(*left[0])}"]
    if smooth:
        d += catmull_open(left)
    else:
        d += [f"L {P(*p)}" for p in left[1:]]

    # round cap at the far end: semicircle from left[-1] to right[-1], bulging
    # ALONG the limb's direction of travel
    if cap_end:
        d.append(_cap(left[-1], right[-1], radii[-1], _dir(pts[-2], pts[-1])))
    else:
        d.append(f"L {P(*right[-1])}")

    rev = list(reversed(right))
    if smooth:
        d += catmull_open(rev)
    else:
        d += [f"L {P(*p)}" for p in rev[1:]]

    if cap_start:
        d.append(_cap(right[0], left[0], radii[0], _dir(pts[1], pts[0])))
    d.append("Z")
    return " ".join(d)


def _dir(frm, to):
    """Unit vector frm -> to; the OUTWARD direction a cap must bulge in."""
    dx, dy = to[0] - frm[0], to[1] - frm[1]
    ln = math.hypot(dx, dy) or 1.0
    return (dx / ln, dy / ln)


def _cap(a, b, r, out):
    """Semicircular cap from rail point `a` to rail point `b`, bulging `r` along
    the unit vector `out`.

    The outward direction is passed IN rather than derived from the rail points,
    because at a cap the two rails are exactly diametric (`a` and `b` sit at
    ±normal·r about the same centre): their sum is the zero vector, so a bisector
    is not merely imprecise, it does not exist. Deriving one from a 90° rotation
    of the offset instead — the shape this rig shipped with first — points into
    the limb on this winding, which draws the cap CONCAVE, crosses the rails, and
    lets even-odd cancel the toe. The result is a notched, hollow paw on every
    limb of every pose; short limbs (a gathered run frame, a folded foreleg) lose
    the whole segment and punch a background-coloured hole through the body.
    """
    k = 1.3333 * r
    c1 = (a[0] + out[0] * k, a[1] + out[1] * k)
    c2 = (b[0] + out[0] * k, b[1] + out[1] * k)
    return f"C {P(*c1, *c2, *b)}"


# ── helpers ────────────────────────────────────────────────────────────────

def grow(pts, radii, s):
    return [r + s for r in radii]


def rot_about(px, py, cx, cy, deg):
    th = math.radians(deg)
    c, s = math.cos(th), math.sin(th)
    x, y = px - cx, py - cy
    return (cx + x * c - y * s, cy + x * s + y * c)


def chain(root, bones):
    """Forward-kinematic chain. `bones` = [(length, absolute_angle_deg), ...]
    with 0 deg pointing DOWN the screen, positive rotating toward +x."""
    pts = [root]
    x, y = root
    for ln, ang in bones:
        th = math.radians(ang)
        x += math.sin(th) * ln
        y += math.cos(th) * ln
        pts.append((x, y))
    return pts
