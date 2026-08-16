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
    # The two suspension frames of the gallop are the moments no paw is on the
    # floor. The brain adds no lift during a run (only a pounce arcs), so the
    # clearance has to live in the art or the gallop reads as a shuffle.
    D("pet_run_1", "Run, extension: the body stretches long, every paw off the floor.",
      yaw=1.0,
      airborne=7.0,
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
    # straight down the front edge. The head carries a HINT of the chibi
    # un-hunch — up and back over the chest rather than craned forward — at
    # the artist's scale (hx 134 not the original 138; the full chibi 126
    # over-corrected into a soldier's parade seat).
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
#
# The face is `happy` + `none`, NOT `closed` + `flat` (owner, from a ship-size
# screenshot: "the eyes still look open"). `closed` draws `_lid()`, which is
# fat by contract — a thin lid rasterizes to nothing at 1x — and a fat
# DOWNWARD arc at 24-36 px collapses into a solid dark bar tilted at the outer
# corner, i.e. exactly the silhouette of a narrowed, scowling OPEN eye. The
# `flat` mouth put a second dark bar under it and finished the glare.
# `_arc_eye()`'s upward ^ carries the same ink (so it survives the same
# rasteriser — purr and groom already prove it at 1x) in a shape no open eye
# has, and dropping the mouth lets the ^ ^ pair and the z's carry the read.
SLEEP_BASE = dict(
    curl=True, hide_legs=True, show_far_legs=False,
    bx=104.0, by=88.0, brx=46.0, bry=26.0,
    hx=150.0, hy=76.0, hr=27.0, hrot=20.0,
    eyes="happy", mouth="none", ear_flat=0.30,
    # Both whisker fans are spent here, and for the two reasons the rig
    # already names. The FAR fan lands entirely on the curled torso's dome —
    # the loaf case `whisker_far` was written for, strokes on coat reading as
    # scratches. The NEAR fan roots under an eye this pose has tipped down
    # toward them, and clotted eye + fan + outline into the black smear that
    # survived even after the lids became arcs.
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
      # The "oof" mouth (pet/update session's F6 brief, folded into the
      # roll-cycle regen): the full open blob smeared the muzzle black at
      # ship size.
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
         # fills the counter the reared hind leg's loop encloses — see
         # `Pose.belly`. Pre-registration coords.
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

# ── the direct-address pair: the cat finally looks AT you ─────────────────
ADDRESS = [
    # The bell: a compact seated silhouette whose base is about one head-width
    # across, the head overlapping the chest by a third — the classic
    # face-on cat, not a totem pole. Two neat mitts break the bottom edge and
    # the tail sweeps out beside them, tip flicked up and visible.
    D("pet_sit_front", "Sit, face-on: square to the viewer, tail swept round beside the mitts.",
      front=True, hide_hind=True, show_far_legs=False,
      bx=104.0, by=80.0, brx=23.0, bry=27.0, brot=0.0,
      haunch_at=(104.0, 86.0, 29.0),
      hx=104.0, hy=42.0,
      fl_root=(116.0, 78.0),
      fl_near=leg(0.0, 0.0, 20.0, 14.0), fl_far=leg(0.0, 0.0, 20.0, 14.0),
      tail_root=(121.0, 104.0),
      tail=(95.0, 100.0, 108.0, 125.0), tail_len=11.0, tail_thick=8.0),
    # The over-the-shoulder glance, reconstructed on body/head OPPOSITION
    # (adversarial review, finding 3): the read comes from the body facing
    # AWAY while the head comes BACK, not from the face alone. The hip mass
    # is a big ellipse rotated away (rump left-and-down), the chest rises to
    # a distinct shoulder up-right of it, and the head parks BEHIND and left
    # of that shoulder, overlapping it — so the near cheek crosses the
    # shoulder going forward while the torso's crest emerges behind the far
    # jaw going backward: one contour forward, one backward, which is the
    # essential twist cue. The yaw deepens to 0.8 — a genuine 3/4, no longer
    # the near-frontal half turn that read as "sitting sideways".
    D("pet_peek_shoulder", "Peek: seated facing away, head turned back over the shoulder.",
      hide_hind=True, haunch_at=(84.0, 88.0, 34.0, 27.0, -12.0),
      bx=112.0, by=60.0, brx=21.0, bry=25.0, brot=22.0,
      hx=116.0, hy=40.0, hrot=-6.0, yaw=0.8,
      # seated facing away: only ONE foreleg edge shows past the body — the
      # doubled pair reads as a notched two-toed mess from behind
      show_far_legs=False,
      # the foreleg drops from the SHOULDER, slightly toward the body's own
      # facing — not as a vertical tangent hanging off the cheek
      fl_root=(124.0, 74.0),
      fl_near=leg(-4.0, 3.0, 21.0, 16.0), fl_far=leg(8.0, -4.0, 21.0, 16.0),
      ear_near=10.0, ear_far=-8.0,
      # the head sits ON the torso's top, so the default topline bars would
      # paint across the cheek — they ride the haunch's crown instead, and the
      # off-side whiskers (shoulder coat behind them, not air) stay hidden
      bar_site="haunch", whisker_far=False,
      # from behind the hip, emerging BELOW the rump's curve and lying
      # forward along the ground — dropped clear of the silhouette so the
      # outline separates tail from rump, never extending the hip into one
      # ambiguous taper (the review's flipper warning)
      tail_root=(74.0, 112.0),
      tail=(-72.0, -88.0, -99.0, -107.0), tail_len=9.0, tail_thick=7.5),
]

# ── flight: the two halves of every arc the brain throws ──────────────────
# Unlike `pet_leap` (planted art, arced by the brain's lift, so the pounce
# touches down clean), these two are true mid-air frames — the rise serves
# flight u < 0.25, the descent u > 0.6, and both carry authored clearance the
# way the gallop's suspension frames do.
FLIGHT = [
    # The head sits ON the climbing chest — its circle overlaps the torso's
    # leading end by ~11 units, a connected neck, not a balloon on a string.
    # Forepaws fold tight under the chin; the hind pair still trails the
    # take-off drive.
    D("pet_leap_rise", "Flight, rise: hind legs still driving, forepaws tucked, nose up.",
      yaw=1.0,
      airborne=6.0,
      # the torso axis climbs at 30 degrees (review finding 4: at 22 the
      # centre of mass read as level flight with a lifted chin) — the chest
      # leads the diagonal and the head rides it, still overlapped
      by=62.0, brx=42.0, bry=20.0, brot=30.0, hx=141.0, hy=32.0, hrot=-12.0,
      ear_flat=0.25, eyes="wide", mouth="open",
      fl_near=leg(-30.0, 60.0, 12.0, 10.0), fl_far=leg(-38.0, 52.0, 12.0, 10.0),
      # swept a step further back than at the old 22-degree axis: from the
      # steeper hip they otherwise notch the belly line mid-torso
      hl_near=leg(-46.0, -56.0, 20.0, 16.0), hl_far=leg(-36.0, -48.0, 20.0, 16.0),
      tail=(-45.0, -56.0, -68.0, -80.0), tail_len=15.0),
    # The mirror half: head down-forward of the dropping chest (again
    # overlapped, again connected), forelegs at full reach for the floor,
    # rump and tail the highest things in the frame.
    D("pet_leap_descend", "Flight, descent: forelegs reaching for the floor, rump high.",
      yaw=1.0,
      airborne=7.0,
      # two more degrees of nose-down pitch and the pupils aimed at the
      # landing spot (review finding 5: the gaze read as aimed at the viewer
      # while the body fell — the eyes now look where the cat is going)
      by=58.0, brx=42.0, bry=20.0, brot=-20.0, hx=142.0, hy=61.0, hrot=14.0,
      ear_flat=0.35, eyes="wide", mouth="smile", gaze=(1.5, 2.0),
      fl_near=leg(28.0, 8.0, 20.0, 16.0), fl_far=leg(20.0, 2.0, 20.0, 16.0),
      hl_near=leg(-62.0, -80.0, 16.0, 12.0), hl_far=leg(-52.0, -70.0, 16.0, 12.0),
      # the far eye grown clear of the muzzle boundary so the descent stops
      # reading one-eyed at ship size — see `Pose.far_eye`
      far_eye=(-1.1, -2.4, 1.238, 1.143),
      tail=(-125.0, -140.0, -155.0, -170.0), tail_len=14.5),
]

# ── the wiggle and the loaf ────────────────────────────────────────────────
LOW = [
    # Alternates with pet_crouch at wiggle rate: the forepaws stay pinned on
    # the crouch's exact shoulder root so only the hindquarters shimmy — the
    # rump rises, shifts 4 units, and the tail flags up.
    # The rump raise comes from the TORSO's tilt (brot -8 vs the crouch's +4)
    # over hind legs folded forward under it — NOT from longer legs: the
    # registration plants the lowest ink, so legs one unit longer than the
    # pinned forepaws would hoist the whole cat and float its front off the
    # floor. Both pairs stay level; the body does the shimmy.
    D("pet_crouch_wiggle", "Pounce, wiggle: the crouch with its hindquarters up and offset.",
      yaw=1.0,
      # The anchored-front law (review finding 6): at 7 Hz the head, chest
      # and forepaws must hold within ~a unit of the crouch's while ONLY the
      # pelvis swings — a head that bobbed 6 units with the rump read as
      # whole-body jitter, not a loaded spring. The head now stays on the
      # crouch's mark (152,52) and the rump alone does the shimmy.
      by=76.0, brx=41.0, bry=21.0, brot=-8.0, bx=94.0, hy=52.5, hx=151.5,
      eyes="wide", ear_near=-8.0, ear_far=-6.0,
      fl_root=(114.7, 87.8),
      fl_near=leg(14.0, -20.0, 15.0, 12.0), fl_far=leg(6.0, -26.0, 15.0, 12.0),
      # shins a step shorter than the crouch's: the folded hind pair must
      # finish ABOVE the pinned forepaws' ink, or the registration re-plants
      # on the hind paw and the whole cat hops 0.7 units at wiggle rate
      hl_near=leg(40.0, -22.0, 16.0, 12.0), hl_far=leg(32.0, -28.0, 16.0, 12.0),
      # flagged high per the ratified spec, but eased off dead vertical —
      # a flagpole tail is a social signal, not a hunting one
      tail=(-134.0, -147.0, -157.0, -164.0), tail_len=13.5),
    # The long-dwell settle: awake, legs fully tucked, roughly a row tall so
    # the parked cat never shades the inked line above the prompt. One smooth
    # bread arc, zero visible limbs; the head RESTS on the dome's front end,
    # chin sunk into the crown and leading edge near-flush with the bread's
    # (not perched over it, not hung past it); the tail wraps the rump's
    # curve, tip visible past the left end; and the eyes are the serene happy
    # arcs — a squint here reads as regret, not rest.
    D("pet_loaf", "Loaf: settled flat, paws tucked, eyes soft, tail hugging the side.",
      curl=True, hide_legs=True, show_far_legs=False,
      # a bread loaf, not a seal: a touch shorter and taller than the sleep
      # dome (review finding 2 — the long flat ellipse read as a flipper
      # animal), still under the row-height the settle behavior counts on
      bx=101.0, by=87.0, brx=45.5, bry=27.5,
      # the head RESTS on the dome's front end — its leading edge flush with
      # the bread's, chin sunk into the crown, not hung out past the end into
      # air (140 left the whole right half of the face unsupported)
      hx=128.0, hy=60.0, hrot=0.0, ear_flat=0.12,
      eyes="happy", mouth="smile",
      # the off-side fan would rake black scratches across the dome behind
      # the cheek; a loafed cat's far whiskers are flat against the bread
      whisker_far=False,
      # exposure trimmed to a nub-and-tip (review finding 2: a long left
      # taper competed with the rump for the silhouette)
      tail_root=(68.0, 98.0),
      tail=(-62.0, -82.0, -100.0, -114.0), tail_len=8.5, tail_thick=7.5),
]

# ── the roll: belly-up wriggling, the bored cat's third act ────────────────
# The whole trick is `brot` near 180: the torso blob's chest-arch rotates to
# the floor and the tucked belly line faces the sky, the hip/shoulder roots
# ride to the TOP of the mass (rot_about), and the tabby bars land on the
# grounded back — all for free from the rig's own math. Loose folded legs
# paw at the air; the head lies on the floor beside the body, cheek down.
# The brain cycles 0 → 1 → 2 → 1 at wiggle rate, flip_x giving the
# left-right of the squirm.
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

CAT_POSES = [STAND] + WALK + RUN + SETTLED + SLEEP + REACTIVE + ADDRESS + FLIGHT + LOW + ROLL

# THE DOG IS THE SAME POSE SHEET, RE-SKINNED. Owner, 2026-08-11: "make a dog
# like the walking cat … you can use the same code." Taking that literally is
# also the right engineering: a dog roster authored independently would drift
# out of gait sync with the cat's, and every behaviour in `kitty_pet` — the
# walk cycle's four beats, the settle ladder, the leap's three frames — is
# written against THESE pose idents. Deriving the dog by `replace` guarantees
# the two rosters stay frame-for-frame parallel forever, so the brain can pick
# a cat pose and the species swaps the sprite underneath it.
#
# The ident prefix is `pet_dog_` so the shared codegen (`gen_pet_glyphs` reads
# every TOML in this directory) mints `PetGlyphId::PetDogWalk0` beside
# `PetWalk0` — one roster, one baker, one drift test.
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
