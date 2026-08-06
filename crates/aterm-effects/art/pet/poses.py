#!/usr/bin/env python3
"""The pose sheet: every authored frame of the aterm pet kitty.

Angles are degrees, 0 = straight down the screen, positive swings toward +x
(the direction the cat faces). Every frame shares one viewbox and one anchor
set, so the renderer may swap any frame for any other in place.
"""

import os
import sys
from dataclasses import replace

from pet import Pose, bbox, emit, sheet

TH, SH = 19.0, 15.0          # default thigh / shin


def leg(thigh, shin, tl=TH, sl=SH):
    return (thigh, shin, tl, sl)


# ── the neutral stand: every other frame is a departure from this ──────────

STAND = Pose(
    ident="pet_stand",
    note="Neutral stand. The rig's rest pose and the measure of every other frame.",
    fl_near=leg(5.0, -3.0),
    fl_far=leg(-7.0, 3.0),
    hl_near=leg(-7.0, 7.0),
    hl_far=leg(5.0, -5.0),
)


def D(ident, note, **kw):
    return replace(STAND, ident=ident, note=note, **kw)


# ── walk: a four-beat lateral gait, one full cycle in four frames ──────────
# Reach forward with a nearly straight leg; trail back with a bent one — the
# asymmetry is what makes a cycle read as walking rather than scissoring.
REACH = leg(26.0, 12.0)
PASS_F = leg(6.0, -14.0)
TRAIL = leg(-24.0, -6.0)
PASS_B = leg(-8.0, 16.0)

WALK = [
    D("pet_walk_0", "Walk, contact: near-fore reaches, near-hind drives back.",
      fl_near=REACH, fl_far=TRAIL, hl_near=TRAIL, hl_far=REACH,
      by=64.0, tail=(-108.0, -136.0, -168.0, -202.0)),
    D("pet_walk_1", "Walk, pass: the legs swing under, the body lifts.",
      fl_near=PASS_F, fl_far=PASS_B, hl_near=PASS_B, hl_far=PASS_F,
      by=62.0, tail=(-116.0, -144.0, -176.0, -210.0)),
    D("pet_walk_2", "Walk, contact: the other diagonal.",
      fl_near=TRAIL, fl_far=REACH, hl_near=REACH, hl_far=TRAIL,
      by=64.0, tail=(-120.0, -148.0, -180.0, -214.0)),
    D("pet_walk_3", "Walk, pass: the legs swing under the other way.",
      fl_near=PASS_B, fl_far=PASS_F, hl_near=PASS_F, hl_far=PASS_B,
      by=62.0, tail=(-112.0, -140.0, -172.0, -206.0)),
]

# ── run: a bounding gallop, ears back, tail streaming ─────────────────────
RUN = [
    D("pet_run_0", "Run, gather: the back rounds up and every paw swings under.",
      by=62.0, brx=35.0, bry=26.0, brot=10.0, hy=50.0, hx=142.0, hrot=8.0,
      ear_flat=0.45, eyes="wide", mouth="open",
      fl_near=leg(-34.0, 40.0, 14.0, 11.0), fl_far=leg(-42.0, 34.0, 14.0, 11.0),
      hl_near=leg(44.0, -34.0, 15.0, 12.0), hl_far=leg(36.0, -40.0, 15.0, 12.0),
      tail=(-92.0, -108.0, -124.0, -140.0), tail_len=14.5),
    # The two suspension frames of the gallop are the moments no paw is on the
    # floor. The brain adds no lift during a run (only a pounce arcs), so the
    # clearance has to live in the art or the gallop reads as a shuffle.
    D("pet_run_1", "Run, extension: the body stretches long, every paw off the floor.",
      airborne=7.0,
      by=68.0, brx=45.0, bry=20.0, brot=-4.0, hy=44.0, hx=157.0,
      ear_flat=0.7, eyes="wide", mouth="open",
      fl_near=leg(48.0, 40.0, 18.0, 14.0), fl_far=leg(38.0, 34.0, 18.0, 14.0),
      hl_near=leg(-46.0, -58.0, 19.0, 16.0), hl_far=leg(-36.0, -50.0, 19.0, 16.0),
      tail=(-72.0, -80.0, -88.0, -96.0), tail_len=15.5),
    D("pet_run_2", "Run, contact: the forelegs take the whole cat.",
      by=64.0, brx=42.0, bry=22.0, brot=-8.0, hy=40.0, hx=155.0,
      ear_flat=0.5, eyes="wide", mouth="open",
      fl_near=leg(12.0, 2.0, 18.0, 15.0), fl_far=leg(2.0, -6.0, 18.0, 15.0),
      hl_near=leg(-40.0, -20.0, 19.0, 16.0), hl_far=leg(-30.0, -14.0, 19.0, 16.0),
      tail=(-80.0, -92.0, -104.0, -118.0), tail_len=15.0),
    D("pet_run_3", "Run, suspension: every paw is off the ground at once.",
      airborne=8.0,
      by=60.0, brx=40.0, bry=22.0, brot=2.0, hy=40.0, hx=150.0,
      ear_flat=0.6, eyes="wide", mouth="open",
      fl_near=leg(-18.0, 34.0, 16.0, 13.0), fl_far=leg(-26.0, 28.0, 16.0, 13.0),
      hl_near=leg(28.0, -46.0, 17.0, 14.0), hl_far=leg(20.0, -52.0, 17.0, 14.0),
      tail=(-84.0, -96.0, -110.0, -126.0), tail_len=15.0),
]

# ── sit / purr / groom: the settled family ────────────────────────────────
SIT_BASE = dict(
    # An upright chest over a seated haunch: the hind legs fold away entirely
    # and the rump itself is the ground contact, with the forelegs dropping
    # straight down the front edge.
    bx=104.0, by=52.0, brx=22.0, bry=27.0, brot=-8.0,
    hide_hind=True, haunch_at=(82.0, 88.0, 25.0),
    hx=138.0, hy=38.0,
    fl_root=(124.0, 68.0),
    fl_near=leg(0.0, 0.0, 24.0, 22.0), fl_far=leg(-8.0, 4.0, 24.0, 22.0),
    # rooted at the haunch's front-bottom so the sweep clears the rump and
    # reads as a tail curled around the forepaws instead of vanishing into it
    tail_root=(96.0, 106.0),
    tail=(95.0, 105.0, 122.0, 152.0), tail_len=12.0, tail_thick=7.5,
)

SETTLED = [
    D("pet_sit", "Sit: settled on the haunch, tail swept around to the forepaws.",
      **SIT_BASE),
    D("pet_sit_flick", "Sit, tail flick: the same seat, the tail-tip snapped up.",
      **{**SIT_BASE, "tail": (95.0, 112.0, 152.0, 196.0)}),
    D("pet_purr", "Purr: eyes squeezed shut, cheeks up, the chest swelled a notch.",
      **{**SIT_BASE, "eyes": "happy", "mouth": "smile",
         "brx": 25.5, "bry": 29.0, "hsx": 1.04}),
    D("pet_groom", "Groom: one forepaw lifted to the muzzle, eyes shut.",
      **{**SIT_BASE, "eyes": "happy", "mouth": "open", "hrot": 0.0, "hy": 40.0,
         "fl_near": leg(150.0, 133.0, 16.0, 13.0)}),
]

# ── sleep: a curled loaf that breathes ────────────────────────────────────
SLEEP_BASE = dict(
    curl=True, hide_legs=True, show_far_legs=False,
    bx=104.0, by=88.0, brx=46.0, bry=26.0,
    hx=150.0, hy=76.0, hr=27.0, hrot=20.0,
    eyes="closed", mouth="flat", ear_flat=0.30,
    tail_root=(62.0, 76.0),
    tail=(150.0, 115.0, 95.0, 78.0), tail_len=13.0, tail_thick=8.5,
)

SLEEP = [
    D("pet_sleep_0", "Sleep, breath out: a curled loaf with the tail draped over.",
      **SLEEP_BASE),
    D("pet_sleep_1", "Sleep, breath in: the same loaf, one notch fuller.",
      **{**SLEEP_BASE, "brx": 47.0, "bry": 27.6, "by": 87.0, "hy": 75.0}),
]

# ── the reactive family ───────────────────────────────────────────────────
REACTIVE = [
    D("pet_crouch", "Pounce, crouch: gathered low, eyes locked on the landing.",
      by=76.0, brx=41.0, bry=21.0, brot=4.0, hy=52.0, hx=152.0,
      eyes="wide", ear_near=-8.0, ear_far=-6.0,
      fl_near=leg(14.0, -20.0, 15.0, 12.0), fl_far=leg(6.0, -26.0, 15.0, 12.0),
      hl_near=leg(34.0, -30.0, 16.0, 13.0), hl_far=leg(26.0, -36.0, 16.0, 13.0),
      tail=(-70.0, -58.0, -44.0, -28.0), tail_len=14.0),
    D("pet_leap", "Pounce, flight: stretched along the arc, front paws leading.",
      by=64.0, brx=46.0, bry=19.0, brot=-14.0, hy=39.0, hx=160.0,
      eyes="wide", mouth="open", ear_flat=0.35,
      fl_near=leg(56.0, 48.0, 19.0, 15.0), fl_far=leg(46.0, 40.0, 19.0, 15.0),
      hl_near=leg(-52.0, -64.0, 20.0, 16.0), hl_far=leg(-42.0, -56.0, 20.0, 16.0),
      tail=(-64.0, -70.0, -76.0, -82.0), tail_len=16.0),
    D("pet_land", "Pounce, landing: braced forelegs, the whole cat compressed.",
      by=72.0, brx=44.0, bry=20.0, brot=-6.0, hy=50.0, hx=156.0,
      eyes="squint", mouth="open", ear_flat=0.5,
      fl_near=leg(22.0, 6.0, 17.0, 13.0), fl_far=leg(14.0, -2.0, 17.0, 13.0),
      hl_near=leg(-32.0, 10.0, 17.0, 13.0), hl_far=leg(-24.0, 4.0, 17.0, 13.0),
      tail=(-96.0, -114.0, -134.0, -152.0), tail_len=14.5),
    D("pet_startle", "Startle: back arched, tail bottled, ears pinned, eyes wide.",
      by=60.0, brx=36.0, bry=26.0, brot=0.0, hy=48.0, hx=146.0, hrot=-8.0,
      eyes="wide", mouth="open", ear_flat=1.0, blush=False,
      fl_near=leg(16.0, 18.0, 18.0, 15.0), fl_far=leg(8.0, 12.0, 18.0, 15.0),
      hl_near=leg(-20.0, -18.0, 18.0, 15.0), hl_far=leg(-12.0, -12.0, 18.0, 15.0),
      tail=(-142.0, -160.0, -178.0, -196.0), tail_len=13.0, tail_thick=12.5),
    D("pet_playbow", "Frolic, play bow: chest to the floor, rump and tail high.",
      by=68.0, brx=42.0, bry=22.0, brot=-22.0, hy=64.0, hx=152.0, hrot=10.0,
      eyes="happy", mouth="open",
      fl_near=leg(24.0, 46.0, 15.0, 11.0), fl_far=leg(16.0, 40.0, 15.0, 11.0),
      hl_near=leg(-6.0, 8.0, 20.0, 16.0), hl_far=leg(4.0, 2.0, 20.0, 16.0),
      tail=(-176.0, -196.0, -216.0, -238.0), tail_len=14.5),
    D("pet_bat", "Frolic, swipe: reared back on the haunch, a paw batting the caret.",
      **{**SIT_BASE, "eyes": "wide", "mouth": "open", "hrot": -10.0,
         "brot": -20.0, "hx": 142.0, "hy": 39.0,
         "fl_near": leg(112.0, 116.0, 22.0, 18.0),
         "fl_far": leg(46.0, 26.0, 21.0, 17.0),
         "tail": (92.0, 108.0, 142.0, 184.0)}),
    D("pet_stretch", "Stretch: the long wake-up, forelegs out, back scooped, yawning.",
      by=72.0, brx=48.0, bry=19.0, brot=-16.0, hy=76.0, hx=162.0, hrot=16.0,
      eyes="happy", mouth="open",
      fl_near=leg(58.0, 78.0, 18.0, 14.0), fl_far=leg(50.0, 72.0, 18.0, 14.0),
      hl_near=leg(-10.0, 6.0, 20.0, 16.0), hl_far=leg(0.0, 0.0, 20.0, 16.0),
      tail=(-160.0, -184.0, -206.0, -228.0), tail_len=15.0),
    D("pet_perk", "Alert: ears up, head lifted, tail flagged — it has seen you.",
      by=63.0, hy=39.0, hx=152.0, eyes="wide",
      ear_near=6.0, ear_far=4.0,
      fl_near=leg(2.0, -1.0), fl_far=leg(-6.0, 2.0),
      hl_near=leg(-6.0, 6.0), hl_far=leg(4.0, -4.0),
      tail=(-172.0, -182.0, -190.0, -198.0), tail_len=14.5),
]

POSES = [STAND] + WALK + RUN + SETTLED + SLEEP + REACTIVE


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "out"
    os.makedirs(out, exist_ok=True)
    bad = 0
    for p in POSES:
        text = emit(p)
        with open(os.path.join(out, f"{p.ident}.toml"), "w") as fh:
            fh.write(text)
        x0, y0, x1, y1 = bbox(p)
        from rig import VB_H, VB_W
        ok = x0 >= 0 and y0 >= 0 and x1 <= VB_W and y1 <= VB_H
        bad += 0 if ok else 1
        cmds = sum(text.count(c) for c in ("M ", "L ", "C ", " Z"))
        print(f"{'ok ' if ok else 'OOB'} {p.ident:16s} "
              f"({x0:6.1f},{y0:6.1f})-({x1:6.1f},{y1:6.1f}) "
              f"layers={text.count('[[layer]]'):2d} cmds={cmds:3d}")
    with open(os.path.join(out, "sheet.toml"), "w") as fh:
        fh.write(sheet(POSES, cols=5))
    print(f"{len(POSES)} poses, {bad} out of box; sheet.toml written")
