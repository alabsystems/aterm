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
}

impl KittySing {
    /// Bind the run to `session`, winding down on a switch.
    fn rekey(&mut self, now: Instant, session: u64) {
        if self.session != Some(session) {
            self.release(now);
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
    /// over the same uninterrupted bar grid — and a COMMITTED key switch
    /// additionally reopens the form there on the new key's own verse
    /// ([`Self::bar`]), so the change is heard at once, never buried under
    /// the shared chorus.
    #[must_use]
    pub fn signature(&self) -> u32 {
        self.run.map_or(NEUTRAL_SIGNATURE, song_signature)
    }

    /// COMPATIBILITY SHIM — DELETE WITH THE PENDING GUI PATCH
    /// (`backups/PENDING-sing-sig-app_render.patch`). The clamped pentatonic
    /// root the signature decodes to (-2..=2), kept ONLY so the unpatched
    /// host builder (`app_render::sing_riff_event` and its push sites) still
    /// compiles; it carries at most five key classes, which is exactly the
    /// defect [`Self::signature`] exists to fix.
    #[deprecated(note = "use signature(); the i8 root carries only 5 key classes — \
                         apply backups/PENDING-sing-sig-app_render.patch")]
    #[must_use]
    pub fn key(&self) -> i8 {
        ((self.signature() % 5) as i8) - 2
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
            // left the stage, and no re-earn is owed. The musical half:
            // reopen the form on the new key's own verse at the next bar
            // boundary, so the switch is HEARD immediately instead of hiding
            // for up to three bars behind the shared chorus.
            if self.handover_from.is_some() && self.count >= KEY_SWITCH_REPS {
                self.handover_from = None;
                self.reopen_section(now);
            }
        } else {
            // PROMISE BROKEN: another distinct character before the handed-over
            // key ever proved itself. That was typing, so wind down from the
            // DEPARTURE — the instant the original held key was abandoned —
            // rather than from here, so ordinary typing loses exactly what it
            // lost before.
            if let Some(left) = self.handover_from.take() {
                self.wind_from = Some(left);
            }
            self.release(now);
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
                    let raw = (wind.saturating_duration_since(t0).as_secs_f32()
                        / SING_BAR_SECONDS) as u64;
                    section_reopen_bar(raw + self.shift_at(raw))
                }
                _ => 0,
            };
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

    /// Any word-breaking / editing / navigation key: same law as backspace.
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

    /// A drained detector at rest is byte-identical off — the idle contract.
    /// Called by the host once the drive reads 0 (or on hard resets).
    pub fn settle(&mut self, now: Instant) {
        if self.armed_at.is_some() && self.drive(now) <= 0.0 {
            self.armed_at = None;
            self.wind_from = None;
            self.handover_from = None;
            self.section_shift = 0;
            self.pending_section = None;
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

    /// Advance the field one frame: cull dead notes, and while the drive is
    /// high spawn one note per NEW half-beat (beat-synced streaming — the
    /// notes leave the cat's mouth on the same clock the riff plays on).
    /// Wind-down (`drive < 1`) spawns nothing: the live notes finish their
    /// rise and fade — the visual crossfade.
    pub fn update(&mut self, now: Instant, drive: f32, beat: Option<f32>) {
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
        if drive < 1.0 {
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
            notes.update(now, 1.0, Some(beat));
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
            notes.update(now, 1.0, Some(hb as f32 * 0.5));
        }
        assert!(notes.is_active());
        // Drive drops below 1 (wind-down): updates cull but never spawn.
        let later = t0 + Duration::from_millis(8 * 200);
        notes.update(later, 0.6, Some(8.0 * 0.5));
        let live_at_release = notes.ring.iter().flatten().count();
        for step in 0..20u64 {
            let now = later + Duration::from_millis(step * 100);
            notes.update(now, 0.4, Some((8 + step) as f32 * 0.5));
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
        notes.update(t0, 1.0, Some(0.0));
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
                if let Some(bar) = self.d.bar(self.clock) {
                    if self.latch != Some(bar) {
                        self.latch = Some(bar);
                        self.pushed.push((bar, self.d.signature()));
                    }
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
            let from = if i == 0 { t0 } else { t + Duration::from_millis(30) };
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
            host.pushed.iter().all(|&(b, _)| b < bar || b == bar),
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
        let t = hold(&mut d, t0 + Duration::from_secs(2), 'a', SING_ARM_REPEATS, 30);
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

    /// The deprecated 5-class shim (alive only until the pending GUI patch
    /// lands) stays consistent with the signature it delegates into: the
    /// derived root is `sig % 5 - 2`, always inside one pentatonic octave.
    #[test]
    #[allow(deprecated)]
    fn deprecated_key_shim_tracks_the_signature_root() {
        for ch in ['a', 'q', 'w', 'z', '0', '~'] {
            let mut d = KittySing::default();
            let t = hold(&mut d, Instant::now(), ch, SING_ARM_REPEATS, 30);
            assert!(d.is_armed(t));
            let expect = ((song_signature(ch) % 5) as i8) - 2;
            assert_eq!(d.key(), expect);
            assert!((-2..=2).contains(&d.key()));
        }
    }
}
