// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Aos M4 corpus, generator v1.

use aterm_aos::Digest;
use aterm_aos::wire::Enc;

use super::lane::{Exo, Presentation, Scenario, ScenarioFault, s0, s0_geom};
use crate::cursor_momentum::{MOMENTUM_GLOW_RADIUS_CELLS, MomentumGlowConfig};

/// The generator's version, which names its tables, draw order and strata.
pub const GENERATOR_VERSION: u32 = 1;

/// The domain of [`Corpus::digest`].
pub const CORPUS_DOMAIN: &[u8] = b"aterm-aos-m4-corpus-v1\0";

/// The domain of the golden trace's digest.
pub const GOLDEN_TRACE_DOMAIN: &[u8] = b"aterm-aos-m4-golden-trace-v1\0";

/// The target the golden trace was recorded on (its profile names it).
pub const GOLDEN_TRACE_TARGET: &str = "aarch64-macos";

/// The golden trace: `tests/fixtures/aos-momentum/aarch64-macos.trace`, compiled in.
pub static GOLDEN_TRACE: &[u8] =
    include_bytes!("../../tests/fixtures/aos-momentum/aarch64-macos.trace");

/// The generated training seeds.
pub const TRAIN_SEEDS: [u64; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

/// The generated held-out seeds.
pub const HELD_OUT_SEEDS: [u64; 8] = [101, 102, 103, 104, 105, 106, 107, 108];

/// The training items: the golden trace and one per training seed.
pub const TRAIN_ITEMS: usize = 1 + TRAIN_SEEDS.len();

/// The held-out items: `S0` and one per held-out seed.
pub const HELD_OUT_ITEMS: usize = 1 + HELD_OUT_SEEDS.len();

/// The frame intervals a scenario runs at (ns): 120, 60 and 30 Hz.
pub const FRAME_INTERVALS_NS: [u64; 3] = [8_333_333, 16_666_667, 33_333_333];

/// The cool-down τ of a scenario's keys and ticks (s).
pub const TAUS_S: [f32; 3] = [0.3, 1.2, 4.0];

const TAUS_NS: [u64; 3] = [300_000_000, 1_200_000_000, 4_000_000_000];

/// How long after a burst's last key a park starts the next burst, beside [`TAUS_S`].
pub const PARK_AFTER_NS: [u64; 3] = [1_800_000_000, 5_350_000_000, 16_300_000_000];

/// The intensities a scenario's ticks present.
pub const INTENSITIES: [f32; 3] = [1.0, 0.6, 0.3];

/// A burst's cool-down: a park, or a cut this many ns after its last key.
pub const COOLS: [Option<u64>; 4] = [
    None,
    Some(150_000_000),
    Some(400_000_000),
    Some(1_200_000_000),
];

/// The end comes this long after a scenario's last input.
pub const END_AFTER_NS: u64 = 20_000_000_000;

const ROWS: u16 = 24;
const COLS: u16 = 100;

const BASE: u32 = 0x0050_FA7B;

const RADII: [f32; 3] = [MOMENTUM_GLOW_RADIUS_CELLS, 1.2, 2.0];
const BASES: [u32; 3] = [BASE, 0x00FF_4FA0, 0x0040_80FF];

const HUES: [u32; 6] = [
    0x0050_FA7B,
    0x00FF_4FA0,
    0x0040_80FF,
    0x00FF_B000,
    0x00A0_60FF,
    0x0000_C8C8,
];
const HUE_WINDOWS_NS: [u64; 2] = [500_000_000, 1_500_000_000];

const RAMP_WINDOWS_NS: [u64; 2] = [300_000_000, 1_000_000_000];

const RESET_TENTHS_OF_TAU: [u64; 3] = [5, 10, 15];

/// Deterministic `splitmix64` generator behind the corpus draws and momentum test streams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitMix(u64);

impl SplitMix {
    /// The generator seeded with `seed`.
    #[inline]
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        SplitMix(seed)
    }

    /// Its state, from which [`Self::new`] continues the same sequence.
    #[inline]
    #[must_use]
    pub const fn state(&self) -> u64 {
        self.0
    }

    /// The next 64-bit draw.
    #[inline]
    pub const fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A draw in `0..n` (the draw mod `n`); 0 when `n` is 0.
    #[inline]
    pub const fn below(&mut self, n: u64) -> u64 {
        match n {
            0 => 0,
            n => self.next_u64() % n,
        }
    }

    /// An element of `xs` drawn by [`Self::below`] of its length; `None` when it is empty.
    #[inline]
    pub const fn pick<T: Copy>(&mut self, xs: &[T]) -> Option<T> {
        if xs.is_empty() {
            return None;
        }
        let i = self.below(xs.len() as u64) as usize;
        Some(xs[i])
    }
}

/// An element of a non-empty table.
#[inline]
const fn pick<T: Copy, const N: usize>(r: &mut SplitMix, xs: &[T; N]) -> T {
    const { assert!(N > 0) };
    let i = r.below(N as u64) as usize;
    xs[if i < N { i } else { N - 1 }]
}

/// What a generated scenario exercises beyond typing and cooling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stratum {
    /// Typing and cooling only.
    Plain,
    /// The caret toggles block / bar at every fourth key.
    CaretAtKeys,
    /// The radius and the colour step at every third key.
    ConfigAtKeys,
    /// On a light page, the hue moves at every frame boundary after the last key.
    MovingHue,
    /// Intensity falls linearly to half over a window after the last key, stepped each frame.
    IntensityRamp,
    /// A reset in the last cool-down.
    ResetInCoolDown,
}

impl Stratum {
    /// Every stratum, in the order [`stratum_of`] cycles through.
    pub const ALL: [Stratum; 6] = [
        Stratum::Plain,
        Stratum::CaretAtKeys,
        Stratum::ConfigAtKeys,
        Stratum::MovingHue,
        Stratum::IntensityRamp,
        Stratum::ResetInCoolDown,
    ];

    /// Its name in a report.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Stratum::Plain => "plain",
            Stratum::CaretAtKeys => "caret-at-keys",
            Stratum::ConfigAtKeys => "config-at-keys",
            Stratum::MovingHue => "moving-hue",
            Stratum::IntensityRamp => "intensity-ramp",
            Stratum::ResetInCoolDown => "reset-in-cool-down",
        }
    }
}

/// The stratum seed `seed` generates: `Stratum::ALL[seed mod 6]`.
#[inline]
#[must_use]
pub const fn stratum_of(seed: u64) -> Stratum {
    match seed % 6 {
        0 => Stratum::Plain,
        1 => Stratum::CaretAtKeys,
        2 => Stratum::ConfigAtKeys,
        3 => Stratum::MovingHue,
        4 => Stratum::IntensityRamp,
        _ => Stratum::ResetInCoolDown,
    }
}

/// What a generated tick presents, before its cell.
#[derive(Clone, Copy)]
struct Look {
    dark: bool,
    block: bool,
    intensity: f32,
    tau: f32,
    radius: f32,
    base: u32,
    hue: u32,
}

impl Look {
    #[inline]
    const fn at(&self, row: u16, col: u16) -> Presentation {
        let cfg = MomentumGlowConfig {
            light: crate::cursor_glow::Light::OFF,
            enabled: true,
            intensity: self.intensity,
            tau_s: self.tau,
            radius_cells: self.radius,
            base: self.base,
            dark_theme: self.dark,
            hue: self.hue,
            theme_fg: if self.dark { 0x00C8_C8D2 } else { 0x0024_292F },
            theme_bg: if self.dark { 0x0010_1016 } else { 0x00FF_FFFF },
            block: self.block,
            glyph_span: Some((0, col as i32)),
            light_vivid: false,
        };
        Presentation::of(Some((row, col)), &s0_geom(), &cfg)
    }
}

#[inline]
const fn advance(row: u16, col: u16) -> (u16, u16) {
    let col = col + 1;
    if col >= COLS - 1 {
        (if row + 1 >= ROWS { 0 } else { row + 1 }, 4)
    } else {
        (row, col)
    }
}

#[inline]
const fn boundary_range(after: u64, window: u64, f: u64) -> (u64, u64) {
    let first = (after / f).saturating_add(1);
    let last = after.saturating_add(window) / f;
    (first, last)
}

/// The scenario generator v1 draws for `seed` from its fixed tables (design §10.3).
/// # Errors
/// A [`ScenarioFault`] if admission refuses what the tables make; none does (`corpus_tests`).
#[inline]
pub fn scenario(seed: u64) -> Result<Scenario, ScenarioFault> {
    let mut r = SplitMix::new(seed);
    let stratum = stratum_of(seed);
    let f = pick(&mut r, &FRAME_INTERVALS_NS);
    let t_ix = r.below(3) as usize;
    let (tau, tau_ns, park_ns) = (TAUS_S[t_ix], TAUS_NS[t_ix], PARK_AFTER_NS[t_ix]);
    let intensity = pick(&mut r, &INTENSITIES);
    let dark = r.below(2) == 0 && stratum != Stratum::MovingHue;
    let block = r.below(2) == 0;
    let bursts = 1 + r.below(4);
    let mut row = r.below(u64::from(ROWS)) as u16;
    let mut col = 4 + r.below(40) as u16;
    let mut t = r.below(1_000_000) * 1_000;
    let mut look = Look {
        dark,
        block,
        intensity,
        tau,
        radius: MOMENTUM_GLOW_RADIUS_CELLS,
        base: BASE,
        hue: BASE,
    };
    let tau_bits = tau.to_bits();
    let start = look.at(row, col);
    let mut inputs = Vec::new();
    let mut last_key = t;
    let mut config_step = 0usize;
    let mut burst = 0;
    while burst < bursts {
        let keys = 4 + r.below(21);
        let rate = 2 + r.below(15);
        let gap = 1_000_000_000 / rate;
        let cool = pick(&mut r, &COOLS);
        let mut k = 0;
        while k < keys {
            let at = t + k * gap;
            inputs.push((at, Exo::Key { tau_bits }));
            (row, col) = advance(row, col);
            match stratum {
                Stratum::CaretAtKeys if k % 4 == 3 => look.block = !look.block,
                Stratum::ConfigAtKeys if k % 3 == 2 => {
                    config_step = (config_step + 1) % RADII.len();
                    look.radius = RADII[config_step];
                    look.base = BASES[config_step];
                    look.hue = BASES[config_step];
                }
                _ => {}
            }
            inputs.push((at, Exo::Present(look.at(row, col))));
            last_key = at;
            k += 1;
        }
        if burst + 1 < bursts {
            t = last_key + cool.unwrap_or(park_ns);
        }
        burst += 1;
    }
    match stratum {
        Stratum::MovingHue => {
            let window = pick(&mut r, &HUE_WINDOWS_NS);
            let (mut k, last) = boundary_range(last_key, window, f);
            let mut i = 0usize;
            while k <= last {
                let at = k.saturating_mul(f);
                look.hue = HUES[(i + 1) % HUES.len()];
                inputs.push((at, Exo::Present(look.at(row, col))));
                i += 1;
                k += 1;
            }
        }
        Stratum::IntensityRamp => {
            let window = pick(&mut r, &RAMP_WINDOWS_NS);
            let (mut k, last) = boundary_range(last_key, window, f);
            while k <= last {
                let at = k.saturating_mul(f);
                let frac = (at - last_key) as f32 / window as f32;
                look.intensity = intensity * (1.0 - 0.5 * frac);
                inputs.push((at, Exo::Present(look.at(row, col))));
                k += 1;
            }
        }
        Stratum::ResetInCoolDown => {
            let tenths = pick(&mut r, &RESET_TENTHS_OF_TAU);
            inputs.push((last_key + tau_ns / 10 * tenths, Exo::Reset));
        }
        Stratum::Plain | Stratum::CaretAtKeys | Stratum::ConfigAtKeys => {}
    }
    let last = match inputs.last() {
        Some(&(at, _)) => at,
        None => 0,
    };
    Scenario::new(f, start, inputs, last + END_AFTER_NS)
}

/// Which split an item is in, as the policy fixes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CorpusSplit {
    /// Read for every candidate.
    Train,
    /// Read once, for the training winner.
    HeldOut,
}

impl CorpusSplit {
    /// Its tag in the digest.
    #[inline]
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            CorpusSplit::Train => 0,
            CorpusSplit::HeldOut => 1,
        }
    }
}

/// What an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// The checked-in golden trace, replayed open loop.
    GoldenTrace,
    /// `S0`, the M3 story frozen.
    M3Story,
    /// A generator v1 scenario.
    Generated { seed: u64, stratum: Stratum },
}

impl ItemKind {
    /// Its tag in the digest.
    #[inline]
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            ItemKind::GoldenTrace => 0,
            ItemKind::M3Story => 1,
            ItemKind::Generated { .. } => 2,
        }
    }

    /// Its seed (0 for the golden trace and `S0`).
    #[inline]
    #[must_use]
    pub const fn seed(self) -> u64 {
        match self {
            ItemKind::Generated { seed, .. } => seed,
            ItemKind::GoldenTrace | ItemKind::M3Story => 0,
        }
    }
}

/// What an item is run as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemBody {
    /// A trace file, replayed open loop.
    Trace(&'static [u8]),
    /// A scenario, run closed loop by the lane.
    Lane(Scenario),
}

/// One corpus item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    id: String,
    kind: ItemKind,
    split: CorpusSplit,
    body: ItemBody,
}

impl Item {
    /// Its stable id.
    #[inline]
    #[must_use]
    pub const fn id(&self) -> &str {
        self.id.as_str()
    }

    /// Its kind.
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> ItemKind {
        self.kind
    }

    /// Its split.
    #[inline]
    #[must_use]
    pub const fn split(&self) -> CorpusSplit {
        self.split
    }

    /// What it is run as.
    #[inline]
    #[must_use]
    pub const fn body(&self) -> &ItemBody {
        &self.body
    }

    /// Its scenario, for a lane item.
    #[inline]
    #[must_use]
    pub const fn scenario(&self) -> Option<&Scenario> {
        match &self.body {
            ItemBody::Lane(s) => Some(s),
            ItemBody::Trace(_) => None,
        }
    }
}

/// The corpus generator v1 makes, with its digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Corpus {
    items: Vec<Item>,
    digest: Digest,
}

/// The golden trace's digest: `Digest::of(GOLDEN_TRACE_DOMAIN, [the file's bytes])`.
#[inline]
#[must_use]
pub fn golden_trace_digest() -> Digest {
    Digest::of(GOLDEN_TRACE_DOMAIN, &[GOLDEN_TRACE])
}

/// One lane item's part of the corpus digest: its kind tag, seed, split and scenario encoding.
#[inline]
#[must_use]
pub fn item_part(kind: ItemKind, split: CorpusSplit, encoding: &[u8]) -> Vec<u8> {
    let mut e = Enc::with_capacity(18 + encoding.len());
    e.u8(kind.tag());
    e.u64(kind.seed());
    e.u8(split.tag());
    e.bytes(encoding);
    e.finish()
}

/// Corpus digest over the generator version, golden-trace digest, and lane items' [`item_part`]s.
#[inline]
#[must_use]
pub fn corpus_digest(golden: &Digest, parts: &[Vec<u8>]) -> Digest {
    let version = GENERATOR_VERSION.to_le_bytes();
    let mut all: Vec<&[u8]> = Vec::with_capacity(parts.len() + 2);
    all.push(&version);
    all.push(&golden.0);
    let mut pi = 0;
    while pi < parts.len() {
        all.push(parts[pi].as_slice());
        pi += 1;
    }
    Digest::of(CORPUS_DOMAIN, &all)
}

impl Corpus {
    /// The corpus: train (the golden trace, then seeds 1–16), then held out.
    /// # Errors
    /// The first scenario admission refuses; none does (`corpus_tests`).
    #[inline]
    pub fn generate() -> Result<Corpus, ScenarioFault> {
        let generated = |seed: u64, split| -> Result<Item, ScenarioFault> {
            Ok(Item {
                id: match split {
                    CorpusSplit::Train => format!("train-{seed:02}"),
                    CorpusSplit::HeldOut => format!("held-{seed}"),
                },
                kind: ItemKind::Generated {
                    seed,
                    stratum: stratum_of(seed),
                },
                split,
                body: ItemBody::Lane(scenario(seed)?),
            })
        };
        let mut items = Vec::with_capacity(TRAIN_ITEMS + HELD_OUT_ITEMS);
        items.push(Item {
            id: "golden".to_owned(),
            kind: ItemKind::GoldenTrace,
            split: CorpusSplit::Train,
            body: ItemBody::Trace(GOLDEN_TRACE),
        });
        let mut si = 0;
        while si < TRAIN_SEEDS.len() {
            items.push(generated(TRAIN_SEEDS[si], CorpusSplit::Train)?);
            si += 1;
        }
        let s0 = s0();
        s0.validate()?;
        items.push(Item {
            id: "s0".to_owned(),
            kind: ItemKind::M3Story,
            split: CorpusSplit::HeldOut,
            body: ItemBody::Lane(s0),
        });
        let mut si = 0;
        while si < HELD_OUT_SEEDS.len() {
            items.push(generated(HELD_OUT_SEEDS[si], CorpusSplit::HeldOut)?);
            si += 1;
        }
        let digest = corpus_digest(&golden_trace_digest(), &Self::parts_of(&items));
        Ok(Corpus { items, digest })
    }

    #[inline]
    fn parts_of(items: &[Item]) -> Vec<Vec<u8>> {
        let mut out = Vec::with_capacity(items.len());
        let mut ii = 0;
        while ii < items.len() {
            let i = &items[ii];
            if let Some(s) = i.scenario() {
                out.push(item_part(i.kind, i.split, &s.encode()));
            }
            ii += 1;
        }
        out
    }

    /// The lane items' digest parts, in corpus order.
    #[inline]
    #[must_use]
    pub fn parts(&self) -> Vec<Vec<u8>> {
        Self::parts_of(&self.items)
    }

    /// Every item, train first.
    #[inline]
    #[must_use]
    pub const fn items(&self) -> &[Item] {
        self.items.as_slice()
    }

    /// The items of `split`, in order.
    #[inline]
    pub fn split(&self, split: CorpusSplit) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(move |i| i.split == split)
    }

    /// Corpus digest over the generator version, golden-trace digest, and lane items' parts.
    #[inline]
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }
}
