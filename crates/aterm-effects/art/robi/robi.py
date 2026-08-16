#!/usr/bin/env python3
"""The aterm ROBI rig: one parameterized full-body helper-robot, emitted as
glyph-asset TOML in the exact cat-sheet schema (see art/glyphs/README.md).

Robi is the helper robot from the user's Nitro Keyboard game, ported shape by
shape from its SVG (80x100 viewbox, scaled x1.6 into a 128x176 frame with
headroom for raised arms): rounded-rect head with a dark visor and a glowing
cyan face, antenna bulb, ear pods, chest light in a ring, belt vent, ball-joint
hands and stubby legs with rounded feet. Every pose is the same rig with the
limbs rotated about their NK pivots (arms: top-center of the shoulder rect;
legs: top-center of the hip rect), so the body can never drift off-model
between animation frames.

Poses (all one viewbox, swap-in-place): stand, a two-frame walk, two-frame
jumping jacks, two-frame ladder climb, and the three monkey-bar hangs (both
hands, left-hand swing, right-hand swing — the swings sway the whole body
about the gripping hand). Hanging arms stretch telescopically (a cartoon robot
courtesy: the NK arm is shorter than the head is tall, so a literal hang would
bury the bar in the visor).

The `robi_ladder` glyph is a VERTICALLY TILING segment (rails edge-to-edge at
y = 0 and y = H) — the emitter stacks N of them to any height, so the tile
must meet itself seamlessly.

Run from this directory:  python3 robi.py
Then regenerate the drawlist:  cargo run -q -p aterm-effects --example gen_robi_glyphs
"""

import math

VB_W, VB_H = 128.0, 176.0
GROUND = 170.0          # standing poses put the lowest foot texel here
GRIP_Y = 10.0           # hanging poses put the gripping hand center here

# ── palette (the REAL NK robot, chip-upgraded cyan) ────────────────────────
# Rendered side-by-side against the NK SVG: Robi has NO dark ink — his look
# is a white shell with SOFT light-gray strokes (#c2c9d6), small cyan DOT
# eyes with a thin smile, and the smart-chip cyan (#39d7ff) lights.
INK = "#C2C9D6"         # NK's stroke gray — the outline inflate IS the stroke
BODY = "#F7F9FC"        # white shell (NK gradient #ffffff->#d5dbe6, averaged light)
PLATE = "#C9CFDA"       # plating: ears, neck, antenna stalk, belt, chest ring
BOLT = "#AAB3C2"        # shoulder bolts
VISOR = "#1D2638"       # visor glass (NK #232c40->#131a2a, mid)
FACE = "#39D7FF"        # eyes + mouth — the NK smart-chip cyan, exactly
GLOW = "#39D7FF"        # chest light, antenna bulb
CHEEK = "#A6E4F4"       # cheek dots (NK cyan at 0.45 opacity over white)
LIGHT = "#FFFFFF"       # shell glint
RAIL = "#9AA3B2"        # ladder rails
RUNG = "#C9D0DC"        # ladder rungs

S = 2.6                 # outline margin grown around every silhouette part

K = 0.5522847498        # circle-to-cubic constant


# ── segment-list path model (the dogs.py vocabulary) ───────────────────────
# A path is a list of segments: ("M",x,y) ("L",x,y) ("C",x1,y1,x2,y2,x,y) ("Z",)

def fmt(v):
    s = f"{v:.1f}"
    return s[:-2] if s.endswith(".0") else s


def emit(path):
    out = []
    for seg in path:
        op = seg[0]
        if op == "Z":
            out.append("Z")
        else:
            out.append(op + " " + " ".join(fmt(c) for c in seg[1:]))
    return " ".join(out)


def apply(path, f):
    """Map every coordinate pair through f(x, y) -> (x, y)."""
    out = []
    for seg in path:
        op = seg[0]
        if op == "Z":
            out.append(("Z",))
        else:
            cs = seg[1:]
            pts = [f(cs[i], cs[i + 1]) for i in range(0, len(cs), 2)]
            out.append((op, *[c for p in pts for c in p]))
    return out


def translate(path, dx, dy):
    return apply(path, lambda x, y: (x + dx, y + dy))


def rotate(path, cx, cy, deg):
    a = math.radians(deg)
    ca, sa = math.cos(a), math.sin(a)

    def f(x, y):
        dx, dy = x - cx, y - cy
        return (cx + dx * ca - dy * sa, cy + dx * sa + dy * ca)

    return apply(path, f)


def ellipse(cx, cy, rx, ry):
    """Closed 4-arc cubic ellipse."""
    kx, ky = rx * K, ry * K
    return [
        ("M", cx + rx, cy),
        ("C", cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry),
        ("C", cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy),
        ("C", cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry),
        ("C", cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy),
        ("Z",),
    ]


def circle(cx, cy, r):
    return ellipse(cx, cy, r, r)


def rrect(x, y, w, h, r):
    """Rounded rect, radius clamped to the half-extents."""
    r = min(r, w / 2.0, h / 2.0)
    k = r * (1.0 - K)
    x1, y1 = x + w, y + h
    return [
        ("M", x + r, y),
        ("L", x1 - r, y),
        ("C", x1 - k, y, x1, y + k, x1, y + r),
        ("L", x1, y1 - r),
        ("C", x1, y1 - k, x1 - k, y1, x1 - r, y1),
        ("L", x + r, y1),
        ("C", x + k, y1, x, y1 - k, x, y1 - r),
        ("L", x, y + r),
        ("C", x, y + k, x + k, y, x + r, y),
        ("Z",),
    ]


def capsule(x1, y1, x2, y2, w):
    """A stadium from (x1,y1) to (x2,y2), width w (semicircle caps)."""
    dx, dy = x2 - x1, y2 - y1
    ln = math.hypot(dx, dy) or 1.0
    ux, uy = dx / ln, dy / ln
    nx, ny = -uy, ux
    h = w / 2.0
    kk = h * K
    # corners
    a = (x1 + nx * h, y1 + ny * h)
    b = (x2 + nx * h, y2 + ny * h)
    c = (x2 - nx * h, y2 - ny * h)
    d = (x1 - nx * h, y1 - ny * h)
    # cap apex points
    e2 = (x2 + ux * h, y2 + uy * h)
    e1 = (x1 - ux * h, y1 - uy * h)
    return [
        ("M", *a),
        ("L", *b),
        ("C", b[0] + ux * kk, b[1] + uy * kk, e2[0] + nx * kk, e2[1] + ny * kk, *e2),
        ("C", e2[0] - nx * kk, e2[1] - ny * kk, c[0] + ux * kk, c[1] + uy * kk, *c),
        ("L", *d),
        ("C", d[0] - ux * kk, d[1] - uy * kk, e1[0] - nx * kk, e1[1] - ny * kk, *e1),
        ("C", e1[0] + nx * kk, e1[1] + ny * kk, a[0] - ux * kk, a[1] - uy * kk, *a),
        ("Z",),
    ]


def smile(cx, cy, rx, ry, t=3.0):
    """A filled crescent smile arc (open at the top)."""
    kx, ky = rx * K, ry * K
    irx, iry = rx - t, max(ry - t, 1.0)
    ikx, iky = irx * K, iry * K
    return [
        ("M", cx - rx, cy),
        ("C", cx - rx, cy + ky, cx - kx, cy + ry, cx, cy + ry),
        ("C", cx + kx, cy + ry, cx + rx, cy + ky, cx + rx, cy),
        ("L", cx + irx, cy),
        ("C", cx + irx, cy + iky, cx + ikx, cy + iry, cx, cy + iry),
        ("C", cx - ikx, cy + iry, cx - irx, cy + iky, cx - irx, cy),
        ("Z",),
    ]


def mouth_open(cx, cy, rx, ry):
    """A filled open-smile (lower half-ellipse)."""
    kx, ky = rx * K, ry * K
    return [
        ("M", cx - rx, cy),
        ("C", cx - kx, cy, cx - kx, cy + ry, cx, cy + ry),
        ("C", cx + kx, cy + ry, cx + kx, cy, cx + rx, cy),
        ("Z",),
    ]


# ── the rig (all coordinates: NK SVG x 1.6, before per-pose offset) ────────
ARM_PIV = {"l": (26.8, 76.8), "r": (101.2, 76.8)}
ARM_LEN, ARM_W, HAND_R = 25.6, 12.0, 5.8
HIP = {"l": (52.0, 115.2), "r": (76.0, 115.2)}
LEG_LEN, LEG_W = 22.4, 14.4
FOOT_RX, FOOT_RY = 11.2, 6.4


def arm_paths(side, ang, stretch=1.0, m=0.0):
    """Shoulder capsule + ball hand, rotated `ang` about the NK shoulder pivot.
    ang 0 = hanging straight down; the rotation is the screen-space rotate()
    (positive swings the hand toward -x). `stretch` telescopes the arm."""
    px, py = ARM_PIV[side]
    ln = ARM_LEN * stretch
    cap = capsule(px, py + 1.0, px, py + ln, ARM_W + 2 * m)
    hand = circle(px, py + ln + 4.0, HAND_R + m)
    return [rotate(cap, px, py, ang), rotate(hand, px, py, ang)]


def hand_center(side, ang, stretch=1.0):
    """Where arm_paths puts the hand center (pre-offset)."""
    px, py = ARM_PIV[side]
    ln = ARM_LEN * stretch + 4.0
    a = math.radians(ang)
    return (px - ln * math.sin(a), py + ln * math.cos(a))


def leg_paths(side, ang, m=0.0):
    """Hip capsule + foot ellipse, rotated `ang` about the NK hip pivot."""
    px, py = HIP[side]
    cap = capsule(px, py + 1.0, px, py + LEG_LEN, LEG_W + 2 * m)
    foot = ellipse(px, py + LEG_LEN + 4.6, FOOT_RX + m, FOOT_RY + m)
    return [rotate(cap, px, py, ang), rotate(foot, px, py, ang)]


def foot_bottom(side, ang):
    """Lowest y the rotated foot reaches (pre-offset), approximated at the
    foot center plus its larger radius (close enough to plant the ground)."""
    px, py = HIP[side]
    ln = LEG_LEN + 4.6
    a = math.radians(ang)
    fy = py + ln * math.cos(a)
    return fy + FOOT_RY + abs(math.sin(a)) * (FOOT_RX - FOOT_RY)


def face_paths(kind):
    """Visor face features — the NK text-face `•‿•`: small cyan DOT eyes and a
    THIN smile curve (never big round cartoon eyes). Expression varies."""
    exl, exr, ey = 52.8, 75.2, 37.6
    if kind == "happy":
        return [circle(exl, ey, 2.8), circle(exr, ey, 2.8),
                smile(64.0, 42.5, 6.5, 4.0, 1.9)]
    if kind == "effort":
        # squeezed-shut effort eyes (>‿<) + a small open mouth
        return [capsule(exl - 3.6, ey, exl + 3.6, ey, 2.4),
                capsule(exr - 3.6, ey, exr + 3.6, ey, 2.4),
                mouth_open(64.0, 43.5, 3.4, 3.0)]
    if kind == "focus":
        return [circle(exl, ey, 2.4), circle(exr, ey, 2.4),
                capsule(60.0, 45.0, 68.0, 45.0, 2.2)]
    if kind == "wow":
        return [circle(exl, ey, 3.4), circle(exr, ey, 3.4),
                circle(64.0, 44.5, 2.8)]
    raise ValueError(kind)


def body_layers(arms, legs, face_kind):
    """Assemble the painter-ordered (role, fill, recolor, paths) layer list.
    arms/legs: {"l": (ang, stretch), "r": (ang, stretch)} (legs: ang only)."""

    def silhouettes(m):
        ps = []
        ps.append(capsule(64.0, 16.0, 64.0, 7.0, 3.2 + 2 * m))          # stalk
        ps.append(circle(64.0, 5.6, 5.0 + m))                            # bulb
        ps.append(rrect(17.6 - m, 30.4 - m, 11.2 + 2 * m, 20.8 + 2 * m, 5.6 + m))
        ps.append(rrect(99.2 - m, 30.4 - m, 11.2 + 2 * m, 20.8 + 2 * m, 5.6 + m))
        ps.append(rrect(25.6 - m, 14.4 - m, 76.8 + 2 * m, 51.2 + 2 * m, 20.8 + m))
        ps.append(rrect(56.0 - m, 65.6 - m, 16.0 + 2 * m, 6.4 + 2 * m, 2.4 + m))
        ps.append(rrect(36.8 - m, 72.0 - m, 54.4 + 2 * m, 44.8 + 2 * m, 14.4 + m))
        for side in ("l", "r"):
            a, st = arms[side]
            ps += arm_paths(side, a, st, m)
            ps += leg_paths(side, legs[side], m)
        return ps

    coat = [rrect(25.6, 14.4, 76.8, 51.2, 20.8),
            rrect(36.8, 72.0, 54.4, 44.8, 14.4),
            circle(64.0, 88.0, 8.8),                                     # ring inner
            rrect(50.0, 104.4, 28.0, 4.8, 2.4)]                          # belt inner
    for side in ("l", "r"):
        a, st = arms[side]
        coat += arm_paths(side, a, st)
        coat += leg_paths(side, legs[side])

    plate = [capsule(64.0, 16.0, 64.0, 7.0, 3.2),
             rrect(17.6, 30.4, 11.2, 20.8, 5.6),
             rrect(99.2, 30.4, 11.2, 20.8, 5.6),
             rrect(56.0, 65.6, 16.0, 6.4, 2.4),
             circle(64.0, 88.0, 11.2),                                   # ring outer
             rrect(48.0, 102.4, 32.0, 8.8, 4.4)]                        # belt frame

    return [
        ("outline", INK, "fixed", silhouettes(S)),
        ("coat", BODY, "fixed", coat),
        ("coat_shade", PLATE, "fixed", plate),
        ("eye", VISOR, "fixed", [rrect(36.8, 24.0, 54.4, 30.4, 13.6)]),
        ("detail", BOLT, "fixed", [circle(44.0, 77.6, 1.6), circle(84.0, 77.6, 1.6)]),
        ("catch_light", LIGHT, "fixed", [ellipse(37.0, 21.5, 3.0, 2.2)]),
        ("iris", FACE, "fixed", face_paths(face_kind)),
        ("blush", CHEEK, "fixed", [circle(32.0, 48.0, 2.6), circle(96.0, 48.0, 2.6)]),
        ("pattern", GLOW, "fixed", [circle(64.0, 88.0, 7.2), circle(64.0, 5.6, 5.0)]),
    ]


# NOTE: painter order puts every silhouette in one grown outline layer under
# one coat layer — so no limb may overlap the head/torso (its outline would
# vanish under the shared coat). Poses keep hands clear of the shell.

POSES = [
    dict(id="robi_stand", note="At ease: arms relaxed, happy visor.",
         arms={"l": (10, 1.0), "r": (-10, 1.0)},
         legs={"l": 0, "r": 0}, face="happy", plant=True),
    dict(id="robi_walk_0", note="Walk contact: left leg forward (facing right = flip_x).",
         arms={"l": (-16, 1.0), "r": (16, 1.0)},
         legs={"l": 20, "r": -20}, face="happy", plant=True),
    dict(id="robi_walk_1", note="Walk contact, other phase.",
         arms={"l": (16, 1.0), "r": (-16, 1.0)},
         legs={"l": -20, "r": 20}, face="happy", plant=True),
    dict(id="robi_jacks_0", note="Jumping jacks, closed: arms down, feet together.",
         arms={"l": (6, 1.0), "r": (-6, 1.0)},
         legs={"l": -4, "r": 4}, face="effort", plant=True),
    dict(id="robi_jacks_1", note="Jumping jacks, open: star jump at the hop apex.",
         arms={"l": (150, 1.0), "r": (-150, 1.0)},
         legs={"l": 22, "r": -22}, face="effort", plant=True, lift=8.0),
    dict(id="robi_climb_0", note="Ladder climb: left hand reaches, right hauls.",
         arms={"l": (174, 2.0), "r": (-158, 1.35)},
         legs={"l": 16, "r": -14}, face="focus", grip="l"),
    dict(id="robi_climb_1", note="Ladder climb, other hand.",
         arms={"l": (158, 1.35), "r": (-174, 2.0)},
         legs={"l": -14, "r": 16}, face="focus", grip="r"),
    dict(id="robi_hang_both", note="Monkey bars: both hands on the bar, happy dangle.",
         arms={"l": (172, 2.3), "r": (-172, 2.3)},
         legs={"l": 6, "r": -6}, face="happy", grip="l"),
    dict(id="robi_hang_l", note="Monkey bars: left hand grips, body sways right.",
         arms={"l": (178, 2.3), "r": (-40, 1.1)},
         legs={"l": 14, "r": 22}, face="wow", grip="l", sway=-10.0),
    dict(id="robi_hang_r", note="Monkey bars: right hand grips, body sways left.",
         arms={"l": (40, 1.1), "r": (-178, 2.3)},
         legs={"l": -22, "r": -14}, face="wow", grip="r", sway=10.0),
]


def build_pose(spec):
    layers = body_layers(spec["arms"], spec["legs"], spec["face"])
    if spec.get("plant"):
        bottom = max(foot_bottom(s, spec["legs"][s]) for s in ("l", "r"))
        oy = (GROUND - spec.get("lift", 0.0)) - bottom
        grip_frac = None
    else:
        side = spec["grip"]
        ang, st = spec["arms"][side]
        hy = hand_center(side, ang, st)[1]
        oy = GRIP_Y - hy
        gx, gy = hand_center(side, ang, st)
        grip = (gx, gy + oy)
        grip_frac = grip
    out = []
    for role, fill, recolor, paths in layers:
        moved = [translate(p, 0.0, oy) for p in paths]
        if spec.get("sway") and grip_frac:
            moved = [rotate(p, grip_frac[0], grip_frac[1], spec["sway"]) for p in moved]
        out.append((role, fill, recolor, moved))
    return out


def pose_toml(spec, layers):
    out = [
        f'id = "{spec["id"]}"',
        'kind = "special"',
        f"viewbox = [{int(VB_W)}, {int(VB_H)}]",
        "anchor = { eye_y = 0.3, center_x = 0.5, word_top = 1.0 }",
        "",
        f"# {spec['note']}",
        "# Generated by robi.py -- edit the rig, not this file.",
    ]
    for role, fill, recolor, paths in layers:
        out += [
            "",
            "[[layer]]",
            f'role = "{role}"',
            f'ref_fill = "{fill}"',
            f'recolor = "{recolor}"',
            "paths = [" + ", ".join('"%s"' % emit(p) for p in paths) + "]",
        ]
    return "\n".join(out) + "\n"


def ladder_toml():
    """The vertically tiling ladder segment: 48x32, rails edge-to-edge."""
    w, h = 48.0, 32.0
    layers = [
        ("outline", INK, "fixed",
         [rrect(1.6, 0.0, 8.8, h, 0.0), rrect(37.6, 0.0, 8.8, h, 0.0)]),
        ("coat_shade", RAIL, "fixed",
         [rrect(3.6, 0.0, 4.8, h, 0.0), rrect(39.6, 0.0, 4.8, h, 0.0)]),
        ("detail", RUNG, "fixed",
         [rrect(6.0, 5.5, 36.0, 5.0, 2.5), rrect(6.0, 21.5, 36.0, 5.0, 2.5)]),
    ]
    out = [
        'id = "robi_ladder"',
        'kind = "special"',
        f"viewbox = [{int(w)}, {int(h)}]",
        "anchor = { eye_y = 0.5, center_x = 0.5, word_top = 1.0 }",
        "",
        "# Vertically tiling ladder segment (rails meet at y=0 and y=H).",
        "# Generated by robi.py -- edit the rig, not this file.",
    ]
    for role, fill, recolor, paths in layers:
        out += [
            "",
            "[[layer]]",
            f'role = "{role}"',
            f'ref_fill = "{fill}"',
            f'recolor = "{recolor}"',
            "paths = [" + ", ".join('"%s"' % emit(p) for p in paths) + "]",
        ]
    return "\n".join(out) + "\n"


def main():
    import pathlib

    here = pathlib.Path(__file__).parent
    for spec in POSES:
        layers = build_pose(spec)
        (here / f"{spec['id']}.toml").write_text(pose_toml(spec, layers))
        print("wrote", f"{spec['id']}.toml")
    (here / "robi_ladder.toml").write_text(ladder_toml())
    print("wrote robi_ladder.toml")


if __name__ == "__main__":
    main()
