#!/usr/bin/env python3
"""The aterm dog roster: one parameterized head rig, ten breeds, emitted as
glyph-asset TOML in the exact cat-head schema (see art/glyphs/README.md).

Run from this directory:  python3 dogs.py
Then regenerate the drawlist:  cargo run -q -p aterm-effects --example gen_dog_glyphs
"""

import math

VB_W, VB_H = 160.0, 136.0

# ── ink (the cat sheet's reference palette, dog-adjusted) ──────────────────
INK = "#2B2530"        # outline (recolored to context ink at bake time)
COAT = "#C8955C"       # recolorable coat reference
MUZZLE = "#F7E9DE"     # fixed cream muzzle
EYE = "#241F29"        # fixed dark eye (DogBaker swaps to adaptive eye ink)
LIGHT = "#FFFFFF"      # glints
NOSE = "#2A2029"       # the big dog nose
MOUTH = "#2A2029"
TONGUE = "#E87E8E"     # fixed pink tongue
INNER_EAR = "#EFA3AE"
BLUSH = "#F2A9B4"
PATTERN = "#8A6242"    # fixed mid-brown markings
PALE = "#F3EBDF"       # fixed pale markings (mask / blaze)
SPOT = "#332C38"       # dalmatian spots / dark points
WHITE_COAT = "#F2EEE6" # the dalmatian's fixed coat

S = 4.0                # outline margin grown around every coat part

K = 0.5522847498       # circle-to-cubic constant


# ── segment-list path model ────────────────────────────────────────────────
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


def mirror(path):
    """Reflect about the vertical centreline."""
    return apply(path, lambda x, y: (VB_W - x, y))


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


def teardrop(cx, base_y, w, h, tip_round=0.55):
    """An upright rounded-triangle ear: base centred at (cx, base_y), apex
    above by h, half-width w. tip_round softens the apex."""
    ax, ay = cx, base_y - h
    r = w * tip_round
    return [
        ("M", cx - w, base_y),
        ("C", cx - w, base_y - h * 0.55, ax - r, ay + h * 0.18, ax, ay),
        ("C", ax + r, ay + h * 0.18, cx + w, base_y - h * 0.55, cx + w, base_y),
        ("C", cx + w * 0.7, base_y + h * 0.10, cx - w * 0.7, base_y + h * 0.10, cx - w, base_y),
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


def grow(shape_fn, margin):
    """Regenerate a parameterized shape with all radii grown by margin — the
    outline discipline used across the roster."""
    return shape_fn(margin)


# ── the head rig ───────────────────────────────────────────────────────────

class Head:
    """One breed's drawlist under construction, painter order."""

    def __init__(self, ident, note, eye_y=70.0):
        self.ident = ident
        self.note = note
        self.eye_y = eye_y
        self.layers = []  # (role, ref_fill, recolor, [path strings])

    def layer(self, role, fill, recolor, paths):
        self.layers.append((role, fill, recolor, [emit(p) for p in paths]))

    def toml(self):
        out = [
            f'id = "{self.ident}"',
            'kind = "head"',
            f"viewbox = [{int(VB_W)}, {int(VB_H)}]",
            "anchor = { eye_y = %s, center_x = 0.5, word_top = 1.0 }"
            % fmt(round(self.eye_y / VB_H, 2)),
            "",
            f"# {self.note}",
            "# Generated by dogs.py — edit the rig, not this file.",
        ]
        for role, fill, recolor, paths in self.layers:
            out += [
                "",
                "[[layer]]",
                f'role = "{role}"',
                f'ref_fill = "{fill}"',
                f'recolor = "{recolor}"',
                "paths = [" + ", ".join('"%s"' % p for p in paths) + "]",
            ]
        return "\n".join(out) + "\n"


def eyes(cy=70.0, dx=27.0, rx=7.5, ry=9.0):
    return [ellipse(80 - dx, cy, rx, ry), ellipse(80 + dx, cy, rx, ry)]


def glints(cy=70.0, dx=27.0, r=2.6):
    return [ellipse(80 - dx - 2.2, cy - 3.0, r, r), ellipse(80 + dx - 2.2, cy - 3.0, r, r)]


def blush(cy=84.0, dx=42.0):
    return [ellipse(80 - dx, cy, 8.5, 5.5), ellipse(80 + dx, cy, 8.5, 5.5)]


def nose(cy=88.0, rx=10.0, ry=7.0):
    return [ellipse(80, cy, rx, ry)]


def build(breed):
    """Assemble one breed from its feature table."""
    h = Head(breed["id"], breed["note"], eye_y=breed.get("eye_cy", 70.0))
    hx, hy = 80.0, breed.get("head_cy", 76.0)
    hrx, hry = breed.get("head_rx", 55.0), breed.get("head_ry", 52.0)

    def head_shape(m):
        return ellipse(hx, hy, hrx + m, hry + m)

    ear_fn = breed["ears"]  # margin -> [paths] (left side; mirrored here)
    extra_fn = breed.get("extra_coat", lambda m: [])  # unmirrored extras
    def ear_paths(m):
        left = ear_fn(m)
        return left + [mirror(p) for p in left] + extra_fn(m)

    # outline under everything: head + ears grown by S
    h.layer("outline", INK, "fixed", ear_paths(S) + [head_shape(S)])
    # coat: ears then head over them (drop ears tuck behind the cheeks)
    coat_fill, coat_recolor = breed.get("coat", (COAT, "coat"))
    h.layer("coat", coat_fill, coat_recolor, ear_paths(0.0) + [head_shape(0.0)])
    if "inner_ear" in breed:
        h.layer("inner_ear", INNER_EAR, "fixed", breed["inner_ear"]())
    for fill, paths in breed.get("patterns", []):
        h.layer("pattern", fill, "fixed", paths)
    mz = breed.get("muzzle", (80.0, 98.0, 30.0, 22.0))
    h.layer("muzzle", breed.get("muzzle_fill", MUZZLE), "fixed", [ellipse(*mz)])
    h.layer("blush", BLUSH, "fixed", blush(cy=breed.get("blush_cy", 84.0)))
    h.layer("eye", EYE, "fixed", eyes(cy=breed.get("eye_cy", 70.0), dx=breed.get("eye_dx", 27.0)))
    h.layer("detail", LIGHT, "fixed", glints(cy=breed.get("eye_cy", 70.0), dx=breed.get("eye_dx", 27.0)))
    h.layer("nose", NOSE, "fixed", nose(cy=breed.get("nose_cy", 88.0), rx=breed.get("nose_rx", 10.0)))
    m = breed.get("mouth", "tongue")
    mcy = breed.get("mouth_cy", 100.0)
    if m == "tongue":
        h.layer("mouth", MOUTH, "fixed", [mouth_open(80.0, mcy, 13.0, 9.0)])
        h.layer("detail", TONGUE, "fixed", [ellipse(80.0, mcy + 7.5, 7.5, 9.5)])
    elif m == "smile":
        h.layer("mouth", MOUTH, "fixed", [smile(80.0, mcy - 2.0, 13.0, 8.0)])
    elif m == "jowl":
        h.layer("mouth", MOUTH, "fixed",
                [smile(64.0, mcy, 11.0, 8.0), smile(96.0, mcy, 11.0, 8.0)])
        h.layer("detail", LIGHT, "fixed",
                [ellipse(72.0, mcy + 5.0, 3.5, 5.0), ellipse(88.0, mcy + 5.0, 3.5, 5.0)])
    elif m == "pug":
        h.layer("detail", TONGUE, "fixed", [ellipse(80.0, mcy + 4.0, 6.0, 7.5)])
    return h


# ── ear builders (left ear; the rig mirrors) ───────────────────────────────

def drop_ear(m, cx=26.0, cy=64.0, rx=16.0, ry=38.0, tilt=22.0):
    return [rotate(ellipse(cx - m * 0.3, cy, rx + m, ry + m), cx, cy, tilt)]


def long_drop_ear(m):
    return drop_ear(m, cx=23.0, cy=76.0, rx=15.0, ry=46.0, tilt=18.0)


def prick_ear(m, cx=42.0, base_y=48.0, w=21.0, hh=44.0, tilt=-14.0):
    return [rotate(teardrop(cx, base_y, w + m, hh + m * 1.5), cx, base_y - hh / 2, tilt)]


def big_prick_ear(m):
    return prick_ear(m, cx=40.0, base_y=50.0, w=25.0, hh=50.0, tilt=-17.0)


def fold_ear(m, cx=31.0, cy=40.0, rx=10.0, ry=17.0, tilt=-38.0):
    return [rotate(ellipse(cx, cy, rx + m, ry + m), cx, cy, tilt)]


def pom_ear(m):
    return [ellipse(28.0, 66.0, 17.0 + m, 24.0 + m)]


def inner_prick(cx=42.0, base_y=46.0, w=11.0, hh=28.0, tilt=-14.0):
    left = rotate(teardrop(cx, base_y, w, hh), cx, base_y - (44.0) / 2, tilt)
    return [left, mirror(left)]


# ── the breeds ─────────────────────────────────────────────────────────────

BREEDS = [
    dict(
        id="d1_retriever",
        note="Golden retriever: soft drop ears, tongue-out grin.",
        ears=drop_ear,
        mouth="tongue",
    ),
    dict(
        id="d1_husky",
        note="Husky: tall prick ears, pale mask, knowing smile.",
        ears=prick_ear,
        inner_ear=lambda: inner_prick(),
        patterns=[(PALE, [ellipse(80, 92, 34, 30), ellipse(62, 66, 12, 9), ellipse(98, 66, 12, 9)])],
        mouth="smile",
    ),
    dict(
        id="d1_corgi",
        note="Corgi: enormous rounded prick ears, pale blaze and cheeks.",
        ears=big_prick_ear,
        inner_ear=lambda: inner_prick(cx=40.0, base_y=48.0, w=13.0, hh=32.0, tilt=-17.0),
        patterns=[(PALE, [ellipse(80, 100, 26, 26), ellipse(80, 62, 7, 26)])],
        mouth="tongue",
    ),
    dict(
        id="d1_pug",
        note="Pug: folded flap ears, the dark mask muzzle, big worried eyes.",
        ears=fold_ear,
        head_ry=50.0,
        patterns=[(PATTERN, [ellipse(80, 44, 16, 6)])],
        muzzle=(80.0, 98.0, 27.0, 21.0),
        muzzle_fill="#4A4048",
        eye_dx=29.0,
        mouth="pug",
        mouth_cy=102.0,
    ),
    dict(
        id="d1_beagle",
        note="Beagle: long velvet drop ears, pale blaze up the brow.",
        ears=long_drop_ear,
        patterns=[(PATTERN, [ellipse(80, 34, 34, 12)]),
                  (PALE, [ellipse(80, 96, 24, 24), ellipse(80, 52, 9, 34)])],
        mouth="tongue",
    ),
    dict(
        id="d1_dalmatian",
        note="Dalmatian: fixed white coat, ink spots, spotted drop ears.",
        ears=drop_ear,
        coat=(WHITE_COAT, "fixed"),
        patterns=[(SPOT, [ellipse(52, 44, 6.5, 5.5), ellipse(104, 38, 5.5, 4.5),
                          ellipse(118, 60, 6, 5), ellipse(44, 92, 5, 4.5),
                          ellipse(96, 116, 5.5, 4.5), ellipse(64, 24, 4.5, 4),
                          ellipse(30, 62, 7, 9), ellipse(130, 74, 7, 9)])],
        mouth="tongue",
    ),
    dict(
        id="d1_shepherd",
        note="German shepherd: alert prick ears, tan brows on a saddle brow band.",
        ears=prick_ear,
        inner_ear=lambda: inner_prick(),
        patterns=[(SPOT, [ellipse(80, 40, 40, 16)]),
                  (PATTERN, [ellipse(57, 56, 8, 4.5), ellipse(103, 56, 8, 4.5)])],
        muzzle=(80.0, 100.0, 32.0, 24.0),
        mouth="tongue",
    ),
    dict(
        id="d1_poodle",
        note="Poodle: cloud topknot and pom ears.",
        ears=pom_ear,
        extra_coat=lambda m: [ellipse(58, 26, 17 + m, 14 + m), ellipse(80, 20, 19 + m, 15 + m),
                              ellipse(102, 26, 17 + m, 14 + m)],
        mouth="smile",
    ),
    dict(
        id="d1_dachshund",
        note="Dachshund: the long face, longer ears, biggest nose.",
        ears=long_drop_ear,
        head_ry=48.0,
        muzzle=(80.0, 100.0, 34.0, 25.0),
        nose_cy=90.0,
        nose_rx=12.0,
        mouth="tongue",
        mouth_cy=103.0,
    ),
    dict(
        id="d1_bulldog",
        note="Bulldog: wide jowly head, folded ears, proud underbite.",
        ears=fold_ear,
        head_rx=62.0,
        head_ry=48.0,
        head_cy=78.0,
        muzzle=(80.0, 100.0, 34.0, 22.0),
        eye_dx=31.0,
        mouth="jowl",
        mouth_cy=98.0,
    ),
]


def main():
    import pathlib

    here = pathlib.Path(__file__).parent
    for breed in BREEDS:
        head = build(breed)
        (here / f"{breed['id']}.toml").write_text(head.toml())
        print("wrote", f"{breed['id']}.toml")


if __name__ == "__main__":
    main()
