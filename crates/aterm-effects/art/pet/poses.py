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


# ── walk: a diagonal trot, one full cycle in four frames ───────────────────
# Reach forward with a nearly straight leg; trail back with a bent one — the
# asymmetry is what makes a cycle read as walking rather than scissoring.
REACH = leg(26.0, 12.0)
PASS_F = leg(6.0, -14.0)
TRAIL = leg(-24.0, -6.0)
PASS_B = leg(-8.0, 16.0)

WALK = [
    D("pet_walk_0", "Walk, contact: near-fore reaches, near-hind drives back.",
      yaw=1.0, fl_near=REACH, fl_far=TRAIL, hl_near=TRAIL, hl_far=REACH,
      by=64.0, tail=(-108.0, -136.0, -168.0, -202.0)),
    D("pet_walk_1", "Walk, pass: the legs swing under, the body lifts.",
      yaw=1.0, fl_near=PASS_F, fl_far=PASS_B, hl_near=PASS_B, hl_far=PASS_F,
      by=62.0, tail=(-116.0, -144.0, -176.0, -210.0)),
    D("pet_walk_2", "Walk, contact: the other diagonal.",
      yaw=1.0, fl_near=TRAIL, fl_far=REACH, hl_near=REACH, hl_far=TRAIL,
      by=64.0, tail=(-120.0, -148.0, -180.0, -214.0)),
    D("pet_walk_3", "Walk, pass: the legs swing under the other way.",
      yaw=1.0, fl_near=PASS_B, fl_far=PASS_F, hl_near=PASS_F, hl_far=PASS_B,
      by=62.0, tail=(-112.0, -140.0, -172.0, -206.0)),
]

# ── run: a bounding gallop, ears back, tail streaming ─────────────────────
RUN = [
    D("pet_run_0", "Run, gather: the back rounds up and every paw swings under.",
      yaw=1.0,
      by=62.0, brx=35.0, bry=26.0, brot=10.0, hy=50.0, hx=142.0, hrot=8.0,
      ear_flat=0.45, eyes="wide", mouth="open",
      fl_near=leg(-34.0, 40.0, 14.0, 11.0), fl_far=leg(-42.0, 34.0, 14.0, 11.0),
      hl_near=leg(44.0, -34.0, 15.0, 12.0), hl_far=leg(36.0, -40.0, 15.0, 12.0),
      tail=(-92.0, -108.0, -124.0, -140.0), tail_len=14.5),
    # The suspension frames carry their clearance in the art: the brain keeps
    # `lift == 0.0` on a run (`the_gallop_keeps_its_lift_out_of_the_pixel_grid`)
    # because `body_px` rounds a lift to whole pixels, while `registration()` bakes
    # this sub-pixel and anti-aliased. One viewbox unit is ART_ROWS·cell_h/148 px,
    # so 11 / 13 units are ~2.0 / 2.4 px at cell_h 16; both clear PLANT_TOL's
    # 4.5 units, as the AIRBORNE check needs.
    D("pet_run_1", "Run, extension: the body stretches long, every paw off the floor.",
      yaw=1.0,
      airborne=11.0,
      by=68.0, brx=45.0, bry=20.0, brot=-4.0, hy=44.0, hx=157.0,
      ear_flat=0.7, eyes="wide", mouth="open",
      fl_near=leg(48.0, 40.0, 18.0, 14.0), fl_far=leg(38.0, 34.0, 18.0, 14.0),
      hl_near=leg(-46.0, -58.0, 19.0, 16.0), hl_far=leg(-36.0, -50.0, 19.0, 16.0),
      tail=(-72.0, -80.0, -88.0, -96.0), tail_len=15.5),
    D("pet_run_2", "Run, contact: the forelegs take the whole cat.",
      yaw=1.0,
      by=64.0, brx=42.0, bry=22.0, brot=-8.0, hy=40.0, hx=155.0,
      ear_flat=0.5, eyes="wide", mouth="open",
      fl_near=leg(12.0, 2.0, 18.0, 15.0), fl_far=leg(2.0, -6.0, 18.0, 15.0),
      hl_near=leg(-40.0, -20.0, 19.0, 16.0), hl_far=leg(-30.0, -14.0, 19.0, 16.0),
      tail=(-80.0, -92.0, -104.0, -118.0), tail_len=15.0),
    D("pet_run_3", "Run, suspension: every paw is off the ground at once.",
      yaw=1.0,
      airborne=13.0,
      by=60.0, brx=40.0, bry=22.0, brot=2.0, hy=40.0, hx=150.0,
      ear_flat=0.6, eyes="wide", mouth="open",
      fl_near=leg(-18.0, 34.0, 16.0, 13.0), fl_far=leg(-26.0, 28.0, 16.0, 13.0),
      hl_near=leg(28.0, -46.0, 17.0, 14.0), hl_far=leg(20.0, -52.0, 17.0, 14.0),
      tail=(-84.0, -96.0, -110.0, -126.0), tail_len=15.0),
]

# ── sit / purr / groom: the settled family ────────────────────────────────
SIT_BASE = dict(
    # An upright chest over a seated haunch: the hind legs fold away, the rump is
    # the ground contact and the forelegs drop straight down the front edge. The
    # head sits up and back over the chest (hx 134); at 126 it reads as a
    # parade-ground seat.
    bx=104.0, by=52.0, brx=22.0, bry=27.0, brot=-10.0,
    hide_hind=True, haunch_at=(82.0, 88.0, 25.0),
    hx=134.0, hy=38.0,
    fl_root=(124.0, 68.0),
    fl_near=leg(0.0, 0.0, 24.0, 22.0), fl_far=leg(-8.0, 4.0, 24.0, 22.0),
    # rooted at the haunch's front-bottom so the sweep clears the rump and
    # reads as a tail curled around the forepaws instead of vanishing into it
    tail_root=(96.0, 105.0),
    tail=(95.0, 105.0, 122.0, 152.0), tail_len=12.0, tail_thick=8.5,
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
# The face is `happy` + `none`, not `closed` + `flat`: at 24-36 px `_lid()`'s fat
# downward arc rasterises to a dark bar tilted at the outer corner, the silhouette
# of a narrowed open eye, and a flat mouth under it reads as a glare. The ^ arc
# carries the same ink in a shape no open eye has.
SLEEP_BASE = dict(
    curl=True, hide_legs=True, show_far_legs=False,
    bx=104.0, by=88.0, brx=46.0, bry=26.0,
    hx=150.0, hy=76.0, hr=27.0, hrot=20.0,
    eyes="happy", mouth="none", ear_flat=0.30,
    # Both fans off: the far one lands on the curled dome (`Pose.whisker_far`), the
    # near one clots with the tipped-down eye (`Pose.whisker_near`).
    whisker_far=False, whisker_near=False,
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
      yaw=1.0,
      by=76.0, brx=41.0, bry=21.0, brot=4.0, hy=52.0, hx=152.0,
      eyes="wide", ear_near=-8.0, ear_far=-6.0,
      fl_near=leg(14.0, -20.0, 15.0, 12.0), fl_far=leg(6.0, -26.0, 15.0, 12.0),
      hl_near=leg(34.0, -30.0, 16.0, 13.0), hl_far=leg(26.0, -36.0, 16.0, 13.0),
      tail=(-70.0, -58.0, -44.0, -28.0), tail_len=14.0),
    D("pet_leap", "Pounce, flight: stretched along the arc, front paws leading.",
      yaw=1.0,
      by=64.0, brx=46.0, bry=19.0, brot=-14.0, hy=39.0, hx=160.0,
      eyes="wide", mouth="open", ear_flat=0.35,
      fl_near=leg(56.0, 48.0, 19.0, 15.0), fl_far=leg(46.0, 40.0, 19.0, 15.0),
      hl_near=leg(-52.0, -64.0, 20.0, 16.0), hl_far=leg(-42.0, -56.0, 20.0, 16.0),
      tail=(-64.0, -70.0, -76.0, -82.0), tail_len=16.0),
    D("pet_land", "Pounce, landing: braced forelegs, the whole cat compressed.",
      yaw=1.0,
      by=72.0, brx=44.0, bry=20.0, brot=-6.0, hy=50.0, hx=156.0,
      # "oof": the full open blob smears the muzzle black at ship size
      eyes="squint", mouth="oof", ear_flat=0.5,
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
      yaw=1.0,
      by=68.0, brx=42.0, bry=22.0, brot=-22.0, hy=64.0, hx=152.0, hrot=10.0,
      eyes="happy", mouth="open",
      fl_near=leg(24.0, 46.0, 15.0, 11.0), fl_far=leg(16.0, 40.0, 15.0, 11.0),
      hl_near=leg(-6.0, 8.0, 20.0, 16.0), hl_far=leg(4.0, 2.0, 20.0, 16.0),
      tail=(-176.0, -196.0, -216.0, -238.0), tail_len=14.5),
    D("pet_bat", "Frolic, swipe: reared back on the haunch, a paw batting the caret.",
      **{**SIT_BASE, "eyes": "wide", "mouth": "open", "hrot": -10.0,
         "yaw": 1.0,
         "brot": -20.0, "hx": 142.0, "hy": 39.0,
         "fl_near": leg(112.0, 116.0, 22.0, 18.0),
         "fl_far": leg(46.0, 26.0, 21.0, 17.0),
         "tail": (92.0, 108.0, 142.0, 184.0),
         # fills a counter the limbs enclose: see `Pose.belly`
         "belly": (139.0, 108.6, 11.5, 13.5)}),
    D("pet_stretch", "Stretch: the long wake-up, forelegs out, back scooped, yawning.",
      yaw=0.6,
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

# ── the direct-address pair: the cat finally looks at you ─────────────────
ADDRESS = [
    # The bell: a seated base about one head-width across, the head overlapping
    # the chest by a third (not a totem pole), two mitts breaking the bottom edge.
    D("pet_sit_front", "Sit, face-on: square to the viewer, tail swept round beside the mitts.",
      front=True, hide_hind=True, show_far_legs=False,
      bx=104.0, by=80.0, brx=23.0, bry=27.0, brot=0.0,
      haunch_at=(104.0, 86.0, 29.0),
      hx=104.0, hy=42.0,
      fl_root=(116.0, 78.0),
      fl_near=leg(0.0, 0.0, 20.0, 14.0), fl_far=leg(0.0, 0.0, 20.0, 14.0),
      tail_root=(121.0, 104.0),
      tail=(95.0, 100.0, 108.0, 125.0), tail_len=11.0, tail_thick=8.0),
    # The face-on blink: `pet_sit_front` is the most-worn awake frame (~9 % of all
    # frames). Only `eyes` departs from it, so nothing but the lids can move.
    D("pet_sit_front_blink", "Sit, face-on, blink: eye contact, both lids down.",
      front=True, hide_hind=True, show_far_legs=False,
      bx=104.0, by=80.0, brx=23.0, bry=27.0, brot=0.0,
      haunch_at=(104.0, 86.0, 29.0),
      hx=104.0, hy=42.0, eyes="closed",
      fl_root=(116.0, 78.0),
      fl_near=leg(0.0, 0.0, 20.0, 14.0), fl_far=leg(0.0, 0.0, 20.0, 14.0),
      tail_root=(121.0, 104.0),
      tail=(95.0, 100.0, 108.0, 125.0), tail_len=11.0, tail_thick=8.0),
    # The over-the-shoulder glance reads from body/head opposition: the hip
    # ellipse rotated away, the chest rising to a shoulder up-right of it, and the
    # head parked behind and left of that shoulder, overlapping it, so one contour
    # runs forward and one backward. Yaw 0.8 is a true 3/4; a half turn reads as
    # sitting sideways.
    D("pet_peek_shoulder", "Peek: seated facing away, head turned back over the shoulder.",
      hide_hind=True, haunch_at=(84.0, 88.0, 34.0, 27.0, -12.0),
      bx=112.0, by=60.0, brx=21.0, bry=25.0, brot=22.0,
      hx=116.0, hy=40.0, hrot=-6.0, yaw=0.8,
      # from behind only one foreleg edge shows; a doubled pair reads as a
      # notched two-toed paw
      show_far_legs=False,
      # the foreleg drops from the shoulder, not as a tangent off the cheek
      fl_root=(124.0, 74.0),
      fl_near=leg(-4.0, 3.0, 21.0, 16.0), fl_far=leg(8.0, -4.0, 21.0, 16.0),
      ear_near=10.0, ear_far=-8.0,
      # the head covers the torso's top: see `Pose.bar_site` and `Pose.whisker_far`
      bar_site="haunch", whisker_far=False,
      # the tail emerges below the rump's curve, clear of it, so the outline
      # separates tail from rump instead of extending the hip into one taper
      tail_root=(74.0, 112.0),
      tail=(-72.0, -88.0, -99.0, -107.0), tail_len=9.0, tail_thick=7.5),
]

# ── flight: the two halves of every arc the brain throws ──────────────────
# True mid-air frames with authored clearance for the rise and descent ends of
# every flight (`pose_schedule`), unlike `pet_leap`: planted art the brain's lift
# arcs, so the pounce touches down clean.
FLIGHT = [
    # The head overlaps the climbing chest's leading end (~11 units): a connected
    # neck, not a balloon on a string.
    D("pet_leap_rise", "Flight, rise: hind legs still driving, forepaws tucked, nose up.",
      yaw=1.0,
      airborne=6.0,
      # the torso climbs at 30 degrees: at 22 it reads as level flight with a
      # lifted chin
      by=62.0, brx=42.0, bry=20.0, brot=30.0, hx=141.0, hy=32.0, hrot=-12.0,
      ear_flat=0.25, eyes="wide", mouth="open",
      fl_near=leg(-30.0, 60.0, 12.0, 10.0), fl_far=leg(-38.0, 52.0, 12.0, 10.0),
      # swept back: from the steep hip they would otherwise notch the belly line
      hl_near=leg(-46.0, -56.0, 20.0, 16.0), hl_far=leg(-36.0, -48.0, 20.0, 16.0),
      tail=(-45.0, -56.0, -68.0, -80.0), tail_len=15.0),
    # The mirror half: the head overlapped down-forward of the dropping chest.
    D("pet_leap_descend", "Flight, descent: forelegs reaching for the floor, rump high.",
      yaw=1.0,
      airborne=7.0,
      # nose down, pupils aimed at the landing spot rather than the viewer
      by=58.0, brx=42.0, bry=20.0, brot=-20.0, hx=142.0, hy=61.0, hrot=14.0,
      ear_flat=0.35, eyes="wide", mouth="smile", gaze=(1.5, 2.0),
      fl_near=leg(28.0, 8.0, 20.0, 16.0), fl_far=leg(20.0, 2.0, 20.0, 16.0),
      hl_near=leg(-62.0, -80.0, 16.0, 12.0), hl_far=leg(-52.0, -70.0, 16.0, 12.0),
      # without it the descent reads one-eyed at ship size: see `Pose.far_eye`
      far_eye=(-1.1, -2.4, 1.238, 1.143),
      tail=(-125.0, -140.0, -155.0, -170.0), tail_len=14.5),
    # The top of a big bound. The schedule holds the hero frame for
    # `dur - LEAP_RISE_T - LEAP_DESC_T` (0.27-0.67 s on a bound) at a near-constant
    # lift and scale, so the apex splits that window with its own silhouette: the
    # hero is long and low with the legs fore-and-aft, the apex compact and high
    # with the legs gathered under. Mid-air, so it is in `pet_art_quality`'s AIRBORNE.
    D("pet_apex", "Flight, apex: the top of the bound — the back rounds, the hind legs swing under.",
      yaw=1.0,
      airborne=9.0,
      by=58.0, brx=40.0, bry=24.0, brot=0.0, hx=150.0, hy=34.0, hrot=-6.0,
      ear_flat=0.4, eyes="wide", mouth="open", gaze=(1.0, 1.0),
      fl_near=leg(24.0, 74.0, 13.0, 10.0), fl_far=leg(14.0, 66.0, 13.0, 10.0),
      hl_near=leg(-18.0, -62.0, 15.0, 12.0), hl_far=leg(-10.0, -56.0, 15.0, 12.0),
      tail=(-88.0, -104.0, -122.0, -140.0), tail_len=15.0),
]

# ── the wiggle and the loaf ────────────────────────────────────────────────
LOW = [
    # Alternates with pet_crouch at wiggle rate (7 Hz): the head, chest and
    # forepaws hold the crouch's marks (the forepaws on its exact shoulder root)
    # and only the rump shimmies. The rump rises by torso tilt (brot -8 vs +4)
    # over folded hind legs, not by longer legs: registration plants the lowest
    # ink, so longer legs would hoist the whole cat and float its front.
    D("pet_crouch_wiggle", "Pounce, wiggle: the crouch with its hindquarters up and offset.",
      yaw=1.0,
      by=76.0, brx=41.0, bry=21.0, brot=-8.0, bx=94.0, hy=52.5, hx=151.5,
      eyes="wide", ear_near=-8.0, ear_far=-6.0,
      fl_root=(114.7, 87.8),
      fl_near=leg(14.0, -20.0, 15.0, 12.0), fl_far=leg(6.0, -26.0, 15.0, 12.0),
      # shins shorter than the crouch's: the folded hind pair must finish above
      # the pinned forepaws' ink, or registration re-plants on the hind paw and
      # the cat hops at wiggle rate
      hl_near=leg(40.0, -22.0, 16.0, 12.0), hl_far=leg(32.0, -28.0, 16.0, 12.0),
      # flagged high but eased off vertical: a flagpole tail is a social signal,
      # not a hunting one
      tail=(-134.0, -147.0, -157.0, -164.0), tail_len=13.5),
    # The long-dwell settle: awake, legs tucked, about a row tall so the parked cat
    # never shades the inked line above the prompt. The head rests on the dome's
    # front end, its leading edge near-flush with the bread's; the happy arcs,
    # because a squint here reads as regret.
    D("pet_loaf", "Loaf: settled flat, paws tucked, eyes soft, tail hugging the side.",
      curl=True, hide_legs=True, show_far_legs=False,
      # a bread loaf, not a seal: shorter and taller than the sleep dome, still
      # under the row height the settle counts on
      bx=101.0, by=87.0, brx=45.5, bry=27.5,
      hx=128.0, hy=60.0, hrot=0.0, ear_flat=0.12,
      eyes="happy", mouth="smile",
      whisker_far=False,
      # a nub and tip: a long taper competes with the rump for the silhouette
      tail_root=(68.0, 98.0),
      tail=(-62.0, -82.0, -100.0, -114.0), tail_len=8.5, tail_thick=7.5),
]

# ── the roll: belly-up wriggling, the bored cat's third act ────────────────
# `brot` near 180 rotates the chest arch to the floor and the belly line to the
# sky; the hip/shoulder roots ride to the top of the mass (rot_about) and the
# tabby bars land on the grounded back, all from the rig's own math. The brain
# cycles 0 → 1 → 2 → 1 at wiggle rate, flip_x giving the squirm's left-right.
ROLL = [
    D("pet_roll_0", "Roll, flop: hip over, shoulders following, paws folding skyward.",
      by=84.0, bx=96.0, brx=40.0, bry=24.0, brot=150.0,
      hx=150.0, hy=76.0, hr=27.0, hrot=-18.0,
      eyes="happy", mouth="smile", ear_flat=0.30, whisker_far=False,
      fl_near=leg(150.0, -175.0, 15.0, 11.0), fl_far=leg(162.0, -160.0, 13.0, 10.0),
      hl_near=leg(-160.0, 172.0, 16.0, 12.0), hl_far=leg(-172.0, 158.0, 14.0, 11.0),
      tail_root=(58.0, 96.0),
      tail=(-95.0, -82.0, -70.0, -58.0), tail_len=12.0, tail_thick=8.0),
    D("pet_roll_1", "Roll, belly-up: flat on the back, paws loose in the air, bliss.",
      by=86.0, bx=96.0, brx=41.0, bry=25.0, brot=180.0,
      hx=152.0, hy=84.0, hr=27.0, hrot=-30.0,
      eyes="happy", mouth="open", ear_flat=0.40, whisker_far=False,
      fl_near=leg(174.0, -150.0, 16.0, 12.0), fl_far=leg(-172.0, 146.0, 14.0, 10.0),
      hl_near=leg(-178.0, 154.0, 17.0, 12.0), hl_far=leg(170.0, -148.0, 15.0, 11.0),
      tail_root=(56.0, 100.0),
      tail=(-88.0, -98.0, -108.0, -96.0), tail_len=12.5, tail_thick=8.0),
    D("pet_roll_2", "Roll, over-twist: hips past vertical, the squirm's far side.",
      by=84.0, bx=96.0, brx=40.0, bry=24.0, brot=210.0,
      hx=150.0, hy=80.0, hr=27.0, hrot=-40.0,
      eyes="happy", mouth="smile", ear_flat=0.35, whisker_far=False,
      fl_near=leg(-166.0, 168.0, 15.0, 11.0), fl_far=leg(-152.0, 178.0, 13.0, 10.0),
      hl_near=leg(168.0, -158.0, 16.0, 12.0), hl_far=leg(155.0, -170.0, 14.0, 11.0),
      tail_root=(58.0, 96.0),
      tail=(-100.0, -112.0, -122.0, -110.0), tail_len=12.0, tail_thick=8.0),
]

# ── lateral walk: the slow gait, one paw at a time ─────────────────────────
# WALK is a trot (diagonal pairs) and reads as one at any speed; a strolling cat
# moves one leg at a time (LH -> LF -> RH -> RF), three paws planted while the
# fourth swings. Each frame is sampled mid-swing. The brain drives it by distance
# like the trot.
LW_FWD = leg(19.0, 10.0)
LW_MID = leg(1.0, -1.0)
LW_BACK = leg(-18.0, -6.0)
LW_SWING = leg(10.0, -58.0)

LWALK = [
    D("pet_lwalk_0", "Lateral walk: near-hind swings under, far-fore leads.",
      yaw=1.0, hl_near=LW_SWING, fl_near=LW_BACK, hl_far=LW_MID, fl_far=LW_FWD,
      by=64.0, hy=41.0, tail=(-108.0, -136.0, -168.0, -202.0)),
    D("pet_lwalk_1", "Lateral walk: near-fore swings, near-hind just planted ahead.",
      yaw=1.0, hl_near=LW_FWD, fl_near=LW_SWING, hl_far=LW_BACK, fl_far=LW_MID,
      by=63.5, hy=42.0, tail=(-114.0, -142.0, -174.0, -208.0)),
    D("pet_lwalk_2", "Lateral walk: far-hind swings, near-fore reaches ahead.",
      yaw=1.0, hl_near=LW_MID, fl_near=LW_FWD, hl_far=LW_SWING, fl_far=LW_BACK,
      by=64.0, hy=41.0, tail=(-120.0, -148.0, -180.0, -214.0)),
    D("pet_lwalk_3", "Lateral walk: far-fore swings, near side trails.",
      yaw=1.0, hl_near=LW_BACK, fl_near=LW_MID, hl_far=LW_FWD, fl_far=LW_SWING,
      by=63.5, hy=42.0, tail=(-114.0, -142.0, -174.0, -208.0)),
]

# ── launch, hop, skid: the beats around a flight ───────────────────────────
LOCO = [
    # Planted on the hind paws, forepaws already clear: it plays on the coil's last
    # tick and the flight's u = 0 frame, where the brain's lift is zero, so it is
    # not in AIRBORNE.
    D("pet_launch", "Pounce, launch: hind legs driving off the floor, chest up, forepaws lifting.",
      yaw=1.0,
      by=68.0, brx=42.0, bry=21.0, brot=-25.0, hx=146.0, hy=43.0, hrot=-8.0,
      ear_flat=0.2, eyes="wide", mouth="open",
      fl_near=leg(30.0, 14.0, 19.0, 15.0), fl_far=leg(22.0, 6.0, 19.0, 15.0),
      hl_near=leg(-18.0, -32.0, 19.0, 15.0), hl_far=leg(-26.0, -40.0, 19.0, 15.0),
      tail=(-60.0, -72.0, -84.0, -96.0), tail_len=14.5),
    # The row hop's tuck: a short vertical flight never stretches long like a
    # pounce, so it has its own mid-air frame with authored clearance (AIRBORNE).
    D("pet_hop", "Hop: a tight tuck, every paw gathered, tail up, eyes on the landing.",
      yaw=1.0,
      airborne=6.0,
      by=62.0, brx=37.0, bry=24.0, brot=-8.0, hx=148.0, hy=40.0, hrot=-2.0,
      ear_flat=0.3, eyes="wide", mouth="smile", gaze=(1.0, 1.0),
      fl_near=leg(-36.0, 46.0, 13.0, 10.0), fl_far=leg(-44.0, 38.0, 13.0, 10.0),
      hl_near=leg(48.0, -44.0, 14.0, 11.0), hl_far=leg(40.0, -52.0, 14.0, 11.0),
      tail=(-128.0, -144.0, -156.0, -166.0), tail_len=13.5),
    # The drift-brake: planted on the forepaws; the folded hind pair sits under a
    # unit above the ground line, inside PLANT_TOL, so registration does not
    # re-plant on it.
    D("pet_skid", "Skid: forelegs braced ahead, rump dropped, ears pinned — the drift-brake.",
      yaw=1.0,
      by=78.0, brx=42.0, bry=21.0, brot=-14.0, hx=156.0, hy=50.0, hrot=-4.0,
      ear_flat=0.85, eyes="wide", mouth="flat",
      fl_near=leg(34.0, 20.0, 20.0, 16.0), fl_far=leg(26.0, 12.0, 20.0, 16.0),
      hl_near=leg(48.0, -56.0, 18.0, 14.0), hl_far=leg(40.0, -64.0, 18.0, 14.0),
      tail=(-140.0, -160.0, -185.0, -210.0), tail_len=14.0),
]

# ── idle: the small beats of a settled cat ─────────────────────────────────
# Each frame departs from a shipped base (the stand, SIT_BASE, the loaf, the peek,
# the perk) by a field or two, so the swap-in-place rule holds by construction.
# The brain deals these on the settle ladder's timers; none carries lift.
IDLE = [
    D("pet_droop", "Droop: the sulk — flat to the floor, ears pinned, head hung, tail limp behind.",
      by=84.0, brx=42.0, bry=20.0, brot=0.0,
      hx=150.0, hy=68.0, hrot=14.0,
      ear_flat=1.0, ear_near=-10.0, ear_far=-8.0,
      eyes="halflid", mouth="flat", blush=False,
      fl_near=leg(20.0, -20.0, 12.0, 10.0), fl_far=leg(12.0, -26.0, 12.0, 10.0),
      hl_near=leg(30.0, -30.0, 12.0, 10.0), hl_far=leg(22.0, -36.0, 12.0, 10.0),
      tail_root=(50.0, 92.0),
      tail=(-35.0, -65.0, -90.0, -95.0), tail_len=13.0, tail_thick=8.0),
    D("pet_droop_sit", "Droop, seated: the seat, head hung, ears pinned, tail flat forward.",
      **{**SIT_BASE, "hy": 50.0, "hx": 131.0, "hrot": 12.0, "brot": -4.0, "ear_flat": 1.0,
         "eyes": "halflid", "mouth": "flat", "blush": False,
         "tail": (88.0, 84.0, 82.0, 80.0)}),
    # Ear flicks: a sweep this large (+70) is what moves a device pixel at 34 px.
    D("pet_sit_ear", "Sit, ear flick: the near ear swivelled out sideways.",
      **{**SIT_BASE, "ear_near": 70.0}),
    D("pet_sit_ear_far", "Sit, ear flick: the far ear swung out.",
      **{**SIT_BASE, "ear_far": -60.0}),
    D("pet_stand_ear", "Stand, ear flick: the near ear swivelled out sideways.", ear_near=70.0),
    D("pet_loaf_ear", "Loaf, ear flick: the near ear swivelled out.",
      curl=True, hide_legs=True, show_far_legs=False,
      bx=101.0, by=87.0, brx=45.5, bry=27.5,
      hx=128.0, hy=60.0, hrot=0.0, ear_flat=0.12, ear_near=55.0,
      eyes="happy", mouth="smile", whisker_far=False,
      tail_root=(68.0, 98.0),
      tail=(-62.0, -82.0, -100.0, -114.0), tail_len=8.5, tail_thick=7.5),
    # `closed` reads as a scowl only when held (see SLEEP_BASE); a blink lasts 0.1 s.
    D("pet_sit_blink", "Sit, blink: the seat with both lids down.",
      **{**SIT_BASE, "eyes": "closed"}),
    # `yawn` is the one mouth the rig paints twice (see `nose_paths`).
    D("pet_yawn", "Yawn: head tipped back, eyes squeezed, the gape wide.",
      **{**SIT_BASE, "eyes": "happy", "mouth": "yawn", "hrot": -18.0, "hy": 36.0, "hx": 133.0,
         "ear_flat": 0.35}),
    # The seated tail beat's second side: swept behind the seat.
    D("pet_sit_tail_low", "Sit, tail behind: swept back along the floor.",
      **{**SIT_BASE, "tail_root": (80.0, 108.0), "tail": (-85.0, -90.0, -98.0, -110.0)}),
    D("pet_sit_tail_back", "Sit, tail behind, tip flicked up.",
      **{**SIT_BASE, "tail_root": (78.0, 106.0), "tail": (-95.0, -115.0, -150.0, -178.0)}),
    D("pet_loaf_open", "Loaf, eyes open: the drowsy check.",
      curl=True, hide_legs=True, show_far_legs=False,
      bx=101.0, by=87.0, brx=45.5, bry=27.5,
      hx=128.0, hy=60.0, hrot=0.0, ear_flat=0.12,
      eyes="open", gaze=(0.6, 0.4), mouth="smile", whisker_far=False,
      tail_root=(68.0, 98.0),
      tail=(-62.0, -82.0, -100.0, -114.0), tail_len=8.5, tail_thick=7.5),
    # A seated cat looks at a caret on the line above or below: chin and pupils together.
    D("pet_sit_lookup", "Sit, look up: chin lifted, pupils raised to the line above.",
      **{**SIT_BASE, "hrot": -20.0, "hy": 36.0, "gaze": (0.6, -2.4), "ear_near": 4.0, "ear_far": 2.0}),
    D("pet_sit_lookdown", "Sit, look down: chin tucked, pupils dropped to the line below.",
      **{**SIT_BASE, "hrot": 16.0, "hy": 42.0, "gaze": (0.4, 2.2)}),
    # The flank lick: the peek's body, the head turned back and down.
    D("pet_groom_flank", "Groom, flank: seated facing away, head turned back and down into the shoulder.",
      hide_hind=True, haunch_at=(84.0, 88.0, 34.0, 27.0, -12.0),
      bx=112.0, by=60.0, brx=21.0, bry=25.0, brot=22.0,
      hx=112.0, hy=52.0, hrot=14.0, yaw=0.8,
      show_far_legs=False, fl_root=(124.0, 74.0),
      fl_near=leg(-4.0, 3.0, 21.0, 16.0), fl_far=leg(8.0, -4.0, 21.0, 16.0),
      ear_near=10.0, ear_far=-8.0, bar_site="haunch", whisker_far=False,
      eyes="happy", mouth="open",
      tail_root=(74.0, 112.0), tail=(-72.0, -88.0, -99.0, -107.0), tail_len=9.0, tail_thick=7.5),
    # The wake-up's second half, after `pet_stretch`; planted on the forepaws and
    # the other hind paw.
    D("pet_stretch_hind", "Stretch, hind: forepaws planted, rump up, a hind leg reaching straight back.",
      yaw=0.6, by=72.0, brx=44.0, bry=20.0, brot=10.0, hy=50.0, hx=158.0, hrot=-4.0,
      eyes="happy", mouth="smile",
      fl_near=leg(4.0, -2.0, 16.0, 12.0), fl_far=leg(-4.0, 4.0, 16.0, 12.0),
      hl_near=leg(-45.0, -42.0, 24.0, 22.0), hl_far=leg(-10.0, 10.0, 18.0, 14.0),
      tail=(-140.0, -155.0, -165.0, -170.0), tail_len=14.0),
    # Before a hop or a bound: looking where it is about to go, not at you.
    D("pet_perk_turn", "Alert, turned: the perk with the head yawed into the facing.",
      yaw=1.0, by=63.0, hy=39.0, hx=152.0, eyes="wide",
      ear_near=6.0, ear_far=4.0,
      fl_near=leg(2.0, -1.0), fl_far=leg(-6.0, 2.0),
      hl_near=leg(-6.0, 6.0), hl_far=leg(4.0, -4.0),
      tail=(-172.0, -182.0, -190.0, -198.0), tail_len=14.5),
]

# ── console contact: weight, local attention, and a supported edge ─────────
# Authored joint poses, all weight-bearing: registration plants the lowest ink on
# GROUND_INK and the engine supplies movement between safe places, so an edge pose
# never dangles a limb below the shared contact plane.
CONTACT = [
    D("pet_contact_brace", "Ink contact: chest recoils over spread planted paws, ears back, tail curled clear.",
      yaw=0.8, bx=92.0, by=76.0, brx=39.0, bry=22.0, brot=-7.0,
      hx=139.0, hy=48.0, hrot=-12.0, eyes="wide", mouth="oof", ear_flat=0.72,
      fl_root=(121.0, 86.0),
      fl_near=leg(48.0, 12.0, 19.0, 15.0), fl_far=leg(35.0, 8.0, 19.0, 15.0),
      hl_near=leg(-30.0, 12.0, 17.0, 14.0), hl_far=leg(-20.0, 6.0, 17.0, 14.0),
      tail_root=(55.0, 73.0), tail=(20.0, 72.0, 140.0, 192.0),
      tail_len=10.5, tail_thick=8.0),
    D("pet_tail_tuck", "Ink clearance: the standing cat draws its long tail into a tight hook under the flank.",
      tail_root=(52.0, 77.0), tail=(18.0, 66.0, 133.0, 190.0),
      tail_len=11.5, tail_thick=8.0, gaze=(-1.2, 0.5), ear_far=-12.0),
    D("pet_contact_recover", "Contact recovery: weight returns to three paws while the near forepaw gathers for one step.",
      yaw=0.8, by=65.0, brx=39.0, bry=22.0, hx=148.0, hy=37.0, hrot=-5.0,
      fl_near=leg(-25.0, 60.0, 17.0, 12.0), fl_far=leg(2.0, -2.0),
      hl_near=leg(-12.0, 18.0), hl_far=leg(5.0, -8.0),
      tail=(-105.0, -138.0, -177.0, -214.0), tail_len=12.0),
    D("pet_inspect_down", "Inspect a local edit: neck extends down over the planted forepaws, pupils aimed at the vacancy.",
      **{**SIT_BASE, "hx": 145.0, "hy": 55.0, "hrot": 27.0, "yaw": 0.65,
         "gaze": (1.2, 2.5), "ear_near": 8.0, "ear_far": -12.0,
         "whisker_far": False, "whisker_near": False, "blush": False}),
    D("pet_reach_paw", "Validated edit vacancy: one long forepaw reaches forward and down while the other supports the same seat.",
      **{**SIT_BASE, "hx": 139.0, "hy": 48.0, "hrot": 18.0, "yaw": 0.7,
         "gaze": (1.2, 2.0), "eyes": "open", "mouth": "smile",
         "fl_near": leg(65.0, 78.0, 32.0, 28.0),
         "whisker_far": False, "whisker_near": False}),
    D("pet_withdraw_paw", "Replacement arrives: the reaching forepaw folds back under the chest; the supported seat stays put.",
      **{**SIT_BASE, "hx": 139.0, "hy": 48.0, "hrot": 18.0, "yaw": 0.7,
         "gaze": (1.2, 2.0), "eyes": "open", "mouth": "smile",
         "fl_near": leg(-20.0, 100.0, 17.0, 15.0),
         "whisker_far": False, "whisker_near": False}),
    D("pet_edge_perch", "Safe block edge: a compact supported seat, forepaws together at the lip and tail counterbalancing behind.",
      **{**SIT_BASE, "bx": 102.0, "by": 62.0, "brx": 26.0, "bry": 25.0,
         "brot": -4.0, "haunch_at": (83.0, 88.0, 26.0),
         "hx": 138.0, "hy": 41.0, "hrot": 6.0, "yaw": 0.6,
         "fl_root": (124.0, 77.0),
         "fl_near": leg(24.0, -24.0, 20.0, 18.0),
         "fl_far": leg(-7.0, 4.0, 20.0, 18.0),
         "tail_root": (67.0, 92.0), "tail": (-158.0, -120.0, -68.0, -12.0),
         "tail_len": 11.0, "gaze": (1.0, 1.0), "whisker_far": False}),
    D("pet_edge_lean", "Inspect below an edge: head and shoulder lean beyond the planted mitts, tail opposing the weight.",
      **{**SIT_BASE, "bx": 108.0, "by": 63.0, "brx": 26.0, "bry": 25.0,
         "brot": -15.0, "haunch_at": (83.0, 88.0, 26.0),
         "hx": 153.0, "hy": 57.0, "hrot": 24.0, "yaw": 0.8,
         "fl_root": (129.0, 77.0),
         "fl_near": leg(24.0, -24.0, 20.0, 18.0),
         "fl_far": leg(-7.0, 4.0, 20.0, 18.0),
         "tail_root": (67.0, 92.0), "tail": (-158.0, -120.0, -68.0, -12.0),
         "tail_len": 11.0, "gaze": (1.2, 2.5), "whisker_far": False,
         "whisker_near": False, "blush": False}),
]

# Reading a selected range moves only the head and pupils, so these derive from
# the edge perch; the art test pins their body paths to the perch's.
EDGE_PERCH = next(p for p in CONTACT if p.ident == "pet_edge_perch")
CONTACT += [
    replace(EDGE_PERCH, ident="pet_edge_look_up",
            note="Reading above: only the head lifts and pupils rise; the supported perch remains planted.",
            hrot=-20.0, gaze=(1.0, -2.5)),
    replace(EDGE_PERCH, ident="pet_edge_look_down",
            note="Reading below: only the head dips and pupils lower; the supported perch remains planted.",
            hrot=25.0, gaze=(1.0, 2.5), whisker_near=False),
]

CAT_POSES = [STAND] + WALK + RUN + SETTLED + SLEEP + REACTIVE + ADDRESS + FLIGHT + LOW + ROLL + LWALK + LOCO + IDLE + CONTACT

# The dog is the same pose sheet re-skinned by `replace`, so the two rosters stay
# frame-for-frame parallel: the brain is written against these idents and the
# species swaps the sprite underneath. The `pet_dog_` prefix makes the shared
# codegen (`gen_pet_glyphs`) mint `PetGlyphId::PetDogWalk0` beside `PetWalk0`.
DOG_POSES = [
    replace(p,
            ident=p.ident.replace("pet_", "pet_dog_", 1),
            species="dog",
            note=p.note + " (dog)")
    for p in CAT_POSES
]

POSES = CAT_POSES + DOG_POSES


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
        print(f"{'ok ' if ok else 'OOB'} {p.ident:20s} "
              f"({x0:6.1f},{y0:6.1f})-({x1:6.1f},{y1:6.1f}) "
              f"layers={text.count('[[layer]]'):2d} cmds={cmds:3d}")
    with open(os.path.join(out, "sheet.toml"), "w") as fh:
        fh.write(sheet(CAT_POSES, cols=5))
    with open(os.path.join(out, "dog_sheet.toml"), "w") as fh:
        fh.write(sheet(DOG_POSES, cols=5, ident="pet_dog_sheet"))
    print(f"{len(POSES)} poses ({len(CAT_POSES)} cat + {len(DOG_POSES)} dog), "
          f"{bad} out of box; sheets written")
