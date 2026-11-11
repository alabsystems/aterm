// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! State words: each trail engine's main state as named `u64` words, what `lab trail-diff`
//! holds equal on every frame (`docs/design/PRISM-GLASS.md` §15.0.2(a), §15.1 P0.3).
//!
//! Each engine writes its own words from a child module (`<engine>/state_words.rs`), so it can
//! read its private fields. Every field is named there by hand: an `f32` by its bits, every
//! instant relative to the caller's `t0`, and nothing through `Debug`, so a field added later is
//! not hashed until a harness commit names it. A list or a nested record folds into one word
//! ([`Words::group`]). Debug and test builds only.
//!
//! Every `state_words.rs` is in the harness closure `lab trail-diff` holds fixed between its two
//! builds (§15.0.6: with the dump `aterm-gui/examples/trail_dump.rs`, `examples/support/**`, the
//! rig `src/trail_rig.rs`, its frame reads `src/trail_frame_reads.rs` and
//! `[profile.trail-diff]`), so a change here lands alone, as a harness commit: `--base HEAD~1`
//! refuses it (exit 3), and it is proved by `--control same` (0) and `--control plant` (the
//! plant). A word for a new field takes three commits: the field, read by nothing new (proved by
//! `--base HEAD~1`); then the harness commit that names it; then its reader.
//!
//! The frame's light report ([`ArmsDrawn`]) lives here too: which light arms drew and which
//! streams each touched (§15.0.2(b)).

use crate::cursor_glow::{Geom, GlowConfig, SoundCue};
use crate::trail_sound::{
    CelebrationGesture, OutputGesture, SoundEvent, SoundGesture, SoundKind, SoundVoice, WordGesture,
};
use aterm_time::Instant;
use std::time::Duration;

/// One named word.
pub type Word = (&'static str, u64);

/// The word an absent instant writes: no instant relative to `t0` lands on it
/// ([`instant_word`] never reaches `i64::MIN`).
pub const NO_INSTANT: u64 = i64::MIN as u64;

/// `at` relative to `t0` in signed nanoseconds, as its two's-complement bits: a frame's words
/// are the same whatever instant its script started at.
#[inline]
#[must_use]
pub fn instant_word(t0: Instant, at: Instant) -> u64 {
    const MAX_NS: u128 = i64::MAX as u128;
    let nanos = match at.checked_duration_since(t0) {
        Some(after) => after.as_nanos().min(MAX_NS) as i64,
        None => -(t0.duration_since(at).as_nanos().min(MAX_NS) as i64),
    };
    nanos as u64
}

/// A duration in nanoseconds, saturating.
#[inline]
#[must_use]
pub const fn duration_word(d: Duration) -> u64 {
    let n = d.as_nanos();
    if n > u64::MAX as u128 {
        u64::MAX
    } else {
        n as u64
    }
}

/// A sink of named words: [`StateWords`] keeps each one, [`Fold`] folds them into one.
pub trait Words {
    /// The instant every time is written relative to.
    fn t0(&self) -> Instant;

    /// Write one word.
    fn put(&mut self, name: &'static str, v: u64);

    /// A flag, `0` or `1`.
    #[inline]
    fn flag(&mut self, name: &'static str, v: bool) {
        self.put(name, u64::from(v));
    }

    /// An `f32` by its bits.
    #[inline]
    fn f32(&mut self, name: &'static str, v: f32) {
        self.put(name, u64::from(v.to_bits()));
    }

    /// An `f64` by its bits.
    #[inline]
    fn f64(&mut self, name: &'static str, v: f64) {
        self.put(name, v.to_bits());
    }

    /// A cell `(row, col)`.
    #[inline]
    fn cell(&mut self, name: &'static str, (row, col): (u16, u16)) {
        self.put(name, u64::from(row) << 16 | u64::from(col));
    }

    /// A cell or none: none writes `u64::MAX`, which no cell reaches.
    #[inline]
    fn cell_opt(&mut self, name: &'static str, v: Option<(u16, u16)>) {
        self.put(
            name,
            match v {
                Some((row, col)) => u64::from(row) << 16 | u64::from(col),
                None => u64::MAX,
            },
        );
    }

    /// A row or column, or none: none writes `u64::MAX`, which no `u16` reaches.
    #[inline]
    fn u16_opt(&mut self, name: &'static str, v: Option<u16>) {
        self.put(
            name,
            match v {
                Some(v) => u64::from(v),
                None => u64::MAX,
            },
        );
    }

    /// An instant relative to [`Self::t0`] ([`instant_word`]).
    #[inline]
    fn at(&mut self, name: &'static str, at: Instant) {
        let v = instant_word(self.t0(), at);
        self.put(name, v);
    }

    /// An instant or none ([`NO_INSTANT`]).
    #[inline]
    fn at_opt(&mut self, name: &'static str, at: Option<Instant>) {
        let v = match at {
            Some(at) => instant_word(self.t0(), at),
            None => NO_INSTANT,
        };
        self.put(name, v);
    }

    /// A duration ([`duration_word`]).
    #[inline]
    fn dur(&mut self, name: &'static str, d: Duration) {
        self.put(name, duration_word(d));
    }

    /// A record folded into one word.
    #[inline]
    fn group(&mut self, name: &'static str, f: impl FnOnce(&mut Fold)) {
        let mut fold = Fold::new(self.t0());
        f(&mut fold);
        self.put(name, fold.finish());
    }

    /// A list folded into one word: each item a record, then the count.
    #[inline]
    fn list<T>(
        &mut self,
        name: &'static str,
        items: impl IntoIterator<Item = T>,
        mut each: impl FnMut(&mut Fold, T),
    ) {
        self.group(name, |fold| {
            let mut n = 0_u64;
            for item in items {
                fold.group("item", |f| each(f, item));
                n += 1;
            }
            fold.put("len", n);
        });
    }

    /// An optional record folded into one word, absent apart from every present one.
    #[inline]
    fn opt<T>(&mut self, name: &'static str, v: Option<T>, f: impl FnOnce(&mut Fold, T)) {
        self.group(name, |fold| match v {
            None => fold.put("none", 0),
            Some(v) => {
                fold.put("some", 1);
                f(fold, v);
            }
        });
    }
}

/// One frame's state words, in the order the engines wrote them.
#[derive(Clone, Debug)]
pub struct StateWords {
    t0: Instant,
    words: Vec<Word>,
}

impl StateWords {
    /// No words yet, every time to be written relative to `t0`.
    #[inline]
    #[must_use]
    pub const fn new(t0: Instant) -> Self {
        Self {
            t0,
            words: Vec::new(),
        }
    }

    /// The words, in write order.
    #[inline]
    #[must_use]
    pub const fn words(&self) -> &[Word] {
        self.words.as_slice()
    }

    /// The words, owned.
    #[inline]
    #[must_use]
    pub fn into_words(self) -> Vec<Word> {
        self.words
    }

    /// The first word named `name`.
    #[inline]
    #[must_use]
    pub fn get(&self, name: &str) -> Option<u64> {
        let mut i = 0;
        while i < self.words.len() {
            let (n, v) = self.words[i];
            if n == name {
                return Some(v);
            }
            i += 1;
        }
        None
    }
}

impl Words for StateWords {
    #[inline]
    fn t0(&self) -> Instant {
        self.t0
    }

    #[inline]
    fn put(&mut self, name: &'static str, v: u64) {
        self.words.push((name, v));
    }
}

/// Words folded into one: FNV-1a/64 over each word's name and its little-endian bytes, in order.
#[derive(Clone, Copy, Debug)]
pub struct Fold {
    t0: Instant,
    h: u64,
}

impl Fold {
    /// An empty fold, every time to be written relative to `t0`.
    #[inline]
    #[must_use]
    pub const fn new(t0: Instant) -> Self {
        Self {
            t0,
            h: aterm_hash::FNV64_OFFSET,
        }
    }

    /// The folded word.
    #[inline]
    #[must_use]
    pub const fn finish(self) -> u64 {
        self.h
    }
}

impl Words for Fold {
    #[inline]
    fn t0(&self) -> Instant {
        self.t0
    }

    #[inline]
    fn put(&mut self, name: &'static str, v: u64) {
        self.h = aterm_hash::fnv1a_64_extend(self.h, name.as_bytes());
        self.h = aterm_hash::fnv1a_64_extend(self.h, &v.to_le_bytes());
    }
}

/// Every stored [`GlowConfig`]'s words, the one helper each copy hashes through (§15.0.2(a)):
/// each field by name, except [`GlowConfig::light`] (§15.0.4), which stays out so a latched light
/// never reads as a state difference.
#[inline]
pub fn glow_cfg_words(w: &mut impl Words, cfg: &GlowConfig) {
    w.flag("enabled", cfg.enabled);
    w.put("style", cfg.style as u64);
    w.put("color", u64::from(cfg.color));
    w.put("accent", u64::from(cfg.accent));
    w.dur("duration", cfg.duration);
    w.put("length", cfg.length as u64);
    w.f32("intensity", cfg.intensity);
    w.flag("audible", cfg.audible);
    w.f32("radius", cfg.radius);
    w.flag("ring", cfg.ring);
    w.flag("dark_theme", cfg.dark_theme);
    w.put("theme_fg", u64::from(cfg.theme_fg));
    w.put("theme_bg", u64::from(cfg.theme_bg));
    w.flag("beam", cfg.beam);
    w.f32("head_dx", cfg.head_dx);
    w.flag("ribbon_tall", cfg.ribbon_tall);
    w.flag("ribbon_flat", cfg.ribbon_flat);
    w.flag("classic_mono", cfg.classic_mono);
}

/// A [`Geom`]'s words.
#[inline]
pub fn geom_words(w: &mut impl Words, g: &Geom) {
    w.put("cw", g.cw as u64);
    w.put("ch", g.ch as u64);
    w.put("rows", g.rows as u64);
    w.put("cols", g.cols as u64);
    w.put("origin_x", u64::from(g.origin_x));
    w.put("origin_y", u64::from(g.origin_y));
    w.put("win_w", u64::from(g.win_w));
    w.put("win_h", u64::from(g.win_h));
    w.put("head", u64::from(g.head));
}

/// A [`SoundCue`]'s words: its kind by variant and payload, then its fields.
#[inline]
pub fn sound_cue_words(w: &mut impl Words, cue: &SoundCue) {
    sound_kind_words(w, cue.kind);
    w.put("col", u64::from(cue.col));
    w.f32("heat", cue.heat);
    w.f32("hue", cue.hue);
    w.put("dir", cue.dir as u8 as u64);
    w.flag("shifted", cue.shifted);
}

/// A [`SoundEvent`]'s words, each cue a frame pushes as the dump hashes it: its style, its voice
/// and its gesture by variant and payload, then its fields.
#[inline]
pub fn sound_event_words(w: &mut impl Words, event: &SoundEvent) {
    w.put("style", event.style as u64);
    let (voice, of) = match event.voice {
        SoundVoice::Style => (0, u64::MAX),
        SoundVoice::Of(style) => (1, style as u64),
        SoundVoice::Mech => (2, u64::MAX),
        SoundVoice::Typewriter => (3, u64::MAX),
        SoundVoice::Marimba => (4, u64::MAX),
        SoundVoice::Felt => (5, u64::MAX),
        SoundVoice::RainbowKittyV2 => (6, u64::MAX),
    };
    w.put("voice", voice);
    w.put("voice.of", of);
    match event.kind {
        SoundGesture::Trail(kind) => {
            w.put("gesture", 0);
            sound_kind_words(w, kind);
        }
        SoundGesture::Words(WordGesture::Bonk) => w.put("gesture", 1),
        SoundGesture::Celebration(CelebrationGesture::RiffBar { bar, sig }) => {
            w.put("gesture", 2);
            w.put("bar", u64::from(bar));
            w.put("sig", u64::from(sig));
        }
        SoundGesture::Celebration(CelebrationGesture::Outro { sig }) => {
            w.put("gesture", 3);
            w.put("sig", u64::from(sig));
        }
        SoundGesture::Output(OutputGesture::Shimmer) => w.put("gesture", 4),
        SoundGesture::Output(OutputGesture::Settle) => w.put("gesture", 5),
        SoundGesture::Output(OutputGesture::Verdict { failed, ice }) => {
            w.put("gesture", 6);
            w.flag("failed", failed);
            w.flag("ice", ice);
        }
    }
    w.f32("pan", event.pan);
    w.f32("heat", event.heat);
    w.f32("hue", event.hue);
    w.f32("gain", event.gain);
    w.put("tone", event.tone as u64);
    w.flag("bed", event.bed);
    w.flag("shifted", event.shifted);
}

/// A [`SoundKind`]'s words: its variant and payload.
#[inline]
fn sound_kind_words(w: &mut impl Words, kind: SoundKind) {
    let (kind, a, b, c) = match kind {
        SoundKind::Typed => (0, 0, 0, false),
        SoundKind::Backspace => (1, 0, 0, false),
        SoundKind::Navigation => (2, 0, 0, false),
        SoundKind::Kill => (3, 0, 0, false),
        SoundKind::KillWord => (4, 0, 0, false),
        SoundKind::Poof => (5, 0, 0, false),
        SoundKind::Jump => (6, 0, 0, false),
        SoundKind::Glide { dir } => (7, dir, 0, false),
        SoundKind::Sweep { dir } => (8, dir, 0, false),
        SoundKind::Land => (9, 0, 0, false),
        SoundKind::Space => (10, 0, 0, false),
        SoundKind::Shift => (11, 0, 0, false),
        SoundKind::MeteorArm { dir } => (12, dir, 0, false),
        SoundKind::Meteor { dir, cells, armed } => (13, dir, cells, armed),
        SoundKind::Enter { cells } => (14, 0, cells, false),
        SoundKind::Strum => (15, 0, 0, false),
        SoundKind::Stardust { twinkle_hz } => (16, 0, u16::from(twinkle_hz), false),
    };
    w.put("kind", kind);
    w.put("kind.dir", a as u8 as u64);
    w.put("kind.n", u64::from(b));
    w.flag("kind.armed", c);
}

/// The [`aterm_core::render::RenderInput`] streams a light arm may touch, as bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Streams(u32);

impl Streams {
    /// No stream.
    pub const NONE: Self = Self(0);
    /// `glow_under`.
    pub const GLOW_UNDER: Self = Self(1 << 0);
    /// `glow_under_ramps`.
    pub const GLOW_UNDER_RAMPS: Self = Self(1 << 1);
    /// `cursor_glow_add`.
    pub const CURSOR_GLOW_ADD: Self = Self(1 << 2);
    /// `cursor_glow_add_ramps`.
    pub const CURSOR_GLOW_ADD_RAMPS: Self = Self(1 << 3);
    /// `glow_halo`.
    pub const GLOW_HALO: Self = Self(1 << 4);
    /// `fx_layers`.
    pub const FX_LAYERS: Self = Self(1 << 5);
    /// `word_decorations`, the deco sprites.
    pub const WORD_DECORATIONS: Self = Self(1 << 6);
    /// `cursor_fill_override`.
    pub const CURSOR_FILL_OVERRIDE: Self = Self(1 << 7);
    /// `cursor_ink`, which Phase 3 adds (§15.4).
    pub const CURSOR_INK: Self = Self(1 << 8);
    /// `ink`, the words' cell ink.
    pub const INK: Self = Self(1 << 9);
    /// `nova_add`.
    pub const NOVA_ADD: Self = Self(1 << 10);
    /// `char_fg`, the charred glyphs' ink.
    pub const CHAR_FG: Self = Self(1 << 11);
    /// `fire_halo`.
    pub const FIRE_HALO: Self = Self(1 << 12);
    /// `rain_add`.
    pub const RAIN_ADD: Self = Self(1 << 13);
    /// `cursor_trail`.
    pub const CURSOR_TRAIL: Self = Self(1 << 14);
    /// `fire_patch`.
    pub const FIRE_PATCH: Self = Self(1 << 15);
    /// `bloom_gate`.
    pub const BLOOM_GATE: Self = Self(1 << 16);
    /// `cursor_effect_style_override`.
    pub const CURSOR_EFFECT_STYLE_OVERRIDE: Self = Self(1 << 17);
    /// `cursor_trail_color`.
    pub const CURSOR_TRAIL_COLOR: Self = Self(1 << 18);

    /// Every stream with its `RenderInput` field's name, in bit order.
    pub const NAMED: [(Self, &'static str); 19] = [
        (Self::GLOW_UNDER, "glow_under"),
        (Self::GLOW_UNDER_RAMPS, "glow_under_ramps"),
        (Self::CURSOR_GLOW_ADD, "cursor_glow_add"),
        (Self::CURSOR_GLOW_ADD_RAMPS, "cursor_glow_add_ramps"),
        (Self::GLOW_HALO, "glow_halo"),
        (Self::FX_LAYERS, "fx_layers"),
        (Self::WORD_DECORATIONS, "word_decorations"),
        (Self::CURSOR_FILL_OVERRIDE, "cursor_fill_override"),
        (Self::CURSOR_INK, "cursor_ink"),
        (Self::INK, "ink"),
        (Self::NOVA_ADD, "nova_add"),
        (Self::CHAR_FG, "char_fg"),
        (Self::FIRE_HALO, "fire_halo"),
        (Self::RAIN_ADD, "rain_add"),
        (Self::CURSOR_TRAIL, "cursor_trail"),
        (Self::FIRE_PATCH, "fire_patch"),
        (Self::BLOOM_GATE, "bloom_gate"),
        (
            Self::CURSOR_EFFECT_STYLE_OVERRIDE,
            "cursor_effect_style_override",
        ),
        (Self::CURSOR_TRAIL_COLOR, "cursor_trail_color"),
    ];

    /// Both sets.
    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every stream of `other` is in this set.
    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether the set is empty.
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The set as bits.
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The names of the set's streams, in bit order.
    #[inline]
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::NAMED
            .into_iter()
            .filter(move |(s, _)| self.contains(*s))
            .map(|(_, name)| name)
    }
}

/// Which light arms drew this frame and which streams each touched
/// ([`crate::pipeline::EffectsPipeline::light_arms_drawn`], §15.0.2(b)): on a frame with no arm,
/// every stream must equal main's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArmsDrawn {
    arms: [Option<(&'static str, Streams)>; Self::MAX],
}

impl ArmsDrawn {
    /// The most arms one frame records.
    pub const MAX: usize = 16;

    /// No arm drew.
    pub const NONE: Self = Self {
        arms: [None; Self::MAX],
    };

    /// Record that `arm` drew into `streams`, merged with what it drew before this frame.
    #[inline]
    pub fn note(&mut self, arm: &'static str, streams: Streams) {
        let mut i = 0;
        while i < self.arms.len() {
            match &mut self.arms[i] {
                Some((name, touched)) if *name == arm => {
                    *touched = touched.union(streams);
                    return;
                }
                slot @ None => {
                    *slot = Some((arm, streams));
                    return;
                }
                Some(_) => {}
            }
            i += 1;
        }
        debug_assert!(false, "more than {} light arms on one frame", Self::MAX);
    }

    /// Whether no arm drew.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.arms[0].is_none()
    }

    /// Each arm that drew with the streams it touched, in the order they first drew.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, Streams)> + '_ {
        self.arms.iter().map_while(|a| *a)
    }

    /// Every stream some arm touched.
    #[inline]
    #[must_use]
    pub const fn streams(&self) -> Streams {
        let mut out = Streams::NONE;
        let mut i = 0;
        while i < self.arms.len() {
            match self.arms[i] {
                Some((_, t)) => out = out.union(t),
                None => break,
            }
            i += 1;
        }
        out
    }
}

#[cfg(test)]
#[path = "state_words_tests.rs"]
mod tests;
