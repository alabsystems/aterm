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
        assert!(BAR_RING_EVERY > 0);
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
        ring: bar % BAR_RING_EVERY == BAR_RING_EVERY - 1,
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

/// Per-window sing-along detector.
#[derive(Default)]
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

impl KittySing {
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
        if self.external.is_some() && self.wind_from.is_none() {
            self.run = None;
            self.count = 0;
            self.last = None;
            self.handover_from = None;
            return;
        }
        if self.armed_at.is_some() && self.wind_from.is_none() {
            self.wind_from = Some(self.lazy_release().map_or(at, |lazy| lazy.min(at)));
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
            return (now >= ext.ends_at).then_some(ext.ends_at);
        }
        self.lazy_release().filter(|release| now >= *release)
    }

    /// The raw bar-grid index at `now`: elapsed [`SING_BAR_SECONDS`] periods
    /// since the arm anchor.
    #[inline]
    fn raw_bar(&self, now: Instant) -> Option<u64> {
        let t0 = self.armed_at?;
        Some((now.saturating_duration_since(t0).as_secs_f32() / SING_BAR_SECONDS) as u64)
    }

    /// Feed one committed printed keystroke.
    pub fn note_char(&mut self, now: Instant, session: u64, ch: char) {
        self.rekey(now, session);
        if self.lazy_release().is_some_and(|release| now > release) {
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
            if self.last.is_none_or(|l| now > l) {
                self.count = self.count.saturating_add(1);
            }
            if self.handover_from.is_some() && self.count >= KEY_SWITCH_REPS {
                self.handover_from = None;
            }
        } else {
            let departure = self.handover_from.take();
            self.release(departure.unwrap_or(now));
            self.run = Some(ch);
            self.count = 1;
        }
        self.last = Some(now);
        let threshold = if self.wind_from.is_some() && self.has_drive(now) {
            KEY_SWITCH_REPS
        } else {
            SING_ARM_REPEATS
        };
        if self.count >= threshold && (self.armed_at.is_none() || self.wind_from.is_some()) {
            self.armed_at = Some(now);
            self.wind_from = None;
            self.external = None;
            self.fan_bar = None;
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
            && self.wind_start(now).is_none_or(|start| {
                now.saturating_duration_since(start).as_secs_f32() < SING_WIND_DOWN
            })
    }

    /// The celebration drive `0..=1` at `now`.
    #[inline]
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
        let t0 = self.armed_at?;
        if !self.has_drive(now) {
            return None;
        }
        Some(now.saturating_duration_since(t0).as_secs_f32() / SING_BEAT_SECONDS)
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
        self.last_external_arm.map_or(Duration::ZERO, |t| {
            (t + CELEBRATE_COOLDOWN).saturating_duration_since(now)
        })
    }

    /// True while a fired celebration is playing or winding down.
    #[inline]
    #[must_use]
    pub const fn external_live(&self) -> bool {
        self.external.is_some()
    }

    /// The next external-run event deadline.
    #[must_use]
    pub fn next_external_deadline(&self, now: Instant) -> Option<Instant> {
        let ext = self.external?;
        if let Some(bar) = self.bar(now) {
            if self.fan_bar != Some(bar) {
                return Some(now);
            }
            let anchor = self.armed_at?;
            let next = anchor + Duration::from_secs_f32(SING_BAR_SECONDS * (bar + 1) as f32);
            return Some(next.min(ext.ends_at).max(now + Duration::from_micros(1)));
        }
        let release = self.wind_start(now).unwrap_or(ext.ends_at);
        if !ext.outro_taken {
            return Some(release.max(now));
        }
        Some((release + Duration::from_secs_f32(SING_WIND_DOWN)).max(now))
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
    fn default() -> Self {
        Self {
            ring: [None; MAX_NOTES],
            live: 0,
            head: 0,
            spawned_half_beat: None,
            rng: 0x9E37_79B9,
        }
    }
}

impl MusicNotes {
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
    pub fn update(&mut self, now: Instant, streaming: bool, beat: Option<f32>) {
        if self.live != 0 {
            for slot in &mut self.ring {
                if slot.is_some_and(|n| {
                    now.saturating_duration_since(n.born).as_secs_f32() >= NOTE_LIFE
                }) {
                    *slot = None;
                    self.live -= 1;
                }
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
        let kind = if half_beat % 2 == 0 {
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
        self.head = (self.head + 1) % MAX_NOTES;
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
        let u = age / NOTE_LIFE;
        if !(0.0..1.0).contains(&u) {
            return None;
        }
        let fade_in = (u / 0.15).min(1.0);
        let fade_out = ((1.0 - u) / 0.4).min(1.0);
        let alpha = (fade_in * fade_out * 255.0) as u8;
        let s = note.seed;
        let tint = NOTE_TINTS[(s % NOTE_TINTS.len() as u32) as usize];
        let x0 = 0.1 + 0.8 * ((s >> 8) & 0xff) as f32 / 255.0;
        let phase = ((s >> 16) & 0xff) as f32 / 255.0;
        let (dx, dy) = if reduced_motion {
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

    /// Return this frame's sprites as a fixed `None`-padded array.
    #[inline]
    #[must_use]
    pub fn frame_array(
        &self,
        now: Instant,
        reduced_motion: bool,
    ) -> [Option<NoteSprite>; MAX_NOTES] {
        let mut out = [None; MAX_NOTES];
        if self.live == 0 {
            return out;
        }
        let mut i = 0;
        for note in self.ring.iter().flatten() {
            if let Some(sprite) = Self::resolve(note, now, reduced_motion) {
                out[i] = Some(sprite);
                i += 1;
            }
        }
        out
    }

    /// Resolve this frame's sprites into `out` (bounded by [`MAX_NOTES`]).
    #[cfg(test)]
    pub fn frames(&self, now: Instant, reduced_motion: bool, out: &mut Vec<NoteSprite>) {
        for note in self.ring.iter().flatten() {
            if let Some(sprite) = Self::resolve(note, now, reduced_motion) {
                out.push(sprite);
            }
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
pub fn note_nat_size(kind: NoteKind, cell_h: u16) -> (u16, u16) {
    let h = (f32::from(cell_h.max(1)) * 0.75).round().max(4.0) as u16;
    let aspect = match kind {
        NoteKind::Eighth => 0.72,
        NoteKind::Beamed => 1.05,
    };
    let w = (f32::from(h) * aspect).round().max(4.0) as u16;
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
#[must_use]
pub fn bake_note(w: u16, h: u16, kind: NoteKind) -> Tile {
    let mut tile = Tile::new(u32::from(w), u32::from(h));
    paint_note(&mut tile, kind);
    tile
}

/// Paint one white note glyph onto a cleared `tile`.
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

    fn hold(d: &mut KittySing, t0: Instant, ch: char, n: u32, gap_ms: u64) -> Instant {
        let mut t = t0;
        for i in 0..n {
            t = t0 + Duration::from_millis(u64::from(i) * gap_ms);
            d.note_char(t, S, ch);
        }
        t
    }

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

    #[test]
    fn a_batched_ime_commit_of_repeats_does_not_arm() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        for _ in 0..16 {
            d.note_char(t0, S, 'w');
        }
        assert!(
            !d.is_armed(t0),
            "a single batched commit of repeats is one step, not a hold"
        );
        assert_eq!(d.drive(t0), 0.0);
        let mut held = KittySing::default();
        let armed_at = hold(&mut held, t0, 'w', SING_ARM_REPEATS, 30);
        assert!(
            held.is_armed(armed_at),
            "real repeats across time still arm at {SING_ARM_REPEATS}"
        );
    }

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

    #[test]
    fn backspace_never_arms_and_breaks_a_run() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        for i in 0..32u64 {
            d.note_backspace(t0 + Duration::from_millis(i * 30));
        }
        assert!(!d.is_armed(t0 + Duration::from_secs(1)));
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

    #[test]
    fn wind_down_crossfades_to_zero() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let t = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        assert_eq!(d.drive(t), 1.0);
        let change = t + Duration::from_millis(30);
        d.note_char(change, S, 'b'); // key change: a provisional hand-over
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

    #[test]
    fn session_switch_releases_the_hold() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let t = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        assert!(d.is_armed(t));
        let t1 = t + Duration::from_millis(30);
        d.note_char(t1, 99, 'a'); // same key, other session
        assert!(!d.is_armed(t1), "the switch released the hold");
    }

    #[test]
    fn re_earning_the_threshold_during_wind_down_re_arms() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        assert!(d.is_armed(armed));
        let after_gap = armed + SING_REPEAT_GAP + Duration::from_millis(50);
        d.note_char(after_gap, S, 'a');
        assert!(
            d.drive(after_gap) < 1.0 && d.drive(after_gap) > 0.0,
            "the hiccup started a real wind-down"
        );
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

    #[test]
    fn a_continuous_hold_does_not_rewind_its_beat_clock() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
        let anchor = d.beat(armed);
        let mut t = armed;
        for _ in 0..16 {
            t += Duration::from_millis(30);
            d.note_char(t, S, 'a');
        }
        let expected = t.saturating_duration_since(armed).as_secs_f32() / SING_BEAT_SECONDS;
        let got = d.beat(t).expect("still armed");
        assert!(
            (got - expected).abs() < 1e-3 && anchor == Some(0.0),
            "a continuous hold must never re-anchor: got {got}, expected {expected}"
        );
    }

    #[test]
    fn wind_down_drains_the_note_field() {
        let mut notes = MusicNotes::default();
        let t0 = Instant::now();
        for hb in 0..8u64 {
            let now = t0 + Duration::from_millis(hb * 200);
            notes.update(now, true, Some(hb as f32 * 0.5));
        }
        assert!(notes.is_active());
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

    #[test]
    fn wind_down_clears_spawned_half_beat_so_reanchored_song_spawns_on_downbeat() {
        let mut notes = MusicNotes::default();
        let t0 = Instant::now();
        notes.update(t0, true, Some(0.1));
        assert_eq!(notes.ring.iter().flatten().count(), 1);
        let t1 = t0 + Duration::from_millis(50);
        notes.update(t1, false, Some(0.2));
        assert_eq!(notes.ring.iter().flatten().count(), 1);
        let t2 = t1 + Duration::from_millis(50);
        notes.update(t2, true, Some(0.0));
        assert_eq!(
            notes.ring.iter().flatten().count(),
            2,
            "re-anchored song after wind-down frame must spawn half-beat 0 note"
        );
    }

    #[test]
    fn reduced_motion_notes_hold_still() {
        let mut notes = MusicNotes::default();
        let t0 = Instant::now();
        notes.update(t0, true, Some(0.0));
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

    #[test]
    fn song_signature_mixer_is_bijective_over_all_chars() {
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

        fn hold(&mut self, from: Instant, ch: char, n: u32, gap_ms: u64) -> Instant {
            let mut t = from;
            for i in 0..n {
                t = from + Duration::from_millis(u64::from(i) * gap_ms);
                self.press(t, ch);
            }
            t
        }
    }

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

    #[test]
    fn genuine_typing_still_winds_the_song_down() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let armed = hold(&mut d, t0, 'w', SING_ARM_REPEATS, 30);
        assert!(d.is_armed(armed));
        let a = armed + Duration::from_millis(30);
        d.note_char(a, S, 'a'); // provisional — could still be a switch
        let r = a + Duration::from_millis(30);
        d.note_char(r, S, 'r'); // a second distinct char: typing
        assert!(
            d.drive(r) < 1.0,
            "typing proves itself and the wind-down is already running"
        );
        let gone = a + Duration::from_secs_f32(SING_WIND_DOWN * 1.05);
        assert_eq!(d.drive(gone), 0.0, "the crossfade completes");
        d.settle(gone);
        assert_eq!(d.beat(gone), None, "settled = byte-identical rest");
    }

    #[test]
    fn a_missed_gap_recovers_with_three_reps_inside_the_glow() {
        let t0 = Instant::now();
        let mut host = HostSim::new(t0);
        let armed = host.hold(t0, 'w', 20, 30);
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

        let again = green + Duration::from_secs_f32(SING_BAR_SECONDS + 0.2);
        d.note_green_block(again, S);
        assert_eq!(d.bar(again), Some(1), "the grid did not move");

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
            bar_fan(7).ring && !bar_fan(4).ring && bar_fan(u64::MAX).ring,
            "the ring rides every fourth bar without overflowing at u64::MAX"
        );
    }

    #[test]
    fn an_on_done_arm_fires_on_the_session_s_next_done_and_only_once() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        let sig = song_signature('D');
        let arm = d
            .arm_external(t0, S, sig, 2, CelebrateOn::Done)
            .expect("admitted");
        assert_eq!(arm.on.as_str(), "done");
        d.note_green_block(t0 + Duration::from_millis(10), S);
        d.note_keyed_enter(t0 + Duration::from_millis(20), S);
        assert!(
            !d.note_program_done(t0 + Duration::from_millis(30), S + 1),
            "another session's done is not this one's"
        );
        let waiting = t0 + Duration::from_millis(40);
        assert_eq!(d.armed_external(), Some(arm), "the arm still waits");
        assert_eq!(d.drive(waiting), 0.0, "nothing lit");

        let done = t0 + Duration::from_millis(500);
        assert!(d.note_program_done(done, S), "this session's done fires it");
        assert_eq!(d.armed_external(), None, "the arm is spent by the edge");
        assert!(d.is_armed(done));
        assert_eq!(d.drive(done), 1.0);
        assert_eq!(d.signature(), sig);
        assert_eq!(
            d.beat(done),
            Some(0.0),
            "the beat clock anchors at the fire"
        );

        let again = done + Duration::from_secs_f32(SING_BAR_SECONDS + 0.2);
        assert!(
            !d.note_program_done(again, S),
            "a second done fires nothing"
        );
        assert_eq!(d.bar(again), Some(1), "the grid did not move");

        let mut g = KittySing::default();
        g.arm_external(t0, S, sig, 1, CelebrateOn::Green)
            .expect("admitted");
        assert!(!g.note_program_done(t0 + Duration::from_millis(5), S));
        assert!(g.armed_external().is_some(), "the green arm still waits");
        assert_eq!(g.drive(t0 + Duration::from_millis(5)), 0.0);
    }

    #[test]
    fn an_unarmed_session_never_celebrates_on_a_green_block() {
        let mut d = KittySing::default();
        let t0 = Instant::now();
        d.note_green_block(t0, S);
        d.note_keyed_enter(t0 + Duration::from_millis(10), S);
        assert!(!d.note_program_done(t0 + Duration::from_millis(15), S));
        let t = t0 + Duration::from_millis(20);
        assert_eq!(d.drive(t), 0.0);
        assert!(!d.is_armed(t));
        assert_eq!(d.bar(t), None);
        assert_eq!(d.beat(t), None);
        assert_eq!(d.signature(), NEUTRAL_SIGNATURE);
        assert_eq!(d.take_bar_fan(t), None);
        assert_eq!(d.take_outro(t), None);
        assert!(!d.external_live());
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
    }
}
