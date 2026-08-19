// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE PET SIMULATION — the half of the pets module `effect_bakers` never
// reaches. That bench prices the shared atlas from OUTSIDE (its mote arm uses
// `kitty_sing::bake_note` as a stand-in because the pet's own mote rasterizer
// is private); nothing anywhere timed `PetBrain::tick`, `WordDecorations::
// pet_cursor`, or `RobiShow::frame` — the three per-frame entry points the
// pets.json audit put findings behind. This file is their instrument:
//
//   PET-01  `pet_cursor`'s mote lane re-rasterizes every visible mote per
//           frame (`bake_pet_mote`, ~14 heap allocs + 2 scanline fills for a
//           Note) and `CatBaker::host_tile` discards the pixels on the atlas
//           hit. The previous fix was refused because its A/B never executed
//           the changed code. The `pet_cursor_draw` group drives `pet_cursor`
//           ITSELF, with live motes, in the proven steady state where every
//           `host_tile` call is a hit — so every eagerly-baked byte is
//           discarded, which is exactly the waste the finding names.
//   PET-02  `robi::pick_tip` builds a per-frame `Vec` over the 27-entry const
//           tip bank. The `robi_frame` group holds `RobiShow::frame` inside /
//           outside the tip window OF THE SAME jacks stage, so the A/B delta
//           IS the tip resolver and nothing else.
//   PET-03  `PetBrain::sense_ink` copies the whole per-row ink map into the
//           brain every frame, including frames where the brain provably never
//           reads it. `pet_brain_host_seams/sense_ink_50row_map` prices that
//           copy alone, on a RETIRED brain (the state where it is pure waste).
//           Only the in-crate half: the `aterm-gui` hoist (gating the call
//           behind `pet_visible`) and the producer scan (`WordDecorations::
//           pet_ink`, fed by the deco's own row walk) are out of this bench's
//           reach — see the module report.
//   PET-04  `PetBrain::tick`'s no-caret arm re-clears ~40 already-zero fields
//           and re-resolves a whole frame for a fully RETIRED pet, forever.
//           `pet_brain_tick/retired_no_caret` is 10k+ ticks past the fade's
//           zero, guarded from BOTH sides (retired emits fingerprint 0 on
//           every frame AND a control differing only in "caret present" must
//           light up — a one-sided "it was cheap" would pass on a dead rig).
//
// WHAT IS TIMED, EXACTLY — the `cursor_glow_tick` discipline verbatim. One
// iteration of a timed workload is:
//
//     arm(rig);                    // UNTIMED host half: clock += dt, script
//                                  //   caret move, `sense_ink` feed (the host
//                                  //   seam PET-03 prices separately)
//     let t0 = Instant::now();     // ── timed span opens
//     black_box(rig.tick());       //   the engine, and only the engine
//     total += t0.elapsed();       // ── timed span closes
//
// so the state under test is SUSTAINED across the whole measurement while the
// host's own cost stays out of the engine's number. The price is one
// `Instant::now()` pair inside every reported number, measured by
// `pet_brain_tick/timer_floor` (an empty timed span): subtract it for an
// absolute cost, ignore it for an A/B. The retired-pet frame the host really
// pays is therefore `retired_no_caret + sense_ink_50row_map - timer_floor`.
//
// THE CLOCK IS INJECTED. `PetSense.now` advances by a fixed dt from ONE wall
// sample per fixture; `tick` derives `elapsed`/`dt` from `now - last_now`, so
// a frozen clock would measure an engine doing nothing (no fades, no chase,
// no mote lives — the exact trap `tests/cursor_bench.rs` fell into). Every
// state below is reached by DRIVING UNTIL the public predicates say so
// (`action()`, `is_active()`, `needs_frames()`, `PetFrame::fp()`), never by
// naming the private timing consts (`SLEEP_AFTER`, `ZEE_EVERY`, …) — which
// doubles as the reach guard. The brain has no RNG at all (mote scatter is a
// serial, not a die roll), so every workload is bit-reproducible and the
// bounds below are exact counts or modest margins, not envelopes.
//
// THE REPLAY-CAPTURE DESIGN (the `pet_cursor_draw` group). The mote states
// PET-01 lands on are FINITE in simulation time — light-sleep z's spawn only
// through a bounded window after falling asleep, landing dust lives ~half a
// second — while a criterion measurement runs as long as it likes. Driving
// the brain live under the timed loop would drift out of the claimed state
// mid-measurement and time an empty lane under a workload named "motes". So
// the fixtures CAPTURE real `PetFrame`s from a genuine brain drive (the exact
// frames a host would hand `pet_cursor`) and replay them in a loop: the state
// is sustained indefinitely, and it is still entirely the brain's own output
// — no hand-built frames. The per-frame host prologue (`begin_host_frame`,
// `free.clear()`, the replay index) is armed OUTSIDE the timed span; the
// timed span is `pet_cursor` alone.
//
// THE PET-01 HIT WITNESS. `pet_cursor` returns a fingerprint that folds
// `cat_baker.version()` and `pet_baker.version()` — and a version is bumped
// by exactly a BAKE. After a two-loop warm-up the fixture runs the verify
// loop TWICE and asserts the two fingerprint sequences are IDENTICAL: any
// bake in either pass would bump a version and split the sequences. Equal
// sequences prove the timed steady state resolves every mote tile as an
// atlas HIT — i.e. every per-frame `bake_pet_mote` in the timed run is work
// whose output is discarded, the precise state PET-01's fix must relieve.
// (The distinct-tile count is also asserted well under the 32-slot atlas, so
// "all hits" is structurally possible and not luck; the 12-bucket rotation
// law is mirrored here for that count the way cursor_glow mirrors MAX_HALOS.)
//
// EVERY GUARD IS TWO-SIDED. Dark workloads (retired brain, alpha-0 draw,
// unborn Robi) must emit exactly nothing AND their paired control — the same
// script with only the off-switch removed — must light up. Lit workloads
// assert exact frame counts, action mixes, mote totals and pose families from
// BOTH sides. The counts are handed to criterion as measurements (1 ns == 1
// item, the `cursor_glow_volume` idiom) in `pet_brain_volume`, so a COUNT
// regression is baselined and A/B'd by the same tooling as a time regression.
//
// WHAT EACH WORKLOAD WAS CONFIRMED TO REACH is asserted by its guard before a
// nanosecond is timed; the numbers in the guards below are the measured
// values of this deterministic script (see each site's comment).
//
// WHAT THE FIRST FULL RUN MEASURED (m21, 2026-08-19; --warm-up-time 1
// --measurement-time 2), recorded so the A/Bs these numbers exist for can be
// read without re-deriving them. timer_floor 13.1 ns sits inside every
// `iter_custom` number below (it cancels in an A/B).
//
//   pet_brain_tick   retired_no_caret 34.8 ns (~21.7 ns engine) — PET-04's
//                    whole per-frame price for a pet the user never enabled;
//                    sleeping_resident 40.4 ns, so the retired frame costs
//                    ~80% of a VISIBLE deep sleeper's tick, which is the
//                    finding in one ratio. typing_chase 37.0 ns,
//                    screen_bound_flights 45.0 ns, ink_dense_eviction 43.6 ns.
//   host seams       sense_ink_50row_map 3.9 ns (plain `iter`, no timer
//                    pair) — PET-03's in-crate copy. Small next to PET-04's
//                    21.7 ns; the audit's "low impact" confirmed in-crate.
//   pet_cursor_draw  alpha0_skip 15.6 ns; body_only_deep_sleep 45.0 ns;
//                    zee_lane_light_sleep 1.407 µs at avg 1.60 live z's
//                    (≈ 850 ns per mote per frame of discarded rasterization);
//                    dust_burst_landing 1.53 µs at avg 2.63 dust
//                    (≈ 565 ns per mote — Dust's 3 discs vs the z's
//                    rot_poly+fill_path, so the slope is kind-dependent).
//                    The mote lane multiplies the hit-frame body cost ~30x:
//                    THAT is the number PET-01's fix must move, and the
//                    all-hits witness proves every one of those nanoseconds
//                    produced bytes `host_tile` threw away.
//   robi_frame       unborn 15.0 ns; rest_calm 18.4 ns; jacks_no_tip
//                    17.7 ns; jacks_tip_window 36.1 ns — the SAME stage with
//                    only the tip resolver added, so PET-02's per-frame Vec
//                    build is the 18.4 ns delta (a 2x on the whole frame);
//                    full_cycle_sweep 23.7 ns is the cycle-weighted average.

use std::collections::HashSet;
use std::time::Duration;
use std::time::Instant as WallInstant;

use aterm_core::render::FreeSprite;
use aterm_effects::cat_baker::CatColorKey;
use aterm_effects::kitty_pet::{
    PetAction, PetBrain, PetFrame, PetMoteKind, PetSense, PetSpecies,
};
use aterm_effects::robi::{CYCLE_MS, MAX_HANDHOLDS, RobiFrame, RobiSense, RobiShow, WANDER_END};
use aterm_effects::robi_glyphs_gen::RobiGlyphId;
use aterm_effects::word_decorations::{EffectGeom, PetCursorFrame, WordDecorations};
use criterion::measurement::WallTime;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, black_box, criterion_group, criterion_main,
};
use web_time::Instant;

// ---------------------------------------------------------------- geometry --

/// A maximized 1x window: the grid the audit's bench design names, and the
/// shape that makes the ink map (PET-03's payload) its realistic 50 rows.
const ROWS: u16 = 50;
const COLS: u16 = 200;
const CW: u16 = 10;
const CH: u16 = 20;

const GEOM: EffectGeom = EffectGeom {
    cell_w: CW,
    cell_h: CH,
    rows: ROWS,
    cols: COLS,
};

// ------------------------------------------------------------------ clocks --

/// True 60 fps, the cadence both host render paths tick the brain at. The
/// odd 16_667 µs (rather than a round 16 ms) is deliberate: it is the exact
/// dt the in-crate fixtures use, and nothing below depends on ms-round
/// arithmetic — the brain's clocks are all seconds-typed floats.
const PET_DT: Duration = Duration::from_micros(16_667);

/// Robi's frame dt, in whole milliseconds ON PURPOSE: `RobiShow::frame` is a
/// pure function of `now - since` in ms, and the phase-pinned workloads below
/// wrap the phase with integer-ms modular arithmetic, which must be exact.
const ROBI_DT_MS: u64 = 16;

// ------------------------------------------------------------------ script --

/// Frames of script before anything is sampled or timed — long enough for the
/// slowest build below (the sleep ladder tops out ~600 frames past its drive).
const WARM_FRAMES: usize = 1_200;

/// Frames sampled by every verify pass (the Robi full-cycle sweep excepted,
/// which samples exactly one whole 76 s cycle).
const SAMPLE_FRAMES: usize = 600;

/// Frames between caret teleports in the screen-bound workload: ~2 s at
/// 60 fps, room for the whole Perk → Crouch → Leap → touch-land → second
/// bound → landing-dust choreography to play out before the next launch.
const BOUND_PERIOD: u64 = 120;

/// Frames between one-column caret wiggles in the ink workload: 5 s, long
/// enough for the pet to settle fully (the state whose per-frame ink checks
/// the workload prices), short enough that the quiet clock never reaches the
/// sleep threshold — asserted from outside by `sleep == 0` in the guard.
const INK_WIGGLE: u64 = 300;

/// The typing chase's ping-pong span and cadence: the caret advances one
/// column every `CHASE_STRIDE` ticks (~15 cells/s, hard human typing), a
/// reversal every ~11 s. A ping-pong rather than a wrap because a 199-column
/// wrap is a screen-crossing JUMP (the bound workload's job); and NOT one
/// column per tick, measured: at 60 cells/s the caret outruns the follower
/// so far that the window degenerates into permanent re-anchor hops
/// (airborne 142 / moving 24 of 600) — a flight workload wearing a typing
/// name. At this cadence the cat is on its feet.
const CHASE_LO: u64 = 20;
const CHASE_HI: u64 = 180;
const CHASE_STRIDE: u64 = 4;

// ------------------------------------------------------------------- color --

/// The documented default dark palette (cursor_glow_tick's pair). The color
/// key is context for tint resolution only; it is quantized once here and
/// held constant so the timed span never re-derives it (the host derives it
/// from cell context it already holds).
const DARK_FG: u32 = 0x00C8_D3F5;
const DARK_BG: u32 = 0x001A_1B26;

fn dark_colors() -> CatColorKey {
    CatColorKey::from_rgb(DARK_BG, DARK_FG, DARK_BG)
}

// ---------------------------------------------------------------- ink maps --

/// The hostile map: every row fully inked, the live edge at the bottom —
/// what a full screen of output looks like to `pet_ink`.
fn full_ink() -> Vec<(u16, u16)> {
    vec![(0, COLS); usize::from(ROWS)]
}

/// The blank map: `(0, 0)` per row means "no ink", every ink rule inert.
fn blank_ink() -> Vec<(u16, u16)> {
    vec![(0, 0); usize::from(ROWS)]
}

// --------------------------------------------------------------- brain rig --

/// One brain workload's world: the brain, the INJECTED clock, and the
/// script's caret + ink state. Built once, driven to its claimed state,
/// verified, then timed — the state is sustained by the same arm throughout.
struct BrainRig {
    brain: PetBrain,
    /// Advanced by exactly `PET_DT` per frame; never re-sampled from the wall.
    now: Instant,
    n: u64,
    caret: Option<(u16, u16)>,
    ink: Vec<(u16, u16)>,
    ink_live: Option<u16>,
}

impl BrainRig {
    fn new(ink: Vec<(u16, u16)>, ink_live: Option<u16>) -> Self {
        let mut brain = PetBrain::default();
        brain.set_species(PetSpecies::Cat);
        BrainRig {
            brain,
            // ONE wall sample, for the clock's origin only.
            now: Instant::now(),
            n: 0,
            caret: None,
            ink,
            ink_live,
        }
    }

    /// The host half of one frame: advance the injected clock and feed the
    /// ink map exactly where the host does (immediately before `tick`).
    /// UNTIMED — the copy itself is PET-03's own benchmark, priced under
    /// `pet_brain_host_seams/sense_ink_50row_map`.
    fn feed(&mut self) {
        self.now += PET_DT;
        self.n += 1;
        self.brain.sense_ink(0, &self.ink, self.ink_live);
    }

    /// THE TIMED UNIT: `PetBrain::tick` and nothing else. (Building the
    /// `PetSense` value is nine scalar stores — it is the call's argument,
    /// not host work, so it stays inside.)
    fn tick(&mut self) -> PetFrame {
        self.brain.tick(PetSense {
            now: self.now,
            caret: black_box(self.caret),
            rows: ROWS,
            cols: COLS,
            cell_w: CW,
            cell_h: CH,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }
}

/// The host-side half of one frame, per script. What `step`/`run_brain`
/// drive and what the timed loops arm OUTSIDE the measured span.
type BArm = fn(&mut BrainRig);

fn step(r: &mut BrainRig, arm: BArm) -> PetFrame {
    arm(r);
    r.tick()
}

/// Caret hidden — the retired pet's forever-state.
fn arm_hidden(r: &mut BrainRig) {
    r.caret = None;
    r.feed();
}

/// Caret present and perfectly still: the settle ladder's driver (stand →
/// sit → sleep → deep sleep), and the retired workload's paired control.
fn arm_still(r: &mut BrainRig) {
    r.feed();
    r.caret = Some((25, 100));
}

/// The typing chase: one column per tick, ping-ponging so no single move
/// ever reads as a jump.
fn arm_chase(r: &mut BrainRig) {
    r.feed();
    let leg = CHASE_HI - CHASE_LO;
    let k = (r.n / CHASE_STRIDE) % (2 * leg);
    let col = if k < leg { CHASE_LO + k } else { CHASE_HI - (k - leg) };
    r.caret = Some((25, col as u16));
}

/// A 100-column teleport every `BOUND_PERIOD` frames — over the engine's
/// screen-crossing bar (40% of 200 cols), so each one launches the
/// two-bound flight choreography and lands in dust.
fn arm_bound(r: &mut BrainRig) {
    r.feed();
    let col = if (r.n / BOUND_PERIOD).is_multiple_of(2) {
        30
    } else {
        130
    };
    r.caret = Some((25, col));
}

/// Caret parked mid-line on a FULLY INKED screen, wiggled one column every
/// `INK_WIGGLE` frames so the quiet clock never reaches sleep: the settled
/// pet overlaps ink everywhere it stands, and the per-frame ink rules
/// (`ink_overlaps` → `ink_safe_col` → the `evict_toward` glide) run forever.
fn arm_ink_park(r: &mut BrainRig) {
    r.feed();
    let col = if (r.n / INK_WIGGLE).is_multiple_of(2) {
        100
    } else {
        101
    };
    r.caret = Some((25, col));
}

// ------------------------------------------------------- brain observation --

/// Live motes on a frame, as `pet_cursor` will see them: alpha-0 slots are
/// dropped at its door, so only alpha > 0 counts.
fn live_motes(f: &PetFrame) -> usize {
    f.motes.iter().flatten().filter(|m| m.alpha > 0).count()
}

/// What running a brain script for a while showed — everything the guards
/// bound, all observed through public seams (`fp()`, `needs_frames()`,
/// the emitted frame's own fields).
#[derive(Default)]
struct BrainSampled {
    frames: usize,
    /// Frames whose fingerprint was non-zero (something on glass).
    lit: usize,
    /// Frames after which `needs_frames()` still asked for the 60 fps lane.
    needs: usize,
    sleep: usize,
    /// Walk | Run frames — the gait/follower path.
    moving: usize,
    /// Leap frames — airborne.
    airborne: usize,
    mote_frames: usize,
    mote_total: usize,
    /// Frames carrying at least one live Dust mote (the landing puffs).
    dust_frames: usize,
    fp_first: u64,
    /// Whether every sampled fingerprint equaled the first — the "byte-stable
    /// settle" witness for the deep sleeper, and its refutation for the
    /// animating workloads.
    fp_constant: bool,
    /// The pet's emitted column per frame — the ink workload's divergence
    /// witness against its blank-map twin.
    cols: Vec<f32>,
}

fn run_brain(r: &mut BrainRig, arm: BArm, frames: usize) -> BrainSampled {
    let mut s = BrainSampled {
        fp_constant: true,
        ..BrainSampled::default()
    };
    for i in 0..frames {
        let f = step(r, arm);
        let fp = f.fp();
        if i == 0 {
            s.fp_first = fp;
        } else if fp != s.fp_first {
            s.fp_constant = false;
        }
        s.frames += 1;
        s.lit += usize::from(fp != 0);
        s.needs += usize::from(r.brain.needs_frames());
        match f.action {
            PetAction::Sleep => s.sleep += 1,
            PetAction::Walk | PetAction::Run => s.moving += 1,
            PetAction::Leap => s.airborne += 1,
            _ => {}
        }
        let m = live_motes(&f);
        s.mote_frames += usize::from(m > 0);
        s.mote_total += m;
        s.dust_frames += usize::from(
            f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Dust && m.alpha > 0),
        );
        s.cols.push(f.col);
    }
    s
}

fn report_brain(name: &str, note: &str, s: &BrainSampled) {
    println!(
        "VOLUME {name:<26} | lit {}/{} needs {} sleep {} moving {} airborne {} | \
         mote frames {} total {} dust {} | fp_const {} | {note}",
        s.lit,
        s.frames,
        s.needs,
        s.sleep,
        s.moving,
        s.airborne,
        s.mote_frames,
        s.mote_total,
        s.dust_frames,
        s.fp_constant,
    );
}

// ------------------------------------------------------------ brain builds --

/// PET-04's state: the pet was genuinely ON GLASS, the caret then hid, and
/// the fade ran all the way to zero. Driven, not assumed: the loop exits only
/// when every public retirement predicate agrees, and panics if the engine
/// never gets there.
fn build_retired() -> BrainRig {
    let mut r = BrainRig::new(full_ink(), Some(ROWS - 1));
    // Show the pet: 30 frames (~0.5 s) with a still caret ramps the fade in.
    // Short on purpose — the default brain wakes asleep with its quiet clock
    // at the sleep threshold, and half a second is under the first z's beat,
    // so the hide below starts from a clean, moteless sleeper.
    for _ in 0..30 {
        step(&mut r, arm_still);
    }
    assert!(
        r.brain.is_active(),
        "retired build: the pet never appeared, so \"after the fade\" would \
         time a brain that was never alive"
    );
    let mut k = 0usize;
    loop {
        let f = step(&mut r, arm_hidden);
        let retired = !r.brain.is_active()
            && f.fp() == 0
            && !r.brain.needs_frames()
            && f.motes.iter().all(Option::is_none)
            && f.departures.iter().all(Option::is_none);
        if retired {
            break;
        }
        k += 1;
        assert!(k < 5_000, "retired build: the fade never reached zero");
    }
    r
}

/// The ON floor: a fully settled deep sleeper — visible, byte-stable, not
/// asking for frames. Driven until the public predicates say so.
fn build_sleeper() -> BrainRig {
    let mut r = BrainRig::new(blank_ink(), None);
    let mut k = 0usize;
    loop {
        let f = step(&mut r, arm_still);
        let deep = r.brain.is_active()
            && r.brain.action() == PetAction::Sleep
            && !r.brain.needs_frames()
            && live_motes(&f) == 0;
        if deep {
            break;
        }
        k += 1;
        assert!(k < 5_000, "sleeper build: never reached still deep sleep");
    }
    r
}

fn build_warmed(ink: Vec<(u16, u16)>, live: Option<u16>, arm: BArm) -> BrainRig {
    let mut r = BrainRig::new(ink, live);
    for _ in 0..WARM_FRAMES {
        step(&mut r, arm);
    }
    r
}

// ----------------------------------------------------------- brain verify --
//
// Every count below is a MEASURED value of this deterministic script (no RNG
// anywhere in the brain; the clock is injected), bounded exactly where the
// count is structural and with a modest margin where a timing-const tweak
// could legitimately shift it a little. Both sides are load-bearing.

/// Returns what was sampled plus the CONTROL's lit-frame count (recorded in
/// the volume group so the dark/lit pair stays visible in baselines).
fn verify_retired(r: &mut BrainRig) -> (BrainSampled, usize) {
    let s = run_brain(r, arm_hidden, SAMPLE_FRAMES);
    report_brain("retired_no_caret", "PET-04: fade at zero, caret hidden", &s);
    assert_eq!(
        s.lit, 0,
        "retired_no_caret: a non-zero fingerprint on a retired pet — the \
         workload is not in the state PET-04 is about"
    );
    assert_eq!(
        s.needs, 0,
        "retired_no_caret: a retired pet asked for the 60 fps lane"
    );
    assert_eq!(s.mote_total, 0, "retired_no_caret: motes on a retired pet");
    // THE OTHER SIDE: the identical cadence with only the caret restored must
    // light up, or this zero proves nothing (a dead rig is also dark).
    let mut c = BrainRig::new(full_ink(), Some(ROWS - 1));
    let cs = run_brain(&mut c, arm_still, SAMPLE_FRAMES);
    report_brain(
        "retired_no_caret.control",
        "same cadence, caret present",
        &cs,
    );
    assert!(
        cs.lit >= SAMPLE_FRAMES - 5,
        "retired_no_caret: the caret-present control drew {}/{} frames — the \
         dark workload's zero is not a measurement",
        cs.lit,
        cs.frames
    );
    let control_lit = cs.lit;
    (s, control_lit)
}

fn verify_sleeper(r: &mut BrainRig) -> BrainSampled {
    let s = run_brain(r, arm_still, SAMPLE_FRAMES);
    report_brain("sleeping_resident", "deep sleep: visible, byte-stable", &s);
    assert_eq!(s.lit, SAMPLE_FRAMES, "sleeping_resident: pet not visible");
    assert_eq!(
        s.sleep, SAMPLE_FRAMES,
        "sleeping_resident: not asleep the whole window"
    );
    assert_eq!(
        s.needs, 0,
        "sleeping_resident: a still sleeper asked for frames — this workload \
         would be measuring an animation, not the idle floor"
    );
    assert!(
        s.fp_constant,
        "sleeping_resident: the fingerprint moved — the sleeper has not \
         actually settled (breath or motes still animating)"
    );
    assert_eq!(s.mote_total, 0, "sleeping_resident: motes in deep sleep");
    s
}

fn verify_chase(r: &mut BrainRig) -> BrainSampled {
    let s = run_brain(r, arm_chase, SAMPLE_FRAMES);
    report_brain("typing_chase", "1 col / 4 ticks ping-pong, blank ink", &s);
    assert_eq!(s.lit, SAMPLE_FRAMES, "typing_chase: pet not visible");
    assert_eq!(s.sleep, 0, "typing_chase: the chase fell asleep");
    assert!(
        s.needs >= SAMPLE_FRAMES - 5,
        "typing_chase: needs_frames on only {}/{} frames — nothing is moving",
        s.needs,
        s.frames
    );
    assert!(
        !s.fp_constant,
        "typing_chase: a constant fingerprint on a chase — the follower is \
         not running"
    );
    // The gait mix, measured: 283 Walk/Run + 123 Leap of 600 (the follower
    // is on its feet with pounces closing the reversals; the 35 mote frames
    // are its scramble puffs). Both sides: a chase that stopped walking OR
    // one that degenerated into permanent flight is a different workload
    // than the one this name claims — the 1-col-per-TICK version of this
    // script measured airborne 142 / moving 24, which is why the cadence is
    // a quarter of that.
    assert!(
        (340..=470).contains(&(s.moving + s.airborne)),
        "typing_chase: moving+airborne = {} of {} — outside the measured \
         gait band (406)",
        s.moving + s.airborne,
        s.frames
    );
    s
}

fn verify_bound(r: &mut BrainRig) -> BrainSampled {
    let s = run_brain(r, arm_bound, SAMPLE_FRAMES);
    report_brain("screen_bound_flights", "100-col teleport / 2 s", &s);
    assert_eq!(s.lit, SAMPLE_FRAMES, "screen_bound_flights: pet not visible");
    // Measured: 180 airborne and 70 dust frames per 600 (5 teleports) —
    // 36 flight + 14 dust frames per bound, every bound.
    assert!(
        (150..=210).contains(&s.airborne),
        "screen_bound_flights: {} airborne frames of {} — outside the \
         measured band (180): the teleports are not launching the flight \
         choreography they did",
        s.airborne,
        s.frames
    );
    assert!(
        (55..=85).contains(&s.dust_frames),
        "screen_bound_flights: {} dust frames of {} — outside the measured \
         band (70): the landings are not kicking the dust they did \
         (spawn_dust unreached or the lane's life changed)",
        s.dust_frames,
        s.frames
    );
    s
}

/// The ink workload's witness is the same script over a BLANK map, and what
/// separates the two is not where the pet stands — a TOTALLY inked screen
/// has no safe ground, so the safe-station ladder falls back to the plain
/// station and both rigs converge on the same spot (measured: only 39/600
/// frames differ, transiently, mid-glide) — but HOW it gets there. On blank
/// ground the one-column wiggle is answered by the chase (Walk frames: 52 of
/// 600, measured); on full ink the settled-overlap eviction block owns the
/// motion instead and `evict_toward` GLIDES the column without ever leaving
/// the settled pose (Walk frames: exactly 0). A zero that can only be
/// produced by the eviction lane running, plus a divergence band for the
/// glide's transient — both sides of both counts asserted.
fn verify_ink(r: &mut BrainRig) -> (BrainSampled, usize) {
    let s = run_brain(r, arm_ink_park, SAMPLE_FRAMES);
    report_brain("ink_dense_eviction", "every row inked, caret parked", &s);
    assert_eq!(s.lit, SAMPLE_FRAMES, "ink_dense_eviction: pet not visible");
    assert_eq!(
        s.sleep, 0,
        "ink_dense_eviction: the parked pet fell asleep — the wiggle cadence \
         no longer holds the settle window open"
    );
    let mut twin = build_warmed(blank_ink(), None, arm_ink_park);
    let ts = run_brain(&mut twin, arm_ink_park, SAMPLE_FRAMES);
    report_brain("ink_dense_eviction.control", "same script, blank map", &ts);
    assert_eq!(ts.lit, SAMPLE_FRAMES, "ink control: pet not visible");
    assert_eq!(
        s.moving, 0,
        "ink_dense_eviction: the inked pet WALKED — the eviction glide no \
         longer owns the wiggle response, so the settled-overlap ink lane is \
         not the code this workload reaches"
    );
    assert!(
        (30..=100).contains(&ts.moving),
        "ink_dense_eviction: the blank-map twin walked {} frames of {} — \
         outside the measured band (52), so the two rigs no longer differ in \
         only the ink map's effect",
        ts.moving,
        ts.frames
    );
    let divergent = s
        .cols
        .iter()
        .zip(ts.cols.iter())
        .filter(|(a, b)| (**a - **b).abs() > 0.5)
        .count();
    println!(
        "VOLUME ink_dense_eviction.divergence | {divergent}/{} frames stand \
         >0.5 cols apart from the blank-map twin",
        s.frames
    );
    assert!(
        (20..=80).contains(&divergent),
        "ink_dense_eviction: {divergent}/{} transiently divergent frames, \
         outside the measured band (39) — the glide/walk split has moved",
        s.frames
    );
    (s, divergent)
}

// ------------------------------------------------------------ draw fixture --
//
// PET-01's rig: real captured `PetFrame`s replayed through a resident
// `WordDecorations` (see the header's replay-capture rationale).

struct DrawRig {
    decos: WordDecorations,
    frames: Vec<PetFrame>,
    idx: usize,
    free: Vec<FreeSprite>,
    colors: CatColorKey,
    coat: u8,
    iris: u8,
}

impl DrawRig {
    fn new(frames: Vec<PetFrame>, look: (u8, u8)) -> Self {
        assert!(!frames.is_empty(), "draw rig: empty capture");
        DrawRig {
            decos: WordDecorations::default(),
            frames,
            idx: 0,
            free: Vec::new(),
            colors: dark_colors(),
            coat: look.0,
            iris: look.1,
        }
    }

    /// The host half of one presented frame: open the host frame (which is
    /// what re-arms the shared 2-bakes budget and the LRU clock), clear the
    /// sprite sink, advance the replay. UNTIMED.
    fn arm(&mut self) {
        self.idx = (self.idx + 1) % self.frames.len();
        self.decos.begin_host_frame();
        self.free.clear();
    }

    /// THE TIMED UNIT: `pet_cursor` and nothing else.
    fn draw(&mut self) -> Option<u64> {
        let pet = self.frames[self.idx];
        self.decos.pet_cursor(
            PetCursorFrame {
                geom: GEOM,
                colors: self.colors,
                coat: self.coat,
                iris: self.iris,
                pet: black_box(pet),
            },
            &mut self.free,
        )
    }
}

/// MIRRORS `WordDecorations::PET_MOTE_ROT_BUCKETS` (12) and its quantization,
/// the way cursor_glow_tick mirrors MAX_HALOS: used ONLY to bound the number
/// of distinct mote tiles a replay loop can demand, which is what makes the
/// all-hits steady state structurally possible rather than lucky. If the law
/// drifts, the fingerprint-equality witness (the primary guard) still decides.
fn mote_bucket(rot: f32) -> u32 {
    let turn = rot.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
    ((turn * 12.0) as u32) % 12
}

fn mote_kind_tag(kind: PetMoteKind) -> u8 {
    match kind {
        PetMoteKind::Dust => 1,
        PetMoteKind::Zee | PetMoteKind::ZeePop => 2,
        PetMoteKind::Note => 3,
        PetMoteKind::Heart => 4,
    }
}

struct DrawSampled {
    frames: usize,
    /// Total sprites pushed over the window (body + motes).
    sprites: usize,
    /// Total mote sprites over the window.
    motes: usize,
    /// Frames carrying at least one mote sprite.
    mote_frames: usize,
    /// The per-frame fingerprint sequence — the bake witness (see header).
    fp_seq: Vec<Option<u64>>,
    /// Distinct (kind, rotation-bucket) tile identities the loop demands.
    distinct_tiles: usize,
}

/// One full replay loop, with the per-frame structural asserts inline: the
/// body sprite always lands and NO mote is dropped to the bake budget — which
/// can only hold if the steady state is cache-hitting.
fn run_draw(rig: &mut DrawRig, expect_body: bool) -> DrawSampled {
    let n = rig.frames.len();
    let mut s = DrawSampled {
        frames: n,
        sprites: 0,
        motes: 0,
        mote_frames: 0,
        fp_seq: Vec::with_capacity(n),
        distinct_tiles: 0,
    };
    let mut tiles: HashSet<(u8, u32)> = HashSet::new();
    for _ in 0..n {
        rig.arm();
        let fp = rig.draw();
        let f = &rig.frames[rig.idx];
        let m = live_motes(f);
        for mote in f.motes.iter().flatten().filter(|m| m.alpha > 0) {
            tiles.insert((mote_kind_tag(mote.kind), mote_bucket(mote.rot)));
        }
        let want = m + usize::from(expect_body);
        assert_eq!(
            rig.free.len(),
            want,
            "pet_cursor replay: emitted {} sprites for a frame carrying {m} \
             live motes (body: {expect_body}) — a mote was dropped to the \
             bake budget or the body tile failed to resolve, so the loop is \
             NOT in the warm cache-hit steady state PET-01 is about",
            rig.free.len(),
        );
        s.sprites += rig.free.len();
        s.motes += m;
        s.mote_frames += usize::from(m > 0);
        s.fp_seq.push(fp);
    }
    s.distinct_tiles = tiles.len();
    s
}

fn report_draw(name: &str, note: &str, s: &DrawSampled) {
    println!(
        "VOLUME {name:<26} | sprites {} motes {} over {} frames | mote frames \
         {} | distinct mote tiles {} | {note}",
        s.sprites, s.motes, s.frames, s.mote_frames, s.distinct_tiles,
    );
}

/// Warm two full loops (every tile the loop can demand gets baked), then run
/// the verify loop twice and demand IDENTICAL fingerprint sequences — the
/// zero-bakes witness (see header). `motes` bounds the loop's TOTAL mote
/// sprites and `tiles` its distinct (kind, bucket) identities, both measured
/// and both two-sided: the lower sides prove the lane and the bucket walk
/// were reached, the upper sides catch a lane or a rotation law growing past
/// what the 32-slot atlas makes an all-hits steady state structural for.
/// Returns the second pass.
fn verify_draw(
    rig: &mut DrawRig,
    name: &str,
    note: &str,
    expect_body: bool,
    motes: (usize, usize),
    tiles: (usize, usize),
) -> DrawSampled {
    for _ in 0..2 * rig.frames.len() {
        rig.arm();
        rig.draw();
    }
    let a = run_draw(rig, expect_body);
    let b = run_draw(rig, expect_body);
    assert_eq!(
        a.fp_seq, b.fp_seq,
        "{name}: two identical replay loops produced different fingerprint \
         sequences — a bake (version bump) or an eviction happened in what \
         should be the all-hits steady state, so the workload is not timing \
         the dead-work-on-hit frame PET-01 names"
    );
    assert!(
        b.motes >= motes.0 && b.motes <= motes.1,
        "{name}: {} mote sprites over the loop, outside the measured \
         [{}, {}] — the lane this workload is named for has changed size",
        b.motes,
        motes.0,
        motes.1
    );
    assert!(
        b.distinct_tiles >= tiles.0 && b.distinct_tiles <= tiles.1,
        "{name}: {} distinct mote tiles, outside the measured [{}, {}] — \
         either the rotation walk stopped exercising the bucket law (lower) \
         or the tile population is growing toward the 32-slot atlas and the \
         all-hits steady state stops being structural (upper)",
        b.distinct_tiles,
        tiles.0,
        tiles.1
    );
    report_draw(name, note, &b);
    b
}

// ------------------------------------------------------------ pet captures --

/// Drive a real brain through its settle ladder and capture two windows of
/// its own emitted frames: the light-sleep z lane (PET-01's mote workload)
/// and the moteless deep sleep that follows (the body-only control). One
/// drive, both captures, so the two workloads differ in nothing but the lane.
fn capture_zee_and_body() -> (Vec<PetFrame>, Vec<PetFrame>, (u8, u8)) {
    let mut r = BrainRig::new(blank_ink(), None);
    // The look latch, exactly as the host syncs it (immediate at alpha 0).
    let look = r.brain.sync_look(3, 2);
    let mut zee: Vec<PetFrame> = Vec::new();
    let mut gap = 0usize;
    for _ in 0..3_000 {
        let f = step(&mut r, arm_still);
        let m = live_motes(&f);
        if m > 0 {
            assert!(
                f.motes
                    .iter()
                    .flatten()
                    .filter(|m| m.alpha > 0)
                    .all(|m| matches!(m.kind, PetMoteKind::Zee | PetMoteKind::ZeePop)),
                "zee capture: a non-z mote in the light-sleep window"
            );
            assert_eq!(f.alpha, 255, "zee capture: mote frame mid-fade");
            zee.push(f);
            gap = 0;
        } else if !zee.is_empty() {
            gap += 1;
            if gap > 10 {
                break; // the light-sleep lane has emptied for good
            }
        }
    }
    // Measured: 491 frames of live z's (the light-sleep spawn window at
    // 60 fps). Both sides: too few means the drive missed the window, too
    // many means the lane no longer empties and the "bounded window" premise
    // of the replay design is broken.
    assert!(
        (200..=700).contains(&zee.len()),
        "zee capture: {} frames with live z's, outside the measured band \
         (491) — the light-sleep window this replay is built from has moved",
        zee.len()
    );
    // Ride the same drive to the byte-stable deep sleep for the control.
    let mut k = 0usize;
    loop {
        let f = step(&mut r, arm_still);
        if r.brain.action() == PetAction::Sleep && !r.brain.needs_frames() && live_motes(&f) == 0 {
            break;
        }
        k += 1;
        assert!(k < 3_000, "body capture: never reached still deep sleep");
    }
    let mut body = Vec::with_capacity(240);
    for _ in 0..240 {
        let f = step(&mut r, arm_still);
        assert_eq!(live_motes(&f), 0, "body capture: a mote in deep sleep");
        assert_eq!(f.alpha, 255, "body capture: not fully visible");
        body.push(f);
    }
    (zee, body, look)
}

/// A second real drive for the landing-dust burst: wake the default sleeper
/// with a small move, let the wake-pop z die and the cat settle awake, then
/// teleport the caret 100 columns and capture the frames on which the landing
/// dust is alive.
fn capture_dust() -> (Vec<PetFrame>, (u8, u8)) {
    let mut r = BrainRig::new(blank_ink(), None);
    let look = r.brain.sync_look(3, 2);
    // Appear (still caret at col 20)…
    r.caret = Some((25, 20));
    for _ in 0..30 {
        r.feed();
        r.caret = Some((25, 20));
        r.tick();
    }
    // …wake with a 4-column move, then wait out the startled z and settle.
    let mut k = 0usize;
    loop {
        r.feed();
        r.caret = Some((25, 24));
        let f = r.tick();
        if live_motes(&f) == 0 && f.action.settled() && k > 30 {
            break;
        }
        k += 1;
        assert!(k < 2_000, "dust capture: never settled awake after waking");
    }
    // The screen-crossing teleport: 100 columns, over the 40%-of-grid bar.
    let mut dust: Vec<PetFrame> = Vec::new();
    let mut gap = 0usize;
    for _ in 0..1_000 {
        r.feed();
        r.caret = Some((25, 124));
        let f = r.tick();
        let m = live_motes(&f);
        if m > 0 {
            assert!(
                f.motes
                    .iter()
                    .flatten()
                    .filter(|m| m.alpha > 0)
                    .all(|m| m.kind == PetMoteKind::Dust),
                "dust capture: a non-dust mote in the landing window"
            );
            dust.push(f);
            gap = 0;
        } else if !dust.is_empty() {
            gap += 1;
            if gap > 5 {
                break; // the puffs have faded
            }
        }
    }
    // Measured: 35 frames of live dust (the ~0.45–0.59 s puff lives at
    // 60 fps). Two-sided for the same reason as the z window above.
    assert!(
        (15..=60).contains(&dust.len()),
        "dust capture: {} frames with live dust, outside the measured band \
         (35) — the teleport never landed in spawn_dust, or the puffs no \
         longer fade",
        dust.len()
    );
    (dust, look)
}

// --------------------------------------------------------------- robi rig --
//
// `RobiShow::frame` is a PURE function of `now - since` and the sense
// snapshot (`&self`, no mutation), so a workload is a phase window: the arm
// advances a frame counter and the timed call evaluates the show at
// `t0 + base + (n·dt mod span)` — the phase stays pinned inside its window
// for as long as criterion cares to measure. The windows are chosen from the
// audit (tips at 3.0–8.2 s and 25.0–30.2 s of the 76 s cycle); the GUARDS —
// `tip.is_some()` on every frame vs none — are what prove the windows right,
// so a shifted const shows up as a failed guard, not a silent miss.

struct RobiRig {
    show: RobiShow,
    t0: Instant,
    n: u64,
    base_ms: u64,
    span_ms: u64,
    sense: RobiSense,
}

impl RobiRig {
    fn new(born: bool, base_ms: u64, span_ms: u64) -> Self {
        let t0 = Instant::now();
        let mut show = RobiShow::default();
        if born {
            show.start(t0, 0xA5A5);
        }
        let mut handholds = [0i32; MAX_HANDHOLDS];
        for (i, x) in handholds.iter_mut().enumerate() {
            // Eight holds spread across the 2000 px grid, all in-bounds.
            *x = 150 + 250 * i as i32;
        }
        RobiRig {
            show,
            t0,
            n: 0,
            base_ms,
            span_ms,
            sense: RobiSense {
                geom: GEOM,
                cursor: (24, 80),
                bar_y: -10,
                handholds,
                handhold_count: 8,
            },
        }
    }

    fn arm(&mut self) {
        self.n += 1;
    }

    fn now(&self) -> Instant {
        self.t0 + Duration::from_millis(self.base_ms + (self.n * ROBI_DT_MS) % self.span_ms)
    }

    /// THE TIMED UNIT: one show evaluation.
    fn frame(&self) -> Option<RobiFrame> {
        self.show.frame(black_box(self.now()), &self.sense)
    }
}

/// The tip-A window, inside the jacks stage: every frame resolves a tip, i.e.
/// runs `pick_tip`'s per-frame Vec build (PET-02).
const TIP_BASE_MS: u64 = 3_200;
const TIP_SPAN_MS: u64 = 4_800;

/// The SAME jacks stage past the tip window: the identical pose arithmetic
/// with the tip resolver and nothing else removed — PET-02's A/B partner.
const NO_TIP_BASE_MS: u64 = 8_300;
const NO_TIP_SPAN_MS: u64 = 2_100;

/// The long calm at the rest spot: a static stand, `animating: false` — the
/// enabled-but-idle floor.
const REST_BASE_MS: u64 = WANDER_END + 1_000;
const REST_SPAN_MS: u64 = CYCLE_MS - REST_BASE_MS - 1_000;

#[derive(Default)]
struct RobiSampled {
    frames: usize,
    some: usize,
    tips: usize,
    jacks: usize,
    stand: usize,
    animating: usize,
}

fn run_robi(r: &mut RobiRig, frames: usize) -> RobiSampled {
    let mut s = RobiSampled::default();
    for _ in 0..frames {
        r.arm();
        s.frames += 1;
        let Some(f) = r.frame() else { continue };
        s.some += 1;
        s.tips += usize::from(f.tip.is_some());
        s.jacks += usize::from(matches!(
            f.pose,
            RobiGlyphId::RobiJacks0 | RobiGlyphId::RobiJacks1
        ));
        s.stand += usize::from(matches!(f.pose, RobiGlyphId::RobiStand));
        s.animating += usize::from(f.animating);
    }
    s
}

fn report_robi(name: &str, note: &str, s: &RobiSampled) {
    println!(
        "VOLUME {name:<26} | some {}/{} tips {} jacks {} stand {} animating {} | {note}",
        s.some, s.frames, s.tips, s.jacks, s.stand, s.animating,
    );
}

// ------------------------------------------------------------ count bench --

/// Record a COUNT as a criterion measurement: the reported "time" in
/// nanoseconds IS the item count (1 ns == 1 item) — the cursor_glow_volume
/// idiom, copied with both of its non-ceremonial parts: the spin loop (the
/// warm-up phase measures WALL time, and an instant return would double
/// `iters` forever) and the `k % 4` jitter (a zero-variance sample NaNs
/// criterion's kernel-bandwidth PDF math).
fn bench_count(g: &mut BenchmarkGroup<'_, WallTime>, id: &str, count: usize) {
    assert!(
        count > 0,
        "{id}: a zero count cannot be recorded as a duration — only counts \
         with an asserted positive lower bound are recorded"
    );
    let n = count as u64;
    let mut k = 0u64;
    g.bench_function(BenchmarkId::from_parameter(id), |b| {
        b.iter_custom(|iters| {
            let mut spin = 0u64;
            for i in 0..iters {
                spin = spin.wrapping_add(black_box(i));
            }
            black_box(spin);
            k = k.wrapping_add(1);
            Duration::from_nanos(n.saturating_mul(iters).saturating_add(k % 4))
        });
    });
}

// -------------------------------------------------------------- the groups --

fn pet_brain(c: &mut Criterion) {
    // PROVE FIRST, TIME SECOND: every fixture is built, driven to its claimed
    // state and verified before a nanosecond is measured; the verified
    // fixtures carry forward into the timed loops, which continue the same
    // scripts from the same state.

    // ---- brain fixtures (PET-03/PET-04 + the live-case context) ----------
    let mut retired = build_retired();
    let (_retired_s, retired_control_lit) = verify_retired(&mut retired);
    let mut sleeper = build_sleeper();
    verify_sleeper(&mut sleeper);
    let mut chase = build_warmed(blank_ink(), None, arm_chase);
    let chase_s = verify_chase(&mut chase);
    let mut bound = build_warmed(blank_ink(), None, arm_bound);
    let bound_s = verify_bound(&mut bound);
    let mut ink = build_warmed(full_ink(), Some(ROWS - 1), arm_ink_park);
    let (_ink_s, ink_divergent) = verify_ink(&mut ink);

    // ---- draw fixtures (PET-01) ------------------------------------------
    let (zee_frames, body_frames, look) = capture_zee_and_body();
    // The alpha-0 arm replays the retired brain's own emitted frame: the
    // `pet.alpha == 0` early return is `pet_cursor`'s whole disabled cost.
    let dark_frame = {
        let f = step(&mut retired, arm_hidden);
        assert_eq!(f.alpha, 0, "the retired brain stopped being retired");
        f
    };
    let (dust_frames, dust_look) = capture_dust();
    let mut draw_dark = DrawRig::new(vec![dark_frame; 8], look);
    let mut draw_body = DrawRig::new(body_frames, look);
    let mut draw_zee = DrawRig::new(zee_frames, look);
    let mut draw_dust = DrawRig::new(dust_frames, dust_look);
    // Measured loop totals (deterministic): body 240 frames / 0 motes; zee
    // 491 frames / 786 motes over 4 distinct tiles; dust 35 frames / 92
    // motes over 4 distinct tiles. Bands are ~±10% around those.
    let body_s = verify_draw(
        &mut draw_body,
        "pet_cursor_body_only",
        "deep sleeper: 1 body sprite, 0 motes — the mote-cost control",
        true,
        (0, 0),
        (0, 0),
    );
    let zee_s = verify_draw(
        &mut draw_zee,
        "pet_cursor_zee_lane",
        "light sleep: live z motes every frame — THE PET-01 GATE",
        true,
        (700, 870),
        (2, 8),
    );
    assert!(
        zee_s.mote_frames * 100 >= zee_s.frames * 95,
        "pet_cursor_zee_lane: motes on only {}/{} frames — the lane is not \
         sustained across the loop",
        zee_s.mote_frames,
        zee_s.frames
    );
    let dust_s = verify_draw(
        &mut draw_dust,
        "pet_cursor_dust_burst",
        "landing dust: the 3-disc Dust bake, burst cadence",
        true,
        (80, 105),
        (2, 8),
    );
    assert_eq!(
        dust_s.mote_frames, dust_s.frames,
        "pet_cursor_dust_burst: a captured frame with no dust"
    );
    // The dark arm: nothing may be emitted, and the CONTROL (the body rig,
    // verified lit above) is what makes that zero a measurement.
    {
        for _ in 0..16 {
            draw_dark.arm();
            let fp = draw_dark.draw();
            assert_eq!(fp, None, "pet_cursor_alpha0: drew a retired pet");
            assert!(
                draw_dark.free.is_empty(),
                "pet_cursor_alpha0: sprites from a retired pet"
            );
        }
        assert!(
            body_s.sprites > 0,
            "pet_cursor_alpha0: the lit control emitted nothing — the dark \
             zero proves nothing"
        );
        println!(
            "VOLUME pet_cursor_alpha0          | 0 sprites over 16 frames | \
             the pet.alpha == 0 early return (control: pet_cursor_body_only)"
        );
    }

    // ---- robi fixtures (PET-02) ------------------------------------------
    let mut robi_unborn = RobiRig::new(false, REST_BASE_MS, REST_SPAN_MS);
    let mut robi_tip = RobiRig::new(true, TIP_BASE_MS, TIP_SPAN_MS);
    let mut robi_no_tip = RobiRig::new(true, NO_TIP_BASE_MS, NO_TIP_SPAN_MS);
    let mut robi_rest = RobiRig::new(true, REST_BASE_MS, REST_SPAN_MS);
    let mut robi_cycle = RobiRig::new(true, 0, CYCLE_MS);
    {
        let s = run_robi(&mut robi_unborn, SAMPLE_FRAMES);
        report_robi("robi_unborn", "never started: the disabled early-out", &s);
        assert_eq!(s.some, 0, "robi_unborn: a frame from an unborn resident");
        let cs = run_robi(&mut robi_rest, SAMPLE_FRAMES);
        report_robi("robi_unborn.control", "same phase, born", &cs);
        assert_eq!(
            cs.some, cs.frames,
            "robi_unborn: the born control produced no frames — the unborn \
             zero proves nothing"
        );
        let s = run_robi(&mut robi_tip, SAMPLE_FRAMES);
        report_robi("robi_jacks_tip_window", "jacks stage, tip up: pick_tip/frame", &s);
        assert_eq!(s.some, s.frames, "robi_jacks_tip_window: dropped frames");
        assert_eq!(
            s.tips, s.frames,
            "robi_jacks_tip_window: a frame without a tip — the phase window \
             is not inside the tip window, so pick_tip is NOT running per \
             frame and the PET-02 A/B is void"
        );
        assert_eq!(
            s.jacks, s.frames,
            "robi_jacks_tip_window: a non-jacks pose — the A/B partner would \
             differ in more than the tip resolver"
        );
        assert_eq!(s.animating, s.frames, "robi_jacks_tip_window: static");
        let s = run_robi(&mut robi_no_tip, SAMPLE_FRAMES);
        report_robi("robi_jacks_no_tip", "same jacks stage, tip window over", &s);
        assert_eq!(s.some, s.frames, "robi_jacks_no_tip: dropped frames");
        assert_eq!(
            s.tips, 0,
            "robi_jacks_no_tip: a tip resolved — this arm must NOT run \
             pick_tip, or the A/B delta is no longer the resolver"
        );
        assert_eq!(
            s.jacks, s.frames,
            "robi_jacks_no_tip: a non-jacks pose — stage drifted, A/B void"
        );
        let s = run_robi(&mut robi_rest, SAMPLE_FRAMES);
        report_robi("robi_rest_calm", "the long calm: static stand", &s);
        assert_eq!(s.some, s.frames, "robi_rest_calm: dropped frames");
        assert_eq!(s.tips, 0, "robi_rest_calm: a tip at rest");
        assert_eq!(s.stand, s.frames, "robi_rest_calm: not standing");
        assert_eq!(
            s.animating, 0,
            "robi_rest_calm: animating at rest — this is not the zero-wake \
             idle the contract promises"
        );
    }
    // One whole 76 s cycle: 4750 frames at 16 ms covers every phase position
    // exactly once (gcd(16, 76_000) = 16), so the tip count is a structural
    // constant of the cycle — asserted exactly.
    let cycle_frames = (CYCLE_MS / ROBI_DT_MS) as usize;
    let cycle_s = run_robi(&mut robi_cycle, cycle_frames);
    report_robi("robi_full_cycle", "one whole 76 s cycle, every phase once", &cycle_s);
    assert_eq!(cycle_s.some, cycle_s.frames, "robi_full_cycle: dropped frames");
    // Measured: exactly 650 tip frames — the two 5.2 s windows on the 16 ms
    // phase grid. The ±10 margin is ±160 ms of window drift; a real change
    // to the tip schedule moves this by whole seconds' worth of frames.
    assert!(
        (640..=660).contains(&cycle_s.tips),
        "robi_full_cycle: {} tip frames of {} — outside the measured band \
         (650): the tip windows have moved",
        cycle_s.tips,
        cycle_s.frames
    );

    // ---- the timed groups -------------------------------------------------
    {
        let mut group = c.benchmark_group("pet_brain_tick");
        // THE FLOOR UNDER EVERY NUMBER IN THIS FILE: the `Instant::now()`
        // pair around an empty span. Subtract for absolutes; it cancels in
        // any A/B.
        group.bench_function("timer_floor", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let t0 = WallInstant::now();
                    black_box(0u64);
                    total += t0.elapsed();
                }
                total
            });
        });
        let mut time_brain = |name: &str, rig: &mut BrainRig, arm: BArm| {
            group.bench_function(BenchmarkId::from_parameter(name), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        arm(rig);
                        let t0 = WallInstant::now();
                        black_box(rig.tick());
                        total += t0.elapsed();
                    }
                    total
                });
            });
        };
        time_brain("retired_no_caret", &mut retired, arm_hidden);
        time_brain("sleeping_resident", &mut sleeper, arm_still);
        time_brain("typing_chase", &mut chase, arm_chase);
        time_brain("screen_bound_flights", &mut bound, arm_bound);
        time_brain("ink_dense_eviction", &mut ink, arm_ink_park);
        group.finish();
    }

    {
        let mut group = c.benchmark_group("pet_cursor_draw");
        let mut time_draw = |name: &str, rig: &mut DrawRig| {
            group.bench_function(BenchmarkId::from_parameter(name), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        rig.arm();
                        let t0 = WallInstant::now();
                        black_box(rig.draw());
                        total += t0.elapsed();
                    }
                    total
                });
            });
        };
        time_draw("alpha0_skip", &mut draw_dark);
        time_draw("body_only_deep_sleep", &mut draw_body);
        time_draw("zee_lane_light_sleep", &mut draw_zee);
        time_draw("dust_burst_landing", &mut draw_dust);
        group.finish();
    }

    {
        let mut group = c.benchmark_group("robi_frame");
        let mut time_robi = |name: &str, rig: &mut RobiRig| {
            group.bench_function(BenchmarkId::from_parameter(name), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        rig.arm();
                        let t0 = WallInstant::now();
                        black_box(rig.frame());
                        total += t0.elapsed();
                    }
                    total
                });
            });
        };
        time_robi("unborn", &mut robi_unborn);
        time_robi("jacks_tip_window", &mut robi_tip);
        time_robi("jacks_no_tip", &mut robi_no_tip);
        time_robi("rest_calm", &mut robi_rest);
        time_robi("full_cycle_sweep", &mut robi_cycle);
        group.finish();
    }

    {
        // PET-03: the host-seam copy the tick numbers above deliberately
        // exclude, priced under a name that cannot be mistaken for `tick`.
        // The brain is the verified RETIRED one — the exact frames on which
        // the audit says the copy is provably unread.
        let mut group = c.benchmark_group("pet_brain_host_seams");
        let spans = full_ink();
        let f = step(&mut retired, arm_hidden);
        assert_eq!(
            f.fp(),
            0,
            "sense_ink seam: the brain is no longer retired — this would \
             price the copy on a frame where it might be read"
        );
        group.bench_function("sense_ink_50row_map", |b| {
            b.iter(|| {
                retired
                    .brain
                    .sense_ink(0, black_box(&spans), Some(ROWS - 1));
            });
        });
        group.finish();
    }

    {
        // The verified counts, as criterion measurements (1 ns == 1 item):
        // count regressions get baselines and A/Bs exactly like time ones.
        let mut group = c.benchmark_group("pet_brain_volume");
        group
            .warm_up_time(Duration::from_millis(1))
            .measurement_time(Duration::from_millis(10))
            .sample_size(10);
        bench_count(&mut group, "zee_lane/mote_sprites", zee_s.motes);
        bench_count(&mut group, "zee_lane/sprites", zee_s.sprites);
        bench_count(&mut group, "dust_burst/mote_sprites", dust_s.motes);
        bench_count(&mut group, "body_only/sprites", body_s.sprites);
        bench_count(&mut group, "bound_flights/airborne_frames", bound_s.airborne);
        bench_count(&mut group, "bound_flights/dust_frames", bound_s.dust_frames);
        bench_count(&mut group, "typing_chase/gait_frames", chase_s.moving + chase_s.airborne);
        bench_count(&mut group, "ink_eviction/divergent_frames", ink_divergent);
        bench_count(&mut group, "robi_full_cycle/tip_frames", cycle_s.tips);
        // The retired workload's count IS zero by assertion; record its
        // control's lit count instead so the pair stays visible in baselines.
        bench_count(&mut group, "retired_control/lit_frames", retired_control_lit);
        group.finish();
    }
}

criterion_group!(benches, pet_brain);
criterion_main!(benches);
