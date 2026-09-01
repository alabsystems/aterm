// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// TRAIL SYNTH — what the sound of typing costs (audit findings TS-1..TS-5).
// (The `tone/` groups at the bottom of this file price TN-1, the typing-mood
// classifier that steers the synth's melody tables — see their own header.)
//
// HONEST FRAMING, FIRST. This is the one aterm-effects engine that does NOT run
// on the frame path. `TrailSynth::render` is ticked by AudioToolbox's own
// real-time callback (`aterm_gui::trail_audio::mac::render_cb`) with
// BUFFER_FRAMES = 512, CHANNELS = 2, SAMPLE_RATE = 48_000 — 93.75 buffers per
// second, each covering 10.667 ms of audio. The frame thread's only contact
// with the synth is a `SyncSender::try_send`; a worker thread owns the mutex
// and calls `push`. So no synth work can stall a frame directly — but the
// callback runs ABOVE the renderer's priority, so every cycle it burns is
// stolen from the renderer on a loaded machine, and its deadline is harder
// than a frame's (a late buffer is an audible click, not a dropped frame).
//
// READING THE NUMBERS. The `trail_synth_render` group times ONE host-sized
// buffer, so:
//   * per 512-frame buffer   — the printed time.
//   * per second of audio    — multiply by 93.75 (the `trail_synth_script`
//                              group measures whole seconds directly, as an
//                              independent check on that extrapolation).
//   * headroom               — one buffer is 10_667 us of audio, so
//                              time / 10_667 us is the fraction of a
//                              real-time-priority core the synth holds.
//   * `elem/s` (throughput)  — rendered frames per second; divide by 48_000
//                              for the realtime factor.
//
// STATE SETUP IS THE LOAD-BEARING PART. A fresh `TrailSynth` renders EXACT
// ZEROS in nanoseconds via the `is_quiet()` early-out, so a bench that only
// calls `render` measures nothing. Voices exist only after `push`, and `push`
// has three gates that silently produce an empty benchmark:
//   1. every f32 scalar must be finite and `gain > 0.0`;
//   2. MIN-GAP THINNING — `since_voice` advances only inside `render`, so a
//      tight `for _ in 0..N { synth.push(typed) }` loop admits exactly ONE
//      voice. Typed/Backspace/Navigation/Glide are admitted only every 45 ms
//      of RENDERED sample time (4.22 buffers); Jump/Sweep/Land/Bonk and every
//      Celebration bypass the gap and can be stacked in one call;
//   3. the ambient bed is energised only by `Trail(_)` events carrying
//      `bed: true` — with the shipping default `bed: false`, `bed.level` stays
//      EXACTLY 0.0 and the whole bed layer is structurally silent.
// Every workload therefore interleaves `push` and `render` on a real script,
// and `verify_reaches_target` proves the resulting state from BOTH sides
// (live voices, emitted volume, bed level) before anything is timed. A guard
// that passes on an idle engine is not a guard: `live <= 28` or `nonzero >= 0`
// would both be satisfied by silence, which is the exact failure being
// defended against.
//
// THE CLOCK IS THE SAMPLE COUNT. This module never reads a wall clock — its
// only time base is `frames * inv_sr`, advanced inside `render`. Every state
// here is therefore built by rendering a fixed number of fixed-size buffers,
// and the whole bench is a pure function of (script, seed): bit-replayable,
// no audio device, no I/O, no GPU, no window.
//
// WHY `iter_batched_ref`. `render` MUTATES the synth (voices decay and
// retire), so `b.iter(|| s.render(..))` would measure a state that empties out
// under the benchmark — the polyphony workloads would converge on silence.
// Each iteration instead starts from a clone of the same warm state (~6 KB,
// cloned OUTSIDE the timed region by criterion's batching).
//
// THE WORKLOADS
//   off/silent_idle       Sound ON, nothing to play — the `is_quiet()`
//                         early-out plus `out.fill(0.0)`. The floor every
//                         other number is read against, and the most valuable
//                         number here: most users have this effect off most of
//                         the time, and this is what "off" costs per callback.
//                         (Fully DISABLED is cheaper still and needs no
//                         measurement: `trail_sounds = false` builds
//                         `TrailAudio` with `tx: None` and spawns no worker
//                         thread at all, so neither `push` nor `render` exists
//                         to time. `push/rejected_off` measures the host-gated
//                         intake edge that survives that, and
//                         `script/silent_1s` the per-second idle toll.)
//   polyphony/N           N SOUNDING voices, N in {1,2,4,8,16,28}. Fitting
//                         cost = a + b*N separates TS-1's fixed 28-slot scan
//                         (the intercept a) from TS-4/TS-5's per-voice libm
//                         cost (the slope b), so each fix lands on the right
//                         term. N=1 is the overwhelmingly common state: one
//                         keystroke's 0.24 s tail decaying alone — but note it
//                         is a `Typed` voice while N>=2 are `Jump` voices (see
//                         `polyphony_warm`), so fit the LINE on N>=2 and read
//                         N=1 as its own case rather than as a point on it.
//   style/<palette>       The BUSIEST buffer of a ~0.43 s typing burst at
//                         ~18.7 admitted keystrokes/s — the peak per-callback
//                         cost, which is what a hard deadline is judged
//                         against. Palettes are not interchangeable:
//                         Lumen/Comet are glided sines plus a CONSTANT-cutoff
//                         noise band (the `n_glide == 0.0` case TS-2 hoists a
//                         per-sample `tanf` out of), Fire/Laser/Water are
//                         noise-driven with per-sample one-poles, Mech is
//                         percussive with short voices. Their voice counts
//                         differ too — read the emitted-volume table beside
//                         the times, never the times alone.
//   bed/<style>_{off,on}  The SETTLED end state of the same burst (bed level
//                         is slew-limited, so on/off are only comparable once
//                         it has arrived), with the ambient bed off (shipping
//                         default) and on. `bed_off` is the state TS-3 is
//                         about: `bed.level` is exactly 0.0, yet every
//                         rendered sample still pays a call, a compare and a
//                         `&dyn Palette` decision. NOTE the on/off delta is
//                         NOT TS-3's hoistable cost — `bed: true` also arms
//                         stochastic grain voices, so it moves emitted volume
//                         too (see the table). TS-3's cost is buried inside
//                         the `bed_off` number; only an A/B of the fix prices
//                         it.
//   bed_variant/<cand>    The ambient-bed TOURNAMENT candidates behind
//                         `set_bed_variant` — audition-only (no host path
//                         reaches them), pricing their per-sample
//                         `melody_hz`/LFO work against `bed/water_on`, which
//                         IS `BedVariant::Current`. `bed_variant/silence` is
//                         also the CONTROL the bed pair above cannot be: the
//                         identical warm state with the bed body silenced and
//                         everything else — voices, RNG history, grain
//                         history — held fixed, so `water_on - silence` is the
//                         bed body's true cost.
//   buffer/<frames>       128/256/512/1024-frame buffers from one warm state:
//                         separates per-BLOCK overhead (`tick_bed`, the rate
//                         decay's `exp`) from per-SAMPLE cost. 512 is the
//                         host's.
//   celebration/peak      The sing-along's worst case — riff bars with a
//                         key-repeat typing flood under them, sampled at the
//                         busiest buffer the script passes through. This is
//                         the real-time-thread headroom number.
//   script/*              Whole seconds of audio rendered end to end, pushes
//                         included, so "per second of audio" is MEASURED and
//                         not extrapolated — including `silent_1s`, the
//                         per-second cost of an effect nobody is using.
//   push/*                `TrailSynth::push` alone — the intake path the
//                         worker thread runs (governor, `advance_melody`,
//                         palette `design`, `claim`+`spawn`) — including both
//                         of its reject edges: min-gap thinning and the
//                         host-gated `gain == 0.0` off path.
//
// EMITTED VOLUME IS RECORDED TOO, in the table printed before the timings:
// live voices, samples written, non-zero samples, peak, bed level and an FNV
// checksum of the buffer's raw bits, per workload. A regression in COUNT (more
// voices, longer tails) then stays separable from a regression in per-item
// COST — and because every fix in this module is judged against BYTE identity
// (`palettes_render_byte_identical_to_v056_reference`,
// `brrrring_of_rapid_line_feeds_is_pinned`, `mech_is_deterministic`,
// `bed_variants_render_deterministically_and_decay_to_exact_silence`), a
// "faster" variant that quietly changed the AUDIO shows up here as a changed
// checksum instead of hiding behind a green benchmark.
//
// FIRST RUN, for orientation only — re-measure, never cite these (Apple M5 Max,
// macOS 26.5, `--warm-up-time 1 --measurement-time 2`, other builds sharing the
// machine; time per 512-frame buffer, i.e. per 10_667 us of audio):
//   off/silent_idle    41.9 ns  (script/silent_1s: 4.24 us per SECOND of audio —
//                              0.0004 % of a core to decide there is nothing to
//                              play, and the host parks the queue entirely after
//                              ~0.5 s of that)
//   push/rejected_off   4.9 ns  push/typed_thinned 5.9 ns
//   push/typed_admitted 25.9 ns push/riff_bar 91.2 ns
//   polyphony 1/2/4/8/16/28 voices: 13.7 / 19.9 / 44.7 / 83.9 / 179.1 / 353.6 us
//   styles at their burst peak: fire 19.0, beam 27.6, rainbow_kitty 28.4,
//                              mech 28.7, water 48.9, phaser 49.4, laser 60.9,
//                              lumen 70.6, comet 118.5, sparkle 220.8 (16
//                              voices — always read the table beside these)
//   celebration/peak   75.1 us (13 voices)
//   whole seconds of audio: typing_1s_lumen 7.45 ms, typing_1s_mech 1.94 ms,
//                              celebration_bar 13.8 ms per 1.6 s bar (8.6 ms/s)
// So the WORST measured buffer, sparkle's peak, is 2.1 % of its 10.667 ms
// deadline, and a second of the loudest sustained sound is under 1 % of a core.
// The emitted-volume checksums reproduced BIT-EXACTLY across runs, which is the
// determinism the byte-identity pins depend on.
//
// TWO THINGS THAT RUN CONTRADICTED, both worth re-checking before acting on the
// audit's reasoning:
//   * TS-1 (the fixed 28-slot scan) is small, not "~20 % of the sample". The
//     polyphony line over N = 2..16 is ~11.4 us per voice and extrapolates to a
//     NON-POSITIVE intercept, so the whole fixed per-buffer cost — empty-slot
//     scan, bed early-out, duck, DC blocker, soft clip, `out.fill` — is under
//     ~2 us even against a 13.7 us single-voice buffer. (N = 28 sits above the
//     line, 353.6 us vs ~300 us predicted: the voice array is 5.6 KB and every
//     sample walks all of it, so what is worth chasing at high polyphony looks
//     more like the working set than the flag scan.)
//   * TS-3's bed layer is cheaper than a per-sample `sinf` count suggests, and
//     the off/on pair CANNOT price it: `bed/lumen_on` measured 2.0 us FASTER
//     than `bed/lumen_off` — noise, not a discount. The clean control is
//     `bed_variant/silence` against `bed/water_on`, identical state with only
//     the bed body silenced: 21.0 vs 23.3 us, so Water's entire live bed is
//     2.3 us/buffer (10 %) and the structurally-silent default's per-sample
//     call+compare is necessarily well under that.

use std::time::Duration;

use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::tone::{
    self, BUCKETS, MIN_NGRAMS, Tone, ToneModel, ToneScratch, for_each_ngram_bucket,
};
use aterm_effects::trail_sound::{
    BedVariant, CHANNELS, CelebrationGesture, SoundEvent, SoundGesture, SoundKind, SoundVoice,
    TrailSynth,
};
use criterion::measurement::WallTime;
use criterion::{
    BatchSize, BenchmarkGroup, BenchmarkId, Criterion, Throughput, black_box, criterion_group,
    criterion_main,
};

/// The host's stream rate (`aterm_gui::trail_audio::SAMPLE_RATE`).
const SR: f32 = 48_000.0;
/// The host's callback buffer (`aterm_gui::trail_audio::BUFFER_FRAMES`).
const FRAMES: usize = 512;
/// Fixed seed — the synth's only randomness is a seeded xorshift, so this pins
/// every rendered sample.
const SEED: u32 = 0x5EED_50FD;
/// The host's shipped `trail_sound_volume` default: the gain the loudness
/// ladder is fitted at.
const VOLUME: f32 = 0.4;
/// `trail_sound.rs`'s private `MIN_GAP` — the discrete layer's admission gap.
/// Mirrored (it is private, and widening its visibility to bench a governor is
/// backwards); the guards count REAL admissions, so a change upstream fails
/// them instead of silently emptying the bench.
const MIN_GAP_S: f32 = 0.045;
/// Seconds of audio in one host buffer: 10.667 ms.
const BUFFER_S: f32 = FRAMES as f32 / SR;
/// Push one keystroke every 5th buffer = every 53.3 ms, comfortably past
/// `MIN_GAP`, so every keystroke in the script is ADMITTED (~18.7/s — a fast
/// burst). Four buffers would land at 42.7 ms and be thinned every other push.
const PUSH_EVERY: usize = 5;
const _: () = assert!(PUSH_EVERY as f32 * BUFFER_S > MIN_GAP_S);
/// Buffers in one second of audio (93.75, rounded up).
const BUFFERS_PER_SECOND: usize = 94;
/// Buffers in one celebration bar (`CELEBRATION_BAR_SECONDS` = 1.6 s).
const BUFFERS_PER_BAR: usize = 150;
/// Buffers of typing before a per-buffer workload is sampled: 0.43 s, long
/// enough for the burst to reach its steady state and for the ambient bed's
/// 250 ms rise to arrive. The script's last push lands 5 buffers before the
/// end, so the END state is also one whose `since_voice` is past `MIN_GAP` —
/// which is what `push/typed_admitted` needs.
const WARM_BUFFERS: usize = 40;
/// Buffers rendered after a burst of `Jump`s so every voice is past its
/// pre-delay: 64 ms > Lumen's 55 ms grace-note delay, and well inside the
/// 240 ms voice duration.
const SETTLE_BUFFERS: usize = 6;
/// A held key's song signature (`kitty_sing::KittySing::signature`).
const SING_SIG: u32 = 0x1234_5678;

/// The nine visual styles, each with its own palette and sound design.
const STYLES: [(&str, GlowStyle); 9] = [
    ("lumen", GlowStyle::Lumen),
    ("phaser", GlowStyle::Phaser),
    ("rainbow_kitty", GlowStyle::RainbowKitty),
    ("sparkle", GlowStyle::Sparkle),
    ("fire", GlowStyle::Fire),
    ("laser", GlowStyle::Laser),
    ("beam", GlowStyle::Beam),
    ("water", GlowStyle::Water),
    ("comet", GlowStyle::Comet),
];

/// One event with every scalar pinned. `pan`/`heat`/`hue` shape only
/// spawn-time constants (the equal-power pan law, a level warm, the melodic
/// degree), so fixing them costs no per-sample realism while keeping the whole
/// script replayable.
fn event(style: GlowStyle, voice: SoundVoice, kind: SoundGesture, bed: bool) -> SoundEvent {
    SoundEvent {
        style,
        voice,
        kind,
        pan: 0.0,
        heat: 0.5,
        hue: 0.3,
        gain: VOLUME,
        tone: Tone::Technical,
        bed,
        shifted: false,
    }
}

// ---------------------------------------------------------------------------
// Scripts — the only way state gets into this synth
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Script {
    /// Sound ON, user idle: the host has not yet paused the queue (it does so
    /// after 48 silent buffers, ~0.5 s), so the callback keeps asking for
    /// buffers the synth has nothing to put in.
    Idle,
    /// A typing burst: one `Typed` every `PUSH_EVERY` buffers.
    Typing {
        style: GlowStyle,
        voice: SoundVoice,
        bed: bool,
    },
    /// The sing-along: one riff bar per 1.6 s with a key-repeat typing flood
    /// under it — the shape `celebration_bar_fits_the_polyphony_budget`
    /// exercises, and the worst case the module is designed to survive.
    Celebration,
}

/// What a scripted run passed through — the evidence that the script actually
/// drove the engine instead of being eaten by a gate.
#[derive(Default)]
struct Stats {
    /// Events handed to `push`.
    pushed: usize,
    /// Events that survived the governor and spawned at least one voice.
    admitted: usize,
    /// Most voices live at any pre-render moment.
    peak_live: usize,
    /// Buffers whose render had at least one live voice.
    busy_buffers: usize,
}

/// Run `buffers` host-sized buffers of `script` from a fresh synth.
///
/// `OBS` is a const parameter, not a flag: with `OBS == false` (the copy the
/// `trail_synth_script` benchmarks time) every observation below folds away,
/// so the timed body is exactly the host's push/render interleave and nothing
/// else. One function, so the state the guards prove is the state that is
/// timed — they cannot drift apart.
fn play<const OBS: bool>(
    script: Script,
    buffers: usize,
    buf: &mut [f32],
    stats: &mut Stats,
    busiest: &mut Option<TrailSynth>,
) -> TrailSynth {
    let mut s = TrailSynth::new(SR, SEED);
    for b in 0..buffers {
        let before = if OBS { s.live_voices() } else { 0 };
        let pushed = push_at(&mut s, script, b);
        if OBS {
            stats.pushed += pushed;
            // A push can only ADD voices between renders, so a rise is an
            // admission (these scripts never fill the 28-slot pool, so a steal
            // cannot mask one).
            let live = s.live_voices();
            if pushed > 0 && live > before {
                stats.admitted += 1;
            }
            if live > stats.peak_live {
                stats.peak_live = live;
                *busiest = Some(s.clone());
            }
            if live > 0 {
                stats.busy_buffers += 1;
            }
        }
        s.render(buf);
    }
    s
}

/// Push whatever `script` says happens at buffer `b`; returns how many events
/// were handed to `push`.
fn push_at(s: &mut TrailSynth, script: Script, b: usize) -> usize {
    match script {
        Script::Idle => 0,
        Script::Typing { style, voice, bed } => {
            if b.is_multiple_of(PUSH_EVERY) {
                s.push(event(
                    style,
                    voice,
                    SoundGesture::Trail(SoundKind::Typed),
                    bed,
                ));
                1
            } else {
                0
            }
        }
        Script::Celebration => {
            let mut n = 0;
            if b.is_multiple_of(BUFFERS_PER_BAR) {
                let bar = (b / BUFFERS_PER_BAR) as u16;
                s.push(event(
                    GlowStyle::RainbowKitty,
                    SoundVoice::Style,
                    SoundGesture::Celebration(CelebrationGesture::riff_bar(bar, SING_SIG)),
                    false,
                ));
                n += 1;
            }
            if b.is_multiple_of(PUSH_EVERY) {
                s.push(event(
                    GlowStyle::RainbowKitty,
                    SoundVoice::Style,
                    SoundGesture::Trail(SoundKind::Typed),
                    false,
                ));
                n += 1;
            }
            n
        }
    }
}

/// Run a script with observation on: (final state, busiest state, stats).
fn observe(script: Script, buffers: usize) -> (TrailSynth, Option<TrailSynth>, Stats) {
    let mut buf = vec![0.0f32; FRAMES * CHANNELS];
    let mut stats = Stats::default();
    let mut busiest = None;
    let end = play::<true>(script, buffers, &mut buf, &mut stats, &mut busiest);
    (end, busiest, stats)
}

/// The two states a typing burst offers: its SETTLED end (typical mid-burst
/// polyphony, bed level arrived, `since_voice` past `MIN_GAP`) and its
/// BUSIEST buffer (peak per-callback cost).
fn typing_states(style: GlowStyle, voice: SoundVoice, bed: bool) -> (TrailSynth, TrailSynth) {
    let script = Script::Typing { style, voice, bed };
    let (end, busiest, stats) = observe(script, WARM_BUFFERS);
    // The trap this whole file is written against: if the governor ate the
    // script, the warm state is near-silence and every timing below is a lie.
    assert!(
        stats.admitted >= 6 && stats.admitted <= stats.pushed,
        "typing warm-up for {style:?}/{voice:?} admitted {} of {} pushes — the \
         min-gap governor ate the script (PUSH_EVERY = {PUSH_EVERY} buffers \
         must exceed MIN_GAP = {MIN_GAP_S} s)",
        stats.admitted,
        stats.pushed
    );
    assert_eq!(
        stats.busy_buffers, WARM_BUFFERS,
        "typing warm-up for {style:?}/{voice:?}: only {} of {WARM_BUFFERS} \
         buffers had a live voice — the burst is not continuous",
        stats.busy_buffers
    );
    assert!(
        (2..=24).contains(&stats.peak_live),
        "typing warm-up for {style:?}/{voice:?} peaked at {} live voices — a \
         burst at this rate must OVERLAP tails, and must not run away",
        stats.peak_live
    );
    (end, busiest.expect("a busy script has a busiest buffer"))
}

/// Exactly `n` SOUNDING voices.
///
/// One `Typed` is one Lumen voice; one `Jump` is exactly two (the bloom plus a
/// grace note 55 ms behind it), and `Jump` bypasses min-gap so they stack in a
/// single call. `SETTLE_BUFFERS` of render then carries every grace note past
/// its pre-delay — a voice still inside `v.t < 0.0` counts as live but
/// `continue`s immediately, which would make the polyphony line lie.
fn polyphony_warm(n: usize) -> TrailSynth {
    let mut s = TrailSynth::new(SR, SEED);
    if n == 1 {
        s.push(event(
            GlowStyle::Lumen,
            SoundVoice::Style,
            SoundGesture::Trail(SoundKind::Typed),
            false,
        ));
    } else {
        assert!(n.is_multiple_of(2), "a Lumen Jump spawns voices in pairs");
        for _ in 0..n / 2 {
            s.push(event(
                GlowStyle::Lumen,
                SoundVoice::Style,
                SoundGesture::Trail(SoundKind::Jump),
                false,
            ));
        }
    }
    let mut buf = vec![0.0f32; FRAMES * CHANNELS];
    for _ in 0..SETTLE_BUFFERS {
        s.render(&mut buf);
    }
    s
}

// ---------------------------------------------------------------------------
// Emitted volume + the reach guard
// ---------------------------------------------------------------------------

/// What one rendered buffer actually EMITTED — recorded per workload so a
/// regression in COUNT stays separable from one in per-item COST, and so a
/// change in the AUDIO cannot hide behind a green timing.
struct Emission {
    live: usize,
    samples: usize,
    nonzero: usize,
    peak: f32,
    bed_level: f32,
    checksum: u64,
}

/// FNV-1a over the raw sample bits. Bit-level on purpose: it separates `+0.0`
/// from `-0.0`, which is exactly the difference TS-3's bed hoist would
/// introduce and which the byte-identity pins exist to catch.
fn checksum(buf: &[f32]) -> u64 {
    buf.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, s| {
        (h ^ u64::from(s.to_bits())).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// What the bed must be doing — asserted from both sides, because "the bed is
/// quiet" and "the bed is not there" are different claims and only the second
/// is what `bed: false` means.
#[derive(Clone, Copy)]
enum Bed {
    /// Structurally silent: `bed.level` and `bed.energy` are EXACTLY zero, so
    /// `bed_sample` takes its floor early-out on every sample.
    Floor,
    /// Energised: the bed mixer is really running its per-sample DSP.
    Live,
}

/// What the discrete layer must be doing.
#[derive(Clone, Copy)]
enum Voices {
    /// `render` must take the `is_quiet()` early-out — the OFF/idle path.
    Quiet,
    /// The per-sample loop must run and produce audible samples.
    Audible,
}

struct Workload {
    kind: &'static str,
    param: String,
    frames: usize,
    warm: TrailSynth,
    /// Inclusive live-voice window. BOTH sides: an upper bound alone is
    /// satisfied by an idle engine, which is the failure this file exists to
    /// prevent.
    live: (usize, usize),
    voices: Voices,
    bed: Bed,
}

/// Prove the workload reaches the code it claims to, before it is timed.
///
/// Everything here is asserted on a CLONE of the exact state each timed
/// iteration starts from, so the guard describes the thing being measured
/// rather than something adjacent to it.
fn verify_reaches_target(w: &Workload) -> Emission {
    let mut probe = w.warm.clone();
    let live = probe.live_voices();
    assert!(
        live >= w.live.0 && live <= w.live.1,
        "{}/{}: {live} live voices, outside the expected {}..={} — the state \
         this workload times is not the state it documents",
        w.kind,
        w.param,
        w.live.0,
        w.live.1
    );

    // `is_quiet()` is `render`'s first branch: false here proves the per-sample
    // loop is entered, true proves the early-out is (which for `off/*` IS the
    // target).
    let quiet = probe.is_quiet();
    match w.voices {
        Voices::Quiet => assert!(
            quiet,
            "{}/{}: expected the is_quiet() early-out, but the synth has work",
            w.kind, w.param
        ),
        Voices::Audible => assert!(
            !quiet,
            "{}/{}: is_quiet() — `render` would fill zeros and return, so this \
             workload would measure the early-out and nothing else",
            w.kind, w.param
        ),
    }

    let (bed_energy, bed_level) = probe.debug_bed();
    match w.bed {
        Bed::Floor => assert!(
            bed_level == 0.0 && bed_energy == 0.0,
            "{}/{}: bed level {bed_level} / energy {bed_energy} are not EXACTLY \
             zero — this workload is the shipping `bed: false` default, whose \
             whole claim is that the bed layer is structurally silent",
            w.kind,
            w.param
        ),
        Bed::Live => assert!(
            (0.3..=1.0).contains(&bed_level) && bed_energy > 0.1,
            "{}/{}: bed level {bed_level} / energy {bed_energy} — the bed is \
             not energised, so the bed mixer takes its floor early-out and this \
             workload measures the discrete layer twice",
            w.kind,
            w.param
        ),
    }

    let mut buf = vec![0.0f32; w.frames * CHANNELS];
    probe.render(&mut buf);
    let nonzero = buf.iter().filter(|s| **s != 0.0).count();
    let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    match w.voices {
        Voices::Quiet => {
            assert_eq!(
                nonzero, 0,
                "{}/{}: the idle path must write EXACT zeros",
                w.kind, w.param
            );
            assert_eq!(
                peak, 0.0,
                "{}/{}: the idle path emitted signal",
                w.kind, w.param
            );
            // Silence is a FIXED POINT — rendering it leaves the synth in the
            // same state, voices and all. That is what licenses the idle
            // workload to skip the per-iteration clone below, so it is proven
            // here rather than assumed.
            assert!(
                probe.is_quiet() && probe.live_voices() == 0,
                "{}/{}: rendering silence changed the state, so the timed loop \
                 cannot reuse one synth",
                w.kind,
                w.param
            );
        }
        Voices::Audible => {
            // Both sides again: `nonzero > 0` alone passes on a buffer with a
            // single audible sample in it, i.e. on a voice that died on frame 1.
            assert!(
                nonzero >= w.frames && nonzero <= w.frames * CHANNELS,
                "{}/{}: {nonzero} non-zero samples of {} — the voices are not \
                 sounding across the buffer",
                w.kind,
                w.param,
                w.frames * CHANNELS
            );
            assert!(
                peak > 1e-4 && peak <= 1.0,
                "{}/{}: peak {peak} — inaudible (or past the master clamp)",
                w.kind,
                w.param
            );
        }
    }

    Emission {
        live,
        samples: buf.len(),
        nonzero,
        peak,
        bed_level,
        checksum: checksum(&buf),
    }
}

// ---------------------------------------------------------------------------
// Workload construction
// ---------------------------------------------------------------------------

fn workloads() -> Vec<Workload> {
    let mut out = Vec::new();

    // Sound ON, nothing to play. A synth that has never been fed is in the
    // same state as one whose last tail expired: `is_quiet()` is a fixed
    // point, so this is exactly what the callback renders between bursts.
    out.push(Workload {
        kind: "off",
        param: "silent_idle".into(),
        frames: FRAMES,
        warm: TrailSynth::new(SR, SEED),
        live: (0, 0),
        voices: Voices::Quiet,
        bed: Bed::Floor,
    });

    for n in [1usize, 2, 4, 8, 16, 28] {
        out.push(Workload {
            kind: "polyphony",
            param: format!("{n:02}"),
            frames: FRAMES,
            warm: polyphony_warm(n),
            live: (n, n),
            voices: Voices::Audible,
            bed: Bed::Floor,
        });
    }

    // Palette sweep at each burst's peak buffer. The mechanical-keyboard voice
    // routes every style to `MechPalette`, so one entry covers it.
    let palettes = STYLES
        .iter()
        .map(|(name, style)| (*name, *style, SoundVoice::Style))
        .chain(std::iter::once((
            "mech",
            GlowStyle::Lumen,
            SoundVoice::Mech,
        )));
    for (name, style, voice) in palettes {
        let (_, busiest) = typing_states(style, voice, false);
        out.push(Workload {
            kind: "style",
            param: name.into(),
            frames: FRAMES,
            warm: busiest,
            // A burst at this rate overlaps a handful of tails; the percussive
            // palettes stack two short voices per keystroke. Outside this
            // window the script has changed shape.
            live: (2, 24),
            voices: Voices::Audible,
            bed: Bed::Floor,
        });
    }

    // The bed pair, on the settled end state so the level has arrived.
    for (name, style) in [("lumen", GlowStyle::Lumen), ("water", GlowStyle::Water)] {
        for bed_on in [false, true] {
            let (end, _) = typing_states(style, SoundVoice::Style, bed_on);
            out.push(Workload {
                kind: "bed",
                param: format!("{name}_{}", if bed_on { "on" } else { "off" }),
                frames: FRAMES,
                warm: end,
                // `bed: true` also arms stochastic grain voices, so its live
                // window is the wider one.
                live: if bed_on { (1, 24) } else { (1, 14) },
                voices: Voices::Audible,
                bed: if bed_on { Bed::Live } else { Bed::Floor },
            });
        }
    }

    // The tournament candidates, against `bed/water_on` (= `Current`).
    for (name, variant) in [
        ("chord_drift", BedVariant::ChordDrift),
        ("breathing", BedVariant::Breathing),
        ("shimmer", BedVariant::Shimmer),
        ("silence", BedVariant::Silence),
    ] {
        let (mut end, _) = typing_states(GlowStyle::Water, SoundVoice::Style, true);
        end.set_bed_variant(variant);
        out.push(Workload {
            kind: "bed_variant",
            param: name.into(),
            frames: FRAMES,
            warm: end,
            live: (1, 24),
            voices: Voices::Audible,
            bed: Bed::Live,
        });
    }

    // Block size: one state, four buffer lengths.
    let (_, busiest) = typing_states(GlowStyle::Lumen, SoundVoice::Style, false);
    for frames in [128usize, 256, 512, 1024] {
        out.push(Workload {
            kind: "buffer",
            param: format!("{frames:04}"),
            frames,
            warm: busiest.clone(),
            live: (2, 24),
            voices: Voices::Audible,
            bed: Bed::Floor,
        });
    }

    // The worst case: the busiest buffer a sing-along with typing under it
    // passes through.
    let (_, busiest, stats) = observe(Script::Celebration, BUFFERS_PER_BAR * 4);
    assert!(
        stats.peak_live >= 10,
        "the celebration script peaked at {} live voices — that is not a full \
         bar under a typing flood, so it is not the polyphony budget's worst \
         case",
        stats.peak_live
    );
    out.push(Workload {
        kind: "celebration",
        param: "peak".into(),
        frames: FRAMES,
        warm: busiest.expect("a busy script has a busiest buffer"),
        live: (10, 28),
        voices: Voices::Audible,
        bed: Bed::Floor,
    });

    out
}

/// Cross-workload proof for the tournament sweep: every candidate must render
/// DIFFERENT bytes from `Current`, which is the only externally visible
/// evidence that `set_bed_variant` really re-routed `bed_sample` — the
/// `Silence` candidate emits no bed at all, so nothing else could witness it.
fn verify_variants_diverge(rows: &[(&Workload, Emission)]) {
    let current = rows
        .iter()
        .find(|(w, _)| w.kind == "bed" && w.param == "water_on")
        .map(|(_, e)| e.checksum)
        .expect("bed/water_on is the Current baseline");
    for (w, e) in rows.iter().filter(|(w, _)| w.kind == "bed_variant") {
        assert_ne!(
            e.checksum, current,
            "bed_variant/{}: renders byte-identically to BedVariant::Current — \
             `set_bed_variant` did not reach the bed mixer, so this workload \
             times the shipping path under a candidate's name",
            w.param
        );
    }
}

// ---------------------------------------------------------------------------
// The benchmarks
// ---------------------------------------------------------------------------

fn trail_synth_render(c: &mut Criterion) {
    let workloads = workloads();
    let rows: Vec<(&Workload, Emission)> = workloads
        .iter()
        .map(|w| (w, verify_reaches_target(w)))
        .collect();
    verify_variants_diverge(&rows);

    println!(
        "\nTRAIL SYNTH — emitted volume per rendered buffer (48 kHz stereo; a \
         512-frame buffer is 10.667 ms of audio)\n\
         {:<26} {:>6} {:>7} {:>8} {:>8} {:>8} {:>6}  checksum",
        "workload", "frames", "voices", "samples", "nonzero", "peak", "bed"
    );
    for (w, e) in &rows {
        println!(
            "{:<26} {:>6} {:>7} {:>8} {:>8} {:>8.4} {:>6.3}  {:016x}",
            format!("{}/{}", w.kind, w.param),
            w.frames,
            e.live,
            e.samples,
            e.nonzero,
            e.peak,
            e.bed_level,
            e.checksum
        );
    }
    println!();

    let mut group = c.benchmark_group("trail_synth_render");
    for (w, _) in &rows {
        // Frames per second of wall time; divide by 48_000 for the realtime
        // factor (how many audio streams one core could carry).
        group.throughput(Throughput::Elements(w.frames as u64));
        group.bench_with_input(BenchmarkId::new(w.kind, &w.param), w, |b, w| {
            let mut buf = vec![0.0f32; w.frames * CHANNELS];
            match w.voices {
                // Silence is a proven fixed point (see `verify_reaches_target`),
                // so this one skips the clone: at a few hundred nanoseconds a
                // batched timer's per-iteration overhead would be a visible
                // fraction of the very number this workload exists to report.
                Voices::Quiet => {
                    let mut s = w.warm.clone();
                    b.iter(|| {
                        s.render(&mut buf);
                        black_box(buf[0])
                    });
                }
                // Cloned OUTSIDE the timed region: every iteration renders the
                // identical warm state instead of a tail that empties out.
                Voices::Audible => b.iter_batched_ref(
                    || w.warm.clone(),
                    |s| {
                        s.render(&mut buf);
                        black_box(buf[0])
                    },
                    BatchSize::PerIteration,
                ),
            }
        });
    }
    group.finish();
}

fn trail_synth_script(c: &mut Criterion) {
    let scripts: [(&str, Script, usize); 4] = [
        // The OFF cost per second of audio: 94 callbacks that find nothing to
        // play. This is what most users pay most of the time.
        ("silent_1s", Script::Idle, BUFFERS_PER_SECOND),
        (
            "typing_1s_lumen",
            Script::Typing {
                style: GlowStyle::Lumen,
                voice: SoundVoice::Style,
                bed: false,
            },
            BUFFERS_PER_SECOND,
        ),
        (
            "typing_1s_mech",
            Script::Typing {
                style: GlowStyle::Lumen,
                voice: SoundVoice::Mech,
                bed: false,
            },
            BUFFERS_PER_SECOND,
        ),
        // One whole sing-along bar with a typing flood under it.
        ("celebration_bar", Script::Celebration, BUFFERS_PER_BAR),
    ];

    let mut group = c.benchmark_group("trail_synth_script");
    for (name, script, buffers) in scripts {
        // Prove the script drives the engine before timing it: an idle script
        // must stay on the silent path for every buffer, a driven one must be
        // busy for every buffer and must get real events past the governor.
        let (_, _, stats) = observe(script, buffers);
        if matches!(script, Script::Idle) {
            assert_eq!(
                (
                    stats.pushed,
                    stats.admitted,
                    stats.peak_live,
                    stats.busy_buffers
                ),
                (0, 0, 0, 0),
                "{name}: the idle script must never leave the is_quiet() path"
            );
        } else {
            assert!(
                stats.admitted >= 6 && stats.admitted <= stats.pushed,
                "{name}: admitted {} of {} pushes — the governor ate the script",
                stats.admitted,
                stats.pushed
            );
            assert_eq!(
                stats.busy_buffers, buffers,
                "{name}: only {} of {buffers} buffers had a live voice",
                stats.busy_buffers
            );
        }

        group.throughput(Throughput::Elements((buffers * FRAMES) as u64));
        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            let mut buf = vec![0.0f32; FRAMES * CHANNELS];
            let mut stats = Stats::default();
            let mut busiest = None;
            b.iter(|| {
                // `OBS == false`: every observation above compiles out, so this
                // times the host's push/render interleave and nothing else.
                black_box(play::<false>(
                    black_box(script),
                    buffers,
                    &mut buf,
                    &mut stats,
                    &mut busiest,
                ))
            });
        });
    }
    group.finish();
}

fn trail_synth_push(c: &mut Criterion) {
    // A typing steady state whose `since_voice` is past MIN_GAP, so the next
    // push is ADMITTED (the warm-up ends 5 buffers after its last push).
    let (admitting, _) = typing_states(GlowStyle::Lumen, SoundVoice::Style, false);
    // The same state one keystroke later: `since_voice` is 0, so the next push
    // is THINNED — the governor's reject edge, which still pays the rate
    // estimate's `exp` and the bed's style/tone latch before returning.
    let thinning = {
        let mut s = admitting.clone();
        s.push(event(
            GlowStyle::Lumen,
            SoundVoice::Style,
            SoundGesture::Trail(SoundKind::Typed),
            false,
        ));
        s
    };

    let typed = event(
        GlowStyle::Lumen,
        SoundVoice::Style,
        SoundGesture::Trail(SoundKind::Typed),
        false,
    );
    let riff = event(
        GlowStyle::RainbowKitty,
        SoundVoice::Style,
        SoundGesture::Celebration(CelebrationGesture::riff_bar(6, SING_SIG)),
        false,
    );
    // The host-gated OFF event: reduced motion, `trail_sound_volume = 0` or a
    // per-source enable turned off all resolve to `gain == 0.0`.
    let off = SoundEvent { gain: 0.0, ..typed };

    // Reach proofs, each on a clone of the state its benchmark starts from.
    {
        let mut s = admitting.clone();
        s.push(typed);
        assert_eq!(
            s.live_voices(),
            admitting.live_voices() + 1,
            "push/typed_admitted: the governor thinned the event this workload \
             exists to measure (Lumen designs exactly one voice per keystroke)"
        );

        let mut s = thinning.clone();
        s.push(typed);
        assert_eq!(
            s.live_voices(),
            thinning.live_voices(),
            "push/typed_thinned: the event was ADMITTED — this workload is the \
             min-gap reject edge, not the spawn path"
        );

        let mut s = admitting.clone();
        s.push(riff);
        let spawned = s.live_voices() - admitting.live_voices();
        assert!(
            (3..=10).contains(&spawned),
            "push/riff_bar: spawned {spawned} voices, outside a bar's 3..=10 — \
             the riff did not reach `design_celebration`"
        );

        // The OFF path must be provably INERT: `push` returns before it mutates
        // anything, so a thousand gated events must leave the synth rendering
        // the identical bytes.
        let mut buf = vec![0.0f32; FRAMES * CHANNELS];
        let mut reference = admitting.clone();
        reference.render(&mut buf);
        let unfed = checksum(&buf);
        let mut s = admitting.clone();
        for _ in 0..1_000 {
            s.push(off);
        }
        assert_eq!(
            s.live_voices(),
            admitting.live_voices(),
            "push/rejected_off: a gain-0 event spawned a voice"
        );
        s.render(&mut buf);
        assert_eq!(
            checksum(&buf),
            unfed,
            "push/rejected_off: 1000 gain-0 events changed the rendered audio — \
             the gate is not the untouched early-out this workload times"
        );
    }

    let mut group = c.benchmark_group("trail_synth_push");
    group.bench_function("typed_admitted", |b| {
        b.iter_batched_ref(
            || admitting.clone(),
            |s| s.push(black_box(typed)),
            // 64 clones per batch amortizes the timer over a sub-microsecond
            // call without letting criterion hold thousands of 6 KB synths.
            BatchSize::NumIterations(64),
        );
    });
    group.bench_function("typed_thinned", |b| {
        b.iter_batched_ref(
            || thinning.clone(),
            |s| s.push(black_box(typed)),
            BatchSize::NumIterations(64),
        );
    });
    group.bench_function("riff_bar", |b| {
        b.iter_batched_ref(
            || admitting.clone(),
            |s| s.push(black_box(riff)),
            BatchSize::NumIterations(64),
        );
    });
    group.bench_function("rejected_off", |b| {
        // Proven inert above, so one resident synth is a valid fixed point: no
        // per-iteration clone to pay for, and none to distort the number.
        let mut s = admitting.clone();
        b.iter(|| s.push(black_box(off)));
    });
    group.finish();
}

// ===========================================================================
// tone/ — the typing-mood classifier (audit finding TN-1)
// ===========================================================================
//
// A DIFFERENT THREAD, A DIFFERENT DEADLINE. Unlike everything above, the tone
// classifier never touches the audio callback OR the frame path: it runs on
// the winit KEY-PRESS thread. The host (`aterm_gui::tone_infer::ToneTracker`)
// keeps a rolling window of the committed text before the caret — at most
// BUF_CAP = 160 chars (`TONE_WINDOW_CAP` below) — and calls
// `ToneModel::classify_opt` at most once per 6 keystrokes or once per 500 ms,
// whichever opens first. SLOW TYPING THEREFORE INFERS ON EVERY KEYSTROKE, on
// the input path whose latency budget is the sacred one. The in-crate
// `classification_fits_the_line_budget` test pins a <100 us/line CEILING; a
// ceiling proves "under 100 us", not "did my change make it faster", which is
// what this group exists to answer. When tone inference is inactive
// (`tone_melody` off, sound muted, no live audio host) the model is never
// even loaded and none of this code runs — the truly-disabled path costs this
// crate nothing, which is why the idle arm here is the ABSTENTION path (the
// cheapest call a live tracker can make: a near-empty window right after
// Enter), not a disabled engine.
//
// WHAT ONE INFERENCE IS. `classify_opt` -> `scores`, two halves:
//   1. FEATURIZE — `for_each_ngram_bucket` FNV-hashes every 1/2/3-gram of the
//      window (exactly 3*chars - 3 of them when no whitespace runs collapse)
//      and, per n-gram, gathers a 32-float embedding row into the mean-pool
//      sum. Cost scales with WINDOW LENGTH.
//   2. CLASSIFY — a fixed 32x64 matmul + 64 tanh + 64x5 output + softmax.
//      Cost is CONSTANT per call.
// TN-1 lives entirely in half 2: the mean-pool normaliser `inv = 1/n` is
// multiplied inside the 2048-iteration matmul instead of scaling the pooled
// vector once, and the inner loop strides `w1` by 256 bytes so the 8 KB
// matrix is walked 64 times scattered. Both halves of the audited fix are
// bit-identical by construction (same operands, same order — the verdict is
// an argmax over near-tied softmax scores, so bit identity is the acceptance
// bar), which means the ONLY way to price the fix is a time A/B. That shapes
// the workloads:
//   * the fix must shift `classify/*` by the SAME ABSOLUTE amount at 6, 40
//     and 160 chars — the matmul does not scale with the window, so a delta
//     that grows with length is NOT TN-1's fix;
//   * `classify/ascii_006` is the sharpest needle: at 6 chars the featurizer
//     is 15 hashes and the fixed matmul dominates the call;
//   * `featurize/*` times half 1's HASHING alone, so `classify - featurize`
//     at equal length is the gather + matmul + softmax term the fix lands in.
//     (The featurize body keeps the bucket values live through an XOR fold —
//     an EMPTY closure would let the optimiser delete the very masking/
//     hashing being timed, since the returned count alone needs neither. It
//     still understates production featurization, which also gathers 32
//     floats per n-gram; that gather is deliberately left on the classify
//     side of the split.)
//
// WHY ASCII AND CJK BOTH. Equal CHAR counts give equal N-GRAM counts by
// construction (the featurizer is script-agnostic — that is its whole design
// point), so the ascii/cjk pair at the same length isolates embedding-gather
// cache behaviour (CJK unigrams scatter across different rows of the 256 KB
// dequantized embedding table) from featurization cost. `mixed_160` is the
// shape the budget test uses — prose + CJK + a shell command — the realistic
// worst window. The `_160` workloads sit AT the host's window cap:
// production cannot hand this code a longer input, so there is no bigger
// state to price.
//
// GUARDS, TWO-SIDED BY CONSTRUCTION. The classifier is a pure function of
// (frozen text, frozen weights) — no clock, no rng, no allocation — so the
// guards pin EXACT values rather than envelopes: the n-gram count must equal
// 3*chars - 3 exactly (the bench texts contain no whitespace runs, and a
// drift here is a FEATURIZATION change, which would silently invalidate the
// shipped weights — training and inference share this exact function), the
// distinct-bucket count is pinned exactly, the argmax verdict is pinned
// exactly, and the softmax must be a genuine distribution (sum ~ 1, every
// class > 0, top strictly between uniform 0.2 and 1.0). An empty or
// degenerate input cannot pass: it produces `None` and fails every
// Some-shaped guard, and the abstain arms assert the mirror image (exact
// count BELOW the evidence floor, `None`, and the neutral fold). Determinism
// itself is CHECKED (two runs, bit-compared), which is what licenses the
// resident-scratch `b.iter` loop — production reuses one `ToneScratch`
// forever, and so does the timed body.
//
// EMITTED VOLUME: the hashed n-gram count per window is this classifier's
// volume, recorded in `tone_volume` as counts-as-nanoseconds (the
// cursor_glow_volume idiom: 1 ns == 1 n-gram) so a featurization change
// lands in criterion's baseline/A-B machinery instead of only this file's
// asserts.
//
// FIRST RUN, for orientation only — re-measure, never cite these (Apple M5
// Max, macOS 26.5, `--warm-up-time 1 --measurement-time 2`, other builds
// sharing the machine; time per INFERENCE):
//   abstain/empty 3.3 ns   abstain/thin 25.4 ns
//   classify ascii 6/40/160 chars: 1.18 / 2.13 / 4.57 us
//   classify cjk   6/40/160 chars: 1.18 / 2.05 / 4.83 us   mixed_160 4.82 us
//   featurize ascii_160 394 ns    featurize cjk_160 678 ns
// Reading those against TN-1: the classify-over-length line has a ~1.04 us
// intercept (ascii slope ~22 ns/char over 6..160), and intercept ~= the
// fixed matmul + softmax half — i.e. the term TN-1's fix must move, ~88 % of
// a 6-char call and ~23 % of a cap-length one. `classify - featurize` at 160
// chars (4.57 - 0.39 = 4.18 us ascii) is gather + matmul + softmax; the
// hash-only featurize half is ~0.8 ns/n-gram ascii, ~1.4 cjk (multi-byte
// folds). The worst production window costs ~4.9 us — 20x inside the 100 us
// contract — so TN-1 stays impact-low exactly as audited; this group's job
// is to make its bit-identical fix PRICEABLE, and to catch the day an
// innocent-looking model change stops being cheap.

/// `aterm_gui::tone_infer::BUF_CAP`, mirrored (it is private to the host, and
/// widening a host's visibility to bench a callee is backwards): the rolling
/// char window the tracker classifies is never longer than this, so the
/// `_160` workloads sit AT the production input cap.
const TONE_WINDOW_CAP: usize = 160;

/// ASCII prose, >= `TONE_WINDOW_CAP` chars, SINGLE spaces only — a
/// whitespace run collapses inside the featurizer and would silently shrink
/// the n-gram count the guards pin (the count guard fails loudly if this
/// stops being true). The register is deliberately the loud Frustrated
/// surface the corpus trains on: an unambiguous line keeps the pinned
/// verdict far from a coin flip.
const TONE_ASCII: &str = "why does the build keep failing on me again ugh i swear i \
     fixed this exact bug last week and now the linker is angry about something \
     new every single time i touch it honestly";

/// CJK text (Japanese + Chinese + a Korean laughter token), no whitespace at
/// all: same char counts as the ASCII texts, so identical n-gram counts with
/// a completely different embedding-row access pattern.
const TONE_CJK: &str = "なんでまた壊れたの今日はとてもいい天気ですね太棒了我们成功了そ\
     れはいいですねゆっくりで大丈夫です为什么又坏了ㅋㅋㅋㅋ너무웃겨ビルドがまた失敗し\
     て本当に困っています今度こそ直したと思ったのにリンカがまた怒っている毎回同じエラ\
     ーで嫌になるでも諦めないで頑張りましょう成功するまで何度でも試すつもりです応援し\
     ていますありがとうございました";

/// The budget test's shape, extended past the window cap: multilingual prose
/// with a shell command in the middle — the realistic worst window.
const TONE_MIXED: &str = "why does the build break every time i touch this file \
     为什么又坏了 なんでまた壊れたの systemctl restart nginx --now ok let me try \
     again 다시 해볼게요 git rebase --continue && cargo test --workspace \
     ちょっと待ってね done";

/// What a tone workload must prove about its classification output before it
/// may be timed.
#[derive(Clone, Copy)]
enum ToneExpect {
    /// Below [`MIN_NGRAMS`] the model must return `None` (and `classify`
    /// folds to the neutral Technical). The cheap path a real host hits right
    /// after Enter clears the window.
    Abstain,
    /// The exact argmax verdict of the shipped weights on this frozen text.
    /// Pinned, not ranged: inference is bit-deterministic, so a moved verdict
    /// is a changed model or featurizer, never noise — and the in-crate
    /// conformance tests already pin verdicts this way.
    Classified(Tone),
}

#[derive(Clone)]
struct ToneWorkload {
    /// "abstain" | "classify" | "featurize" — also selects the timed body.
    kind: &'static str,
    param: &'static str,
    text: String,
    /// Effective chars (== chars handed in: the texts have no whitespace
    /// runs, and the exact-count guard fails if that drifts).
    chars: usize,
    /// Exact distinct-bucket count over the window, pinned because both the
    /// featurizer and the text are frozen: the hash constants ARE the trained
    /// weights' view of text, so a drift here invalidates the model asset —
    /// exactly the change this pin exists to catch.
    distinct: usize,
    expect: ToneExpect,
}

/// First `n` chars of `base` — the texts are real prose truncated, never
/// cycled (cycling repeats n-grams and flatters the embedding-gather cache).
fn tone_take(base: &str, n: usize) -> String {
    assert!(
        base.chars().count() >= n,
        "tone bench text is only {} chars, need {n}",
        base.chars().count()
    );
    base.chars().take(n).collect()
}

fn tone_workloads() -> Vec<ToneWorkload> {
    let mut out = Vec::new();
    // The idle/trivial arms: what the tracker pays when the window carries no
    // evidence. `empty` is the just-cleared window (zero n-grams — the
    // featurizer sees no chars at all); `thin` is two typed chars (3 n-grams,
    // REAL hashing, still below the MIN_NGRAMS = 6 evidence floor). Both must
    // abstain; `thin` proves the floor is what stops them, not an accidental
    // empty input.
    out.push(ToneWorkload {
        kind: "abstain",
        param: "empty",
        text: String::new(),
        chars: 0,
        distinct: 0,
        expect: ToneExpect::Abstain,
    });
    out.push(ToneWorkload {
        kind: "abstain",
        param: "thin",
        text: "ok".into(),
        chars: 2,
        distinct: 3,
        expect: ToneExpect::Abstain,
    });
    // The classify sweep: 6 chars (the smallest window past the evidence
    // floor for full words), 40 (a typical part-line), 160 (the host cap).
    // Verdicts and distinct-bucket counts are the measured values of the
    // shipped weights on these frozen texts — see the guard docs for why
    // exact pins are correct here.
    // ("why do" reading as Excited is a fact about the shipped weights, not a
    // claim this file makes about six chars of text — the pin only has to be
    // STABLE, and bit-determinism makes it so.)
    let sweep: [(&'static str, &'static str, usize, Tone, usize); 7] = [
        ("ascii_006", TONE_ASCII, 6, Tone::Excited, 15),
        ("ascii_040", TONE_ASCII, 40, Tone::Frustrated, 92),
        (
            "ascii_160",
            TONE_ASCII,
            TONE_WINDOW_CAP,
            Tone::Frustrated,
            242,
        ),
        ("cjk_006", TONE_CJK, 6, Tone::Frustrated, 14),
        ("cjk_040", TONE_CJK, 40, Tone::Frustrated, 99),
        ("cjk_160", TONE_CJK, TONE_WINDOW_CAP, Tone::Frustrated, 342),
        (
            "mixed_160",
            TONE_MIXED,
            TONE_WINDOW_CAP,
            Tone::Technical,
            301,
        ),
    ];
    for (param, base, n, want, distinct) in sweep {
        out.push(ToneWorkload {
            kind: "classify",
            param,
            text: tone_take(base, n),
            chars: n,
            distinct,
            expect: ToneExpect::Classified(want),
        });
    }
    // The featurize halves of the two cap-length scripts, sharing text and
    // pins with their classify twins so the subtraction is exact.
    for param in ["ascii_160", "cjk_160"] {
        let twin = out
            .iter()
            .find(|w| w.kind == "classify" && w.param == param)
            .expect("featurize twins mirror classify workloads")
            .clone();
        out.push(ToneWorkload {
            kind: "featurize",
            ..twin
        });
    }
    out
}

/// Everything one workload's run showed — gathered for ALL workloads first,
/// printed as a table, and only then asserted, so a broken pin still prints
/// the full evidence it should be corrected from.
struct ToneEvidence {
    ngrams: usize,
    distinct: usize,
    /// Two full `scores` runs were bit-identical.
    deterministic: bool,
    verdict: Option<Tone>,
    sum: f32,
    top: f32,
    min: f32,
    checksum: u64,
}

fn observe_tone(w: &ToneWorkload, m: &ToneModel) -> ToneEvidence {
    let mut seen = vec![false; BUCKETS];
    let mut distinct = 0usize;
    let ngrams = for_each_ngram_bucket(&w.text, |b| {
        if !seen[b] {
            seen[b] = true;
            distinct += 1;
        }
    });
    let mut s1 = ToneScratch::default();
    let mut s2 = ToneScratch::default();
    let a = m.scores(&w.text, &mut s1);
    let b = m.scores(&w.text, &mut s2);
    let deterministic = match (&a, &b) {
        (None, None) => true,
        (Some(x), Some(y)) => x
            .iter()
            .zip(y.iter())
            .all(|(p, q)| p.to_bits() == q.to_bits()),
        _ => false,
    };
    let verdict = m.classify_opt(&w.text, &mut s1);
    let (sum, top, min, cks) = match a {
        Some(sc) => (
            sc.iter().sum::<f32>(),
            sc.iter().fold(0.0f32, |acc, &x| acc.max(x)),
            sc.iter().fold(f32::MAX, |acc, &x| acc.min(x)),
            checksum(&sc),
        ),
        None => (0.0, 0.0, 0.0, 0),
    };
    ToneEvidence {
        ngrams,
        distinct,
        deterministic,
        verdict,
        sum,
        top,
        min,
        checksum: cks,
    }
}

/// Prove the workload reaches the code it claims — the featurizer really
/// hashed, the matmul really ran (or provably did not, for the abstain arms)
/// — before a nanosecond is timed.
fn verify_tone_reaches_target(w: &ToneWorkload, e: &ToneEvidence) {
    // Exact from BOTH sides: 3*chars - 3 n-grams, no more, no fewer. An empty
    // input yields 0 and cannot impersonate a real window; a featurization
    // change (new order, changed folding) moves the count and fails here.
    let expected = if w.chars == 0 { 0 } else { 3 * w.chars - 3 };
    assert_eq!(
        e.ngrams, expected,
        "tone {}/{}: {} n-grams from {} chars, expected exactly {expected} — \
         either the text grew a whitespace run or the featurizer changed shape \
         (which would invalidate the shipped weights)",
        w.kind, w.param, e.ngrams, w.chars
    );
    assert_eq!(
        e.distinct, w.distinct,
        "tone {}/{}: {} distinct buckets, pinned {} — the FNV stream moved, \
         which is a featurization change, not noise",
        w.kind, w.param, e.distinct, w.distinct
    );
    assert!(
        e.deterministic,
        "tone {}/{}: two runs over the same text disagreed at the bit level — \
         the purity that licenses the resident-scratch timing loop is gone",
        w.kind, w.param
    );
    match w.expect {
        ToneExpect::Abstain => {
            assert!(
                e.ngrams < MIN_NGRAMS,
                "tone {}/{}: {} n-grams reaches the evidence floor ({MIN_NGRAMS}) \
                 — this arm exists to time the abstention path and would be \
                 timing a full inference under an idle arm's name",
                w.kind,
                w.param,
                e.ngrams
            );
            assert_eq!(
                e.verdict, None,
                "tone {}/{}: the model spoke on evidence below the floor",
                w.kind, w.param
            );
        }
        ToneExpect::Classified(want) => {
            assert!(
                e.ngrams >= MIN_NGRAMS,
                "tone {}/{}: only {} n-grams — below the evidence floor, so \
                 `scores` would return None before the matmul this workload \
                 exists to reach",
                w.kind,
                w.param,
                e.ngrams
            );
            assert_eq!(
                e.verdict,
                Some(want),
                "tone {}/{}: verdict moved off the pinned {want:?} — inference \
                 is bit-deterministic, so this is a changed model/featurizer \
                 (or a fix that was NOT the bit-identical one TN-1 requires)",
                w.kind,
                w.param
            );
            // A genuine distribution, bounded from BOTH sides: sum ~ 1 and
            // min > 0 prove the softmax ran over finite logits; top > 0.2
            // (uniform) proves a real preference — a degenerate all-equal
            // output cannot pass; top < 1.0 proves no class underflowed to
            // fake certainty.
            assert!(
                e.sum > 0.999 && e.sum < 1.001,
                "tone {}/{}: softmax sum {} is not a distribution",
                w.kind,
                w.param,
                e.sum
            );
            assert!(
                e.top > 0.2 && e.top < 1.0,
                "tone {}/{}: top score {} outside (0.2, 1.0) — uniform or \
                 degenerate output",
                w.kind,
                w.param,
                e.top
            );
            assert!(
                e.min > 0.0,
                "tone {}/{}: a class underflowed to exactly zero",
                w.kind,
                w.param
            );
        }
    }
}

/// Record a COUNT as a criterion measurement: the reported "time" in
/// NANOSECONDS is the item count (1 ns == 1 hashed n-gram), so counts get
/// baselines and regression verdicts exactly like timings. The idiom — spin
/// loop and `k % 4` jitter included — is `cursor_glow_tick.rs`'s
/// `bench_count`, where both non-ceremony parts are documented at length: the
/// spin burns wall time proportional to `iters` so criterion's warm-up
/// (which measures WALL time, not the returned sample) terminates, and the
/// ~3 ns jitter spread over a whole sample keeps the distribution's variance
/// non-zero so criterion's KDE plot does not divide by zero.
fn tone_bench_count(g: &mut BenchmarkGroup<'_, WallTime>, id: &str, count: usize) {
    assert!(
        count > 0,
        "{id}: a zero count cannot be recorded as a duration — only workloads \
         with a proven positive n-gram count are recorded"
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

fn tone_model(c: &mut Criterion) {
    let m = tone::builtin().expect("the shipped tone weight asset must verify");
    let workloads = tone_workloads();
    // PROVE FIRST, TIME SECOND — but gather ALL evidence before asserting any
    // of it, so a broken pin prints the full table it should be corrected
    // from instead of dying on the first row.
    let rows: Vec<(&ToneWorkload, ToneEvidence)> =
        workloads.iter().map(|w| (w, observe_tone(w, m))).collect();

    println!(
        "\nTONE — typing-mood classifier evidence (window chars -> hashed \
         n-grams -> softmax verdict; the host window caps at \
         {TONE_WINDOW_CAP} chars and infers at most once per 6 keys / 500 ms)\n\
         {:<20} {:>5} {:>6} {:>8}  {:<10} {:>7} {:>9} {:>7}  checksum",
        "workload", "chars", "ngrams", "distinct", "verdict", "top", "min", "sum"
    );
    for (w, e) in &rows {
        println!(
            "{:<20} {:>5} {:>6} {:>8}  {:<10} {:>7.4} {:>9.2e} {:>7.4}  {:016x}",
            format!("{}/{}", w.kind, w.param),
            w.chars,
            e.ngrams,
            e.distinct,
            e.verdict.map_or("abstain", Tone::label),
            e.top,
            e.min,
            e.sum,
            e.checksum
        );
    }
    println!();
    for (w, e) in &rows {
        verify_tone_reaches_target(w, e);
    }

    let mut group = c.benchmark_group("tone");
    for (w, _) in &rows {
        match w.kind {
            // The abstain arms carry no throughput: the per-CALL time is the
            // number (they run first, before any throughput is set on the
            // group — group throughput is sticky across benches).
            "abstain" => {
                group.bench_function(BenchmarkId::new(w.kind, w.param), |b| {
                    // Resident scratch, exactly as production: the tracker
                    // owns one `ToneScratch` forever. Purity (checked above)
                    // is what makes reuse sound.
                    let mut scratch = ToneScratch::default();
                    b.iter(|| black_box(m.classify_opt(black_box(w.text.as_str()), &mut scratch)));
                });
            }
            "classify" => {
                // elem/s == chars/s through the classifier; per-inference
                // time is the printed number. Cadence context: at the host's
                // throttle this runs at most ~12/s (6-key batches at fast
                // typing) or once per keystroke at slow typing.
                group.throughput(Throughput::Elements(w.chars as u64));
                group.bench_function(BenchmarkId::new(w.kind, w.param), |b| {
                    let mut scratch = ToneScratch::default();
                    b.iter(|| black_box(m.classify_opt(black_box(w.text.as_str()), &mut scratch)));
                });
            }
            "featurize" => {
                group.throughput(Throughput::Elements(w.chars as u64));
                group.bench_function(BenchmarkId::new(w.kind, w.param), |b| {
                    b.iter(|| {
                        // XOR-fold the buckets so the hash/mask cannot be
                        // dead-code-eliminated under an ignored argument —
                        // one register op per n-gram, noise against the FNV
                        // folds being timed.
                        let mut acc = 0usize;
                        let n = for_each_ngram_bucket(black_box(w.text.as_str()), |bkt| acc ^= bkt);
                        black_box((acc, n))
                    });
                });
            }
            other => unreachable!("unknown tone workload kind {other}"),
        }
    }
    group.finish();

    // The counts, as measurements — near-free to produce, so the group runs
    // at criterion's floor. Featurize twins share their classify twin's text,
    // so each unique window is recorded once.
    let mut vol = c.benchmark_group("tone_volume");
    vol.warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(10))
        .sample_size(10);
    for (w, e) in &rows {
        if w.kind != "featurize" && e.ngrams > 0 {
            tone_bench_count(&mut vol, &format!("ngrams/{}", w.param), e.ngrams);
        }
    }
    vol.finish();
}

criterion_group!(
    benches,
    trail_synth_render,
    trail_synth_script,
    trail_synth_push,
    tone_model
);
criterion_main!(benches);
