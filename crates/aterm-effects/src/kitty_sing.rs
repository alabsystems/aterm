// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Key-repeat sing-along detector, beat clock, and music-note sprite field.

use std::fmt;
use std::time::Duration;

use aterm_time::Instant;

use aterm_scene::{PathCmd, PathTransform, Tile, fill_path};

/// The celebration tempo.
pub const SING_BPM: f32 = 150.0;

/// One beat in seconds (`60 / SING_BPM`).
pub const SING_BEAT_SECONDS: f32 = 60.0 / SING_BPM;

/// Beats per riff bar.
pub const SING_BAR_BEATS: f32 = 4.0;

/// One riff bar in seconds.
pub const SING_BAR_SECONDS: f32 = SING_BEAT_SECONDS * SING_BAR_BEATS;

/// Fixed bijective mixer over a character's code point.
#[inline]
#[must_use]
pub const fn song_signature(ch: char) -> u32 {
    let mut x = ch as u32;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

/// The signature of "nothing held".
pub const NEUTRAL_SIGNATURE: u32 = 12;

/// Maximum bars an armed celebration may run.
pub const CELEBRATE_MAX_BARS: u8 = 4;

/// Cooldown between two arms of one window's detector.
pub const CELEBRATE_COOLDOWN: Duration = Duration::from_secs(30);

/// Fan stars on the first bar of any sing-along.
pub const BAR_FAN_BASE: u8 = 5;

/// Fan stars added per bar.
pub const BAR_FAN_STEP: u8 = 2;

/// Bar fan ceiling.
pub const BAR_FAN_CAP: u8 = 18;

/// Bar interval for the shockwave ring.
pub const BAR_RING_EVERY: u64 = 4;

/// Drop fan star count on an armed celebration's release.
pub const DROP_FAN_N: u8 = 9;

/// Outro length in seconds.
pub const OUTRO_SECONDS: f32 = 1.2;

/// Which edge an armed celebration fires on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CelebrateOn {
    /// The session's next exit-0 command block.
    Green,
    /// The session's next keyed Enter.
    Enter,
    /// The session's next honoured program `done`.
    Done,
}

impl CelebrateOn {
    /// The wire spelling (`on=green|enter|done`).
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Enter => "enter",
            Self::Done => "done",
        }
    }
}

/// One armed, not-yet-fired celebration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArmedCelebration {
    /// The song signature.
    pub sig: u32,
    /// Bars to run (`1..=CELEBRATE_MAX_BARS`).
    pub bars: u8,
    /// The edge it fires on.
    pub on: CelebrateOn,
    /// The session the arm is bound to.
    pub session: u64,
}

/// Why an arm was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmRefusal {
    /// `bars` outside `1..=CELEBRATE_MAX_BARS`.
    Bars(u8),
    /// An arm is already pending on this window.
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

/// One bar's fan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarFan {
    /// The bar index since the arm.
    pub bar: u64,
    /// Star count.
    pub n: u8,
    /// Whether this bar carries the shockwave ring.
    pub ring: bool,
}

/// Compute the fan for `bar`.
#[inline]
#[must_use]
pub const fn bar_fan(bar: u64) -> BarFan {
    const {
        assert!(BAR_RING_EVERY.is_power_of_two());
    }
    let steps = if bar > u8::MAX as u64 {
        u8::MAX
    } else {
        bar as u8
    };
    let sum = BAR_FAN_BASE.saturating_add(BAR_FAN_STEP.saturating_mul(steps));
    let n = if sum < BAR_FAN_CAP { sum } else { BAR_FAN_CAP };
    BarFan {
        bar,
        n,
        ring: (bar & (BAR_RING_EVERY - 1)) == BAR_RING_EVERY - 1,
    }
}

/// The armed celebration's release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outro {
    /// The signature the outro resolves on.
    pub sig: u32,
    /// The drop fan's star count ([`DROP_FAN_N`]).
    pub drop: u8,
}

/// A fired armed celebration: the run the detector drives with no key under
/// it.
#[derive(Clone, Copy, Debug)]
struct ExternalRun {
    sig: u32,
    bars: u8,
    ends_at: Instant,
    outro_taken: bool,
}

/// Consecutive same-character repeats that arm the sing-along.
pub const SING_ARM_REPEATS: u32 = 16;

/// Repeats that commit a key switch.
pub const KEY_SWITCH_REPS: u32 = 3;

/// Maximum gap between two presses of the same character for key repeat.
pub const SING_REPEAT_GAP: Duration = Duration::from_millis(250);

/// Wind-down crossfade length.
pub const SING_WIND_DOWN: f32 = 1.0;

const SING_WIND_DOWN_DUR: Duration =
    Duration::from_nanos((SING_WIND_DOWN as f64 * 1_000_000_000.0).round() as u64);

/// Per-window sing-along detector.
pub struct KittySing {
    /// The character of the current same-key run.
    run: Option<char>,
    /// Consecutive at-cadence repeats of `run` (1 = first press).
    count: u32,
    /// The last press of the run.
    last: Option<Instant>,
    /// The session the run was typed into (session switch clears).
    session: Option<u64>,
    /// Arm instant, the beat-clock anchor.
    armed_at: Option<Instant>,
    /// An eager wind-down start (key change / backspace / break / session switch).
    wind_from: Option<Instant>,
    /// When a live celebration began a provisional key hand-over.
    handover_from: Option<Instant>,
    /// An armed, not-yet-fired celebration (`fx celebrate`), spent by the first edge it names.
    armed_external: Option<ArmedCelebration>,
    /// The fired run while it plays and winds down.
    external: Option<ExternalRun>,
    /// The last arm instant, the cooldown's anchor.
    last_external_arm: Option<Instant>,
    /// The bar-fan latch: the last bar a fan was thrown for (one per bar).
    fan_bar: Option<u64>,
}

impl Default for KittySing {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl KittySing {
    /// Fresh unarmed sing-along detector.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            run: None,
            count: 0,
            last: None,
            session: None,
            armed_at: None,
            wind_from: None,
            handover_from: None,
            armed_external: None,
            external: None,
            last_external_arm: None,
            fan_bar: None,
        }
    }

    /// Bind the run to `session`, winding down on a switch.
    #[inline]
    fn rekey(&mut self, now: Instant, session: u64) {
        if self.session != Some(session) {
            // A fired celebration belongs to its session: a switch cuts it into
            // its wind-down (the outro still plays). A pending arm is
            // session-bound by its own field and waits.
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
    #[inline]
    fn release(&mut self, at: Instant) {
        // A fired celebration has no key under it: typing over it clears the
        // run bookkeeping and leaves the bars playing to their line. Only
        // `settle` (drive 0) or a session switch ends it early.
        if self.external.is_none() && self.armed_at.is_some() && self.wind_from.is_none() {
            let start = match self.lazy_release() {
                Some(lazy) if lazy < at => lazy,
                _ => at,
            };
            self.wind_from = Some(start);
        }
        self.run = None;
        self.count = 0;
        self.last = None;
        self.handover_from = None;
    }

    /// Song signature of the fired celebration, else of the held character, else
    /// [`NEUTRAL_SIGNATURE`].
    #[inline]
    #[must_use]
    pub const fn signature(&self) -> u32 {
        if let Some(ext) = self.external {
            return ext.sig;
        }
        match self.run {
            Some(ch) => song_signature(ch),
            None => NEUTRAL_SIGNATURE,
        }
    }

    /// The derived "finger lifted" instant: one repeat gap after the last
    /// press, if the run has one.
    #[inline]
    fn lazy_release(&self) -> Option<Instant> {
        self.last.map(|l| l + SING_REPEAT_GAP)
    }

    /// The instant the wind-down began, if it has: the eager stamp or the
    /// derived lazy release, whichever the reader's `now` has passed.
    #[inline]
    fn wind_start(&self, now: Instant) -> Option<Instant> {
        if self.wind_from.is_some() {
            return self.wind_from;
        }
        if let Some(ext) = self.external {
            return if now >= ext.ends_at {
                Some(ext.ends_at)
            } else {
                None
            };
        }
        match self.lazy_release() {
            Some(release) if now >= release => Some(release),
            _ => None,
        }
    }

    /// The raw bar-grid index at `now`: elapsed [`SING_BAR_SECONDS`] periods
    /// since the arm anchor.
    #[inline]
    fn raw_bar(&self, now: Instant) -> Option<u64> {
        const INV_SING_BAR_SECONDS: f32 = 1.0 / SING_BAR_SECONDS;
        let t0 = self.armed_at?;
        Some((now.saturating_duration_since(t0).as_secs_f32() * INV_SING_BAR_SECONDS) as u64)
    }

    /// Feed one committed printed keystroke.
    #[inline]
    pub fn note_char(&mut self, now: Instant, session: u64, ch: char) {
        self.rekey(now, session);
        if let Some(release) = self.lazy_release()
            && now > release
        {
            self.release(now);
        }
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
            self.run = Some(ch);
            self.count = 1;
            self.handover_from = Some(now);
            self.last = Some(now);
            return;
        }
        if self.run == Some(ch) {
            // Key repeat is wall time: a batched IME commit of a repeated string hands
            // every char one `now`, so count only when real time has elapsed, or a
            // single commit could arm.
            let advanced = match self.last {
                Some(l) => now > l,
                None => true,
            };
            if advanced {
                self.count = self.count.saturating_add(1);
            }
            if self.handover_from.is_some() && self.count >= KEY_SWITCH_REPS {
                self.handover_from = None;
            }
        } else {
            let departure = self.handover_from.take();
            self.release(match departure {
                Some(d) => d,
                None => now,
            });
            self.run = Some(ch);
            self.count = 1;
        }
        self.last = Some(now);
        const {
            assert!(KEY_SWITCH_REPS <= SING_ARM_REPEATS);
        }
        if self.count >= KEY_SWITCH_REPS && (self.armed_at.is_none() || self.wind_from.is_some()) {
            let threshold = if self.wind_from.is_some() && self.has_drive(now) {
                KEY_SWITCH_REPS
            } else {
                SING_ARM_REPEATS
            };
            if self.count >= threshold {
                self.armed_at = Some(now);
                self.wind_from = None;
                self.external = None;
                self.fan_bar = None;
            }
        }
    }

    /// Release any held-key run on backspace.
    #[inline]
    pub fn note_backspace(&mut self, now: Instant) {
        self.release(now);
    }

    /// Release any held-key run on a word-breaking, editing, or navigation key.
    #[inline]
    pub fn note_break(&mut self, now: Instant) {
        self.release(now);
    }

    /// True while any celebration drive remains at `now` (`drive(now) > 0.0`).
    #[inline]
    fn has_drive(&self, now: Instant) -> bool {
        self.armed_at.is_some()
            && match self.wind_start(now) {
                Some(start) => now.saturating_duration_since(start) < SING_WIND_DOWN_DUR,
                None => true,
            }
    }

    /// The celebration drive `0..=1` at `now`.
    #[inline]
    #[must_use]
    pub fn drive(&self, now: Instant) -> f32 {
        const INV_SING_WIND_DOWN: f32 = 1.0 / SING_WIND_DOWN;
        if self.armed_at.is_none() {
            return 0.0;
        }
        let Some(start) = self.wind_start(now) else {
            return 1.0;
        };
        let elapsed = now.saturating_duration_since(start);
        if elapsed >= SING_WIND_DOWN_DUR {
            return 0.0;
        }
        let t = elapsed.as_secs_f32() * INV_SING_WIND_DOWN;
        if t >= 1.0 {
            return 0.0;
        }
        let u = 1.0 - t;
        u * u * (3.0 - 2.0 * u)
    }

    /// True while armed at full drive.
    #[inline]
    #[must_use]
    pub fn is_armed(&self, now: Instant) -> bool {
        self.armed_at.is_some() && self.wind_start(now).is_none()
    }

    /// Fractional beats elapsed since the arm instant while active.
    #[inline]
    #[must_use]
    pub fn beat(&self, now: Instant) -> Option<f32> {
        const INV_SING_BEAT_SECONDS: f32 = 1.0 / SING_BEAT_SECONDS;
        let t0 = self.armed_at?;
        if !self.has_drive(now) {
            return None;
        }
        Some(now.saturating_duration_since(t0).as_secs_f32() * INV_SING_BEAT_SECONDS)
    }

    /// The riff bar index at `now` while armed.
    #[inline]
    #[must_use]
    pub fn bar(&self, now: Instant) -> Option<u64> {
        if !self.is_armed(now) {
            return None;
        }
        let bar = self.raw_bar(now)?;
        if let Some(ext) = self.external
            && bar >= u64::from(ext.bars)
        {
            return None;
        }
        Some(bar)
    }

    /// Reset a drained detector at rest.
    #[inline]
    pub fn settle(&mut self, now: Instant) {
        if self.armed_at.is_some() && !self.has_drive(now) {
            self.armed_at = None;
            self.wind_from = None;
            self.handover_from = None;
            self.external = None;
            self.fan_bar = None;
        }
    }

    /// Arm a one-shot celebration (`fx celebrate`).
    #[inline]
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
            let ms = remaining.as_millis();
            return Err(ArmRefusal::Cooldown {
                remaining_ms: if ms < u128::from(u64::MAX) {
                    ms as u64
                } else {
                    u64::MAX
                },
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

    /// The pending arm, if any.
    #[inline]
    #[must_use]
    pub const fn armed_external(&self) -> Option<ArmedCelebration> {
        self.armed_external
    }

    /// Time left on the arm cooldown at `now`.
    #[inline]
    #[must_use]
    pub fn cooldown_remaining(&self, now: Instant) -> Duration {
        match self.last_external_arm {
            Some(t) => (t + CELEBRATE_COOLDOWN).saturating_duration_since(now),
            None => Duration::ZERO,
        }
    }

    /// True while a fired celebration is playing or winding down.
    #[inline]
    #[must_use]
    pub const fn external_live(&self) -> bool {
        self.external.is_some()
    }

    /// The next external-run event deadline.
    #[inline]
    #[must_use]
    pub fn next_external_deadline(&self, now: Instant) -> Option<Instant> {
        let ext = self.external?;
        let wind = self.wind_start(now);
        if wind.is_none()
            && let (Some(anchor), Some(bar)) = (self.armed_at, self.raw_bar(now))
            && bar < u64::from(ext.bars)
        {
            if self.fan_bar != Some(bar) {
                return Some(now);
            }
            let next = anchor + Duration::from_secs_f32(SING_BAR_SECONDS * (bar + 1) as f32);
            return Some(next.min(ext.ends_at).max(now + Duration::from_micros(1)));
        }
        let release = wind.unwrap_or(ext.ends_at);
        if !ext.outro_taken {
            return Some(release.max(now));
        }
        Some((release + SING_WIND_DOWN_DUR).max(now))
    }

    /// Report `session`'s exit-0 block.
    #[inline]
    pub fn note_green_block(&mut self, now: Instant, session: u64) {
        if let Some(arm) = self.armed_external
            && arm.on == CelebrateOn::Green
            && arm.session == session
        {
            self.fire_external(now, session, arm);
        }
    }

    /// Report a keyed Enter in `session`.
    #[inline]
    pub fn note_keyed_enter(&mut self, now: Instant, session: u64) {
        if let Some(arm) = self.armed_external
            && arm.on == CelebrateOn::Enter
            && arm.session == session
        {
            self.fire_external(now, session, arm);
        }
    }

    /// Report an honoured `done` from the program in front of `session`.
    #[inline]
    pub fn note_program_done(&mut self, now: Instant, session: u64) -> bool {
        if let Some(arm) = self.armed_external
            && arm.on == CelebrateOn::Done
            && arm.session == session
        {
            self.fire_external(now, session, arm);
            return true;
        }
        false
    }

    #[inline]
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

    /// Take the current bar's fan once per bar while armed.
    #[inline]
    pub fn take_bar_fan(&mut self, now: Instant) -> Option<BarFan> {
        let bar = self.bar(now)?;
        if self.fan_bar == Some(bar) {
            return None;
        }
        self.fan_bar = Some(bar);
        Some(bar_fan(bar))
    }

    /// Return the outro release once when an armed celebration ends.
    #[inline]
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

    /// Clear any held-key run and pending celebration arm.
    #[inline]
    pub const fn drain_serious(&mut self) {
        self.run = None;
        self.count = 0;
        self.last = None;
        self.session = None;
        self.armed_at = None;
        self.wind_from = None;
        self.handover_from = None;
        self.armed_external = None;
        self.external = None;
        self.fan_bar = None;
    }
}

/// Ring capacity for live note sprites.
pub const MAX_NOTES: usize = 16;

/// Note sprite lifetime in seconds.
pub const NOTE_LIFE: f32 = 1.6;
const NOTE_LIFE_DUR: Duration = Duration::from_millis(1_600);

const NOTE_RISE_CELLS: f32 = 1.4;

/// Rainbow palette cycled by notes.
pub const NOTE_TINTS: [u32; 6] = [
    0x00FF_5A5A,
    0x00FF_A94D,
    0x00FF_E15A,
    0x005A_D95A,
    0x005A_A8FF,
    0x00B0_6AFF,
];

/// Authored note glyph kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoteKind {
    /// Single eighth note.
    Eighth,
    /// Beamed pair.
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

/// Resolved note sprite in cell units relative to the mouth anchor.
#[derive(Clone, Copy, Debug)]
pub struct NoteSprite {
    pub dx: f32,
    pub dy: f32,
    /// `0..=255` fade envelope.
    pub alpha: u8,
    pub kind: NoteKind,
    /// `0x00RRGGBB` rainbow tint.
    pub tint: u32,
}

/// Ring-capped note pool.
pub struct MusicNotes {
    ring: [Option<Note>; MAX_NOTES],
    /// Number of `Some` slots in `ring`.
    live: u8,
    /// Next ring slot to (over)write.
    head: usize,
    /// Last half-beat index a note was spawned for (one note per half-beat).
    spawned_half_beat: Option<i64>,
    /// Xorshift32 scatter rng.
    rng: u32,
}

impl Default for MusicNotes {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl MusicNotes {
    /// Construct an empty note pool.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ring: [None; MAX_NOTES],
            live: 0,
            head: 0,
            spawned_half_beat: None,
            rng: 0x9E37_79B9,
        }
    }

    #[inline]
    const fn rnd(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// Advance the field one frame.
    #[inline]
    pub fn update(&mut self, now: Instant, streaming: bool, beat: Option<f32>) {
        if self.live != 0 {
            let mut rem = self.live;
            let mut i = 0;
            while i < MAX_NOTES {
                if let Some(n) = self.ring[i] {
                    if now.saturating_duration_since(n.born) >= NOTE_LIFE_DUR {
                        self.ring[i] = None;
                        self.live -= 1;
                    }
                    rem -= 1;
                    if rem == 0 {
                        break;
                    }
                }
                i += 1;
            }
        }
        let Some(beat) = beat else {
            self.spawned_half_beat = None;
            return;
        };
        if !streaming {
            self.spawned_half_beat = None;
            return;
        }
        let half_beat = (beat * 2.0).floor() as i64;
        if self.spawned_half_beat == Some(half_beat) {
            return;
        }
        self.spawned_half_beat = Some(half_beat);
        let seed = self.rnd();
        let kind = if half_beat & 1 == 0 {
            NoteKind::Eighth
        } else {
            NoteKind::Beamed
        };
        if self.ring[self.head].is_none() {
            self.live += 1;
        }
        self.ring[self.head] = Some(Note {
            born: now,
            kind,
            seed,
        });
        const {
            assert!(MAX_NOTES.is_power_of_two());
        }
        self.head = (self.head + 1) & (MAX_NOTES - 1);
    }

    /// True while any note is alive.
    #[inline]
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.live != 0
    }

    /// Clear the field.
    #[inline]
    pub const fn clear(&mut self) {
        if self.live != 0 {
            self.ring = [None; MAX_NOTES];
            self.live = 0;
        }
        self.spawned_half_beat = None;
    }

    /// Resolve one live note at `now`. Full motion: it rises with a sideways
    /// wobble.
    #[inline]
    fn resolve(note: &Note, now: Instant, reduced_motion: bool) -> Option<NoteSprite> {
        let age = now.saturating_duration_since(note.born).as_secs_f32();
        Self::resolve_age(note.kind, note.seed, age, reduced_motion)
    }

    #[inline]
    const fn resolve_age(
        kind: NoteKind,
        seed: u32,
        age: f32,
        reduced_motion: bool,
    ) -> Option<NoteSprite> {
        let u = age * (1.0 / NOTE_LIFE);
        if !(u >= 0.0 && u < 1.0) {
            return None;
        }
        let alpha = if u < 0.15 {
            (u * (1.0 / 0.15) * 255.0) as u8
        } else if u <= 0.6 {
            255
        } else {
            ((1.0 - u) * (1.0 / 0.4) * 255.0) as u8
        };
        let s = seed;
        let tint = NOTE_TINTS[(s % NOTE_TINTS.len() as u32) as usize];
        let x0 = 0.1 + (0.8 / 255.0) * ((s >> 8) & 0xff) as f32;
        let phase = ((s >> 16) & 0xff) as f32 * (1.0 / 255.0);
        let (dx, dy) = if reduced_motion {
            (x0, -0.3 - 0.8 * phase)
        } else {
            let (sin_w, _) =
                crate::supernova::sin_cos_f32(std::f32::consts::TAU * (u * 1.5 + phase));
            (x0 + 0.18 * sin_w, -NOTE_RISE_CELLS * u)
        };
        Some(NoteSprite {
            dx,
            dy,
            alpha,
            kind,
            tint,
        })
    }

    /// Return this frame's sprites as a fixed `None`-padded array.
    #[inline]
    #[must_use]
    pub fn frame_array(
        &self,
        now: Instant,
        reduced_motion: bool,
    ) -> [Option<NoteSprite>; MAX_NOTES] {
        let mut out = [None; MAX_NOTES];
        let mut rem = usize::from(self.live);
        if rem == 0 {
            return out;
        }
        let mut i = 0;
        let mut slot = 0;
        while slot < MAX_NOTES {
            if let Some(note) = &self.ring[slot] {
                if let Some(sprite) = Self::resolve(note, now, reduced_motion) {
                    out[i] = Some(sprite);
                    i += 1;
                }
                rem -= 1;
                if rem == 0 {
                    break;
                }
            }
            slot += 1;
        }
        out
    }

    /// Resolve this frame's sprites into `out` (bounded by [`MAX_NOTES`]).
    #[cfg(test)]
    pub fn frames(&self, now: Instant, reduced_motion: bool, out: &mut Vec<NoteSprite>) {
        let mut i = 0;
        while i < MAX_NOTES {
            if let Some(note) = &self.ring[i]
                && let Some(sprite) = Self::resolve(note, now, reduced_motion)
            {
                out.push(sprite);
            }
            i += 1;
        }
    }
}

const NOTE_HOST_SALT: u64 = 0x5150_9A7E_D1B2_C4F3;

/// Stable atlas identity for one baked note tile.
#[inline]
#[must_use]
pub const fn note_host_id(kind: NoteKind, w: u16, h: u16) -> u64 {
    let k = match kind {
        NoteKind::Eighth => 1u64,
        NoteKind::Beamed => 2,
    };
    let mut x = NOTE_HOST_SALT ^ (k << 32) ^ ((w as u64) << 16) ^ (h as u64);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Natural note tile size for a cell of height `cell_h`.
#[inline]
#[must_use]
pub const fn note_nat_size(kind: NoteKind, cell_h: u16) -> (u16, u16) {
    let ch = if cell_h > 1 { cell_h } else { 1 };
    let h = ((ch as f32) * 0.75).round().max(4.0) as u16;
    let aspect = match kind {
        NoteKind::Eighth => 0.72,
        NoteKind::Beamed => 1.05,
    };
    let w = ((h as f32) * aspect).round().max(4.0) as u16;
    (w, h)
}

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

const NOTE_A: f32 = 0.88;

/// Bake one white note glyph at `w × h`.
#[inline]
#[must_use]
pub fn bake_note(w: u16, h: u16, kind: NoteKind) -> Tile {
    let mut tile = Tile::new(u32::from(w), u32::from(h));
    paint_note(&mut tile, kind);
    tile
}

/// Paint one white note glyph onto a cleared `tile`.
#[inline]
pub fn paint_note(tile: &mut Tile, kind: NoteKind) {
    let (w, h) = (tile.width(), tile.height());
    if w == 0 || h == 0 {
        return;
    }
    let white = (1.0, 1.0, 1.0);
    let fit = PathTransform::fit(w, h);
    let (wf, hf) = (w as f32, h as f32);
    match kind {
        NoteKind::Eighth => {
            fill_path(tile, &[&EIGHTH_STEM], white, NOTE_A, fit);
            fill_path(tile, &[&EIGHTH_FLAG], white, NOTE_A, fit);
            tile.disc(0.40 * wf, 0.82 * hf, (0.16 * hf).max(1.2), white, NOTE_A);
        }
        NoteKind::Beamed => {
            fill_path(tile, &[&BEAM_BAR], white, NOTE_A, fit);
            fill_path(tile, &[&BEAM_STEM_L], white, NOTE_A, fit);
            fill_path(tile, &[&BEAM_STEM_R], white, NOTE_A, fit);
            tile.disc(0.24 * wf, 0.84 * hf, (0.14 * hf).max(1.2), white, NOTE_A);
            tile.disc(0.76 * wf, 0.84 * hf, (0.14 * hf).max(1.2), white, NOTE_A);
        }
    }
}

#[cfg(test)]
#[path = "kitty_sing_tests.rs"]
mod tests;
