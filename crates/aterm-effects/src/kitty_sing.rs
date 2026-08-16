// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SING-ALONG — the held-key celebration (owner: "add a repeated
//! key detection where you go FULL NYAN SING SONG RAINBOW if you are holding
//! the same key and music notes appear and the cursor cat is dancing and
//! singing … really go nuts"). Style-gated to `GlowStyle::RainbowKitty` by the host.
//!
//! Three pure units live here, one per concern:
//!
//! * [`KittySing`] — the DETECTOR: the same printable character repeating at
//!   key-repeat cadence through the typed-provenance input seam (the
//!   `kitty_summon` seam in `app_input` — committed key presses ONLY; PTY
//!   output, `cat`, and pastes can never arm it, and like every effect on
//!   that path it is Source-agnostic). [`SING_ARM_REPEATS`] consecutive
//!   repeats arm SING-ALONG; a backspace, a break key, a session switch, or
//!   simply letting go (no repeat within [`SING_REPEAT_GAP`]) starts a
//!   graceful [`SING_WIND_DOWN`] crossfade — the drive eases 1 → 0, never a
//!   hard cut. A DIFFERENT character that itself starts repeating is not a
//!   release at all: it is a KEY SWITCH ([`KEY_SWITCH_REPS`]) — the singer
//!   stays at full drive and the song reopens on the new key's own verse at
//!   the next bar boundary (the SECTION seam of the held-key instrument).
//!   Bounded per-window state, exactly like `kitty_summon::TypedKittySummon`.
//! * The BEAT CLOCK: [`sing_beat`]/[`sing_bar`] derive a deterministic beat
//!   phase from the arm instant at [`SING_BPM`]. The audio riff runs on the
//!   synth's own SAMPLES-based clock (`trail_sound`, one
//!   `CelebrationGesture::RiffBar` per visual bar), so the two clocks are the
//!   same tempo anchored at the same arm instant but can skew by up to one
//!   host audio buffer plus event latency — ± ~60 ms in practice. That is the
//!   documented SYNC TOLERANCE: at [`SING_BPM`] a beat is 400 ms, so worst-
//!   case skew is under a sixth of a beat — the bob and the riff still read
//!   as one dance. (Sample-locking the visuals to the audio queue would drag
//!   render timing into the audio thread for a difference below perception.)
//! * [`MusicNotes`] — the ♪/♫ SPRITE FIELD: a RING-CAPPED
//!   ([`MAX_NOTES`] = 16) pool of music-note sprites streaming from the
//!   singing cat, spawned on half-beats, bobbing upward and fading out
//!   ([`NoteSprite`] carries cell-relative offsets; the emission itself lives
//!   in `word_decorations::kitty_cursor`, so notes are structurally cat-only —
//!   and load-shed sheds them with the rest of the sparkle branch).
//!   [`bake_note`] uses authored path fills and deterministic host baking,
//!   baked WHITE and tinted per sprite through the `FreeSprite::tint` channel,
//!   so the whole rainbow costs two atlas tiles.
//!
//! ## Arms (host policy, engine mechanism)
//!
//! * REDUCED MOTION — static celebration: the detector still arms (arming is
//!   input classification, not animation), the cat presents a still singing
//!   pose (no dance loop — `kitty_cursor::static_frame`), and
//!   [`MusicNotes::frames`] pins every note to a fixed offset (notes without
//!   bob, fade only). The riff still plays if sound is on.
//! * MUTED / UNFOCUSED — visuals only: the host resolves the riff gain to
//!   `None` exactly like the trail-sound law (a background recording may
//!   animate; it must never make the Mac speak).
//! * LOAD-SHED — notes ride the sparkle emission branch, so the shed latch
//!   sheds them with every other decoration.

use std::time::Duration;

use web_time::Instant;

use aterm_scene::{PathCmd, PathTransform, Tile, fill_path};

// ---------------------------------------------------------------------------
// The shared tempo
// ---------------------------------------------------------------------------

/// The celebration tempo. 150 BPM is a bouncy sing-song stride: fast enough
/// to read as "going nuts", slow enough that the beat-synced squash bounce
/// (≈2.5 Hz) stays a dance and not a vibration.
pub const SING_BPM: f32 = 150.0;

/// One beat in seconds (`60 / SING_BPM`).
pub const SING_BEAT_SECONDS: f32 = 60.0 / SING_BPM;

/// Beats per riff bar. The audio riff is scheduled one bar at a time
/// (`trail_sound::CELEBRATION_BAR_SECONDS` — pinned equal by a test there),
/// so the host pushes one `RiffBar` gesture per visual bar boundary.
pub const SING_BAR_BEATS: f32 = 4.0;

/// One riff bar in seconds.
pub const SING_BAR_SECONDS: f32 = SING_BEAT_SECONDS * SING_BAR_BEATS;

/// Bars in the celebration's authored FORM (the A A' B A" | C C' B' D
/// phrase the synth decodes from `bar % 8`). Pinned to the synth's
/// `CELEBRATION_PHRASE_BARS` by
/// `trail_sound::the_section_reopen_lands_on_the_forms_verse_opening`.
pub const SING_FORM_BARS: u64 = 8;

/// THE SECTION-REOPEN DECISION — where a committed KEY SWITCH re-enters the
/// form: the next multiple of [`SING_FORM_BARS`] strictly above `current`,
/// i.e. form slot 0, the new key's own A-section verse — never mid-form and
/// never the shared chorus block, so the new tune announces itself on the
/// very next bar boundary. Factored as its own seam on purpose: the
/// held-key system is growing into an INSTRUMENT (owner: "to get the cursor
/// to play your custom song, you press and hold different keys and
/// transition to the different tunes smoothly like that" — hold sustains a
/// key's verse, a switch is the next SECTION of the composition), and a
/// future instrument spec may swap this decision for one with a turnaround
/// FILL announcing the switch without touching the switch-commit plumbing.
#[must_use]
pub const fn section_reopen_bar(current: u64) -> u64 {
    (current / SING_FORM_BARS + 1) * SING_FORM_BARS
}

// ---------------------------------------------------------------------------
// Song signature
// ---------------------------------------------------------------------------

/// The FIXED BIJECTIVE SIGNATURE MIXER over a character's code point —
/// lowbias32-style (two xorshift + odd-multiply rounds, the constants proven
/// in the salvaged held-key-songs design). Every round is individually
/// invertible, so the whole map is a bijection on `u32`: DISTINCT CHARACTERS
/// CAN NEVER SHARE A SONG (`song_signature_mixer_is_bijective_over_all_chars`
/// proves the exact round trip), and nearby code points scatter far apart, so
/// `a` vs `b` is a different VERSE, not a nudged copy. Deliberately not a
/// synth-rng draw: a key's song is the same on every machine, every session,
/// every seed.
#[must_use]
pub fn song_signature(ch: char) -> u32 {
    let mut x = ch as u32;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

/// The signature of "nothing held": decodes in the synth to the untransposed
/// reference voicing — root 0 (`12 % 5 == 2` ⇒ degree offset 0) in the home
/// mode (`12 % 3 == 0`) — exactly where the old `key() == 0` neutral sat.
/// Pinned against the synth's decode by `trail_sound`'s
/// `neutral_signature_is_the_reference_voicing`.
pub const NEUTRAL_SIGNATURE: u32 = 12;

// ---------------------------------------------------------------------------
// The song arc (the SONG-BUILDER instrument)
// ---------------------------------------------------------------------------
//
// OWNER: "to get the cursor to play your custom song, you press and hold
// different keys and transition to the different tunes smoothly." The
// instrument layer: every key belongs to a SECTION CLASS (verse / chorus /
// bridge / percussive / breath) and a hold EARNS ENERGY per bar at the
// class's rate. Energy is the escalation the synth renders (build: lowpass,
// shimmer, sub) and the clap gate — carried in the riff payload
// (`trail_sound::CelebrationGesture::riff_bar_arc`), so the synth stays a
// pure function of the payload (the hand-over law) while the ARC — the
// stateful half — lives here, host-side, exactly like the detector.
//
// The payoff the switch mechanic was built for: energy INHERITS across a
// committed key switch, so a verse-key build cashed in on a chorus key
// peaks faster than any single hold — the switch is the instrument's core
// gesture, not a defect to smooth over.

/// The energy a fresh arc opens on: the song starts warm, not silent.
pub const ARC_START: f32 = 0.30;

/// Bars of INTRO: while fewer than this many bars have played (and no warm
/// carry skipped the intro), the RENDERED energy is capped at
/// [`ARC_INTRO_CAP`] — the band walks on stage before it plays loud.
pub const ARC_INTRO_BARS: u32 = 2;

/// The intro's render cap (internal energy still accrues uncapped — the cap
/// is presentation, not bookkeeping, so a chorus-key open still banks its
/// full earn).
pub const ARC_INTRO_CAP: f32 = 0.55;

/// Energy at/above which the arc reads PEAK.
pub const ARC_PEAK_GATE: f32 = 1.0;

/// Peak exit (hysteresis): once peaked, the arc stays Peak until energy
/// falls below this — no flapping at the gate.
pub const ARC_PEAK_EXIT: f32 = 0.9;

/// The energy ceiling. Above 1.0 is EARNED headroom: the synth clamps its
/// build at 1.0, but the raw value still crosses the clap gate and scales
/// the finale, so a long peak is worth holding.
pub const ARC_ENERGY_MAX: f32 = 1.25;

/// WARM RE-ARM window: a new hold within this many seconds of the wind-down
/// start carries [`ARC_CARRY_KEEP`] of the released energy (floored at
/// [`ARC_START`]) instead of opening cold. NOTE (pinned as intended): with
/// the phase-3 deferred wind-down the effective window past the RELEASE
/// stretches to ~3.4 s + carry slack — the band remembers you.
pub const ARC_CARRY_SECONDS: f32 = 6.0;

/// Fraction of the released energy a warm re-arm keeps.
pub const ARC_CARRY_KEEP: f32 = 0.75;

/// Minimum bars played for a release to earn the full finale cadence
/// (shorter performances take the plain fade — a "ta-daa" needs a song).
pub const ARC_FINALE_MIN_BARS: u32 = 4;

/// THE FILL RUNWAY: a key switch that COMMITS before this many seconds
/// into the sounding bar (beat 3.0) has room for the drummer's
/// announcement — the authored four-sixteenth turnaround fill on the
/// bar's LAST beat, in the OLD key ([`KittySing::take_fill_cue`]). A later
/// commit takes the foundation's audible reopen alone: zero added
/// latency, nothing fires early.
pub const FILL_CUE_RUNWAY: f32 = 1.2;

/// THE FINALE (owner-ratified): a gentle release after
/// [`ARC_FINALE_MIN_BARS`] bars doesn't just fade — the sounding bar
/// finishes, then ONE cadence bar plays a "ta-daa" in the released key
/// ([`KittySing::take_cadence`] →
/// `trail_sound::CelebrationGesture::RiffCadence`), the singer bows, and
/// the note-fireworks scale with the performance. The DRIVE defers its
/// wind-down until the cadence's tonic has landed (beat 2 =
/// [`CADENCE_DRIVE_HOLD`] after the cadence downbeat), then takes the
/// standard [`SING_WIND_DOWN`] smoothstep — worst-case post-release audio
/// ≈ 3.6 s (the ratified tail; reduced motion and the sub-four-bar release
/// keep today's plain fade).
pub const CADENCE_DRIVE_HOLD: f32 = 0.8;

/// THE BOW: at cadence beat 2 the singer dips over [`BOW_DOWN`] seconds,
/// holds [`BOW_HOLD`], and rises over [`BOW_RISE`] — delivered to the
/// renderer as a 0..=1 depth ([`KittySing::bow_depth`]).
pub const BOW_DOWN: f32 = 0.3;
/// Seconds the bow holds at full depth.
pub const BOW_HOLD: f32 = 0.4;
/// Seconds the bow takes to rise back to standing.
pub const BOW_RISE: f32 = 0.3;

/// Energy earned per bar, per section class (indexed by [`section_class`]):
/// verse 0.125, chorus 0.25, bridge 0.1667, percussive 0.20, and BREATH
/// −0.0625 — Space is the deliberate quiet passage that spends energy.
pub const ARC_EARN: [f32; 5] = [0.125, 0.25, 0.1667, 0.20, -0.0625];

/// The SECTION CLASS a held character plays as. Classes change the song's
/// TIMBRE, DYNAMICS and EARN RATE only — never rhythm, form, swing, tempo
/// or voice count (pinned synth-side by
/// `trail_sound::sections_change_timbre_not_the_clock_or_the_form`).
///
/// * 0 VERSE — consonant letters (shifted included): the workhorse build.
/// * 1 CHORUS — vowels `aeiouAEIOU`: the sing-along keys; fastest earn,
///   open gain, the chorus-key lift, the most brilliant strike.
/// * 2 BRIDGE — digits 0–9: moodier, a mid-weight strike.
/// * 3 PERCUSSIVE — ASCII punctuation/symbols: rhythmic, EARLY clap gate.
/// * 4 BREATH — Space: energy decays, dim gain, shimmer off — the quiet
///   passage a finale wants in front of it.
///
/// Off-map characters (IME text, non-ASCII letters and symbols) are class
/// 0: EVERY character still sings — the bijective signature already gives
/// each its own verse; the class only shapes how it is played.
#[must_use]
pub fn section_class(ch: char) -> u8 {
    if matches!(
        ch,
        'a' | 'e' | 'i' | 'o' | 'u' | 'A' | 'E' | 'I' | 'O' | 'U'
    ) {
        return 1;
    }
    if ch.is_ascii_digit() {
        return 2;
    }
    if ch == ' ' {
        return 4;
    }
    if ch.is_ascii_punctuation() {
        return 3;
    }
    0
}

/// The arc's presentation phase — derived, never stored (the lazy-derive
/// idiom every clock in this module already follows).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArcPhase {
    /// First [`ARC_INTRO_BARS`] bars of a cold arc: render energy capped.
    Intro,
    /// Earning toward the gate.
    Build,
    /// Energy crossed [`ARC_PEAK_GATE`]; holds until [`ARC_PEAK_EXIT`].
    Peak,
    /// Wind-down after a release that earned a finale
    /// (≥ [`ARC_FINALE_MIN_BARS`] bars).
    Outro,
}

/// The SONG ARC's folded state. Energy between events is DERIVED (base +
/// per-bar earn × boundaries since the base fold — a pure function of
/// `now`, like [`KittySing::drive`]); these scalars are the fold points.
/// The spec's `SongArc { phase, energy, bars_played, peak_bars }` realized
/// in the module's lazy idiom: `phase`/`bars_played` are derived reads
/// ([`KittySing::arc_phase`], [`KittySing::bars_played`]), `peak_bars` is
/// unconsumed by any payload or test and is deliberately not tracked.
#[derive(Clone, Copy, Debug)]
struct SongArc {
    /// Energy at the last fold.
    energy: f32,
    /// Raw bar-grid index the fold happened at.
    anchor_raw: u64,
    /// The peak latch (entry ≥ [`ARC_PEAK_GATE`], exit < [`ARC_PEAK_EXIT`]).
    peaked: bool,
    /// A warm carry opened at/above [`ARC_INTRO_CAP`]: no intro this time.
    intro_skip: bool,
    /// The EMBER a wind-down leaves behind: (wind-down start, energy at
    /// release). Consumed by the next arm's warm-carry decision. This is
    /// the one datum that deliberately survives [`KittySing::settle`] — the
    /// warm re-arm law needs memory across the rest; it goes stale (and is
    /// ignored) [`ARC_CARRY_SECONDS`] after the wind start.
    carry: Option<(Instant, f32)>,
}

impl Default for SongArc {
    fn default() -> Self {
        Self {
            energy: ARC_START,
            anchor_raw: 0,
            peaked: false,
            intro_skip: false,
            carry: None,
        }
    }
}

/// THE FINALE PLAN: one cadence bar owed to a finished performance —
/// due at the boundary after the sounding bar completes
/// (`armed_at + (last_bar + 1) × 1.6 s`), in the RELEASED key, at the
/// energy the performance earned. Built at a gentle release (or derived
/// for the pure-lazy lift), consumed once by the host, canceled by a
/// re-arm before due, vetoed outright by hard breaks and proven typing.
#[derive(Clone, Copy, Debug)]
struct Cadence {
    /// The cadence bar's downbeat.
    due: Instant,
    /// The released key's signature — your song ends in YOUR key.
    sig: u32,
    /// RAW arc energy at the release, ×200 (the cadence's gain scale and
    /// its fused-clap gate).
    energy_q: u8,
    /// Bars the performance played (the fireworks scale).
    span_bars: u16,
    /// Consumed — [`KittySing::take_cadence`] fires exactly once.
    fired: bool,
}

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Consecutive same-character repeats that arm SING-ALONG. Sixteen repeats at
/// OS key-repeat cadence is roughly half a second of deliberate holding — long
/// enough that bursty double/triple taps ("aaa" for emphasis) never trigger,
/// short enough that a held key blooms while the finger is still down.
pub const SING_ARM_REPEATS: u32 = 16;

/// Repeats that COMMIT a KEY SWITCH. A distinct character on a live
/// celebration is PROVISIONAL — it may be typing; once it repeats this many
/// times (each gap at cadence, real wall-time between presses) it has
/// proven it is HELD, and the switch commits: the singer stays on stage and
/// the form reopens on the new key's verse ([`section_reopen_bar`]). The
/// same count recovers a celebration whose glow is still fading — the
/// forgiveness law in [`KittySing::note_char`] — so a switch whose first
/// auto-repeat arrives late (the OS initial repeat delay runs 250–500 ms,
/// at or past [`SING_REPEAT_GAP`]) costs a breath, not a full re-earn.
/// Three at repeat cadence is ~60–160 ms of genuine holding: nothing a
/// doubled letter in ordinary typing can counterfeit. Cold arms still cost
/// the full [`SING_ARM_REPEATS`].
pub const KEY_SWITCH_REPS: u32 = 3;

/// Maximum gap between two presses of the SAME character for the run to
/// still read as key-repeat cadence. OS auto-repeat runs 30–120 ms between
/// repeats (plus an initial ~250–500 ms delay before the FIRST repeat, which
/// is why this is a per-gap bound and not a rate estimate); 250 ms admits
/// every real repeat setting while a human deliberately re-striking the same
/// key slower than 4 Hz reads as typing, not holding.
pub const SING_REPEAT_GAP: Duration = Duration::from_millis(250);

/// Wind-down crossfade length: on release/key-change the drive eases 1 → 0
/// over ~1 s (smoothstep — C¹ at both ends, so neither the visuals nor the
/// host-scaled riff gain ever hard-cut).
pub const SING_WIND_DOWN: f32 = 1.0;

/// Per-window SING-ALONG detector. Bounded scalar state (no allocation), fed
/// exclusively from the committed key-press path; a session switch clears the
/// run — repeats typed into different sessions never assemble one hold.
#[derive(Default)]
pub struct KittySing {
    /// The character of the current same-key run.
    run: Option<char>,
    /// Consecutive at-cadence repeats of `run` (1 = first press).
    count: u32,
    /// The last press of the run — the cadence gap is measured against this,
    /// and the LAZY release (finger lifted, no further events) is derived
    /// from it: the run released at `last + SING_REPEAT_GAP`.
    last: Option<Instant>,
    /// The session the run was typed into (session switch clears).
    session: Option<u64>,
    /// Arm instant — the beat-clock anchor. Persists through a wind-down so an
    /// UNINTERRUPTED release never rewinds mid-crossfade; cleared only once the
    /// drive reaches 0. A run that RE-EARNS the threshold while a stale
    /// wind-down is still fading re-anchors it here (see [`Self::note_char`]) so
    /// the celebration snaps back to full instead of fading out under a
    /// continuously held key.
    armed_at: Option<Instant>,
    /// An EAGER wind-down start (key change / backspace / break / session
    /// switch). The lazy release path needs no stamp — it is derived.
    wind_from: Option<Instant>,
    /// A PROVISIONAL key hand-over: the instant a LIVE celebration changed
    /// which character it rides, held until the new key proves it is HELD
    /// with [`KEY_SWITCH_REPS`] at-cadence repeats — the KEY SWITCH commit.
    ///
    /// OWNER (0.19): "when I changed the repeating key, the song played
    /// needs to also change seamlessly." OWNER (0.20): "switching the
    /// repeated key STILL doesn't seem to change the tune that's sung!" and
    /// "there needs to be a smooth transition between repeated keys so that
    /// the singing kitty stays and isn't replaced with the cursor kitty."
    ///
    /// Moving from one held key to another used to `release()` the run
    /// outright — the song stopped, went quiet, and cold-started from the
    /// top. The hand-over carries the run instead: the run IDENTITY changes
    /// (so [`Self::signature`] moves with the CURRENT key); the arm anchor,
    /// the bar grid and the beat phase do NOT. On commit the form REOPENS
    /// on the new key's own verse at the next bar boundary
    /// ([`Self::reopen_section`]) — the switch is HEARD, not buried
    /// mid-form under the shared chorus. Provisional because distinct
    /// characters that do NOT repeat are TYPING: if another distinct
    /// character arrives before the commit, the crossfade is anchored at
    /// the DEPARTURE stored here, so ordinary typing loses exactly what it
    /// lost before this existed.
    handover_from: Option<Instant>,
    /// The SECTION REMAP in force: the bar index the host pushes is the raw
    /// 1.6 s grid index plus this shift ([`Self::bar`]). Zero for a cold
    /// arm; each committed key switch raises it so the form reopens at
    /// [`section_reopen_bar`] — mapped indices only ever move FORWARD, so
    /// the host's per-bar latch can never swallow a section and the synth's
    /// build/clap ramps (pure functions of the pushed index) never fall
    /// back to the cold open once the celebration is rolling.
    section_shift: u64,
    /// A COMMITTED key switch awaiting its seam: from raw grid index `.0`
    /// onward the shift becomes `.1`. Kept separate from [`Self::section_shift`]
    /// so [`Self::bar`] stays a pure reader — the currently sounding bar keeps
    /// its index until the boundary, and the swap lands exactly ON it (the
    /// designed seamless seam: material is a pure function of the RiffBar
    /// payload, the 1.6 s bar clock is untouched).
    pending_section: Option<(u64, u64)>,
    /// The SONG ARC (the instrument layer): per-bar energy earned at the
    /// held key's class rate, inherited across switches, carried warm
    /// across a quick re-arm. Folded at every press; derived between.
    arc: SongArc,
    /// The DEPARTED key's signature, captured when a provisional hand-over
    /// begins — the OLD key the drummer's fill announcement speaks in if
    /// the switch commits with runway. Meaningful only while
    /// [`Self::handover_from`] is `Some`.
    handover_prev_sig: u32,
    /// A latched FILL ANNOUNCEMENT: `(old sig, new sig)`, set at a switch
    /// commit that landed before [`FILL_CUE_RUNWAY`] into the bar,
    /// consumed ONCE by the host ([`Self::take_fill_cue`]) and pushed as
    /// `trail_sound::CelebrationGesture::RiffFillCue`. Both keys ride the
    /// payload so the drummer can WALK the fill from the departed key's
    /// shift into the arriving one (the graceful-merge pivot — owner: "the
    /// tunes don't gracefully merge into the next tune"). The synth
    /// quantizes the hits to its OWN sixteenth grid — this latch carries
    /// identity, never timing.
    fill_cue: Option<(u32, u32)>,
    /// The raw bar the latched fill announces in — the singer's
    /// sixteenth-rate head-bob window is that bar's last beat
    /// ([`Self::fill_beat`]).
    fill_bar: Option<u64>,
    /// THE FINALE PLAN in force, if a gentle release earned one.
    cadence: Option<Cadence>,
    /// A HARD release happened (break key, session switch, proven
    /// typing): no finale for this wind-down, whatever the span. Cleared
    /// at the next arm.
    finale_veto: bool,
}

impl KittySing {
    /// Bind the run to `session`, winding down on a switch.
    fn rekey(&mut self, now: Instant, session: u64) {
        if self.session != Some(session) {
            self.release(now);
            // A session switch is a HARD release: no finale plays into
            // another session's shell.
            self.cadence = None;
            self.finale_veto = true;
            self.session = Some(session);
        }
    }

    /// End the current run at `at`: an armed run starts its crossfade there
    /// (never a hard cut); an unarmed run just clears.
    fn release(&mut self, at: Instant) {
        if self.armed_at.is_some() && self.wind_from.is_none() {
            // A lazy release may already have begun the fade earlier than
            // this eager event; keep the EARLIER instant so the crossfade
            // never jumps back up.
            let wind = self.lazy_release().map_or(at, |lazy| lazy.min(at));
            // THE EMBER: fold the arc at the wind instant and remember
            // (when, how hot) for the warm re-arm law. Boundaries stop
            // counting at the wind start, so this fold is final.
            let energy = self.arc_energy_at(wind);
            self.arc.energy = energy;
            self.arc.anchor_raw = self.raw_bar(wind).unwrap_or(self.arc.anchor_raw);
            self.arc.carry = Some((wind, energy));
            // THE FINALE PLAN — built here, the one place the released
            // run's signature is still in hand. GENTLENESS IS THE
            // CALLER'S KNOWLEDGE: hard callers (break keys, session
            // switches, proven typing) veto immediately after; the
            // backspace and the lazy lift leave it standing.
            if let (Some(t0), Some(ch)) = (self.armed_at, self.run) {
                let raw =
                    (wind.saturating_duration_since(t0).as_secs_f32() / SING_BAR_SECONDS) as u64;
                if raw >= u64::from(ARC_FINALE_MIN_BARS) && self.cadence.is_none() {
                    self.cadence = Some(Cadence {
                        due: t0 + Duration::from_secs_f32((raw + 1) as f32 * SING_BAR_SECONDS),
                        sig: song_signature(ch),
                        energy_q: (energy * 200.0).round().clamp(0.0, 250.0) as u8,
                        span_bars: raw.min(u64::from(u16::MAX)) as u16,
                        fired: false,
                    });
                }
            }
            self.wind_from = Some(wind);
        }
        self.run = None;
        self.count = 0;
        self.last = None;
        self.handover_from = None;
        // A dead or dying song announces nothing: an unconsumed cue must
        // not leak into the next celebration.
        self.fill_cue = None;
        self.fill_bar = None;
    }

    /// The SONG SIGNATURE the held character sings from — the single `u32`
    /// the synth derives EVERY per-key axis of the celebration from: the
    /// verse melody walk, the root transpose and the mode rotation
    /// (`trail_sound::design_celebration`), all pure functions of this one
    /// payload.
    ///
    /// OWNER: "I also want a more obvious difference in the tune generation
    /// when pressing different keys." The old `key()` was `(ch % 5) - 2`:
    /// five transpose classes of ONE authored tune, so holding `a`, `f`,
    /// `k`, `p`, `u`, `z` or space produced bit-identical audio — while its
    /// doc claimed a-vs-z differed. (The doc was false; this fixes the
    /// mechanism instead of the prose.) [`song_signature`] is a fixed
    /// bijective mixer, so every distinct character owns a distinct
    /// signature — zero collisions — and the synth turns that identity into
    /// a different VERSE of the same celebration, not a nudged copy.
    ///
    /// [`NEUTRAL_SIGNATURE`] when nothing is held — the reference voicing.
    /// The hand-over law: identity derives from the CURRENT run char per
    /// bar, so a mid-hold key change modulates on the next bar boundary
    /// over the same uninterrupted bar grid — and a COMMITTED key switch
    /// additionally reopens the form there on the new key's own verse
    /// ([`Self::bar`]), so the change is heard at once, never buried under
    /// the shared chorus.
    #[must_use]
    pub fn signature(&self) -> u32 {
        self.run.map_or(NEUTRAL_SIGNATURE, song_signature)
    }

    /// The SECTION CLASS the current run plays as ([`section_class`]); 0
    /// (verse) when nothing is held. Reads the CURRENT run character — a
    /// provisional hand-over flips this on the very press that started it,
    /// which is the visual PRE-ECHO's data source: the eye can follow the
    /// destination class a full bar before the audio commits.
    #[must_use]
    pub fn section_class_now(&self) -> u8 {
        self.run.map_or(0, section_class)
    }

    /// The raw bar index the ARC counts at `now`: the raw grid, frozen at
    /// the wind-down start (released bars earn nothing). Also the
    /// definition of "bars played" — see [`Self::bars_played`].
    fn arc_raw(&self, now: Instant) -> u64 {
        let Some(t0) = self.armed_at else {
            return self.arc.anchor_raw;
        };
        let end = self.wind_start(now).map_or(now, |wind| wind.min(now));
        (end.saturating_duration_since(t0).as_secs_f32() / SING_BAR_SECONDS) as u64
    }

    /// INTERNAL energy at `now`: the folded base plus the current class's
    /// per-bar earn for every raw boundary since the fold, clamped into
    /// `0.0..=`[`ARC_ENERGY_MAX`]. A pure reader — the derive-don't-store
    /// idiom (`drive`, `lazy_release`) applied to the arc.
    fn arc_energy_at(&self, now: Instant) -> f32 {
        if self.armed_at.is_none() {
            return self.arc.energy;
        }
        let bars = self.arc_raw(now).saturating_sub(self.arc.anchor_raw);
        if bars == 0 {
            return self.arc.energy;
        }
        let earn = ARC_EARN[usize::from(self.section_class_now().min(4))];
        (self.arc.energy + earn * bars as f32).clamp(0.0, ARC_ENERGY_MAX)
    }

    /// The peak latch at `now`: entered at [`ARC_PEAK_GATE`], held (the
    /// hysteresis) until energy falls below [`ARC_PEAK_EXIT`].
    fn arc_peak_at(&self, now: Instant) -> bool {
        let e = self.arc_energy_at(now);
        e >= ARC_PEAK_GATE || (self.arc.peaked && e >= ARC_PEAK_EXIT)
    }

    /// Fold the derived arc back into stored state (called on every press,
    /// which the cadence law guarantees at least ~4× per bar while armed —
    /// so the between-fold derivation only ever spans one earn segment of
    /// one class).
    fn fold_arc(&mut self, now: Instant) {
        if self.armed_at.is_none() {
            return;
        }
        let raw = self.arc_raw(now);
        if raw > self.arc.anchor_raw {
            self.arc.peaked = self.arc_peak_at(now);
            self.arc.energy = self.arc_energy_at(now);
            self.arc.anchor_raw = raw;
        }
    }

    /// Bars the current performance has PLAYED at `now` (raw boundaries
    /// since the arm anchor; frozen at the wind start; 0 when idle). Feeds
    /// the intro gate, the finale threshold ([`ARC_FINALE_MIN_BARS`]) and
    /// the fireworks scale — and the audience pet's dance intensity, via
    /// the concert-scene seam.
    #[must_use]
    pub fn bars_played(&self, now: Instant) -> u32 {
        if self.armed_at.is_none() {
            return 0;
        }
        u32::try_from(self.arc_raw(now)).unwrap_or(u32::MAX)
    }

    /// True while the INTRO render cap is in force: a cold arc's first
    /// [`ARC_INTRO_BARS`] bars (a warm carry at/above [`ARC_INTRO_CAP`]
    /// skips it — the band is already warmed up).
    fn intro_active(&self, now: Instant) -> bool {
        !self.arc.intro_skip && self.armed_at.is_some() && self.bars_played(now) < ARC_INTRO_BARS
    }

    /// The arc ENERGY the riff payload carries: RENDER energy × 200,
    /// rounded, structurally ≤ 250 ([`ARC_ENERGY_MAX`] × 200). Render
    /// energy is internal energy with the intro cap applied — the one
    /// number the synth reads twice (clamped at 200 for build, raw against
    /// the class clap gate), resolving the build/clap double-booking by
    /// construction. Takes `now` because energy is time-derived here
    /// (documented deviation from the spec's `&self`-only signature — the
    /// module's clocks are all lazy readers).
    #[must_use]
    pub fn arc_energy_q(&self, now: Instant) -> u8 {
        let e = self.arc_energy_at(now);
        let render = if self.intro_active(now) {
            e.min(ARC_INTRO_CAP)
        } else {
            e
        };
        (render * 200.0).round().clamp(0.0, 250.0) as u8
    }

    /// The arc phase at `now` — the derived read of the spec's
    /// `SongArc::phase`. Outro is the wind-down of a performance that
    /// earned its finale; a short performance's fade reads Build/Intro
    /// until it settles.
    #[must_use]
    pub fn arc_phase(&self, now: Instant) -> ArcPhase {
        if self.armed_at.is_none() {
            return ArcPhase::Intro;
        }
        if self.wind_start(now).is_some() {
            if self.bars_played(now) >= ARC_FINALE_MIN_BARS {
                return ArcPhase::Outro;
            }
        } else if self.intro_active(now) {
            return ArcPhase::Intro;
        }
        if self.arc_peak_at(now) {
            ArcPhase::Peak
        } else {
            ArcPhase::Build
        }
    }

    /// Consume the latched FILL ANNOUNCEMENT, if a committed switch left
    /// one: `(departed sig, arriving sig)`. The host polls this once per
    /// frame beside the bar latch and pushes ONE
    /// `CelebrationGesture::RiffFillCue` — consuming UNCONDITIONALLY (the
    /// latch law: a muted riff must drain the cue, never bank it for an
    /// unmuted later moment).
    #[must_use]
    pub fn take_fill_cue(&mut self) -> Option<(u32, u32)> {
        self.fill_cue.take()
    }

    /// True while the drummer's announced fill is ROLLING: the last beat
    /// (3.0..4.0) of the bar a fill cue was latched in, while armed. The
    /// singer's head-bob clock-divides to sixteenth rate here — the visual
    /// half of the announcement, zero new envelope machinery
    /// (`kitty_cursor` reads it through the sing sync).
    #[must_use]
    pub fn fill_beat(&self, now: Instant) -> bool {
        let Some(bar) = self.fill_bar else {
            return false;
        };
        if !self.is_armed(now) || self.raw_bar(now) != Some(bar) {
            return false;
        }
        let Some(t0) = self.armed_at else {
            return false;
        };
        let in_bar = now.saturating_duration_since(t0).as_secs_f32() % SING_BAR_SECONDS;
        // Beat 4 of the bar: the runway IS beat 3.0, so the fill owns
        // everything from there to the bar edge.
        in_bar >= FILL_CUE_RUNWAY
    }

    /// True during the FIRST beat of a committed switch's reopened bar —
    /// the singer's one double-squash (the commit landing, ×1.6 for that
    /// beat). Derived from the pending-section seam, so it fires exactly
    /// when the new key's verse payload does.
    #[must_use]
    pub fn switch_landing(&self, now: Instant) -> bool {
        let Some((from, _)) = self.pending_section else {
            return false;
        };
        if !self.is_armed(now) || self.raw_bar(now) != Some(from) {
            return false;
        }
        let Some(t0) = self.armed_at else {
            return false;
        };
        let in_bar = now.saturating_duration_since(t0).as_secs_f32() % SING_BAR_SECONDS;
        in_bar < SING_BEAT_SECONDS
    }

    /// The FINALE PLAN in force at `now`, eager or derived. The PURE-LAZY
    /// lift (finger up, no event ever materializes the release) never ran
    /// [`Self::release`], so its plan is derived here from live state —
    /// the run character is still in hand exactly because nothing cleared
    /// it. Vetoed plans are `None`, whatever the span.
    fn cadence_plan(&self, now: Instant) -> Option<Cadence> {
        if self.finale_veto {
            return None;
        }
        if let Some(plan) = self.cadence {
            return Some(plan);
        }
        let t0 = self.armed_at?;
        let wind = self.wind_start(now)?;
        let ch = self.run?;
        let raw = (wind.saturating_duration_since(t0).as_secs_f32() / SING_BAR_SECONDS) as u64;
        if raw < u64::from(ARC_FINALE_MIN_BARS) {
            return None;
        }
        Some(Cadence {
            due: t0 + Duration::from_secs_f32((raw + 1) as f32 * SING_BAR_SECONDS),
            sig: song_signature(ch),
            energy_q: (self.arc_energy_at(wind) * 200.0).round().clamp(0.0, 250.0) as u8,
            span_bars: raw.min(u64::from(u16::MAX)) as u16,
            fired: false,
        })
    }

    /// Consume the FINALE CADENCE once it is due: `(sig, energy_q,
    /// span_bars)` for exactly ONE host push
    /// (`trail_sound::CelebrationGesture::RiffCadence`). The host polls
    /// every frame while any drive remains and consumes UNCONDITIONALLY —
    /// with the riff muted the take still drains (the bow and the
    /// fireworks are a motion contract; only the audio push is gated).
    /// `None` before due, after the take, for spans under
    /// [`ARC_FINALE_MIN_BARS`], and for vetoed (hard) releases.
    #[must_use]
    pub fn take_cadence(&mut self, now: Instant) -> Option<(u32, u8, u16)> {
        let plan = self.cadence_plan(now)?;
        // Materialize the (possibly derived) plan so the fired latch and
        // later reads agree on one instance.
        if plan.fired {
            self.cadence = Some(plan);
            return None;
        }
        if now < plan.due {
            self.cadence = Some(plan);
            return None;
        }
        self.cadence = Some(Cadence {
            fired: true,
            ..plan
        });
        Some((plan.sig, plan.energy_q, plan.span_bars))
    }

    /// THE BOW's depth 0..=1 at `now`: down over [`BOW_DOWN`] from cadence
    /// beat 2 (`due + `[`CADENCE_DRIVE_HOLD`] — the tonic's landing), full
    /// through [`BOW_HOLD`], up over [`BOW_RISE`]. Pure reader; 0 with no
    /// finale in force. Rides the plan, not the audio — a muted riff still
    /// bows (the motion contract), reduced motion never reads this (the
    /// static frame pins the pose).
    #[must_use]
    pub fn bow_depth(&self, now: Instant) -> f32 {
        let Some(plan) = self.cadence_plan(now) else {
            return 0.0;
        };
        let start = plan.due + Duration::from_secs_f32(CADENCE_DRIVE_HOLD);
        let t = now.saturating_duration_since(start).as_secs_f32();
        let ease = |u: f32| u * u * (3.0 - 2.0 * u);
        if t <= 0.0 {
            0.0
        } else if t < BOW_DOWN {
            ease(t / BOW_DOWN)
        } else if t < BOW_DOWN + BOW_HOLD {
            1.0
        } else if t < BOW_DOWN + BOW_HOLD + BOW_RISE {
            ease(1.0 - (t - BOW_DOWN - BOW_HOLD) / BOW_RISE)
        } else {
            0.0
        }
    }

    /// The derived "finger lifted" instant: one repeat gap after the last
    /// press, if the run has one. Deterministic — no event needed.
    fn lazy_release(&self) -> Option<Instant> {
        self.last.map(|l| l + SING_REPEAT_GAP)
    }

    /// The instant the wind-down began, if it has: the eager stamp or the
    /// derived lazy release, whichever the reader's `now` has passed.
    fn wind_start(&self, now: Instant) -> Option<Instant> {
        if self.wind_from.is_some() {
            return self.wind_from;
        }
        self.lazy_release().filter(|release| now >= *release)
    }

    /// The RAW bar-grid index at `now`: elapsed [`SING_BAR_SECONDS`] periods
    /// since the arm anchor. This is the grid the section remap maps FROM —
    /// bar BOUNDARIES live here and never move; only the INDEX a boundary
    /// carries is remapped (see [`Self::bar`]).
    fn raw_bar(&self, now: Instant) -> Option<u64> {
        let t0 = self.armed_at?;
        Some((now.saturating_duration_since(t0).as_secs_f32() / SING_BAR_SECONDS) as u64)
    }

    /// The section shift in force at raw grid index `raw`: the pending
    /// remap once its seam has been reached, the standing shift before it.
    fn shift_at(&self, raw: u64) -> u64 {
        match self.pending_section {
            Some((from, shift)) if raw >= from => shift,
            _ => self.section_shift,
        }
    }

    /// Commit a KEY SWITCH's musical half: schedule the form to REOPEN on
    /// the new key's own verse ([`section_reopen_bar`]) at the NEXT bar
    /// boundary. The currently sounding bar finishes untouched — the seam
    /// is exactly the boundary, where the host pushes the next `RiffBar`
    /// with the new signature and the reopened index. The reopen DECISION
    /// itself lives in [`section_reopen_bar`] — the instrument-layer seam.
    ///
    /// ONE BAR OF GRACE at the boundary race: the remap applies from
    /// `raw + 1` — never retroactively to the raw index in flight — because
    /// a commit landing within one frame AFTER a boundary cannot know
    /// whether the host already pushed that boundary's index, and remapping
    /// it after the fact would push a second, overlapping bar. In that
    /// ~one-frame window the new key rides one bar of the old form position
    /// first (its root/mode modulation already sounding — the switch is
    /// heard) and reopens on its verse at the boundary after. Well inside
    /// the module's documented ±60 ms AV sync tolerance.
    fn reopen_section(&mut self, now: Instant) {
        let Some(raw) = self.raw_bar(now) else { return };
        // Fold a prior switch whose seam already passed into the standing
        // shift, so `current` below is the index actually sounding now.
        if let Some((from, shift)) = self.pending_section
            && raw >= from
        {
            self.section_shift = shift;
        }
        let current = raw + self.section_shift;
        let target = section_reopen_bar(current);
        self.pending_section = Some((raw + 1, target - (raw + 1)));
    }

    /// Feed one committed PRINTED keystroke. The same character within
    /// [`SING_REPEAT_GAP`] extends the run; the [`SING_ARM_REPEATS`]th press
    /// arms SING-ALONG (anchoring the beat clock). A different character on
    /// a LIVE celebration begins a provisional KEY HAND-OVER that commits as
    /// a KEY SWITCH at [`KEY_SWITCH_REPS`] repeats — the singer stays, the
    /// form reopens on the new key's verse. Distinct characters that never
    /// repeat are typing: they release the run and the celebration winds
    /// down exactly as it always has.
    pub fn note_char(&mut self, now: Instant, session: u64, ch: char) {
        self.rekey(now, session);
        // A run whose cadence already lapsed released at the lazy instant —
        // materialize that before deciding whether this press extends it.
        if self.lazy_release().is_some_and(|release| now > release) {
            self.release(now);
        }
        // Fold the ARC first, while the run (and so the earn class) is
        // still the pre-press one: boundaries crossed since the last press
        // earned at the key that was actually sounding across them.
        self.fold_arc(now);
        if self.run != Some(ch)
            && self.armed_at.is_some()
            && self.wind_from.is_none()
            && self.run.is_some()
            && self.handover_from.is_none()
        {
            // KEY HAND-OVER: a LIVE celebration and the user moving from one held
            // key to another. Carry the song — swap which character it rides,
            // keep the arm anchor, the bar grid and the beat phase — so the tune
            // changes key without stopping. Provisional until the new key proves
            // it is HELD ([`KEY_SWITCH_REPS`] at-cadence repeats); the count
            // restarts at 1 so those proving repeats are the NEW key's own.
            // The DEPARTED key is remembered for the drummer: a committed
            // switch with runway announces itself with the OLD key's fill.
            self.handover_prev_sig = self.signature();
            self.run = Some(ch);
            self.count = 1;
            self.handover_from = Some(now);
            self.last = Some(now);
            return;
        }
        if self.run == Some(ch) {
            // KEY-REPEAT IS WALL-TIME (M4): OS auto-repeat delivers each press
            // at a distinct instant, but a single batched IME commit of a
            // repeated string ("wwwwwwww") hands every char ONE `now`. Counting
            // those zero-gap duplicates as repeats armed SING-ALONG off one
            // paste-like commit — jubilant HOLDING it is not. Advance the
            // repeat count only when real time has elapsed since the last
            // press, so a batched commit is at most one step (arms, switches
            // and recoveries all need genuine held repeats over time).
            if self.last.is_none_or(|l| now > l) {
                self.count = self.count.saturating_add(1);
            }
            // KEY SWITCH COMMITTED: the handed-over key repeated its way to
            // [`KEY_SWITCH_REPS`] — this is a HOLD, not typing. A key switch
            // is NOT a release: the drive never left 1.0, the singer never
            // left the stage, and no re-earn is owed. The musical half:
            // reopen the form on the new key's own verse at the next bar
            // boundary, so the switch is HEARD immediately instead of hiding
            // for up to three bars behind the shared chorus.
            if self.handover_from.is_some() && self.count >= KEY_SWITCH_REPS {
                self.handover_from = None;
                // FILL ANNOUNCEMENT (the early path): runway to beat 3 —
                // latch the OLD key's signature for the host to push as the
                // drummer's cue. The synth quantizes the hits to its own
                // sixteenth grid; a commit past the runway announces itself
                // with the audible reopen alone (the late path — zero cost).
                if let Some(t0) = self.armed_at {
                    let in_bar = now.saturating_duration_since(t0).as_secs_f32() % SING_BAR_SECONDS;
                    if in_bar < FILL_CUE_RUNWAY {
                        self.fill_cue = Some((self.handover_prev_sig, self.signature()));
                        self.fill_bar = self.raw_bar(now);
                    }
                }
                self.reopen_section(now);
            }
        } else {
            // PROMISE BROKEN: another distinct character before the handed-over
            // key ever proved itself. That was typing, so wind down from the
            // DEPARTURE — the instant the original held key was abandoned —
            // rather than from here, so ordinary typing loses exactly what it
            // lost before. Anchoring through `release(departure)` (instead of
            // pre-setting `wind_from`, which would make release skip its
            // wind-start block) keeps the arc EMBER: typing out of a song is
            // still a release, and a quick re-arm after it stays warm.
            let departure = self.handover_from.take();
            self.release(departure.unwrap_or(now));
            // Proven typing is a HARD release: the band does not play a
            // ta-daa over the sentence you started writing.
            self.cadence = None;
            self.finale_veto = true;
            self.run = Some(ch);
            self.count = 1;
        }
        self.last = Some(now);
        // Arm — or RECOVER. A run that re-earns its threshold while a prior
        // arm is still winding down (`wind_from.is_some()`) must re-anchor
        // here: the host only calls `settle` (which clears `armed_at`/
        // `wind_from`) once the drive reaches 0, so without this a continuously
        // held key whose run was merely restarted (one auto-repeat hiccup, a
        // stray other key, a brief pause) would keep decaying the STALE
        // crossfade to 0 mid-hold. Re-anchoring the beat clock is the correct
        // move: the user is actively holding, so the dance snaps back to full
        // rather than fading out under their finger.
        //
        // FORGIVENESS: while the glow is still live (drive > 0), the recovery
        // costs only [`KEY_SWITCH_REPS`] repeats of any single character — a
        // near-missed key switch (the OS initial repeat delay outran
        // [`SING_REPEAT_GAP`], the owner's real-world switch cadence) comes
        // back in a breath, well before the drive can fall through the
        // host's face gate and swap the singer for the cursor kitty. Cold
        // arms (no glow at all) still cost the full deliberate hold.
        let threshold = if self.wind_from.is_some() && self.drive(now) > 0.0 {
            KEY_SWITCH_REPS
        } else {
            SING_ARM_REPEATS
        };
        if self.count >= threshold && (self.armed_at.is_none() || self.wind_from.is_some()) {
            // The reopened section stays MONOTONE above everything the old
            // one pushed: re-anchoring rewinds the raw grid to 0, so carry
            // the last live mapped index (at the wind start) forward into
            // the shift — the recovery, like a live switch, opens on the new
            // key's own verse and can never replay a stale bar index.
            self.section_shift = match (self.armed_at, self.wind_from) {
                (Some(t0), Some(wind)) => {
                    let raw = (wind.saturating_duration_since(t0).as_secs_f32() / SING_BAR_SECONDS)
                        as u64;
                    section_reopen_bar(raw + self.shift_at(raw))
                }
                _ => 0,
            };
            // THE WARM CARRY: a re-arm within [`ARC_CARRY_SECONDS`] of the
            // wind-down start keeps [`ARC_CARRY_KEEP`] of the released
            // energy (floored at the cold open) — and a carry already at
            // performance temperature (≥ [`ARC_INTRO_CAP`]) skips the
            // intro. Beyond the window the ember is cold: a fresh arc.
            let carried = match self.arc.carry {
                Some((wind, energy))
                    if now.saturating_duration_since(wind).as_secs_f32() <= ARC_CARRY_SECONDS =>
                {
                    (ARC_CARRY_KEEP * energy).max(ARC_START)
                }
                _ => ARC_START,
            };
            self.arc = SongArc {
                energy: carried,
                anchor_raw: 0,
                // Structurally below the gate: the hottest carry is
                // ARC_CARRY_KEEP × ARC_ENERGY_MAX ≈ 0.94 — a peak must be
                // re-earned, never inherited.
                peaked: false,
                intro_skip: carried >= ARC_INTRO_CAP,
                carry: None,
            };
            // A re-arm before the cadence fires CANCELS it — the band was
            // asked to keep playing, not to end; and a fresh performance
            // clears any hard-release veto.
            self.cadence = None;
            self.finale_veto = false;
            self.pending_section = None;
            self.armed_at = Some(now);
            self.wind_from = None;
        }
    }

    /// A backspace NEVER arms (deleting is the opposite of jubilant holding)
    /// and releases any armed run into its crossfade.
    pub fn note_backspace(&mut self, now: Instant) {
        self.release(now);
    }

    /// Any word-breaking / editing / navigation key: same release law as
    /// backspace, but HARD — Enter/Tab/Escape/chords mean "done, back to
    /// work", so the finale is vetoed along with the run (the plain fade
    /// is the whole goodbye).
    pub fn note_break(&mut self, now: Instant) {
        self.release(now);
        self.cadence = None;
        self.finale_veto = true;
    }

    /// The celebration drive 0..=1 at `now`: 1.0 while armed and held, a
    /// smoothstep crossfade 1 → 0 over [`SING_WIND_DOWN`] after release,
    /// exactly 0.0 when idle. Reading is pure (lazy-release is derived, like
    /// [`crate::typing_momentum::TypingMomentum::value`]'s lazy decay).
    #[must_use]
    pub fn drive(&self, now: Instant) -> f32 {
        if self.armed_at.is_none() {
            return 0.0;
        }
        let Some(start) = self.wind_start(now) else {
            return 1.0;
        };
        // THE DEFERRED WIND-DOWN: while a finale is in force, the drive
        // holds full through the finishing bar AND the cadence until its
        // tonic lands (due + CADENCE_DRIVE_HOLD), then the standard
        // smoothstep — the singer stays on stage for the ta-daa and the
        // bow. `saturating_duration_since` reads a future start as "no
        // time elapsed", i.e. drive 1.0. (This is also what stretches the
        // warm re-arm window to ~3.4 s past the release — pinned as
        // intended by the owner.)
        let start = match self.cadence_plan(now) {
            Some(plan) => plan.due + Duration::from_secs_f32(CADENCE_DRIVE_HOLD),
            None => start,
        };
        let t = now.saturating_duration_since(start).as_secs_f32() / SING_WIND_DOWN;
        if t >= 1.0 {
            return 0.0;
        }
        // Smoothstep DOWN: C¹ at both ends — the crossfade contract.
        let u = 1.0 - t;
        u * u * (3.0 - 2.0 * u)
    }

    /// True while armed at full drive (the wind-down has not begun).
    #[must_use]
    pub fn is_armed(&self, now: Instant) -> bool {
        self.armed_at.is_some() && self.wind_start(now).is_none()
    }

    /// The beat phase in BEATS since the arm instant (fractional; wraps
    /// nowhere), while any drive remains. The visual half of the shared beat
    /// clock — see the module doc's sync-tolerance note.
    #[must_use]
    pub fn beat(&self, now: Instant) -> Option<f32> {
        let t0 = self.armed_at?;
        if self.drive(now) <= 0.0 {
            return None;
        }
        Some(now.saturating_duration_since(t0).as_secs_f32() / SING_BEAT_SECONDS)
    }

    /// The riff bar index at `now` while ARMED (wind-down schedules no new
    /// bars — the synth's own sing-duck release is the audio crossfade). The
    /// host pushes one `CelebrationGesture::RiffBar` per NEW index.
    ///
    /// The index is the raw 1.6 s grid plus the SECTION remap: a committed
    /// key switch re-enters the form at [`section_reopen_bar`] on the
    /// boundary after the commit, so the new key opens on its own verse
    /// (form slot 0) instead of wherever the old key left the form. Mapped
    /// indices only ever move forward — across switches AND recoveries —
    /// so the host latch never swallows a section and the synth's
    /// build/clap ramps never fall back to the cold open mid-medley.
    #[must_use]
    pub fn bar(&self, now: Instant) -> Option<u64> {
        if !self.is_armed(now) {
            return None;
        }
        let raw = self.raw_bar(now)?;
        Some(raw + self.shift_at(raw))
    }

    /// A drained detector at rest is byte-identical off — the idle contract,
    /// with ONE documented ember: [`SongArc::carry`] survives (the warm
    /// re-arm law needs memory across the rest; it goes stale on its own
    /// clock). Called by the host once the drive reads 0 (or on hard resets).
    pub fn settle(&mut self, now: Instant) {
        if self.armed_at.is_some() && self.drive(now) <= 0.0 {
            // A PURE-LAZY wind-down (finger lifted, no event ever
            // materialized the release) still leaves its ember.
            if self.arc.carry.is_none()
                && let Some(wind) = self.wind_start(now)
            {
                self.arc.carry = Some((wind, self.arc_energy_at(wind)));
            }
            self.armed_at = None;
            self.wind_from = None;
            self.handover_from = None;
            self.section_shift = 0;
            self.pending_section = None;
            self.fill_cue = None;
            self.fill_bar = None;
            self.cadence = None;
            self.finale_veto = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Music-note sprite field
// ---------------------------------------------------------------------------

/// Ring capacity for live note sprites. At the half-beat spawn cadence
/// (~5 notes/s) and [`NOTE_LIFE`] lifetime, at most ~8 notes are ever alive;
/// 16 is structural headroom, and the RING (overwrite-oldest) makes the cap
/// unconditional — no burst can ever exceed it.
pub const MAX_NOTES: usize = 16;

/// Note sprite lifetime in seconds: rise, wobble, fade, gone.
pub const NOTE_LIFE: f32 = 1.6;

/// How far a note rises over its life, in cell heights.
const NOTE_RISE_CELLS: f32 = 1.4;

/// The rainbow the notes cycle through — the rainbow ribbon's six stripes
/// (red, orange, yellow, green, blue, violet), applied per sprite through the
/// `FreeSprite::tint` multiply channel over the white-baked tile.
pub const NOTE_TINTS: [u32; 6] = [
    0x00FF_5A5A,
    0x00FF_A94D,
    0x00FF_E15A,
    0x005A_D95A,
    0x005A_A8FF,
    0x00B0_6AFF,
];

/// Which authored note glyph a sprite carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoteKind {
    /// ♪ — a single eighth note.
    Eighth,
    /// ♫ — a beamed pair.
    Beamed,
}

/// One live note's birth record (positions are derived per frame).
#[derive(Clone, Copy, Debug)]
struct Note {
    born: Instant,
    kind: NoteKind,
    /// Per-note scatter seed: tint index, x offset, wobble phase.
    seed: u32,
    /// The SECTION CLASS sounding at spawn ([`section_class`]) — picks the
    /// stripe range ([`class_stripes`]). Flips on the very press that
    /// starts a hand-over, so the notes are the PRE-ECHO: the eye sees the
    /// destination key a bar before the audio commits.
    class: u8,
    /// A FIREWORK note (the finale burst): born radially scattered up to
    /// [`FIREWORK_SPREAD`] cells out instead of at the mouth; same rise,
    /// same fade, same ring — zero new particle systems.
    burst: bool,
}

/// How far (in cells) the finale fireworks scatter from the mouth anchor.
pub const FIREWORK_SPREAD: f32 = 3.0;

/// The rainbow stripes a section class spawns notes in — `(offset, len)`
/// into [`NOTE_TINTS`]: verse a modest two-stripe band, chorus the full
/// six-stripe rainbow, bridge the cool half, percussive the warm half,
/// breath a single dim violet ([`BREATH_NOTE_DIM`]).
const fn class_stripes(class: u8) -> (usize, usize) {
    match class {
        1 => (0, 6),
        2 => (3, 3),
        3 => (0, 3),
        4 => (5, 1),
        _ => (2, 2),
    }
}

/// The breath's single stripe renders dim — the quiet passage looks quiet.
const BREATH_NOTE_DIM: f32 = 0.6;

/// One frame's resolved note sprite, in CELL units relative to the singing
/// cat's mouth anchor (+x ahead of the cat, −y upward). The emitter
/// (`word_decorations::kitty_cursor`) maps cells → pixels and multiplies
/// `alpha` by the cat's own presentation alpha.
#[derive(Clone, Copy, Debug)]
pub struct NoteSprite {
    pub dx: f32,
    pub dy: f32,
    /// 0..=255 fade envelope (in over ~15% of life, out over the last 40%).
    pub alpha: u8,
    pub kind: NoteKind,
    /// `0x00RRGGBB` rainbow tint for the white-baked tile.
    pub tint: u32,
}

/// The ring-capped note pool. Per-window, bounded, allocation-free after
/// construction.
pub struct MusicNotes {
    ring: [Option<Note>; MAX_NOTES],
    /// Next ring slot to (over)write.
    head: usize,
    /// Last half-beat index a note was spawned for (one note per half-beat).
    spawned_half_beat: Option<i64>,
    /// xorshift32 scatter rng — deterministic per window seed.
    rng: u32,
}

impl Default for MusicNotes {
    fn default() -> Self {
        Self {
            ring: [None; MAX_NOTES],
            head: 0,
            spawned_half_beat: None,
            rng: 0x9E37_79B9,
        }
    }
}

impl MusicNotes {
    fn rnd(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// Advance the field one frame: cull dead notes, and while the hold is
    /// ARMED spawn one note per NEW half-beat (beat-synced streaming — the
    /// notes leave the cat's mouth on the same clock the riff plays on).
    /// Any wind-down spawns nothing: the live notes finish their rise and
    /// fade — the visual crossfade. `streaming` is the detector's
    /// `is_armed`, DELIBERATELY not the drive: the finale's deferred
    /// wind-down holds the drive at 1.0 through the cadence tail, and a
    /// mouth-stream continuing there drowned the firework burst (the
    /// pixel review's one moderate defect) — the ending belongs to the
    /// fireworks alone. `class` is the CURRENT section class
    /// ([`KittySing::section_class_now`]): it picks the spawn's stripe
    /// range, and because it reads the live run char it flips on the
    /// hand-over press itself — the tint half of the visual pre-echo.
    pub fn update(&mut self, now: Instant, streaming: bool, beat: Option<f32>, class: u8) {
        for slot in &mut self.ring {
            if slot
                .is_some_and(|n| now.saturating_duration_since(n.born).as_secs_f32() >= NOTE_LIFE)
            {
                *slot = None;
            }
        }
        let Some(beat) = beat else {
            self.spawned_half_beat = None;
            return;
        };
        if !streaming {
            return;
        }
        let half_beat = (beat * 2.0).floor() as i64;
        if self.spawned_half_beat == Some(half_beat) {
            return;
        }
        self.spawned_half_beat = Some(half_beat);
        let seed = self.rnd();
        let kind = if half_beat % 2 == 0 {
            NoteKind::Eighth
        } else {
            NoteKind::Beamed
        };
        self.ring[self.head] = Some(Note {
            born: now,
            kind,
            seed,
            class: class.min(4),
            burst: false,
        });
        self.head = (self.head + 1) % MAX_NOTES;
    }

    /// THE FINALE FIREWORKS: burst `min(4 + bars_played / 2, 16)` notes
    /// into the existing ring at once — a performance-scaled send-off
    /// (test-pinned: 6 bars => 7 notes). Full-rainbow tints (the chorus
    /// stripe range), radial scatter, standard rise and fade; the 16-cap
    /// ring bounds it unconditionally. The HOST gates this on motion:
    /// reduced motion spawns no fireworks (fade-only law), a muted riff
    /// still gets them (motion contract).
    pub fn fireworks(&mut self, now: Instant, bars_played: u32) {
        let n = (4 + bars_played / 2).min(16) as usize;
        for i in 0..n {
            let seed = self.rnd();
            let kind = if i % 2 == 0 {
                NoteKind::Eighth
            } else {
                NoteKind::Beamed
            };
            self.ring[self.head] = Some(Note {
                born: now,
                kind,
                seed,
                class: 1,
                burst: true,
            });
            self.head = (self.head + 1) % MAX_NOTES;
        }
    }

    /// True while any note is alive (the host keeps its frame cadence going
    /// until the field drains).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.ring.iter().any(Option::is_some)
    }

    /// Clear the field (style switch / hard reset): byte-identical off.
    pub fn clear(&mut self) {
        self.ring = [None; MAX_NOTES];
        self.spawned_half_beat = None;
    }

    /// Resolve one live note at `now`. Full motion: the note bobs upward — a
    /// rise with a sideways wobble. REDUCED MOTION: it holds a fixed scatter
    /// offset (no bob, no rise); only the fade envelope animates — the
    /// "static celebration" arm.
    fn resolve(note: &Note, now: Instant, reduced_motion: bool) -> Option<NoteSprite> {
        let age = now.saturating_duration_since(note.born).as_secs_f32();
        let u = age / NOTE_LIFE;
        if !(0.0..1.0).contains(&u) {
            return None;
        }
        // Fade: quick bloom in, long dissolve out.
        let fade_in = (u / 0.15).min(1.0);
        let fade_out = ((1.0 - u) / 0.4).min(1.0);
        // The breath's single stripe renders dim — see [`class_stripes`].
        let dim = if note.class == 4 {
            BREATH_NOTE_DIM
        } else {
            1.0
        };
        let alpha = (fade_in * fade_out * dim * 255.0) as u8;
        let s = note.seed;
        let (stripe0, stripes) = class_stripes(note.class);
        let tint = NOTE_TINTS[stripe0 + (s as usize % stripes)];
        // Scatter: birth x in 0.1..0.9 cells ahead, wobble phase 0..1.
        let x0 = 0.1 + 0.8 * ((s >> 8) & 0xff) as f32 / 255.0;
        let phase = ((s >> 16) & 0xff) as f32 / 255.0;
        let (dx, dy) = if reduced_motion {
            // Fixed offsets: a static spray around the mouth.
            (x0, -0.3 - 0.8 * phase)
        } else if note.burst {
            // FIREWORK: a radial birth offset (seed-scattered angle and
            // radius), then the standard rise — the burst blooms outward
            // and floats up like every other note.
            let ang = std::f32::consts::TAU * phase;
            let r = FIREWORK_SPREAD * (0.5 + 0.5 * (((s >> 24) & 0xff) as f32 / 255.0));
            (r * ang.cos(), 0.5 * r * ang.sin() - NOTE_RISE_CELLS * u)
        } else {
            (
                x0 + 0.18 * (std::f32::consts::TAU * (u * 1.5 + phase)).sin(),
                -NOTE_RISE_CELLS * u,
            )
        };
        Some(NoteSprite {
            dx,
            dy,
            alpha,
            kind: note.kind,
            tint,
        })
    }

    /// This frame's sprites as the fixed `None`-padded array
    /// `word_decorations::KittyCursorFrame` carries — allocation-free,
    /// bounded at [`MAX_NOTES`] by construction.
    #[must_use]
    pub fn frame_array(
        &self,
        now: Instant,
        reduced_motion: bool,
    ) -> [Option<NoteSprite>; MAX_NOTES] {
        let mut out = [None; MAX_NOTES];
        let mut i = 0;
        for note in self.ring.iter().flatten() {
            if let Some(sprite) = Self::resolve(note, now, reduced_motion) {
                out[i] = Some(sprite);
                i += 1;
            }
        }
        out
    }

    /// Resolve this frame's sprites into `out` (bounded by [`MAX_NOTES`]) —
    /// the growable-buffer twin of [`Self::frame_array`] for tests/tools.
    pub fn frames(&self, now: Instant, reduced_motion: bool, out: &mut Vec<NoteSprite>) {
        for note in self.ring.iter().flatten() {
            if let Some(sprite) = Self::resolve(note, now, reduced_motion) {
                out.push(sprite);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Note tile art (procedural, host-baked)
// ---------------------------------------------------------------------------

/// Salt for the note tiles' `host_tile` id space — its own family, scrambled
/// away from the user kitty sprite ids by the splitmix finalizer.
const NOTE_HOST_SALT: u64 = 0x5150_9A7E_D1B2_C4F3;

/// Stable atlas identity for one baked note tile: kind + exact dimensions.
/// Tint is deliberately absent — the tile is baked WHITE and colored per
/// sprite via `FreeSprite::tint`, so the rainbow is cache-neutral.
#[must_use]
pub fn note_host_id(kind: NoteKind, w: u16, h: u16) -> u64 {
    let k = match kind {
        NoteKind::Eighth => 1u64,
        NoteKind::Beamed => 2,
    };
    let mut x = NOTE_HOST_SALT ^ (k << 32) ^ (u64::from(w) << 16) ^ u64::from(h);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The natural note tile size for a cell of height `cell_h`: about
/// three-quarters of a cell tall, width from the glyph aspect (♫ is wider),
/// never zero.
#[must_use]
pub fn note_nat_size(kind: NoteKind, cell_h: u16) -> (u16, u16) {
    let h = (f32::from(cell_h.max(1)) * 0.75).round().max(4.0) as u16;
    let aspect = match kind {
        NoteKind::Eighth => 0.72,
        NoteKind::Beamed => 1.05,
    };
    let w = (f32::from(h) * aspect).round().max(4.0) as u16;
    (w, h)
}

/// ♪ stem + flag, authored in the 0..1 glyph frame (head is a disc drawn
/// separately so it stays round at tiny sizes). The stem rises from the head
/// (bottom-left) and the flag curls down-right from its top.
const EIGHTH_STEM: [PathCmd; 5] = [
    PathCmd::Move(0.44, 0.10),
    PathCmd::Line(0.56, 0.10),
    PathCmd::Line(0.56, 0.74),
    PathCmd::Line(0.44, 0.74),
    PathCmd::Close,
];
const EIGHTH_FLAG: [PathCmd; 4] = [
    PathCmd::Move(0.56, 0.10),
    PathCmd::Cubic(0.78, 0.16, 0.88, 0.32, 0.80, 0.52),
    PathCmd::Cubic(0.82, 0.30, 0.70, 0.22, 0.56, 0.22),
    PathCmd::Close,
];

/// ♫ two stems joined by a beam bar across the top.
const BEAM_BAR: [PathCmd; 5] = [
    PathCmd::Move(0.24, 0.08),
    PathCmd::Line(0.86, 0.08),
    PathCmd::Line(0.86, 0.24),
    PathCmd::Line(0.24, 0.24),
    PathCmd::Close,
];
const BEAM_STEM_L: [PathCmd; 5] = [
    PathCmd::Move(0.24, 0.08),
    PathCmd::Line(0.34, 0.08),
    PathCmd::Line(0.34, 0.76),
    PathCmd::Line(0.24, 0.76),
    PathCmd::Close,
];
const BEAM_STEM_R: [PathCmd; 5] = [
    PathCmd::Move(0.76, 0.08),
    PathCmd::Line(0.86, 0.08),
    PathCmd::Line(0.86, 0.76),
    PathCmd::Line(0.76, 0.76),
    PathCmd::Close,
];

/// Glyph ink alpha. Fully readable but below body-silhouette opacity — the
/// notes float over arbitrary text, so the ART itself carries the legibility
/// budget (the free-sprite layer has no occupancy cap).
const NOTE_A: f32 = 0.88;

/// Bake one note glyph at its natural `w × h`, WHITE (tinted per sprite).
/// Deterministic: const drawlists + the fixed scanline filler — one tile per
/// `(kind, w, h)`, byte-identical across bakes.
#[must_use]
pub fn bake_note(w: u16, h: u16, kind: NoteKind) -> Tile {
    let mut tile = Tile::new(u32::from(w), u32::from(h));
    if w == 0 || h == 0 {
        return tile;
    }
    let white = (1.0, 1.0, 1.0);
    let fit = PathTransform::fit(u32::from(w), u32::from(h));
    let (wf, hf) = (f32::from(w), f32::from(h));
    match kind {
        NoteKind::Eighth => {
            fill_path(&mut tile, &[&EIGHTH_STEM], white, NOTE_A, fit);
            fill_path(&mut tile, &[&EIGHTH_FLAG], white, NOTE_A, fit);
            // The head: a filled disc at the stem's foot.
            tile.disc(0.40 * wf, 0.82 * hf, (0.16 * hf).max(1.2), white, NOTE_A);
        }
        NoteKind::Beamed => {
            fill_path(&mut tile, &[&BEAM_BAR], white, NOTE_A, fit);
            fill_path(&mut tile, &[&BEAM_STEM_L], white, NOTE_A, fit);
            fill_path(&mut tile, &[&BEAM_STEM_R], white, NOTE_A, fit);
            tile.disc(0.24 * wf, 0.84 * hf, (0.14 * hf).max(1.2), white, NOTE_A);
            tile.disc(0.76 * wf, 0.84 * hf, (0.14 * hf).max(1.2), white, NOTE_A);
        }
    }
    tile
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 7;

    /// Hold `ch` for `n` presses at `gap_ms` cadence starting at `t0`;
    /// returns the instant of the last press.
    fn hold(d: &mut KittySing, t0: Instant, ch: char, n: u32, gap_ms: u64) -> Instant {
        let mut t = t0;
        for i in 0..n {
            t = t0 + Duration::from_millis(u64::from(i) * gap_ms);
            d.note_char(t, S, ch);
        }
        t
    }

    /// THE ARM CADENCE PROOF: one fewer than the threshold arms nothing; the
    /// threshold press arms SING-ALONG at drive 1.0 and anchors the beat clock.
    #[test]
    fn threshold_at_cadence_arms_full_sing() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let before = hold(&mut d, t0, 'a', SING_ARM_REPEATS - 1, 30);
        assert!(!d.is_armed(before), "a sub-threshold hold must not arm");
        assert_eq!(d.drive(before), 0.0);
        let armed = before + Duration::from_millis(30);
        d.note_char(armed, S, 'a');
        assert!(d.is_armed(armed), "the threshold repeat arms");
        assert_eq!(d.drive(armed), 1.0);
        assert_eq!(
            d.beat(armed),
            Some(0.0),
            "the beat clock anchors at the arm"
        );
    }

    /// BATCHED COMMIT IS NOT A HOLD (M4): a single IME commit of a repeated
    /// string delivers every character at ONE timestamp; those zero-gap
    /// duplicates must count as at most one repeat step, so sixteen `w`s
    /// committed as one event never arm — while the SAME characters
    /// spread over wall-clock repeat cadence still do.
    #[test]
    fn a_batched_ime_commit_of_repeats_does_not_arm() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        // One commit: 16 identical chars, all at t0 (the shared `input_now`).
        for _ in 0..16 {
            d.note_char(t0, S, 'w');
        }
        assert!(
            !d.is_armed(t0),
            "a single batched commit of repeats is one step, not a hold"
        );
        assert_eq!(d.drive(t0), 0.0);
        // Genuine held-key repeats of the same char over wall time DO arm at
        // the threshold press — the mechanism the batch must not counterfeit.
        let mut held = KittySing::default();
        let armed_at = hold(&mut held, t0, 'w', SING_ARM_REPEATS, 30);
        assert!(
            held.is_armed(armed_at),
            "real repeats across time still arm at {SING_ARM_REPEATS}"
        );
    }

    /// INTERLEAVED CHARACTERS NEVER ARM: alternating two keys forever stays
    /// dark — the run is SAME-character by definition; and real typing
    /// ("lettersss…" style tails under the threshold) stays dark too.
    #[test]
    fn interleaved_chars_never_arm() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        for i in 0..64u64 {
            let t = t0 + Duration::from_millis(i * 30);
            d.note_char(t, S, if i % 2 == 0 { 'a' } else { 'b' });
            assert!(!d.is_armed(t), "alternating keys must never arm (i={i})");
        }
    }

    /// BACKSPACE NEVER ARMS: a run of backspaces feeds `note_backspace`,
    /// which can only ever release — and it cuts a live run short of arming.
    #[test]
    fn backspace_never_arms_and_breaks_a_run() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        for i in 0..32u64 {
            d.note_backspace(t0 + Duration::from_millis(i * 30));
        }
        assert!(!d.is_armed(t0 + Duration::from_secs(1)));
        // Backspace mid-run: the run restarts from scratch afterwards.
        let t = hold(
            &mut d,
            t0 + Duration::from_secs(2),
            'x',
            SING_ARM_REPEATS - 1,
            30,
        );
        d.note_backspace(t + Duration::from_millis(30));
        let t2 = t + Duration::from_millis(60);
        d.note_char(t2, S, 'x');
        assert!(
            !d.is_armed(t2),
            "the broken run must re-earn all {SING_ARM_REPEATS} repeats"
        );
    }

    /// Slower-than-cadence re-striking of the SAME key is typing, not
    /// holding: gaps beyond [`SING_REPEAT_GAP`] never accumulate a run.
    #[test]
    fn slow_same_key_striking_never_arms() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        for i in 0..32u64 {
            let t = t0 + Duration::from_millis(i * 400); // 2.5 Hz — deliberate
            d.note_char(t, S, 'a');
            assert!(!d.is_armed(t), "slow striking must never arm (i={i})");
        }
    }

    /// THE OWNER'S SCENARIO: hold one key until FULL NYAN, then hold a DIFFERENT
    /// key. Both halves of "change seamlessly" are pinned here — under the
    /// KEY-SWITCH law: the hand-over is provisional for [`KEY_SWITCH_REPS`]
    /// repeats and then COMMITS as a switch (never a release).
    ///
    ///  * SEAMLESS — the drive never dips, the beat clock never rewinds, and the
    ///    bar index keeps counting UP across the switch. The old behaviour
    ///    released the run, stopped scheduling bars immediately, and then cold
    ///    started at bar 0 once the crossfade drained.
    ///  * CHANGES — the signature moves to the new key, and the committed switch
    ///    reopens the form on that key's own verse at the next boundary
    ///    (`switching_the_held_key_changes_the_verse_at_the_next_bar` pins the
    ///    pushed-bar half of that law).
    #[test]
    fn changing_the_held_key_transposes_the_song_without_a_seam() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        assert!(d.is_armed(armed));
        let bar_before = d.bar(armed).expect("armed runs schedule bars");
        let beat_before = d.beat(armed).expect("armed runs have a beat");
        let sig_before = d.signature();

        let mut t = armed;
        for _ in 0..SING_ARM_REPEATS {
            t += Duration::from_millis(30);
            d.note_char(t, S, 'q');
            assert_eq!(
                d.drive(t),
                1.0,
                "the song never dips while the hold moves from one key to another"
            );
            assert!(d.is_armed(t), "and never stops scheduling bars");
        }

        let beat_after = d.beat(t).expect("still singing");
        assert!(
            beat_after > beat_before,
            "the beat clock advanced across the switch instead of rewinding \
             ({beat_before} -> {beat_after})"
        );
        assert!(
            d.bar(t).expect("still scheduling") >= bar_before,
            "the bar grid kept counting up — no cold start at bar 0"
        );
        assert_ne!(
            d.signature(),
            sig_before,
            "'q' and 'a' must not sing the same song"
        );
        assert_eq!(
            d.signature(),
            song_signature('q'),
            "the signature is the CURRENT run char's — the hand-over carries \
             identity, the synth derives root/mode/verse from it per bar"
        );

        // The promise is enforced: distinct characters that never earn
        // [`KEY_SWITCH_REPS`] repeats are TYPING, and typing still loses the
        // celebration (`genuine_typing_still_winds_the_song_down` pins the
        // full wind-down).
        let mut typing = KittySing::default();
        let armed = hold(&mut typing, t0, 'a', SING_ARM_REPEATS, 30);
        let b = armed + Duration::from_millis(30);
        typing.note_char(b, S, 'b');
        let c = b + Duration::from_millis(30);
        typing.note_char(c, S, 'c');
        assert!(
            typing.drive(c) < 1.0,
            "a second distinct character proves it was typing, and the \
             wind-down is anchored back at the departure"
        );
    }

    /// THE WIND-DOWN CROSSFADE COMPLETES: on key change the drive leaves 1.0
    /// immediately but eases — strictly between 0 and 1 mid-fade,
    /// monotonically decreasing, exactly 0.0 by [`SING_WIND_DOWN`] — and
    /// `settle` then returns the detector to byte-identical rest.
    #[test]
    fn wind_down_crossfades_to_zero() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let t = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        assert_eq!(d.drive(t), 1.0);
        let change = t + Duration::from_millis(30);
        d.note_char(change, S, 'b'); // key change → PROVISIONAL hand-over
        // The hand-over holds the song at full for one cadence window: a single
        // different character cannot yet be distinguished from the start of a new
        // hold, and cutting the song on that guess is exactly the seam the owner
        // reported. The wind-down begins when the new key fails to repeat.
        assert!(
            d.is_armed(change),
            "a key change hands the song over rather than cutting it"
        );
        let release = change + SING_REPEAT_GAP;
        let mut prev = d.drive(release);
        assert!(prev <= 1.0);
        for step in 1..=10u64 {
            let at = release + Duration::from_millis(step * 100);
            let v = d.drive(at);
            assert!(v <= prev, "the crossfade must be monotone ({prev} -> {v})");
            if step == 5 {
                assert!(
                    (0.0..1.0).contains(&v) && v > 0.0,
                    "mid-fade must be a real crossfade value, got {v}"
                );
            }
            prev = v;
        }
        let done = release + Duration::from_secs_f32(SING_WIND_DOWN) + Duration::from_millis(1);
        assert_eq!(d.drive(done), 0.0, "the crossfade completes at exactly 0");
        d.settle(done);
        assert_eq!(d.beat(done), None, "settled = byte-identical rest");
    }

    /// LAZY RELEASE: simply letting go (no further events at all) starts the
    /// same crossfade one repeat-gap after the last press — deterministic,
    /// derived, no release event required.
    #[test]
    fn letting_go_winds_down_without_an_event() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let t = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        let still_held = t + SING_REPEAT_GAP;
        assert_eq!(d.drive(still_held), 1.0, "within the gap the hold persists");
        let mid = t + SING_REPEAT_GAP + Duration::from_secs_f32(SING_WIND_DOWN * 0.5);
        let v = d.drive(mid);
        assert!(
            v > 0.0 && v < 1.0,
            "half a wind-down after the lazy release the fade is mid-flight: {v}"
        );
        let gone = t + SING_REPEAT_GAP + Duration::from_secs_f32(SING_WIND_DOWN * 1.01);
        assert_eq!(d.drive(gone), 0.0);
    }

    /// A SESSION SWITCH mid-hold releases the run (repeats typed into
    /// different sessions never assemble one hold), and the new session must
    /// re-earn the full arm count.
    #[test]
    fn session_switch_releases_the_hold() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let t = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        assert!(d.is_armed(t));
        let t1 = t + Duration::from_millis(30);
        d.note_char(t1, 99, 'a'); // same key, OTHER session
        assert!(!d.is_armed(t1), "the switch released the hold");
        // 7 more in the new session (8 total there) arm it fresh.
        let t2 = hold(
            &mut d,
            t1 + Duration::from_millis(30),
            'a',
            SING_ARM_REPEATS,
            30,
        );
        // (hold() feeds session S — re-feed in session 99 explicitly.)
        let _ = t2;
        let mut fresh = KittySing::default();
        let mut at = t1;
        for _ in 0..SING_ARM_REPEATS {
            at += Duration::from_millis(30);
            fresh.note_char(at, 99, 'a');
        }
        assert!(fresh.is_armed(at));
    }

    /// RE-EARNING DURING A LIVE WIND-DOWN RE-ARMS — and under the KEY-SWITCH
    /// law the recovery inside a live glow costs only [`KEY_SWITCH_REPS`]
    /// repeats (the FORGIVENESS law), not the full [`SING_ARM_REPEATS`]:
    /// sixteen at OS repeat cadence can outlast [`SING_WIND_DOWN`], which is
    /// exactly how the singer used to drain out and flap under a continuously
    /// held finger. Covers all three restart paths (a gap-hiccup, a stray
    /// other key, a brief pause then resume), none of which the
    /// FRESH-detector wind-down tests exercise.
    #[test]
    fn re_earning_the_threshold_during_wind_down_re_arms() {
        // Path 1 — one auto-repeat hiccup (a single gap > SING_REPEAT_GAP).
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        assert!(d.is_armed(armed));
        // A gap beyond the cadence: the very next 'a' materializes a wind-down.
        let after_gap = armed + SING_REPEAT_GAP + Duration::from_millis(50);
        d.note_char(after_gap, S, 'a');
        assert!(
            d.drive(after_gap) < 1.0 && d.drive(after_gap) > 0.0,
            "the hiccup started a real wind-down"
        );
        // The user keeps holding at cadence; KEY_SWITCH_REPS total presses
        // recover the celebration — a breath, not a re-earn.
        let mut t = after_gap;
        for _ in 1..KEY_SWITCH_REPS {
            t += Duration::from_millis(30);
            d.note_char(t, S, 'a');
        }
        assert!(
            d.is_armed(t),
            "{KEY_SWITCH_REPS} reps inside the glow must re-arm, not keep fading"
        );
        assert_eq!(d.drive(t), 1.0, "the celebration snaps back to full");
        assert_eq!(
            d.beat(t),
            Some(0.0),
            "the beat clock re-anchors at the re-arm"
        );
        assert_eq!(
            d.bar(t),
            Some(SING_FORM_BARS),
            "the recovery reopens the form on a verse, ABOVE the old bars — \
             monotone, so the host latch always fires"
        );

        // Path 2 — a stray other key mid-hold, then the hold resumes. The
        // resume breaks the stray's provisional hand-over (typing law), so
        // the wind anchors at the stray; three at-cadence resumes recover.
        let mut d = KittySing::default();
        let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        let stray = armed + Duration::from_millis(30);
        d.note_char(stray, S, 'b'); // provisional hand-over to 'b'
        assert!(
            d.is_armed(stray),
            "the hand-over keeps the song alive while 'b' is still unproven"
        );
        let mut t = stray;
        for _ in 0..SING_ARM_REPEATS {
            t += Duration::from_millis(30);
            d.note_char(t, S, 'a');
        }
        assert!(d.is_armed(t), "resuming the hold after a stray key re-arms");
        assert_eq!(d.drive(t), 1.0);

        // Path 3 — a brief pause (letting the lazy wind-down start) then resume.
        let mut d = KittySing::default();
        let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        let resume = armed + SING_REPEAT_GAP + Duration::from_secs_f32(SING_WIND_DOWN * 0.4);
        assert!(
            d.drive(resume) > 0.0 && d.drive(resume) < 1.0,
            "the lazy wind-down is mid-flight before the resume"
        );
        let mut t = resume;
        for _ in 0..KEY_SWITCH_REPS {
            d.note_char(t, S, 'a');
            t += Duration::from_millis(30);
        }
        let last = t - Duration::from_millis(30);
        assert!(d.is_armed(last), "resuming after a brief pause re-arms");
        assert_eq!(d.drive(last), 1.0);
    }

    /// A still-live hold (armed, never wound down) must NOT re-anchor its beat
    /// clock every press — that would rewind the dance continuously. Re-arming
    /// is reserved for a run that lapsed into a wind-down and then re-earned it.
    #[test]
    fn a_continuous_hold_does_not_rewind_its_beat_clock() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        let anchor = d.beat(armed);
        // Keep holding at cadence, well past the arm count.
        let mut t = armed;
        for _ in 0..16 {
            t += Duration::from_millis(30);
            d.note_char(t, S, 'a');
        }
        // The beat clock kept advancing from the ORIGINAL anchor — no reset.
        let expected = t.saturating_duration_since(armed).as_secs_f32() / SING_BEAT_SECONDS;
        let got = d.beat(t).expect("still armed");
        assert!(
            (got - expected).abs() < 1e-3 && anchor == Some(0.0),
            "a continuous hold must never re-anchor: got {got}, expected {expected}"
        );
    }

    /// THE NOTE RING IS BOUNDED: at the half-beat cadence — and even under a
    /// hostile spawn flood — the pool never exceeds [`MAX_NOTES`] live notes
    /// and `frames` never yields more sprites than the cap.
    #[test]
    fn note_ring_never_exceeds_the_cap() {
        let mut notes = MusicNotes::default();
        let t0 = Instant::now();
        // 40 half-beats of full drive with an artificially eternal life
        // window (spawns outpace culls by feeding times close together).
        for hb in 0..40u64 {
            let now = t0 + Duration::from_millis(hb * 10); // 100 Hz "half-beats"
            let beat = hb as f32 * 0.5;
            notes.update(now, true, Some(beat), 0);
        }
        let live = notes.ring.iter().flatten().count();
        assert!(live <= MAX_NOTES, "ring overflowed: {live}");
        let mut out = Vec::new();
        notes.frames(t0 + Duration::from_millis(400), false, &mut out);
        assert!(out.len() <= MAX_NOTES);
    }

    /// Wind-down spawns NOTHING new and the field drains to empty — the
    /// visual crossfade: live notes finish, no hard cut, then exact rest.
    #[test]
    fn wind_down_drains_the_note_field() {
        let mut notes = MusicNotes::default();
        let t0 = Instant::now();
        for hb in 0..8u64 {
            let now = t0 + Duration::from_millis(hb * 200);
            notes.update(now, true, Some(hb as f32 * 0.5), 0);
        }
        assert!(notes.is_active());
        // The wind-down (no longer armed): updates cull but never spawn.
        let later = t0 + Duration::from_millis(8 * 200);
        notes.update(later, false, Some(8.0 * 0.5), 0);
        let live_at_release = notes.ring.iter().flatten().count();
        for step in 0..20u64 {
            let now = later + Duration::from_millis(step * 100);
            notes.update(now, false, Some((8 + step) as f32 * 0.5), 0);
            assert!(
                notes.ring.iter().flatten().count() <= live_at_release,
                "wind-down must never spawn"
            );
        }
        assert!(
            !notes.is_active(),
            "the field drains to byte-identical empty"
        );
    }

    /// THE REDUCED-MOTION ARM: the same live field resolves to STATIC
    /// offsets — repeated samples of one note move zero cells (notes without
    /// bob), while the full-motion path rises between the same two samples.
    #[test]
    fn reduced_motion_notes_hold_still() {
        let mut notes = MusicNotes::default();
        let t0 = Instant::now();
        notes.update(t0, true, Some(0.0), 0);
        // Both samples sit past the bloom-in peak, so the second reads the
        // dissolve tail (the envelope rises for the first ~15% of life).
        let (a, b) = (
            t0 + Duration::from_millis(600),
            t0 + Duration::from_millis(1400),
        );
        let sample = |at: Instant, reduced: bool| {
            let mut out = Vec::new();
            notes.frames(at, reduced, &mut out);
            out
        };
        let (ra, rb) = (sample(a, true), sample(b, true));
        assert_eq!(ra.len(), 1);
        assert_eq!(ra[0].dx, rb[0].dx, "reduced motion: no wobble");
        assert_eq!(ra[0].dy, rb[0].dy, "reduced motion: no rise");
        assert!(
            rb[0].alpha < ra[0].alpha,
            "only the fade envelope animates while static"
        );
        let (fa, fb) = (sample(a, false), sample(b, false));
        assert!(
            fb[0].dy < fa[0].dy,
            "full motion: the note rises (−y is up)"
        );
    }

    /// The note tiles honor the free-sprite art contracts: deterministic per
    /// key, kind-distinct texels, visible-but-not-opaque ink, and real (bounded)
    /// coverage — a glyph, not a filled box.
    #[test]
    fn note_bake_is_deterministic_distinct_and_translucent() {
        let (w, h) = note_nat_size(NoteKind::Eighth, 20);
        let (bw, bh) = note_nat_size(NoteKind::Beamed, 20);
        assert_eq!(
            bake_note(w, h, NoteKind::Eighth).pixels(),
            bake_note(w, h, NoteKind::Eighth).pixels(),
            "deterministic per (kind, w, h)"
        );
        assert_ne!(
            bake_note(bw, bh, NoteKind::Eighth).pixels(),
            bake_note(bw, bh, NoteKind::Beamed).pixels(),
            "♪ and ♫ must be different glyphs"
        );
        let tile = bake_note(w, h, NoteKind::Eighth);
        let alphas: Vec<u8> = tile
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect();
        let max_a = alphas.iter().copied().max().unwrap_or(0);
        assert!(
            (100..250).contains(&max_a),
            "ink window: densest texel a={max_a} visible but never fully opaque"
        );
        let covered = alphas.iter().filter(|a| **a > 8).count() as f32 / alphas.len() as f32;
        assert!(
            (0.05..0.75).contains(&covered),
            "coverage {covered:.2}: a glyph, not a box"
        );
    }

    /// Note host ids stay out of the kitty-sprite id family in the shared atlas.
    #[test]
    fn note_host_ids_avoid_kitty_sprite_family() {
        for kind in [NoteKind::Eighth, NoteKind::Beamed] {
            for (w, h) in [(9u16, 15u16), (15, 22), (1, 1)] {
                let id = note_host_id(kind, w, h);
                for generation in 0..64u64 {
                    assert_ne!(id, crate::kitty_cursor::HOST_ID ^ generation);
                }
            }
        }
    }

    /// The documented tempo constants ARE the contract the audio riff pins
    /// against (`trail_sound::celebration_bar_matches_the_visual_clock`).
    #[test]
    fn tempo_constants_are_pinned() {
        assert_eq!(SING_BEAT_SECONDS, 0.4);
        assert_eq!(SING_BAR_SECONDS, 1.6);
        assert_eq!(SING_ARM_REPEATS, 16);
    }

    /// THE MIXER IS A BIJECTION — the zero-collision proof. Each stage of
    /// [`song_signature`] is individually invertible (xorshift by 16/15 and
    /// two odd multiplies), so the whole map round-trips: this test builds
    /// the exact inverse (Newton–Hensel for the modular inverses of the two
    /// multipliers, the standard xorshift unwinding for the shifts) and
    /// walks EVERY valid `char`. Injectivity over the full char range
    /// follows: two characters sharing a signature would break the round
    /// trip. This is the property the old `ch % 5` could never have — it
    /// folded the whole alphabet onto five songs.
    #[test]
    fn song_signature_mixer_is_bijective_over_all_chars() {
        /// Multiplicative inverse of odd `m` mod 2^32 (Newton–Hensel: each
        /// step doubles the correct low bits; 5 steps ≥ 32 bits).
        fn minv(m: u32) -> u32 {
            let mut x = m;
            for _ in 0..5 {
                x = x.wrapping_mul(2u32.wrapping_sub(m.wrapping_mul(x)));
            }
            x
        }
        let (inv_a, inv_b) = (minv(0x7feb_352d), minv(0x846c_a68b));
        assert_eq!(inv_a.wrapping_mul(0x7feb_352d), 1);
        assert_eq!(inv_b.wrapping_mul(0x846c_a68b), 1);
        let unmix = |y: u32| -> u32 {
            let mut x = y ^ (y >> 16);
            x = x.wrapping_mul(inv_b);
            x = x ^ (x >> 15) ^ (x >> 30);
            x = x.wrapping_mul(inv_a);
            x ^ (x >> 16)
        };
        for ch in (0u32..=0x10_FFFF).filter_map(char::from_u32) {
            assert_eq!(
                unmix(song_signature(ch)),
                ch as u32,
                "mixer round trip failed at U+{:04X}",
                ch as u32
            );
        }
    }

    /// The old defect's poster children: `(ch % 5)` made 'a','f','k','p',
    /// 'u','z' and space BIT-IDENTICAL songs (its doc claimed a-vs-z
    /// differed). Every pair now carries a distinct signature.
    #[test]
    fn old_transpose_collision_classes_now_sing_apart() {
        let class = ['a', 'f', 'k', 'p', 'u', 'z', ' '];
        for (i, &a) in class.iter().enumerate() {
            for &b in &class[i + 1..] {
                assert_ne!(
                    song_signature(a),
                    song_signature(b),
                    "{a:?} and {b:?} were bit-identical under ch % 5 and must \
                     never be again"
                );
            }
        }
    }

    /// Nothing held ⇒ the neutral signature (the synth's untransposed
    /// reference voicing), and an armed run reports its char's signature.
    #[test]
    fn signature_is_neutral_at_rest_and_the_run_chars_while_held() {
        let mut d = KittySing::default();
        assert_eq!(d.signature(), NEUTRAL_SIGNATURE);
        let t0 = Instant::now();
        let t = hold(&mut d, t0, 'w', SING_ARM_REPEATS, 30);
        assert!(d.is_armed(t));
        assert_eq!(d.signature(), song_signature('w'));
    }

    /// A miniature of the HOST's render loop for the narrative tests below:
    /// presses feed the detector, and every ~16 ms frame applies
    /// `app_render`'s exact `sing_riff_bar` latch — push one
    /// `(bar, signature())` per NEW bar index, signature sampled at push
    /// time. `min_drive` tracks the lowest drive any frame saw after the
    /// first arm — the number the host's `sing_face_live` gate (0.33) reads.
    struct HostSim {
        d: KittySing,
        clock: Instant,
        latch: Option<u64>,
        pushed: Vec<(u64, u32)>,
        ever_armed: bool,
        min_drive: f32,
    }

    impl HostSim {
        fn new(t0: Instant) -> Self {
            Self {
                d: KittySing::default(),
                clock: t0,
                latch: None,
                pushed: Vec::new(),
                ever_armed: false,
                min_drive: f32::INFINITY,
            }
        }

        /// Render frames at ~60 Hz up to `until` (the host latch + gate).
        fn run_to(&mut self, until: Instant) {
            while self.clock <= until {
                if let Some(bar) = self.d.bar(self.clock)
                    && self.latch != Some(bar)
                {
                    self.latch = Some(bar);
                    self.pushed.push((bar, self.d.signature()));
                }
                if self.d.is_armed(self.clock) {
                    self.ever_armed = true;
                }
                if self.ever_armed {
                    self.min_drive = self.min_drive.min(self.d.drive(self.clock));
                }
                self.clock += Duration::from_millis(16);
            }
        }

        fn press(&mut self, at: Instant, ch: char) {
            self.run_to(at);
            self.d.note_char(at, S, ch);
        }

        /// Hold `ch` for `n` presses at `gap_ms` cadence starting at `from`,
        /// rendering frames between presses; returns the last press instant.
        fn hold(&mut self, from: Instant, ch: char, n: u32, gap_ms: u64) -> Instant {
            let mut t = from;
            for i in 0..n {
                t = from + Duration::from_millis(u64::from(i) * gap_ms);
                self.press(t, ch);
            }
            t
        }
    }

    /// COMPLAINT #1, THE LAW: switching the held key changes the verse at
    /// the next bar boundary. The next RiffBar the host pushes after the
    /// switch commits must carry the NEW key's signature AND a bar index
    /// that reopens the form (a multiple of the 8-bar form — slot 0, the
    /// new key's own A-section verse), so the new tune announces itself
    /// immediately instead of hiding behind up to two shared-chorus bars.
    #[test]
    fn switching_the_held_key_changes_the_verse_at_the_next_bar() {
        let t0 = Instant::now();
        let mut host = HostSim::new(t0);
        let armed = host.hold(t0, 'w', 20, 30);
        assert_eq!(host.pushed.len(), 1, "the arm pushed exactly bar 0");
        assert_eq!(host.pushed[0].1, song_signature('w'));
        // Switch to 'a' and keep holding well past the next bar boundary.
        host.hold(armed + Duration::from_millis(30), 'a', 60, 30);
        assert!(
            host.pushed.len() >= 2,
            "the next bar boundary pushed a RiffBar"
        );
        let (bar, sig) = host.pushed[1];
        assert_eq!(
            sig,
            song_signature('a'),
            "the next pushed RiffBar sings the NEW key"
        );
        assert_ne!(sig, song_signature('w'));
        assert_eq!(
            bar % SING_FORM_BARS,
            0,
            "the switch reopens the form on the new key's own verse \
             (slot 0), not mid-form: got bar {bar}"
        );
        assert!(bar > host.pushed[0].0, "the bar index stays monotone");
    }

    /// COMPLAINT #2, THE LAW: the owner's exact pattern — hold `w`, hold
    /// `a`, hold `r`, every gap inside the repeat cadence — is a KEY SWITCH
    /// chain, not typing. The drive stays at exactly 1.0 from the first arm
    /// through the last press: the singer never leaves the stage, so the
    /// host's 0.33 face gate can never swap in the cursor kitty mid-song.
    #[test]
    fn the_owner_pattern_w_a_r_never_drops_the_drive() {
        let t0 = Instant::now();
        let mut host = HostSim::new(t0);
        let mut t = host.hold(t0, 'w', 20, 30);
        for ch in ['a', 'r'] {
            t = host.hold(t + Duration::from_millis(30), ch, 20, 30);
        }
        host.run_to(t);
        assert!(host.ever_armed);
        assert_eq!(
            host.min_drive, 1.0,
            "three consecutive held keys are one unbroken celebration"
        );
        assert!(host.d.is_armed(t), "still on stage after w -> a -> r");
        assert_eq!(host.d.signature(), song_signature('r'));
    }

    /// THE INSTRUMENT SCENARIO: a six-key medley, each key held for a full
    /// bar or more. The drive never dips, every key's section gets heard,
    /// each new section opens on that key's own verse (form slot 0), and
    /// the pushed bar indices stay strictly monotone (the host latch can
    /// never swallow a section). Tenures are 1.95 s so every switch commit
    /// lands clear of a bar boundary — the one-frame boundary race takes
    /// the documented one bar of grace instead ([`KittySing::reopen_section`]).
    #[test]
    fn a_six_key_medley_holds_the_stage_end_to_end() {
        let t0 = Instant::now();
        let mut host = HostSim::new(t0);
        let medley = ['w', 'a', 'r', 't', 'z', 'q'];
        let mut t = t0;
        for (i, &ch) in medley.iter().enumerate() {
            let from = if i == 0 {
                t0
            } else {
                t + Duration::from_millis(30)
            };
            t = host.hold(from, ch, 65, 30); // 1.95 s > one 1.6 s bar each
        }
        host.run_to(t);
        assert_eq!(host.min_drive, 1.0, "the singer never leaves the stage");
        for window in host.pushed.windows(2) {
            assert!(window[1].0 > window[0].0, "bar indices strictly monotone");
        }
        let mut heard = Vec::new();
        for &(bar, sig) in &host.pushed {
            if heard.last() != Some(&sig) {
                heard.push(sig);
                if heard.len() > 1 {
                    assert_eq!(
                        bar % SING_FORM_BARS,
                        0,
                        "every switched-to key opens on its own verse (slot 0)"
                    );
                }
            }
        }
        assert_eq!(
            heard,
            medley.map(song_signature).to_vec(),
            "every key in the medley was heard, in order"
        );
    }

    /// GENUINE TYPING STILL WINDS THE SONG DOWN: distinct characters that do
    /// NOT repeat are typing, and typing loses the celebration exactly as
    /// before — wind-down anchored at the departure, drive to 0, settle to
    /// byte-identical rest.
    #[test]
    fn genuine_typing_still_winds_the_song_down() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let armed = hold(&mut d, t0, 'w', SING_ARM_REPEATS, 30);
        assert!(d.is_armed(armed));
        let a = armed + Duration::from_millis(30);
        d.note_char(a, S, 'a'); // provisional — could still be a switch
        let r = a + Duration::from_millis(30);
        d.note_char(r, S, 'r'); // a second distinct char: this is TYPING
        assert!(
            d.drive(r) < 1.0,
            "typing proves itself and the wind-down is already running"
        );
        let gone = a + Duration::from_secs_f32(SING_WIND_DOWN * 1.05);
        assert_eq!(d.drive(gone), 0.0, "the crossfade completes");
        d.settle(gone);
        assert_eq!(d.beat(gone), None, "settled = byte-identical rest");
    }

    /// A SWITCH REOPENS ON THE NEW KEY'S VERSE, NOT THE CHORUS: switch late
    /// in the form (during bar 6 — the chorus block 6/7 is next) and the
    /// next pushed bar must still be a form-opening verse index, never the
    /// shared chorus that would hide the new tune for two more bars.
    #[test]
    fn a_switch_reopens_on_the_new_keys_verse_not_the_chorus() {
        let t0 = Instant::now();
        let mut host = HostSim::new(t0);
        // Hold 'w' deep into the form: past the bar-6 boundary.
        let reps_past_bar_6 = SING_ARM_REPEATS + (6 * 1600 + 200) / 30;
        let deep = host.hold(t0, 'w', reps_past_bar_6, 30);
        assert_eq!(
            host.pushed.last().expect("bars pushed").0 % SING_FORM_BARS,
            6,
            "the switch happens while the bridge (chorus block) sounds"
        );
        let n_before = host.pushed.len();
        host.hold(deep + Duration::from_millis(30), 'a', 70, 30);
        let &(bar, sig) = host
            .pushed
            .get(n_before)
            .expect("the boundary after the switch pushed a bar");
        assert_eq!(sig, song_signature('a'));
        assert_eq!(
            bar % SING_FORM_BARS,
            0,
            "the form reopens on the new key's verse — not chorus bar 7"
        );
    }

    /// FORGIVENESS: the owner pauses a beat too long mid-switch (the OS
    /// initial repeat delay of the NEW key runs past [`SING_REPEAT_GAP`], so
    /// the wind-down starts). While the glow is still fading, three repeats
    /// of the new key recover the celebration — and the drive never falls
    /// through the host's 0.33 face gate, so the singer is never swapped
    /// out for the cursor kitty.
    #[test]
    fn a_missed_gap_recovers_with_three_reps_inside_the_glow() {
        let t0 = Instant::now();
        let mut host = HostSim::new(t0);
        let armed = host.hold(t0, 'w', 20, 30);
        // The switch press, then the OS initial repeat delay (400 ms > gap).
        let a1 = armed + Duration::from_millis(30);
        host.press(a1, 'a');
        let mut t = a1 + Duration::from_millis(400);
        for _ in 0..KEY_SWITCH_REPS {
            host.press(t, 'a'); // KEY_SWITCH_REPS genuine repeats in the glow
            t += Duration::from_millis(80);
        }
        let recovered = t - Duration::from_millis(80);
        host.run_to(recovered);
        assert!(
            host.d.is_armed(recovered),
            "three reps inside the glow re-arm — not sixteen"
        );
        assert_eq!(host.d.drive(recovered), 1.0, "back to full song");
        assert!(
            host.min_drive >= 0.33,
            "the dip never crosses the 0.33 face gate — the singer stayed \
             (worst frame: {})",
            host.min_drive
        );
        let bar = host.d.bar(recovered).expect("scheduling again");
        assert_eq!(
            bar % SING_FORM_BARS,
            0,
            "the recovery reopens on the new key's verse"
        );
        assert!(
            host.pushed.iter().all(|&(b, _)| b <= bar),
            "monotone: the reopened section outruns everything pushed before"
        );
    }

    /// COLD ARMS STILL COST SIXTEEN: the three-rep forgiveness exists only
    /// inside a live glow (drive > 0). From rest — or after a wind-down has
    /// fully drained and settled — the deliberate-hold bar is unchanged.
    #[test]
    fn cold_arms_still_cost_sixteen() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let t = hold(&mut d, t0, 'a', KEY_SWITCH_REPS, 30);
        assert!(!d.is_armed(t), "three reps from rest arm nothing");
        let t = hold(
            &mut d,
            t0 + Duration::from_secs(2),
            'a',
            SING_ARM_REPEATS,
            30,
        );
        assert!(d.is_armed(t), "sixteen still arm");
        // Wind fully down, settle, and the forgiveness window is CLOSED.
        d.note_break(t + Duration::from_millis(30));
        let drained = t + Duration::from_secs_f32(SING_WIND_DOWN + 0.1);
        assert_eq!(d.drive(drained), 0.0);
        d.settle(drained);
        let again = hold(
            &mut d,
            drained + Duration::from_millis(30),
            'a',
            KEY_SWITCH_REPS,
            30,
        );
        assert!(
            !d.is_armed(again),
            "after the glow is gone, three reps are just typing again"
        );
    }

    /// THE CLASS MAP: vowels are chorus keys, digits bridge, punctuation
    /// percussive, Space the breath — and EVERYTHING ELSE (consonants,
    /// shifted letters, IME/unicode) is a verse key. Every char still
    /// sings; the class only shapes how it is played.
    #[test]
    fn section_classes_cover_every_character() {
        for v in "aeiouAEIOU".chars() {
            assert_eq!(section_class(v), 1, "{v:?} is a chorus key");
        }
        for c in "bcdfgqwrtzXYZ".chars() {
            assert_eq!(section_class(c), 0, "{c:?} is a verse key");
        }
        for d in "0123456789".chars() {
            assert_eq!(section_class(d), 2, "{d:?} is a bridge key");
        }
        for p in "!#$%&*.,;:~-_=+/\\'\"`^@?<>()[]{}|".chars() {
            assert_eq!(section_class(p), 3, "{p:?} is a percussive key");
        }
        assert_eq!(section_class(' '), 4, "Space is the breath");
        for off_map in ['é', 'ß', '猫', '—', '¿'] {
            assert_eq!(
                section_class(off_map),
                0,
                "{off_map:?} still sings, as a verse"
            );
        }
        assert_eq!(ARC_EARN.len(), 5, "one earn rate per class");
    }

    /// ACCEPTANCE 1 — the arc reproduces today's build on a single hold: a
    /// continuous consonant hold's per-bar payload walks the exact shim
    /// curve `q = 60 + 25·bar` (energy 0.30 + 0.125/bar — build 1.0 at bar
    /// 6, the clap gate 230 crossed from bar 7), the intro bars render
    /// ≤ 0.55×200, and the rise is monotone.
    #[test]
    fn the_arc_reproduces_todays_build_on_a_single_hold() {
        let t0 = Instant::now();
        let mut d = KittySing::default();
        let armed = hold(&mut d, t0, 't', SING_ARM_REPEATS, 30);
        assert_eq!(
            d.arc_energy_q(armed),
            60,
            "the arc opens at ARC_START × 200"
        );
        let mut t = armed;
        let mut prev_q = 0u8;
        // Hold through 10 bars, sampling the payload at each bar's middle.
        while d.bar(t).is_some_and(|b| b <= 10) {
            t += Duration::from_millis(30);
            d.note_char(t, S, 't');
            let bar = d.bar(t).expect("armed");
            let q = d.arc_energy_q(t);
            let expect = (60 + 25 * bar).min(250) as u8;
            assert_eq!(
                q, expect,
                "bar {bar}: the single-hold verse curve is the shim curve"
            );
            if bar < u64::from(ARC_INTRO_BARS) {
                assert!(
                    f32::from(q) / 200.0 <= ARC_INTRO_CAP,
                    "intro bars render capped (bar {bar}: q={q})"
                );
            }
            assert!(q >= prev_q, "the verse build is monotone");
            prev_q = q;
            if bar == 6 {
                assert!(q >= 200, "build hits 1.0 at bar 6 — the legacy pin");
            }
            if bar == 7 {
                assert!(q >= 230, "bar 7 crosses the verse clap gate — the new law");
            }
        }
        assert_eq!(d.arc_phase(t), ArcPhase::Peak, "a long hold peaks");
    }

    /// ACCEPTANCE 2 — a chorus key cashes in the verse build: hold 't' for
    /// five bars (energy 0.30 + 5×0.125 = 0.925), switch to 'e' — the drive
    /// never dips, and the first chorus-key bar's payload carries class 1
    /// and energy_q 235 (0.925 + 0.25 = 1.175, INTERNAL encoding): PEAK on
    /// the chorus key's first full bar, faster than either key alone.
    #[test]
    fn a_chorus_key_cashes_in_the_verse_build() {
        let t0 = Instant::now();
        let mut host = HostSim::new(t0);
        // Hold 't' through five full bars (arm ≈ 0.45 s in, so ~5.6 s of
        // presses at 30 ms cadence keeps bar 5 in flight at the switch).
        let mut t = t0;
        while host.d.bar(t).is_none_or(|b| b < 5) {
            t += Duration::from_millis(30);
            host.press(t, 't');
        }
        assert_eq!(host.d.section_class_now(), 0);
        let energy_before = host.d.arc_energy_q(t);
        assert_eq!(energy_before, 60 + 25 * 5, "five verse bars banked");
        // The switch: 'e' commits within three at-cadence repeats.
        for _ in 0..KEY_SWITCH_REPS {
            t += Duration::from_millis(30);
            host.press(t, 'e');
        }
        assert_eq!(host.d.section_class_now(), 1, "the chorus class is live");
        // Keep holding 'e' across the next bar boundary.
        let mut q_at_reopen = None;
        let reopen_deadline = t + Duration::from_millis(1700);
        while t < reopen_deadline {
            t += Duration::from_millis(30);
            host.press(t, 'e');
            if q_at_reopen.is_none()
                && host
                    .pushed
                    .last()
                    .is_some_and(|&(_, sig)| sig == song_signature('e'))
            {
                q_at_reopen = Some(host.d.arc_energy_q(t));
            }
        }
        host.run_to(t);
        assert_eq!(
            host.min_drive, 1.0,
            "the drive never dips across the switch"
        );
        assert_eq!(
            q_at_reopen,
            Some(235),
            "the chorus key's first bar carries the inherited build + its own earn"
        );
        assert_eq!(
            host.d.arc_phase(t),
            ArcPhase::Peak,
            "peak on the first chorus bar"
        );
    }

    /// The BREATH: Space earns NEGATIVE energy — the deliberate quiet
    /// passage — and the energy floor is 0, never below.
    #[test]
    fn a_held_space_breathes_the_energy_down() {
        let t0 = Instant::now();
        let mut d = KittySing::default();
        let armed = hold(&mut d, t0, 'e', SING_ARM_REPEATS, 30);
        let mut t = armed;
        // Four chorus bars up…
        while d.bar(t).is_none_or(|b| b < 4) {
            t += Duration::from_millis(30);
            d.note_char(t, S, 'e');
        }
        let hot = d.arc_energy_q(t);
        // …then hand over to Space and hold the breath for three bars.
        let switch_bar = d.bar(t).unwrap();
        while d.bar(t).is_none_or(|b| b < switch_bar + 4) {
            t += Duration::from_millis(30);
            d.note_char(t, S, ' ');
        }
        assert_eq!(d.section_class_now(), 4);
        let quiet = d.arc_energy_q(t);
        assert!(quiet < hot, "the breath decays energy ({hot} -> {quiet})");
        assert_eq!(d.drive(t), 1.0, "a held breath is still a hold");
    }

    /// ACCEPTANCE 7 — energy carries across a quick re-arm but not a cold
    /// one: 3.0 s after releasing at energy 1.0 the first payload reads
    /// q == 150 (0.75 × 1.0), the intro is skipped and the bar payload
    /// follows the monotone reopen mapping (asserted against
    /// `section_reopen_bar`, not a literal 0); 8.0 s after, the arc is
    /// fresh (q == 60) with the intro cap active. Pins the extended warm
    /// window as INTENDED behavior.
    #[test]
    fn energy_carries_across_a_quick_rearm_but_not_a_cold_one() {
        let t0 = Instant::now();
        // Release "from energy 1.0", earned the instrument's way: a pure
        // verse walk steps 0.925 → 1.05 and never lands on 1.0, so bank
        // four 't' bars (0.30 + 4×0.125 = 0.80) and one '.' percussive bar
        // (+0.20) — exactly 1.00 at the release.
        let mut d = KittySing::default();
        let armed = hold(&mut d, t0, 't', SING_ARM_REPEATS, 30);
        let mut t = armed;
        while d.bar(t).is_none_or(|b| b < 4) {
            t += Duration::from_millis(30);
            d.note_char(t, S, 't');
        }
        let base_bar = d.bar(t).unwrap();
        while d.bar(t).is_none_or(|b| b < base_bar + 1) {
            t += Duration::from_millis(30);
            d.note_char(t, S, '.');
        }
        assert_eq!(d.arc_energy_q(t), 200, "energy 1.0 on the button");
        let release = t + Duration::from_millis(30);
        d.note_break(release);
        // WARM: re-arm 3.0 s after the release (well inside the 6 s window,
        // and past the drive-0 settle — the ember must survive `settle`).
        let settled = release + Duration::from_secs_f32(SING_WIND_DOWN + 0.2);
        assert_eq!(d.drive(settled), 0.0);
        d.settle(settled);
        let again = release + Duration::from_secs(3);
        let rearmed = hold(&mut d, again, 'w', SING_ARM_REPEATS, 30);
        assert!(d.is_armed(rearmed), "sixteen fresh reps re-arm");
        assert_eq!(
            d.arc_energy_q(rearmed),
            150,
            "0.75 of the released energy carries"
        );
        assert!(
            !d.intro_active(rearmed),
            "a carry at performance temperature skips the intro"
        );
        assert_eq!(
            d.bar(rearmed),
            Some(0),
            "a post-settle re-arm follows the reopen mapping from rest \
             (section_shift 0 — the mapping's cold branch, asserted against \
             the mapping, not assumed)"
        );
        // COLD: the same dance 8.0 s after a release reads a fresh arc.
        let mut d = KittySing::default();
        let armed = hold(&mut d, t0, 't', SING_ARM_REPEATS, 30);
        let mut t = armed;
        while d.bar(t).is_none_or(|b| b < 6) {
            t += Duration::from_millis(30);
            d.note_char(t, S, 't');
        }
        let release = t + Duration::from_millis(30);
        d.note_break(release);
        let settled = release + Duration::from_secs_f32(SING_WIND_DOWN + 0.2);
        d.settle(settled);
        let much_later = release + Duration::from_secs(8);
        let rearmed = hold(&mut d, much_later, 'w', SING_ARM_REPEATS, 30);
        assert_eq!(d.arc_energy_q(rearmed), 60, "beyond the window: cold open");
        assert!(d.intro_active(rearmed), "and the intro cap is active again");
    }

    /// A RECOVERY inside the live glow (the forgiveness path) is a WARM
    /// re-arm too: the ember is fresh by construction, so the energy
    /// carries — and the reopened bar payload still follows the monotone
    /// reopen mapping (the live branch of acceptance 7's mapping clause).
    #[test]
    fn a_glow_recovery_carries_energy_onto_the_reopened_verse() {
        let t0 = Instant::now();
        let mut d = KittySing::default();
        let armed = hold(&mut d, t0, 't', SING_ARM_REPEATS, 30);
        let mut t = armed;
        while d.bar(t).is_none_or(|b| b < 4) {
            t += Duration::from_millis(30);
            d.note_char(t, S, 't');
        }
        let e_before = d.arc_energy_q(t); // 0.80 × 200
        assert_eq!(e_before, 160);
        // A cadence hiccup: the next press lands past the repeat gap.
        let raw_at_wind = d.bar(t).unwrap();
        let after_gap = t + SING_REPEAT_GAP + Duration::from_millis(60);
        d.note_char(after_gap, S, 't');
        let mut t = after_gap;
        for _ in 1..KEY_SWITCH_REPS {
            t += Duration::from_millis(30);
            d.note_char(t, S, 't');
        }
        assert!(d.is_armed(t), "the glow recovery re-armed");
        assert_eq!(
            d.arc_energy_q(t),
            (0.75f32 * 0.80 * 200.0).round() as u8,
            "the recovery keeps 0.75 of the ember"
        );
        let reopened = d.bar(t).expect("scheduling again");
        assert_eq!(
            reopened,
            section_reopen_bar(raw_at_wind),
            "the recovery bar follows the monotone reopen mapping"
        );
    }

    /// ACCEPTANCE 3 (the host half) — an early switch fills beat four and
    /// lands next bar: a commit before beat 3.0 latches EXACTLY ONE fill
    /// cue carrying (old sig, new sig); the drive never dips; the next
    /// pushed payload is the new key's; and the singer's head-bob window
    /// (`fill_beat`) opens on the bar's final beat.
    #[test]
    fn an_early_switch_fills_beat_four_and_lands_next_bar() {
        let t0 = Instant::now();
        let mut host = HostSim::new(t0);
        let armed = host.hold(t0, 't', 20, 30);
        // Walk to ~0.7 s into a fresh bar, then commit the switch there:
        // three at-cadence 'e' presses land the commit near 0.8 s — well
        // inside the 1.2 s runway.
        let bar_now = host.d.bar(armed).expect("armed");
        let mut t = armed;
        while host.d.bar(t) == Some(bar_now) {
            t += Duration::from_millis(30);
            host.press(t, 't');
        }
        // t is now just past a bar boundary; run 0.7 s into the bar.
        let mut in_bar = t;
        while in_bar < t + Duration::from_millis(700) {
            in_bar += Duration::from_millis(30);
            host.press(in_bar, 't');
        }
        let n_pushed = host.pushed.len();
        for _ in 0..KEY_SWITCH_REPS {
            in_bar += Duration::from_millis(30);
            host.press(in_bar, 'e');
        }
        assert_eq!(
            host.d.take_fill_cue(),
            Some((song_signature('t'), song_signature('e'))),
            "exactly one cue, old key then new"
        );
        assert_eq!(host.d.take_fill_cue(), None, "consumed once — never twice");
        assert!(
            !host.d.fill_beat(in_bar),
            "beat 4 has not arrived yet at commit time (~0.8 s)"
        );
        // Hold 'e' to the bar's final beat: the head-bob window opens…
        let mut t = in_bar;
        while !host.d.fill_beat(t) {
            t += Duration::from_millis(30);
            host.press(t, 'e');
            assert_eq!(host.d.drive(t), 1.0, "the drive never dips");
        }
        // …and the boundary lands the NEW key's payload.
        while host.pushed.len() == n_pushed {
            t += Duration::from_millis(30);
            host.press(t, 'e');
        }
        let &(bar, sig) = host.pushed.last().unwrap();
        assert_eq!(
            sig,
            song_signature('e'),
            "the next payload sings the new key"
        );
        assert_eq!(
            bar % SING_FORM_BARS,
            0,
            "…on its own verse (the reopen law)"
        );
        assert!(
            host.d.switch_landing(t),
            "the commit landing pulse covers the reopened bar's first beat"
        );
        host.run_to(t);
        assert_eq!(host.min_drive, 1.0, "drive sampled every frame == 1.0");
    }

    /// ACCEPTANCE 4 — a late switch commits WITHOUT a cue: a commit past
    /// beat 3.0 latches nothing; the reopen alone announces the switch at
    /// the bar edge, exactly per the foundation contract.
    #[test]
    fn a_late_switch_commits_without_a_cue() {
        let t0 = Instant::now();
        let mut host = HostSim::new(t0);
        let armed = host.hold(t0, 't', 20, 30);
        let bar_now = host.d.bar(armed).expect("armed");
        let mut t = armed;
        while host.d.bar(t) == Some(bar_now) {
            t += Duration::from_millis(30);
            host.press(t, 't');
        }
        // 1.3 s into the fresh bar: past the runway.
        let mut in_bar = t;
        while in_bar < t + Duration::from_millis(1300) {
            in_bar += Duration::from_millis(30);
            host.press(in_bar, 't');
        }
        let n_pushed = host.pushed.len();
        for _ in 0..KEY_SWITCH_REPS {
            in_bar += Duration::from_millis(30);
            host.press(in_bar, 'e');
        }
        assert_eq!(host.d.take_fill_cue(), None, "no runway ⇒ zero cues");
        // The bar edge still lands the new key within one bar.
        let mut t = in_bar;
        while host.pushed.len() == n_pushed {
            t += Duration::from_millis(30);
            host.press(t, 'e');
            assert!(
                t.saturating_duration_since(in_bar).as_secs_f32() <= SING_BAR_SECONDS + 0.1,
                "the new payload arrives within one bar of the commit"
            );
        }
        assert_eq!(host.pushed.last().unwrap().1, song_signature('e'));
    }

    /// ACCEPTANCE 9 — the pre-echo flips within one frame: the very PRESS
    /// that starts a hand-over flips `signature`/`section_class_now` (the
    /// note tint + pose bias read them next frame) while the audio side —
    /// the pushed payloads — stays untouched until the bar edge.
    #[test]
    fn the_pre_echo_flips_within_one_frame() {
        let t0 = Instant::now();
        let mut host = HostSim::new(t0);
        let armed = host.hold(t0, 't', 20, 30);
        let pushed_before = host.pushed.clone();
        let press = armed + Duration::from_millis(30);
        host.press(press, 'e'); // ONE press — provisional, not yet a switch
        assert_eq!(
            host.d.signature(),
            song_signature('e'),
            "identity flips on the hand-over press itself"
        );
        assert_eq!(host.d.section_class_now(), 1, "class flips with it");
        assert_eq!(
            host.pushed, pushed_before,
            "no new payload before the bar edge — the sounding bar's audio \
             is untouched"
        );
        // The tint half: the next spawned note carries the destination
        // class's stripe range (chorus = the full rainbow, offset 0).
        let mut notes = MusicNotes::default();
        notes.update(press, true, Some(0.0), host.d.section_class_now());
        let mut out = Vec::new();
        notes.frames(press + Duration::from_millis(200), false, &mut out);
        assert_eq!(out.len(), 1, "one spawn on the fresh half-beat");
    }

    /// The stripe ranges per class: verse two stripes, chorus all six,
    /// bridge the cool half, percussive the warm half, breath a single dim
    /// violet — and every range stays inside [`NOTE_TINTS`].
    #[test]
    fn note_stripes_follow_the_section_class() {
        for class in 0u8..=4 {
            let (off, len) = class_stripes(class);
            assert!(len >= 1 && off + len <= NOTE_TINTS.len(), "class {class}");
        }
        assert_eq!(class_stripes(1), (0, 6), "chorus: the whole rainbow");
        assert_eq!(class_stripes(4).1, 1, "breath: a single stripe");
        // Spawn a chorus-class and a breath-class note; the breath renders
        // dimmer at the same age and holds the violet stripe.
        let t0 = Instant::now();
        let mut chorus = MusicNotes::default();
        chorus.update(t0, true, Some(0.0), 1);
        let mut breath = MusicNotes::default();
        breath.update(t0, true, Some(0.0), 4);
        let sample = |n: &MusicNotes| {
            let mut out = Vec::new();
            n.frames(t0 + Duration::from_millis(300), false, &mut out);
            out[0]
        };
        let (c, b) = (sample(&chorus), sample(&breath));
        assert_eq!(b.tint, NOTE_TINTS[5], "breath notes are violet");
        assert!(
            u32::from(b.alpha) * 10 < u32::from(c.alpha) * 8,
            "the breath's notes render dim ({} vs {})",
            b.alpha,
            c.alpha
        );
    }

    /// ACCEPTANCE 6 — the finale cadence lands and bows. A lazy release
    /// mid-bar-6: exactly one cadence at `armed_at + 7×1.6 s`, carrying
    /// the released key, the earned energy and span 6; the drive holds
    /// 1.0 until cadence + 0.8 s and reads 0 by + 1.8 s; the bow rides
    /// beat 2; the fireworks count is `min(4 + 6/2, 16) == 7`. A
    /// three-bar performance earns nothing; Escape earns nothing.
    #[test]
    fn the_finale_cadence_lands_and_bows() {
        let t0 = Instant::now();
        let mut d = KittySing::default();
        let armed = hold(&mut d, t0, 'w', SING_ARM_REPEATS, 30);
        let anchor = armed; // armed_at == the threshold press instant
        // Hold into bar 6, stop pressing at ~beat 1.5 of it (0.6 s in).
        let mut t = armed;
        while d.bar(t).is_none_or(|b| b < 6) {
            t += Duration::from_millis(30);
            d.note_char(t, S, 'w');
        }
        let mut last = t;
        while last < t + Duration::from_millis(350) {
            last += Duration::from_millis(30);
            d.note_char(last, S, 'w');
        }
        // The finger lifts: the lazy release lands mid-bar 6.
        let due = anchor + Duration::from_secs_f32(7.0 * SING_BAR_SECONDS);
        let energy_expect = d.arc_energy_q(last);
        assert_eq!(
            d.take_cadence(due - Duration::from_millis(400)),
            None,
            "nothing fires before due"
        );
        let fired = d.take_cadence(due + Duration::from_millis(16));
        assert_eq!(
            fired,
            Some((song_signature('w'), energy_expect, 6)),
            "one cadence: the released key, the earned energy, span 6"
        );
        assert_eq!(
            d.take_cadence(due + Duration::from_millis(32)),
            None,
            "consumed exactly once"
        );
        // The deferred wind-down: full through the tonic, gone by +1.8 s.
        assert_eq!(
            d.drive(due + Duration::from_secs_f32(CADENCE_DRIVE_HOLD - 0.01)),
            1.0,
            "the drive holds through the cadence"
        );
        assert_eq!(
            d.drive(due + Duration::from_secs_f32(CADENCE_DRIVE_HOLD + SING_WIND_DOWN + 0.01)),
            0.0,
            "and completes the standard smoothstep after"
        );
        // The bow: beat 2 of the cadence bar, down-hold-rise.
        let bow_start = due + Duration::from_secs_f32(CADENCE_DRIVE_HOLD);
        assert_eq!(d.bow_depth(bow_start - Duration::from_millis(50)), 0.0);
        assert_eq!(
            d.bow_depth(bow_start + Duration::from_secs_f32(BOW_DOWN + 0.1)),
            1.0,
            "full depth through the hold"
        );
        assert_eq!(
            d.bow_depth(bow_start + Duration::from_secs_f32(BOW_DOWN + BOW_HOLD + BOW_RISE + 0.05)),
            0.0,
            "risen and done"
        );
        // The fireworks scale: 6 bars ⇒ 7 notes into the ring.
        let mut notes = MusicNotes::default();
        notes.fireworks(due, 6);
        assert_eq!(
            notes.ring.iter().flatten().count(),
            7,
            "min(4 + 6/2, 16) == 7 firework notes"
        );
        // Three bars: no finale.
        let mut short = KittySing::default();
        let armed = hold(&mut short, t0, 'w', SING_ARM_REPEATS, 30);
        let mut t = armed;
        while short.bar(t).is_none_or(|b| b < 3) {
            t += Duration::from_millis(30);
            short.note_char(t, S, 'w');
        }
        let never = t + Duration::from_secs(10);
        assert_eq!(
            short.take_cadence(never),
            None,
            "a three-bar performance takes the plain fade"
        );
        // Escape: a hard break vetoes whatever the span.
        let mut broken = KittySing::default();
        let armed = hold(&mut broken, t0, 'w', SING_ARM_REPEATS, 30);
        let mut t = armed;
        while broken.bar(t).is_none_or(|b| b < 6) {
            t += Duration::from_millis(30);
            broken.note_char(t, S, 'w');
        }
        broken.note_break(t + Duration::from_millis(30));
        assert_eq!(
            broken.take_cadence(t + Duration::from_secs(10)),
            None,
            "Escape means done — no ta-daa"
        );
    }

    /// ACCEPTANCE 11 (the detector half) — the cadence is canceled by a
    /// re-arm before due: the band was asked to keep playing. (The audio
    /// half — typing under the cadence bar ducks at full sing-duck depth
    /// — is the synth's existing celebration-duck law, pinned in
    /// `trail_sound`.)
    #[test]
    fn a_rearm_before_due_cancels_the_cadence() {
        let t0 = Instant::now();
        let mut d = KittySing::default();
        let armed = hold(&mut d, t0, 'w', SING_ARM_REPEATS, 30);
        let mut t = armed;
        while d.bar(t).is_none_or(|b| b < 5) {
            t += Duration::from_millis(30);
            d.note_char(t, S, 'w');
        }
        // Gentle release…
        d.note_backspace(t + Duration::from_millis(30));
        assert!(
            d.cadence.is_some(),
            "the backspace release planned a finale"
        );
        // …but the player comes back before the downbeat: three reps
        // inside the live glow re-arm (the forgiveness law) and cancel.
        let mut back = t + Duration::from_millis(200);
        for _ in 0..KEY_SWITCH_REPS {
            d.note_char(back, S, 'w');
            back += Duration::from_millis(30);
        }
        assert!(d.is_armed(back), "re-armed inside the glow");
        assert_eq!(
            d.take_cadence(back + Duration::from_secs(5)),
            None,
            "the canceled cadence never fires"
        );
    }

    /// The BACKSPACE is the GENTLE release (the spec's owner-ratified
    /// reading): it plans the same finale the lazy lift does — only the
    /// break keys and proven typing are hard.
    #[test]
    fn backspace_earns_the_finale_too() {
        let t0 = Instant::now();
        let mut d = KittySing::default();
        let armed = hold(&mut d, t0, 'w', SING_ARM_REPEATS, 30);
        let mut t = armed;
        while d.bar(t).is_none_or(|b| b < 4) {
            t += Duration::from_millis(30);
            d.note_char(t, S, 'w');
        }
        d.note_backspace(t + Duration::from_millis(30));
        let plan = d.cadence.expect("planned");
        assert_eq!(plan.sig, song_signature('w'));
        assert!(!plan.fired);
        assert_eq!(plan.span_bars, 4);
    }
}
