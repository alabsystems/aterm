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
//!   stays at full drive and the song simply modulates onto the new key's
//!   verse at the next bar boundary, over the same uninterrupted bar grid.
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

use std::fmt;
use std::time::Duration;

use aterm_time::Instant;

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
// The ARMED celebration (RAINBOW-KITTY-V2.md §27)
// ---------------------------------------------------------------------------

/// The most bars an armed celebration may run. Four bars is 6.4 s — one
/// phrase's worth of the riff's build, and the point past which a celebration
/// nobody is holding a key for becomes a broadcast.
pub const CELEBRATE_MAX_BARS: u8 = 4;

/// The cooldown between two arms of one window's detector, measured from the
/// ARM (not the fire): an agent may schedule one celebration, then wait.
pub const CELEBRATE_COOLDOWN: Duration = Duration::from_secs(30);

/// Fan stars on the FIRST bar of any sing-along (armed or held).
pub const BAR_FAN_BASE: u8 = 5;

/// Fan stars added per bar: 5, 7, 9, 11, …
pub const BAR_FAN_STEP: u8 = 2;

/// The bar fan's ceiling — half the sky's `STAR_CAP`, so a fan on a busy
/// sky lands on the eviction finish and never on a pop.
pub const BAR_FAN_CAP: u8 = 18;

/// The shockwave ring rides every fourth bar (bars 3, 7, …), so a four-bar
/// celebration ends under its ring.
pub const BAR_RING_EVERY: u64 = 4;

/// The "drop" fan thrown on the armed celebration's release, with the outro.
pub const DROP_FAN_N: u8 = 9;

/// The outro's length in seconds — the pulse lead on *do*, its sub and the
/// faraway bell, so the song ENDS on the bar line instead of fading
/// mid-phrase. Pinned equal to `trail_sound::CELEBRATION_OUTRO_S` there.
pub const OUTRO_SECONDS: f32 = 1.2;

/// Which keystroke-caused edge an armed celebration fires on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CelebrateOn {
    /// The session's next exit-0 command block (OSC 133/633 `D` with status
    /// 0) — the verdict's own edge, which the host only reports for a `D`
    /// that a keyed Enter armed.
    Green,
    /// The session's next keyed Enter (a real key event, or raw bytes ending
    /// in a newline — an agent's `send` counts, program output never does).
    Enter,
}

impl CelebrateOn {
    /// The wire spelling (`on=green|enter`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Enter => "enter",
        }
    }
}

/// One armed, not-yet-fired celebration: what `fx celebrate` latched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArmedCelebration {
    /// The song signature ([`song_signature`] of the chosen key).
    pub sig: u32,
    /// Bars to run, `1..=CELEBRATE_MAX_BARS`.
    pub bars: u8,
    /// The edge it fires on.
    pub on: CelebrateOn,
    /// The session the arm is bound to: a green block in another tab of the
    /// same window is not this session's block.
    pub session: u64,
}

/// Why an arm was refused. Every refusal is a wire-visible sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmRefusal {
    /// `bars` outside `1..=CELEBRATE_MAX_BARS`.
    Bars(u8),
    /// An arm is already pending on this window; one arm at a time.
    AlreadyArmed,
    /// Inside [`CELEBRATE_COOLDOWN`] of the last arm.
    Cooldown {
        /// Milliseconds until the next arm is admitted.
        remaining_ms: u64,
    },
}

impl fmt::Display for ArmRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bars(b) => write!(f, "bars={b} is outside 1..={CELEBRATE_MAX_BARS}"),
            Self::AlreadyArmed => f.write_str("a celebration is already armed (one arm at a time)"),
            Self::Cooldown { remaining_ms } => {
                write!(
                    f,
                    "cooldown: {remaining_ms} ms until the next arm is admitted"
                )
            }
        }
    }
}

/// One bar's fan, decided by the detector and thrown by the glow engine on
/// the frame that first sees the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarFan {
    /// The bar index since the arm.
    pub bar: u64,
    /// Star count: `min(5 + 2·bar, 18)`.
    pub n: u8,
    /// Whether this bar carries the shockwave ring (every fourth bar).
    pub ring: bool,
}

/// The fan a bar earns: `5 + 2·bar` stars under [`BAR_FAN_CAP`], the ring
/// on every [`BAR_RING_EVERY`]th bar. A pure function, so the host and the
/// tests read one law.
#[must_use]
pub fn bar_fan(bar: u64) -> BarFan {
    let steps = u8::try_from(bar.min(u64::from(u8::MAX))).unwrap_or(u8::MAX);
    let n = BAR_FAN_BASE
        .saturating_add(BAR_FAN_STEP.saturating_mul(steps))
        .min(BAR_FAN_CAP);
    BarFan {
        bar,
        n,
        ring: (bar + 1).is_multiple_of(BAR_RING_EVERY),
    }
}

/// The armed celebration's release: the drop fan and the outro's signature,
/// handed to the host exactly once on the first frame at or after the run's
/// last bar line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outro {
    /// The signature the outro resolves in — the same key the bars sang.
    pub sig: u32,
    /// The drop fan's star count ([`DROP_FAN_N`]).
    pub drop: u8,
}

/// A FIRED armed celebration: the run the detector is driving with no key
/// under it. Its end is a bar line by construction (`bars × SING_BAR_SECONDS`
/// after the fire), which is what makes the outro beat-quantised.
#[derive(Clone, Copy, Debug)]
struct ExternalRun {
    sig: u32,
    bars: u8,
    ends_at: Instant,
    outro_taken: bool,
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
/// the song modulates onto the new key's verse at the next bar boundary.
/// The same count recovers a celebration whose glow is still fading — the
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
    /// the bar grid and the beat phase do NOT — the tune keeps playing and
    /// changes key on the next bar boundary. Provisional because distinct
    /// characters that do NOT repeat are TYPING: if another distinct
    /// character arrives before the commit, the crossfade is anchored at
    /// the DEPARTURE stored here, so ordinary typing loses exactly what it
    /// lost before this existed.
    handover_from: Option<Instant>,
    /// An ARMED, not-yet-fired celebration (`fx celebrate`): latched here and
    /// spent by the first keystroke-caused edge it named (§27). Latch, don't
    /// act — nothing lights until that edge.
    armed_external: Option<ArmedCelebration>,
    /// The fired run, while it plays and winds down; cleared by `settle`, or
    /// by a human hold that re-earns the stage inside the wind-down.
    external: Option<ExternalRun>,
    /// The last ARM instant — the cooldown's anchor. Survives `settle`.
    last_external_arm: Option<Instant>,
    /// The bar-fan latch: the last bar a fan was thrown for (one per bar).
    fan_bar: Option<u64>,
}

impl KittySing {
    /// Bind the run to `session`, winding down on a switch.
    fn rekey(&mut self, now: Instant, session: u64) {
        if self.session != Some(session) {
            // A fired celebration belongs to the session it fired in: the
            // switch cuts it into its wind-down (the outro still speaks, so it
            // ends rather than vanishes). The PENDING arm is session-bound by
            // its own field and waits.
            if let Some(ext) = self.external
                && self.wind_from.is_none()
                && now < ext.ends_at
            {
                self.wind_from = Some(now);
            }
            self.release(now);
            self.session = Some(session);
        }
    }

    /// End the current run at `at`: an armed run starts its crossfade there
    /// (never a hard cut); an unarmed run just clears.
    fn release(&mut self, at: Instant) {
        // A FIRED celebration has no key under it to let go of: typing over
        // it clears the run bookkeeping and leaves the bars playing to their
        // line. Only `settle` (drive 0) or a session switch ends it early.
        if self.external.is_some() && self.wind_from.is_none() {
            self.run = None;
            self.count = 0;
            self.last = None;
            self.handover_from = None;
            return;
        }
        if self.armed_at.is_some() && self.wind_from.is_none() {
            // A lazy release may already have begun the fade earlier than
            // this eager event; keep the EARLIER instant so the crossfade
            // never jumps back up.
            self.wind_from = Some(self.lazy_release().map_or(at, |lazy| lazy.min(at)));
        }
        self.run = None;
        self.count = 0;
        self.last = None;
        self.handover_from = None;
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
    /// over the same uninterrupted bar grid.
    #[must_use]
    pub fn signature(&self) -> u32 {
        if let Some(ext) = self.external {
            return ext.sig;
        }
        self.run.map_or(NEUTRAL_SIGNATURE, song_signature)
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
        // A fired celebration winds down on its LAST BAR LINE — derived, like
        // the lazy release, so no event is needed and the outro is quantised.
        if let Some(ext) = self.external {
            return (now >= ext.ends_at).then_some(ext.ends_at);
        }
        self.lazy_release().filter(|release| now >= *release)
    }

    /// The RAW bar-grid index at `now`: elapsed [`SING_BAR_SECONDS`] periods
    /// since the arm anchor. Bar BOUNDARIES live here and never move — this
    /// is the one uninterrupted grid the whole celebration plays on.
    fn raw_bar(&self, now: Instant) -> Option<u64> {
        let t0 = self.armed_at?;
        Some((now.saturating_duration_since(t0).as_secs_f32() / SING_BAR_SECONDS) as u64)
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
        // A fired celebration past its bar line is winding down — materialize
        // that stamp, exactly as the lazy release is, so a held key inside the
        // wind-down can re-earn the stage under the forgiveness law below.
        if let Some(ext) = self.external
            && self.wind_from.is_none()
            && now >= ext.ends_at
        {
            self.wind_from = Some(ext.ends_at);
        }
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
            // left the stage, and no re-earn is owed. The musical half needs
            // no machinery at all: the signature already rides the CURRENT
            // run char, so the next bar's payload sings the new key — the
            // switch is heard as a modulation at the boundary.
            if self.handover_from.is_some() && self.count >= KEY_SWITCH_REPS {
                self.handover_from = None;
            }
        } else {
            // PROMISE BROKEN: another distinct character before the handed-over
            // key ever proved itself. That was typing, so wind down from the
            // DEPARTURE — the instant the original held key was abandoned —
            // rather than from here, so ordinary typing loses exactly what it
            // lost before.
            let departure = self.handover_from.take();
            self.release(departure.unwrap_or(now));
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
            self.armed_at = Some(now);
            self.wind_from = None;
            // The hand owns the stage now: the fired run's signature and bar
            // line give way to the held key's, and the bar-fan latch reopens
            // on the new grid.
            self.external = None;
            self.fan_bar = None;
        }
    }

    /// A backspace NEVER arms (deleting is the opposite of jubilant holding)
    /// and releases any armed run into its crossfade.
    pub fn note_backspace(&mut self, now: Instant) {
        self.release(now);
    }

    /// Any word-breaking / editing / navigation key (Enter/Tab/Escape/
    /// chords mean "done, back to work"): same release law as backspace —
    /// the plain fade is the whole goodbye.
    pub fn note_break(&mut self, now: Instant) {
        self.release(now);
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
    #[must_use]
    pub fn bar(&self, now: Instant) -> Option<u64> {
        if !self.is_armed(now) {
            return None;
        }
        let bar = self.raw_bar(now)?;
        // A fired run plays exactly `bars` bars: `ends_at` is `bars ×
        // SING_BAR_SECONDS` in f32, and a frame can land inside that last
        // rounding epsilon still "armed" with the raw index already at
        // `bars` — which would push one bar of riff and one fan too many.
        if let Some(ext) = self.external
            && bar >= u64::from(ext.bars)
        {
            return None;
        }
        Some(bar)
    }

    /// A drained detector at rest is byte-identical off — the idle contract.
    /// Called by the host once the drive reads 0 (or on hard resets).
    pub fn settle(&mut self, now: Instant) {
        if self.armed_at.is_some() && self.drive(now) <= 0.0 {
            self.armed_at = None;
            self.wind_from = None;
            self.handover_from = None;
            self.external = None;
            self.fan_bar = None;
        }
    }

    // -- the armed celebration (§27) ------------------------------------------

    /// ARM a one-shot celebration (`fx celebrate`): LATCHED, never acted on —
    /// it fires on the next keystroke-caused edge `on` names, in `session`.
    /// Refused outside `1..=CELEBRATE_MAX_BARS`, while an arm is pending, or
    /// inside [`CELEBRATE_COOLDOWN`] of the last arm. The cooldown is stamped
    /// on the ARM, so a refused arm costs nothing and an admitted one costs
    /// thirty seconds whether or not its edge ever comes.
    pub fn arm_external(
        &mut self,
        now: Instant,
        session: u64,
        sig: u32,
        bars: u8,
        on: CelebrateOn,
    ) -> Result<ArmedCelebration, ArmRefusal> {
        if !(1..=CELEBRATE_MAX_BARS).contains(&bars) {
            return Err(ArmRefusal::Bars(bars));
        }
        if self.armed_external.is_some() {
            return Err(ArmRefusal::AlreadyArmed);
        }
        let remaining = self.cooldown_remaining(now);
        if !remaining.is_zero() {
            return Err(ArmRefusal::Cooldown {
                remaining_ms: u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX),
            });
        }
        let arm = ArmedCelebration {
            sig,
            bars,
            on,
            session,
        };
        self.last_external_arm = Some(now);
        self.armed_external = Some(arm);
        Ok(arm)
    }

    /// The pending arm, if any (a pure read — `fx status`).
    #[must_use]
    pub fn armed_external(&self) -> Option<ArmedCelebration> {
        self.armed_external
    }

    /// Time left on the arm cooldown at `now`; zero when an arm is admissible.
    #[must_use]
    pub fn cooldown_remaining(&self, now: Instant) -> Duration {
        self.last_external_arm.map_or(Duration::ZERO, |t| {
            (t + CELEBRATE_COOLDOWN).saturating_duration_since(now)
        })
    }

    /// True while a fired celebration is on the stage (playing or winding
    /// down, until `settle`).
    #[must_use]
    pub fn external_live(&self) -> bool {
        self.external.is_some()
    }

    /// The next external-run event that a host must consume: the initial bar,
    /// the next bar, the outro, or final settlement. Held-key songs and pending
    /// arms add no deadline. A due unconsumed event answers `now`; after its
    /// take/settle call the answer advances, so static hosts need no frame train.
    #[must_use]
    pub fn next_external_deadline(&self, now: Instant) -> Option<Instant> {
        let ext = self.external?;
        if let Some(bar) = self.bar(now) {
            if self.fan_bar != Some(bar) {
                return Some(now);
            }
            let anchor = self.armed_at?;
            let next = anchor + Duration::from_secs_f32(SING_BAR_SECONDS * (bar + 1) as f32);
            // The bar API and its authored clock use f32. If the nanosecond
            // conversion lands just before a quotient's rounding boundary,
            // one microsecond crosses it without rearming a passed instant.
            return Some(next.min(ext.ends_at).max(now + Duration::from_micros(1)));
        }
        let release = self.wind_start(now).unwrap_or(ext.ends_at);
        if !ext.outro_taken {
            return Some(release.max(now));
        }
        Some((release + Duration::from_secs_f32(SING_WIND_DOWN)).max(now))
    }

    /// THE GREEN EDGE: the host's verdict path reports `session`'s exit-0
    /// block here — only for a `D` a keyed Enter armed (the verdict's own
    /// law), so the light this fires has that Enter behind it. Fires a
    /// pending [`CelebrateOn::Green`] arm bound to this session; anything
    /// else is a no-op. A block in another session leaves the arm waiting.
    pub fn note_green_block(&mut self, now: Instant, session: u64) {
        if let Some(arm) = self.armed_external
            && arm.on == CelebrateOn::Green
            && arm.session == session
        {
            self.fire_external(now, session, arm);
        }
    }

    /// THE ENTER EDGE: a keyed Enter in `session` (a key event, or raw bytes
    /// ending in a newline from the control socket — an agent's keypress is
    /// a keypress). Fires a pending [`CelebrateOn::Enter`] arm bound to this
    /// session; anything else is a no-op.
    pub fn note_keyed_enter(&mut self, now: Instant, session: u64) {
        if let Some(arm) = self.armed_external
            && arm.on == CelebrateOn::Enter
            && arm.session == session
        {
            self.fire_external(now, session, arm);
        }
    }

    /// Spend the arm: anchor the beat clock at `now` and run `bars` bars with
    /// no key under them. A live human hold is re-anchored onto this grid (the
    /// celebration was asked for; the hand joins it).
    fn fire_external(&mut self, now: Instant, session: u64, arm: ArmedCelebration) {
        self.armed_external = None;
        self.session = Some(session);
        self.run = None;
        self.count = 0;
        self.last = None;
        self.handover_from = None;
        self.armed_at = Some(now);
        self.wind_from = None;
        self.fan_bar = None;
        self.external = Some(ExternalRun {
            sig: arm.sig,
            bars: arm.bars,
            ends_at: now + Duration::from_secs_f32(SING_BAR_SECONDS * f32::from(arm.bars)),
            outro_taken: false,
        });
    }

    /// THE BAR FAN: `Some` exactly once per bar while ARMED (armed run or held
    /// key alike — the fans ride the bars, whoever is singing), on the first
    /// frame that sees the bar; `None` on every other frame and throughout the
    /// wind-down. The host throws it through the glow engine's party seam.
    pub fn take_bar_fan(&mut self, now: Instant) -> Option<BarFan> {
        let bar = self.bar(now)?;
        if self.fan_bar == Some(bar) {
            return None;
        }
        self.fan_bar = Some(bar);
        Some(bar_fan(bar))
    }

    /// THE OUTRO: `Some` exactly once, on the first frame at or after a fired
    /// celebration's release — its last bar line by construction, so the
    /// outro lands on the downbeat and the song ends rather than fades. The
    /// host throws the drop fan and pushes the outro gesture. `None` for a
    /// human's hold: letting go keeps its plain crossfade.
    pub fn take_outro(&mut self, now: Instant) -> Option<Outro> {
        let started = self.wind_start(now).is_some();
        let ext = self.external.as_mut()?;
        if ext.outro_taken || !started {
            return None;
        }
        ext.outro_taken = true;
        Some(Outro {
            sig: ext.sig,
            drop: DROP_FAN_N,
        })
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
}

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
    /// `is_armed`, so the field stops spawning the instant the wind-down
    /// begins, whatever the drive still reads.
    pub fn update(&mut self, now: Instant, streaming: bool, beat: Option<f32>) {
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
        });
        self.head = (self.head + 1) % MAX_NOTES;
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
        let alpha = (fade_in * fade_out * 255.0) as u8;
        let s = note.seed;
        let tint = NOTE_TINTS[(s % NOTE_TINTS.len() as u32) as usize];
        // Scatter: birth x in 0.1..0.9 cells ahead, wobble phase 0..1.
        let x0 = 0.1 + 0.8 * ((s >> 8) & 0xff) as f32 / 255.0;
        let phase = ((s >> 16) & 0xff) as f32 / 255.0;
        let (dx, dy) = if reduced_motion {
            // Fixed offsets: a static spray around the mouth.
            (x0, -0.3 - 0.8 * phase)
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

    fn external_deadline_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            ExternalCelebrationDeadline {
                const Buggy = 0;
                const Bars = 4;
                var phase = 0;
                var consumed = 0;
                var deadline = 0;
                action Consume when (phase <= Bars + 1 && consumed == 0) {
                    consumed = 1;
                    deadline = if Buggy == 1 { phase } else { phase + 1 };
                }
                action Advance when (phase <= Bars && consumed == 1) {
                    phase = phase + 1;
                    consumed = 0;
                }
                invariant AConsumedEventCannotRearmNow: consumed == 0 || deadline > phase;
                invariant Bounded: phase <= Bars + 1 && deadline <= Bars + 2 && consumed <= 1;
            }
        }
    }

    #[test]
    fn external_deadline_model_proves_and_catches_a_rearmed_consumed_event() {
        let model = external_deadline_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn external_deadline_conforms_through_bars_outro_and_settle() {
        let model = external_deadline_model();
        let mut expected = model.init_state();
        let mut sing = KittySing::default();
        let start = Instant::now();
        assert_eq!(sing.next_external_deadline(start), None);
        sing.arm_external(start, S, song_signature('C'), 4, CelebrateOn::Green)
            .unwrap();
        assert_eq!(
            sing.next_external_deadline(start),
            None,
            "a pending arm is inert"
        );
        sing.note_green_block(start, S);
        let end = start + Duration::from_secs_f32(4.0 * SING_BAR_SECONDS);
        let mut now = start;
        for phase in 0..=5 {
            assert_eq!(
                sing.next_external_deadline(now),
                Some(now),
                "the current edge is due"
            );
            if phase < 4 {
                assert_eq!(sing.take_bar_fan(now).unwrap().bar, phase);
            } else if phase == 4 {
                assert!(sing.take_outro(now).is_some());
                assert!(sing.take_outro(now).is_none(), "the outro is consumed once");
            } else {
                sing.settle(now);
            }
            assert!(model.fire("Consume", &mut expected));
            let next = sing.next_external_deadline(now);
            let mut actual = expected.clone();
            actual.insert(
                "deadline",
                match next {
                    Some(at) if at > now => phase as i64 + 1,
                    Some(_) => phase as i64,
                    None => 6,
                },
            );
            assert_eq!(
                actual, expected,
                "the real consumer advances its offered deadline"
            );
            let mut stuck = actual.clone();
            stuck.insert("deadline", phase as i64);
            assert!(!model.check_invariant("AConsumedEventCannotRearmNow", &stuck));
            if phase < 5 {
                let at = next.expect("a finite run still owes an event");
                let exact = if phase < 4 {
                    start + Duration::from_secs_f32((phase + 1) as f32 * SING_BAR_SECONDS)
                } else {
                    end + Duration::from_secs_f32(SING_WIND_DOWN)
                };
                assert_eq!(at, exact);
                assert_eq!(
                    sing.next_external_deadline(now + Duration::from_millis(1)),
                    Some(at),
                    "unrelated parks retain the same future offer"
                );
                now = at;
                assert!(model.fire("Advance", &mut expected));
            } else {
                assert_eq!(next, None, "settled runs owe no idle wake");
            }
        }
    }

    #[test]
    fn external_deadline_does_not_schedule_held_keys_or_replay_missed_bars() {
        let start = Instant::now();
        let mut held = KittySing::default();
        let now = hold(&mut held, start, 'a', SING_ARM_REPEATS, 30);
        assert!(held.is_armed(now));
        assert_eq!(held.next_external_deadline(now), None);
        let mut external = KittySing::default();
        external
            .arm_external(start, S, song_signature('C'), 4, CelebrateOn::Enter)
            .unwrap();
        external.note_keyed_enter(start, S);
        let late = start + Duration::from_secs_f32(2.5 * SING_BAR_SECONDS);
        assert_eq!(external.next_external_deadline(late), Some(late));
        assert_eq!(external.take_bar_fan(late).unwrap().bar, 2);
        assert!(external.next_external_deadline(late).unwrap() > late);
        assert!(
            external.take_bar_fan(late).is_none(),
            "missed bars are not replayed"
        );
    }

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
    ///  * CHANGES — the signature moves to the new key, so the next bar's
    ///    payload sings the new verse over the same uninterrupted grid
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
            Some(0),
            "the re-anchored grid opens at bar 0 — a fresh verse under the \
             same finger"
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
            notes.update(now, true, Some(beat));
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
            notes.update(now, true, Some(hb as f32 * 0.5));
        }
        assert!(notes.is_active());
        // The wind-down (no longer armed): updates cull but never spawn.
        let later = t0 + Duration::from_millis(8 * 200);
        notes.update(later, false, Some(8.0 * 0.5));
        let live_at_release = notes.ring.iter().flatten().count();
        for step in 0..20u64 {
            let now = later + Duration::from_millis(step * 100);
            notes.update(now, false, Some((8 + step) as f32 * 0.5));
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
        notes.update(t0, true, Some(0.0));
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
    /// switch commits must carry the NEW key's signature over the SAME
    /// uninterrupted bar grid — the switch is heard as a modulation, never
    /// announced, never a restart.
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
    /// bar or more. The drive never dips, every key's verse gets heard in
    /// order, and the pushed bar indices stay strictly monotone on the one
    /// raw grid (the host latch can never swallow a bar).
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
        for &(_, sig) in &host.pushed {
            if heard.last() != Some(&sig) {
                heard.push(sig);
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
        assert!(
            host.d.bar(recovered).is_some(),
            "the recovery schedules bars again"
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

    // -- the armed celebration (§27) ------------------------------------------

    /// A green block is the OSC 133/633 `D` (status 0) the host's verdict
    /// path reports for a keyed Enter; a frame loop drives the detector the
    /// way the host does, at ~60 Hz.
    fn frames(
        d: &mut KittySing,
        from: Instant,
        until: Instant,
        mut each: impl FnMut(Instant, &mut KittySing),
    ) {
        let mut t = from;
        while t <= until {
            each(t, d);
            t += Duration::from_millis(16);
        }
    }

    /// AN ARMED CELEBRATION FIRES ON THE NEXT GREEN BLOCK, AND ONLY ONCE: the
    /// arm lights nothing by itself (latch, don't act); a green block in
    /// ANOTHER session leaves it waiting; this session's green block fires it
    /// at full drive with the chosen key's signature; a second green block
    /// while it plays re-anchors nothing, and one after it settles starts
    /// nothing — the arm was spent.
    #[test]
    fn an_armed_celebration_fires_on_the_next_green_block_and_only_once() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let sig = song_signature('C');
        let arm = d
            .arm_external(t0, S, sig, 2, CelebrateOn::Green)
            .expect("the first arm is admitted");
        assert_eq!(arm.bars, 2);
        assert_eq!(d.drive(t0), 0.0, "an arm lights nothing by itself");
        assert!(!d.is_armed(t0));
        assert_eq!(d.armed_external(), Some(arm), "…it is latched");

        let other = t0 + Duration::from_millis(100);
        d.note_green_block(other, S + 1);
        assert_eq!(
            d.drive(other),
            0.0,
            "another session's green block is not this one's"
        );
        assert_eq!(d.armed_external(), Some(arm), "the arm still waits");

        let green = t0 + Duration::from_millis(500);
        d.note_green_block(green, S);
        assert_eq!(d.armed_external(), None, "the arm is spent by the edge");
        assert!(d.is_armed(green), "the green block fires the celebration");
        assert_eq!(d.drive(green), 1.0, "…at full drive");
        assert_eq!(d.signature(), sig, "…in the chosen key");
        assert_eq!(
            d.beat(green),
            Some(0.0),
            "the beat clock anchors at the fire"
        );

        // A second green block one bar in: no re-anchor, no second run.
        let again = green + Duration::from_secs_f32(SING_BAR_SECONDS + 0.2);
        d.note_green_block(again, S);
        assert_eq!(d.bar(again), Some(1), "the grid did not move");

        // Two bars, then the wind-down; then settled, a green block starts nothing.
        let end = green + Duration::from_secs_f32(2.0 * SING_BAR_SECONDS);
        assert!(d.is_armed(end - Duration::from_millis(1)));
        assert!(!d.is_armed(end), "the run ends on its last bar line");
        let done = end + Duration::from_secs_f32(SING_WIND_DOWN) + Duration::from_millis(1);
        assert_eq!(d.drive(done), 0.0);
        d.settle(done);
        assert!(!d.external_live());
        d.note_green_block(done + Duration::from_secs(1), S);
        assert_eq!(
            d.drive(done + Duration::from_secs(1)),
            0.0,
            "spent arms never fire twice"
        );
    }

    /// A CELEBRATION'S FANS RIDE THE BARS AND THE OUTRO ENDS ON *DO*: a
    /// four-bar run hands the host exactly one fan per bar, on the first frame
    /// that sees the bar — 5, 7, 9, 11 stars, the ring on the fourth — and
    /// then, once, on the first frame at or after the last bar line, the
    /// outro in the run's own signature with its drop fan. The synth's half
    /// (the lead resolves on degree 0) is `trail_sound`'s
    /// `the_outro_s_lead_is_do_and_holds_the_sing_duck`.
    #[test]
    fn a_celebration_s_fans_ride_the_bars_and_the_outro_ends_on_do() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let sig = song_signature('G');
        d.arm_external(t0, S, sig, 4, CelebrateOn::Enter)
            .expect("admitted");
        d.note_keyed_enter(t0, S);
        assert!(d.is_armed(t0), "the keyed Enter fires an `on=enter` arm");

        let mut fans = Vec::new();
        let mut outros = Vec::new();
        let end = t0 + Duration::from_secs_f32(4.0 * SING_BAR_SECONDS);
        let until = end + Duration::from_secs_f32(SING_WIND_DOWN) + Duration::from_millis(50);
        frames(&mut d, t0, until, |t, d| {
            if let Some(fan) = d.take_bar_fan(t) {
                fans.push((t, fan));
            }
            if let Some(o) = d.take_outro(t) {
                outros.push((t, o));
            }
        });
        let shape: Vec<(u64, u8, bool)> = fans.iter().map(|(_, f)| (f.bar, f.n, f.ring)).collect();
        assert_eq!(
            shape,
            vec![(0, 5, false), (1, 7, false), (2, 9, false), (3, 11, true)],
            "one fan per bar, 5/7/9/11 stars, the ring on the fourth bar"
        );
        for (t, f) in &fans {
            let line = t0 + Duration::from_secs_f32(f.bar as f32 * SING_BAR_SECONDS);
            // `line` is `bar × 1.6` through f32 — up to a few ns off the
            // frame clock's exact millisecond — so the early side has a 1 ms
            // tolerance and the late side the frame's own 16 ms.
            assert!(
                line.saturating_duration_since(*t) <= Duration::from_millis(1)
                    && t.saturating_duration_since(line) < Duration::from_millis(17),
                "bar {} fan born on the first frame after its bar line ({:?} late)",
                f.bar,
                t.saturating_duration_since(line)
            );
        }
        assert_eq!(outros.len(), 1, "exactly one outro");
        let (at, o) = outros[0];
        assert_eq!(o.sig, sig, "the outro resolves in the run's own key");
        assert_eq!(o.drop, DROP_FAN_N);
        assert!(
            end.saturating_duration_since(at) <= Duration::from_millis(1)
                && at.saturating_duration_since(end) < Duration::from_millis(17),
            "the outro lands on the last bar line, not mid-phrase"
        );
        assert_eq!(
            bar_fan(20).n,
            BAR_FAN_CAP,
            "the fan count caps at {BAR_FAN_CAP}"
        );
        assert!(
            bar_fan(7).ring && !bar_fan(4).ring,
            "the ring rides every fourth bar"
        );
    }

    /// AN UNARMED SESSION NEVER CELEBRATES ON A GREEN BLOCK — nor on a keyed
    /// Enter: with nothing latched, both edges are no-ops, the detector stays
    /// byte-identical off, and no fan or outro is ever handed out.
    #[test]
    fn an_unarmed_session_never_celebrates_on_a_green_block() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        d.note_green_block(t0, S);
        d.note_keyed_enter(t0 + Duration::from_millis(10), S);
        let t = t0 + Duration::from_millis(20);
        assert_eq!(d.drive(t), 0.0);
        assert!(!d.is_armed(t));
        assert_eq!(d.bar(t), None);
        assert_eq!(d.beat(t), None);
        assert_eq!(d.signature(), NEUTRAL_SIGNATURE);
        assert_eq!(d.take_bar_fan(t), None);
        assert_eq!(d.take_outro(t), None);
        assert!(!d.external_live());
        // A HELD KEY still sings — the human path is untouched — and its bars
        // now carry fans too, while the outro stays the armed run's alone.
        let armed = hold(&mut d, t, 'a', SING_ARM_REPEATS, 30);
        assert!(d.is_armed(armed));
        assert_eq!(d.take_bar_fan(armed).map(|f| f.n), Some(BAR_FAN_BASE));
        assert_eq!(
            d.take_bar_fan(armed + Duration::from_millis(16)),
            None,
            "one fan per bar"
        );
        let gone = armed + SING_REPEAT_GAP + Duration::from_millis(100);
        assert!(d.drive(gone) < 1.0, "letting go winds down");
        assert_eq!(
            d.take_outro(gone),
            None,
            "a hold's release keeps its plain crossfade"
        );
    }

    /// THE COOLDOWN REFUSES A SECOND ARM FOR THIRTY SECONDS: a second arm
    /// straight after the first is `AlreadyArmed`; once the first has fired
    /// and settled, a new arm inside thirty seconds of the FIRST ARM is
    /// `Cooldown` with the honest remainder; at thirty seconds it is admitted.
    /// Out-of-range bars are refused before either check and cost nothing.
    #[test]
    fn the_cooldown_refuses_a_second_arm_for_thirty_seconds() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let sig = song_signature('E');
        assert_eq!(
            d.arm_external(t0, S, sig, 0, CelebrateOn::Green),
            Err(ArmRefusal::Bars(0))
        );
        assert_eq!(
            d.arm_external(t0, S, sig, CELEBRATE_MAX_BARS + 1, CelebrateOn::Green),
            Err(ArmRefusal::Bars(CELEBRATE_MAX_BARS + 1))
        );
        assert_eq!(
            d.cooldown_remaining(t0),
            Duration::ZERO,
            "a refused arm costs nothing"
        );
        d.arm_external(t0, S, sig, 1, CelebrateOn::Green)
            .expect("admitted");
        assert_eq!(
            d.arm_external(t0 + Duration::from_millis(5), S, sig, 1, CelebrateOn::Green),
            Err(ArmRefusal::AlreadyArmed)
        );
        // Fire, run one bar, wind down, settle.
        let fire = t0 + Duration::from_secs(1);
        d.note_green_block(fire, S);
        let done = fire
            + Duration::from_secs_f32(SING_BAR_SECONDS + SING_WIND_DOWN)
            + Duration::from_millis(1);
        d.settle(done);
        assert_eq!(d.drive(done), 0.0);
        let early = t0 + Duration::from_secs(10);
        assert_eq!(
            d.arm_external(early, S, sig, 1, CelebrateOn::Green),
            Err(ArmRefusal::Cooldown {
                remaining_ms: 20_000
            }),
            "the cooldown is measured from the ARM"
        );
        let ok = t0 + CELEBRATE_COOLDOWN;
        assert!(
            d.arm_external(ok, S, sig, 1, CelebrateOn::Green).is_ok(),
            "admitted at thirty seconds"
        );
        assert_eq!(
            ArmRefusal::Cooldown {
                remaining_ms: 20_000
            }
            .to_string(),
            "cooldown: 20000 ms until the next arm is admitted"
        );
    }
}
