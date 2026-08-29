// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail Packs — user-generated **cursor trails as data** (design v1,
//! `docs/trail-packs-design.md`). A Trail Pack is one versioned, bounded,
//! fail-closed TOML file that SELECTS among the engine's EXISTING cursor-glow
//! emitters/channels: the additive [`aterm_render::custom_beam_quads`] beam,
//! the [`aterm_render::RainHalo`] crown, ≤3 particle populations, and a colour
//! ramp. It introduces NO new compositing and NO new output channel — the
//! per-pixel fire field, the rainbow ribbon/exit-swoosh state machine, the cursor
//! cat, and the forge fill stay BUILTIN-ONLY (simulations, not parameters).
//!
//! ## The safety envelope (mirrors the sparkle Toy Pack lane in `spec.rs`)
//!
//! Compilation happens only when a host loads/reloads a pack; the render path
//! sees only the copied [`TrailParams`] value (a ~Copy blob of clamped scalars,
//! fixed arrays, and enums — no heap, so [`crate::cursor_glow::GlowConfig`]
//! stays `Copy`). Like the Toy Pack compiler this fails closed:
//!
//! * a **byte cap** is checked BEFORE the TOML parse ([`MAX_TRAIL_PACK_BYTES`]);
//! * every struct is `#[serde(deny_unknown_fields)]`, so an unknown field /
//!   effect / version is rejected;
//! * every scalar is **clamped into an engine-proven range** at compile and any
//!   out-of-range value ALSO records a diagnostic (nothing is silently dropped);
//! * every stored `f32` is **quantized** to the [`QUANT`] grid, so the emitted
//!   premultiplied-`u8` path and the fingerprint fold stay bit-exact and
//!   CPU==GPU parity holds across reloads;
//! * all independent diagnostics are collected in ONE pass into a
//!   [`TrailPackError`] (the same multi-diagnostic posture as
//!   [`crate::spec::ToyPackError`]) so `--validate-config`, the config notice
//!   lane, and the web FFI all speak one surface.
//!
//! The structural legibility ceiling (the occupied-glyph coverage cap) is NOT a
//! pack field: it is enforced downstream in the interpreter's sole emission
//! funnel (`cursor_glow::CursorGlow::emit_custom`), so a pack cannot opt out.

use std::path::Path;

/// Trail Pack schema version accepted by [`compile_trail_pack_toml`].
pub const TRAIL_PACK_SCHEMA_V1: u32 = 1;

/// Source-size ceiling checked before TOML parsing. Trail Packs are tiny (a
/// handful of tables) versus sparkle Toy Packs, so the cap is far smaller.
pub const MAX_TRAIL_PACK_BYTES: usize = 64 * 1024;

/// Maximum beam bloom layers — equal to the widest built-in stack
/// (`aterm_render::MAX_CUSTOM_BEAM_LAYERS` == the LASER 6-layer stack).
pub const MAX_TRAIL_LAYERS: usize = 6;
/// Maximum live particle populations in one pack.
pub const MAX_TRAIL_POPS: usize = 3;
/// Maximum colour-ramp stops.
pub const MAX_RAMP_STOPS: usize = 8;
/// Maximum banded-ramp colours.
pub const MAX_RAMP_BANDS: usize = 8;

/// The quantization grid every stored `f32` is rounded to (1/1024). Keeping all
/// scalars on this grid makes the emitted `u8` coverage/colour path and the
/// fingerprint fold reproducible across reloads and byte-identical CPU↔GPU.
pub const QUANT: f32 = 1024.0;

// Engine-proven clamp bounds (design §"Validation & safety envelope").
const MIN_WINDOW_MS: u16 = 30;
const MAX_WINDOW_MS: u16 = 2000;
const MAX_LAYER_THICKNESS: f32 = 16.0; // LASER_LAYERS max
const MAX_VELOCITY_CELLS: f32 = 4.0; // ≤4 cells/s
const MAX_PARTICLE_LIFE: f32 = 2.0; // ≤2 s
const MAX_TYPING_BURST: u8 = 44; // the fire ember-column maximum
const MAX_JUMP_BURST: u8 = 44;

const MAX_PACK_ID_BYTES: usize = 64;
const MAX_PACK_NAME_BYTES: usize = 80;
const MAX_PACK_DESCRIPTION_BYTES: usize = 512;
const MAX_PACK_AUTHORS: usize = 8;
const MAX_AUTHOR_BYTES: usize = 80;
const MAX_LICENSE_BYTES: usize = 64;

/// Fixed white mix target for every custom beam layer — the hot core. Packs
/// supply the ramp SHAPE (base hue per sample); the layer stack only mixes
/// toward white, exactly as the single-tone built-in stacks do.
pub const CUSTOM_MIX_WHITE: u32 = 0x00FF_FFFF;

/// One resolved Trail Pack, ready for the engine. Fully `Copy` (fixed arrays /
/// enums / scalars — no heap), so it rides the `Copy` `GlowConfig` inline. NOT
/// `Eq` — it carries `f32` fields (matching `GlowConfig`, which is likewise
/// `PartialEq` but never `Eq`). Size ≈ 0.5 KiB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrailParams {
    /// Compile-time fingerprint of `(id, source bytes)` — the style-switch /
    /// pack-swap detector (two packs both resolve to `GlowStyle::Custom`, so
    /// the engine compares this to drop foreign in-flight light).
    pub pack_fp: u32,
    pub beam: BeamParams,
    pub ramp: RampParams,
    pub heat: HeatParams,
    pub crown: CrownParams,
    pub ring: RingParams,
    pub channels: Channels,
    pub particles: [ParticlePop; MAX_TRAIL_POPS],
    /// Live populations, `0..=MAX_TRAIL_POPS`.
    pub particle_count: u8,
    pub theme: ThemeArm,
}

/// The additive beam channel (`aterm_render::custom_beam_quads`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamParams {
    pub enabled: bool,
    pub layers: [BeamLayer; MAX_TRAIL_LAYERS],
    /// Active layers, `0..=MAX_TRAIL_LAYERS`.
    pub layer_count: u8,
    /// Core thickness base as a fraction of cell height.
    pub cell_frac: f32,
    /// Core thickness gain on the shared typing heat (`core += heat_gain·heat`).
    pub heat_gain: f32,
    pub envelope: Envelope,
    pub scint: Option<Scint>,
    /// Birth-coverage ramp base (fraction of full punch).
    pub cov_base: f32,
    /// Birth-coverage ramp slope (× path position).
    pub cov_slope: f32,
}

/// One bloom layer — maps 1:1 to a `layered_beam_quads` tuple
/// `(thickness×, coverage×, mix target, mix base, mix ×pos, stride)`; the mix
/// target is fixed to [`CUSTOM_MIX_WHITE`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamLayer {
    pub thickness_mul: f32,
    pub coverage_mul: f32,
    pub white_base: f32,
    pub white_pos_mul: f32,
    pub stride: u8,
}

impl BeamLayer {
    const ZERO: BeamLayer = BeamLayer {
        thickness_mul: 0.0,
        coverage_mul: 0.0,
        white_base: 0.0,
        white_pos_mul: 0.0,
        stride: 1,
    };

    /// The `layered_beam_quads` tuple for this layer (white mix target fixed).
    #[must_use]
    pub fn tuple(&self) -> (f32, f32, u32, f32, f32, usize) {
        (
            self.thickness_mul,
            self.coverage_mul,
            CUSTOM_MIX_WHITE,
            self.white_base,
            self.white_pos_mul,
            self.stride as usize,
        )
    }
}

/// The beam's time-fade envelope over a spark's life fraction `frac` (0 fresh,
/// 1 dead).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Envelope {
    /// `1 - frac` (the classic comet fade).
    Linear,
    /// Full power for `hold_frac`, then a cosine fade to zero (the typing hold).
    HoldCosine { hold_frac: f32 },
    /// Full power for `hold_frac`, then a burn front of width `front_width`
    /// sweeping tail→head (the laser power-down).
    BurnOut { hold_frac: f32, front_width: f32 },
}

/// Optional beam scintillation — a travelling `±amp` energy ripple at `freq`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scint {
    pub amp: f32,
    pub freq: f32,
}

/// The colour ramp — resolved to a per-sample base hue by the interpreter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RampParams {
    /// The single `GlowConfig::color` hue.
    Mono,
    /// Interpolated `(t, rgb)` stops with an optional per-move hue rotation.
    Stops {
        stops: [(f32, u32); MAX_RAMP_STOPS],
        n: u8,
        hue_step: f32,
    },
    /// An HSV sweep of `span_deg` at `sat`/`val`, rolling `hue_step` per move.
    HsvSweep {
        span_deg: f32,
        sat: f32,
        val: f32,
        hue_step: f32,
    },
    /// Fixed stacked bands (the rainbow-stripe read, generalized).
    Bands { bands: [u32; MAX_RAMP_BANDS], n: u8 },
}

/// Heat-response overrides. `None` overrides keep the engine's shared defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeatParams {
    pub gain: Option<f32>,
    pub tau: Option<f32>,
    pub bright_floor: f32,
    pub bright_slope: f32,
    pub life_base_mul: f32,
    pub life_a: f32,
    pub life_b: f32,
    pub chain_keys: f32,
    pub chain_gap_max: f32,
    pub chain_life_max: f32,
}

/// The RainHalo crown around the cursor head.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrownParams {
    pub enabled: bool,
    pub radius_cells: f32,
    pub peak_cov: f32,
    pub typing_window_ms: u16,
    pub jump_window_ms: u16,
}

/// The landing-ring "ping" on a jump.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingParams {
    pub enabled: bool,
    pub life_ms: u16,
}

/// Which existing output streams the pack writes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Channels {
    pub glow_add: bool,
    pub halo: HaloChannel,
    pub bed: bool,
}

/// The RainHalo compositing mode a pack's crown/particle veils use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaloChannel {
    Off,
    Add,
    Over,
}

/// One ballistic particle population.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParticlePop {
    pub spawn_weight: f32,
    pub vx: (f32, f32),
    pub vy: (f32, f32),
    pub gravity: f32,
    pub life: (f32, f32),
    pub size: f32,
    pub typing_burst_max: u8,
    pub jump_burst_max: u8,
}

impl ParticlePop {
    const ZERO: ParticlePop = ParticlePop {
        spawn_weight: 0.0,
        vx: (0.0, 0.0),
        vy: (0.0, 0.0),
        gravity: 0.0,
        life: (0.0, 0.0),
        size: 0.18,
        typing_burst_max: 0,
        jump_burst_max: 0,
    };
}

/// The optional light-theme arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeArm {
    /// The engine's automatic adaptation.
    Auto,
    /// Source-over veils on light themes.
    OverVeil,
    /// Darkened tints on light themes.
    DarkenTints,
}

/// The default comet-like beam stack a pack inherits when it supplies no
/// `[[beam.layers]]` (mirrors `aterm_render`'s `COMET_LAYERS`).
const DEFAULT_BEAM_LAYERS: [BeamLayer; 4] = [
    BeamLayer {
        thickness_mul: 5.5,
        coverage_mul: 0.16,
        white_base: 0.0,
        white_pos_mul: 0.0,
        stride: 3,
    },
    BeamLayer {
        thickness_mul: 3.0,
        coverage_mul: 0.30,
        white_base: 0.0,
        white_pos_mul: 0.0,
        stride: 2,
    },
    BeamLayer {
        thickness_mul: 1.8,
        coverage_mul: 0.55,
        white_base: 0.12,
        white_pos_mul: 0.0,
        stride: 2,
    },
    BeamLayer {
        thickness_mul: 1.0,
        coverage_mul: 1.00,
        white_base: 0.25,
        white_pos_mul: 0.45,
        stride: 1,
    },
];

impl TrailParams {
    /// The engine's default resolved params (a plain comet-like beam, mono hue,
    /// crown on, no particles). Every compile starts from this and applies the
    /// pack's overrides, so an omitted section keeps the proven default.
    #[must_use]
    pub fn defaults() -> Self {
        let mut layers = [BeamLayer::ZERO; MAX_TRAIL_LAYERS];
        layers[..DEFAULT_BEAM_LAYERS.len()].copy_from_slice(&DEFAULT_BEAM_LAYERS);
        TrailParams {
            pack_fp: 0,
            beam: BeamParams {
                enabled: true,
                layers,
                layer_count: DEFAULT_BEAM_LAYERS.len() as u8,
                cell_frac: 0.13,
                heat_gain: 0.0,
                envelope: Envelope::Linear,
                scint: None,
                cov_base: 0.16,
                cov_slope: 0.69,
            },
            ramp: RampParams::Mono,
            heat: HeatParams {
                gain: None,
                tau: None,
                bright_floor: 0.35,
                bright_slope: 0.85,
                life_base_mul: 0.38,
                life_a: 1.3,
                life_b: 0.0,
                chain_keys: 1.0,
                chain_gap_max: 0.14,
                chain_life_max: 0.6,
            },
            crown: CrownParams {
                enabled: true,
                radius_cells: 0.9,
                peak_cov: 0.20,
                typing_window_ms: 350,
                jump_window_ms: 200,
            },
            ring: RingParams {
                enabled: false,
                life_ms: 320,
            },
            channels: Channels {
                glow_add: true,
                halo: HaloChannel::Add,
                bed: false,
            },
            particles: [ParticlePop::ZERO; MAX_TRAIL_POPS],
            particle_count: 0,
            theme: ThemeArm::Auto,
        }
    }

    /// The live beam layers as `layered_beam_quads` tuples.
    #[must_use]
    pub fn beam_layer_tuples(&self) -> [(f32, f32, u32, f32, f32, usize); MAX_TRAIL_LAYERS] {
        let mut out =
            [(0.0f32, 0.0f32, CUSTOM_MIX_WHITE, 0.0f32, 0.0f32, 1usize); MAX_TRAIL_LAYERS];
        for (o, l) in out.iter_mut().zip(self.beam.layers.iter()) {
            *o = l.tuple();
        }
        out
    }
}

/// Contributor-facing metadata retained with a compiled pack. `id` is the
/// picker key (`cursor_trail_style = "pack:<id>"`), not display text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrailPackMetadata {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub authors: Vec<String>,
    pub license: String,
    pub description: Option<String>,
}

/// A validated pack lowered to its engine artifact ([`TrailParams`]) plus
/// metadata. Hosts retain only these values; no TOML decision enters the frame.
#[derive(Clone, Debug)]
pub struct CompiledTrailPack {
    metadata: TrailPackMetadata,
    params: TrailParams,
}

impl CompiledTrailPack {
    #[must_use]
    pub fn metadata(&self) -> &TrailPackMetadata {
        &self.metadata
    }

    #[must_use]
    pub fn params(&self) -> &TrailParams {
        &self.params
    }

    /// Consume into `(metadata, params)`.
    #[must_use]
    pub fn into_parts(self) -> (TrailPackMetadata, TrailParams) {
        (self.metadata, self.params)
    }
}

/// One or more actionable Trail Pack diagnostics — the same multi-diagnostic
/// posture as [`crate::spec::ToyPackError`], so every surface (validate-config,
/// config notices, web FFI) speaks one shape and an artist fixes all fields in
/// one pass rather than one-at-a-time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrailPackError {
    diagnostics: Vec<String>,
}

impl TrailPackError {
    fn one(message: impl Into<String>) -> Self {
        Self {
            diagnostics: vec![message.into()],
        }
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
}

impl std::fmt::Display for TrailPackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.diagnostics.len() == 1 {
            return f.write_str(&self.diagnostics[0]);
        }
        writeln!(f, "trail pack has {} errors:", self.diagnostics.len())?;
        for diagnostic in &self.diagnostics {
            writeln!(f, "- {diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for TrailPackError {}

/// Read one Trail Pack manifest without permitting special files or unbounded
/// allocation (the FIFO-safe bounded read, mirroring
/// [`crate::spec::read_toy_pack_file`]). Hosts call this before
/// [`compile_trail_pack_toml`].
pub fn read_trail_pack_file(path: &Path) -> std::io::Result<String> {
    crate::file_feed::read_bounded_regular_utf8(path, MAX_TRAIL_PACK_BYTES)
}

// ---------------------------------------------------------------------------
// Raw serde shape — every table `deny_unknown_fields` (the fail-closed lever).
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTrailPackDoc {
    /// The schema/format version (`pack = 1`).
    pack: u32,
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    authors: Option<Vec<String>>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    beam: Option<RawBeam>,
    #[serde(default)]
    ramp: Option<RawRamp>,
    #[serde(default)]
    heat: Option<RawHeat>,
    #[serde(default)]
    crown: Option<RawCrown>,
    #[serde(default)]
    ring: Option<RawRing>,
    #[serde(default)]
    channels: Option<RawChannels>,
    #[serde(default)]
    theme: Option<RawTheme>,
    #[serde(default)]
    particles: Vec<RawPop>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBeam {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    layers: Option<Vec<RawLayer>>,
    #[serde(default)]
    envelope: Option<RawEnvelope>,
    #[serde(default)]
    scintillation: Option<RawScint>,
    #[serde(default)]
    cell_frac: Option<f32>,
    #[serde(default)]
    heat_gain: Option<f32>,
    #[serde(default)]
    cov_base: Option<f32>,
    #[serde(default)]
    cov_slope: Option<f32>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLayer {
    thickness_mul: f32,
    coverage_mul: f32,
    #[serde(default)]
    white_base: Option<f32>,
    #[serde(default)]
    white_pos_mul: Option<f32>,
    #[serde(default)]
    stride: Option<u32>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnvelope {
    kind: String,
    #[serde(default)]
    hold_frac: Option<f32>,
    #[serde(default)]
    front_width: Option<f32>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScint {
    amp: f32,
    freq: f32,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRamp {
    kind: String,
    #[serde(default)]
    stops: Option<Vec<RawStop>>,
    #[serde(default)]
    bands: Option<Vec<String>>,
    #[serde(default)]
    span_deg: Option<f32>,
    #[serde(default)]
    sat: Option<f32>,
    #[serde(default)]
    val: Option<f32>,
    #[serde(default)]
    hue_step_per_move: Option<f32>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStop {
    t: f32,
    color: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHeat {
    #[serde(default)]
    gain: Option<f32>,
    #[serde(default)]
    tau: Option<f32>,
    #[serde(default)]
    bright_floor: Option<f32>,
    #[serde(default)]
    bright_slope: Option<f32>,
    #[serde(default)]
    life_base_mul: Option<f32>,
    #[serde(default)]
    life_a: Option<f32>,
    #[serde(default)]
    life_b: Option<f32>,
    #[serde(default)]
    chain_keys: Option<f32>,
    #[serde(default)]
    chain_gap_max: Option<f32>,
    #[serde(default)]
    chain_life_max: Option<f32>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCrown {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    radius_cells: Option<f32>,
    #[serde(default)]
    peak_cov: Option<f32>,
    #[serde(default)]
    typing_window_ms: Option<u32>,
    #[serde(default)]
    jump_window_ms: Option<u32>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRing {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    life_ms: Option<u32>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChannels {
    #[serde(default)]
    glow_add: Option<bool>,
    #[serde(default)]
    halo: Option<String>,
    #[serde(default)]
    bed: Option<bool>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTheme {
    kind: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPop {
    #[serde(default)]
    spawn_weight: Option<f32>,
    // `[min, max]` pairs are deserialized into a `Vec` (NOT a fixed `[f32; 2]`):
    // serde's fixed-array impl silently TRUNCATES a too-long TOML array to the
    // first N, so `vy = [1.0, 2.0, 3.0]` would sail through as `[1.0, 2.0]`. A
    // `Vec` preserves the real length so `bounded_pair` can fail closed on any
    // arity other than 2 (the fail-closed lever the rest of the schema relies on).
    #[serde(default)]
    vx: Option<Vec<f32>>,
    #[serde(default)]
    vy: Option<Vec<f32>>,
    #[serde(default)]
    gravity: Option<f32>,
    #[serde(default)]
    life: Option<Vec<f32>>,
    #[serde(default)]
    size: Option<f32>,
    #[serde(default)]
    typing_burst_max: Option<u32>,
    #[serde(default)]
    jump_burst_max: Option<u32>,
}

/// Parse and compile one Trail Pack manifest. Cold-path only: the source is
/// capped BEFORE parsing, every scalar is clamped into an engine-proven range
/// (out-of-range values also recording a diagnostic), every stored `f32` is
/// quantized, and all independent errors are collected in one pass.
pub fn compile_trail_pack_toml(source: &str) -> Result<CompiledTrailPack, TrailPackError> {
    if source.len() > MAX_TRAIL_PACK_BYTES {
        return Err(TrailPackError::one(format!(
            "source is {} bytes; maximum is {MAX_TRAIL_PACK_BYTES}",
            source.len()
        )));
    }
    let raw: RawTrailPackDoc = aterm_toml::from_str(source)
        .map_err(|e| TrailPackError::one(format!("TOML schema error: {e}")))?;
    let mut diag = Vec::new();

    if raw.pack != TRAIL_PACK_SCHEMA_V1 {
        diag.push(format!(
            "pack: unsupported schema version {}; expected {TRAIL_PACK_SCHEMA_V1}",
            raw.pack
        ));
    }
    if !valid_id(&raw.id, MAX_PACK_ID_BYTES) {
        diag.push(format!(
            "id: expected a lowercase ASCII id (letters/digits plus ._-), \
             1..={MAX_PACK_ID_BYTES} bytes"
        ));
    }
    validate_metadata(&raw, &mut diag);

    let mut params = TrailParams::defaults();
    if let Some(beam) = raw.beam.as_ref() {
        params.beam = compile_beam(beam, &mut diag);
    }
    if let Some(ramp) = raw.ramp.as_ref() {
        params.ramp = compile_ramp(ramp, &mut diag);
    }
    if let Some(heat) = raw.heat.as_ref() {
        params.heat = compile_heat(heat, &mut diag);
    }
    if let Some(crown) = raw.crown.as_ref() {
        params.crown = compile_crown(crown, &mut diag);
    }
    if let Some(ring) = raw.ring.as_ref() {
        params.ring = compile_ring(ring, &mut diag);
    }
    if let Some(ch) = raw.channels.as_ref() {
        params.channels = compile_channels(ch, &mut diag);
    }
    if let Some(theme) = raw.theme.as_ref() {
        params.theme = compile_theme(theme, &mut diag);
    }

    if raw.particles.len() > MAX_TRAIL_POPS {
        diag.push(format!(
            "particles: {} populations exceed the maximum {MAX_TRAIL_POPS}",
            raw.particles.len()
        ));
    }
    let mut pops = [ParticlePop::ZERO; MAX_TRAIL_POPS];
    let mut count = 0u8;
    for (i, raw_pop) in raw.particles.iter().take(MAX_TRAIL_POPS).enumerate() {
        pops[i] = compile_pop(raw_pop, i, &mut diag);
        count += 1;
    }
    params.particles = pops;
    params.particle_count = count;

    params.pack_fp = pack_fingerprint(&raw.id, source);

    if !diag.is_empty() {
        return Err(TrailPackError { diagnostics: diag });
    }
    Ok(CompiledTrailPack {
        metadata: TrailPackMetadata {
            id: raw.id,
            name: raw.name.unwrap_or_else(|| "Trail Pack".to_string()),
            version: 1,
            authors: raw.authors.unwrap_or_default(),
            license: raw.license.unwrap_or_else(|| "Unlicensed".to_string()),
            description: raw.description,
        },
        params,
    })
}

fn validate_metadata(raw: &RawTrailPackDoc, diag: &mut Vec<String>) {
    if let Some(name) = raw.name.as_deref() {
        validate_text("name", name, MAX_PACK_NAME_BYTES, diag);
    }
    if let Some(authors) = raw.authors.as_ref() {
        if authors.len() > MAX_PACK_AUTHORS {
            diag.push(format!(
                "authors: {} credits exceed the maximum {MAX_PACK_AUTHORS}",
                authors.len()
            ));
        }
        for (i, a) in authors.iter().take(MAX_PACK_AUTHORS).enumerate() {
            validate_text(&format!("authors[{i}]"), a, MAX_AUTHOR_BYTES, diag);
        }
    }
    if let Some(license) = raw.license.as_deref() {
        validate_text("license", license, MAX_LICENSE_BYTES, diag);
    }
    if let Some(description) = raw.description.as_deref() {
        validate_text("description", description, MAX_PACK_DESCRIPTION_BYTES, diag);
    }
}

fn compile_beam(raw: &RawBeam, diag: &mut Vec<String>) -> BeamParams {
    let mut b = TrailParams::defaults().beam;
    b.enabled = raw.enabled.unwrap_or(true);
    b.cell_frac = bounded_f32(raw.cell_frac, b.cell_frac, 0.0, 1.0, "beam.cell_frac", diag);
    b.heat_gain = bounded_f32(raw.heat_gain, b.heat_gain, 0.0, 1.0, "beam.heat_gain", diag);
    b.cov_base = bounded_f32(raw.cov_base, b.cov_base, 0.0, 1.0, "beam.cov_base", diag);
    b.cov_slope = bounded_f32(raw.cov_slope, b.cov_slope, 0.0, 1.0, "beam.cov_slope", diag);

    if let Some(layers) = raw.layers.as_ref() {
        if layers.len() > MAX_TRAIL_LAYERS {
            diag.push(format!(
                "beam.layers: {} layers exceed the maximum {MAX_TRAIL_LAYERS}",
                layers.len()
            ));
        }
        if layers.is_empty() {
            diag.push("beam.layers: at least one layer is required when present".to_string());
        }
        let mut arr = [BeamLayer::ZERO; MAX_TRAIL_LAYERS];
        let mut n = 0u8;
        for (i, l) in layers.iter().take(MAX_TRAIL_LAYERS).enumerate() {
            let label = format!("beam.layers[{i}]");
            arr[i] = BeamLayer {
                thickness_mul: bounded_f32(
                    Some(l.thickness_mul),
                    1.0,
                    0.0,
                    MAX_LAYER_THICKNESS,
                    &format!("{label}.thickness_mul"),
                    diag,
                ),
                coverage_mul: bounded_f32(
                    Some(l.coverage_mul),
                    1.0,
                    0.0,
                    1.0,
                    &format!("{label}.coverage_mul"),
                    diag,
                ),
                white_base: bounded_f32(
                    l.white_base,
                    0.0,
                    0.0,
                    1.0,
                    &format!("{label}.white_base"),
                    diag,
                ),
                white_pos_mul: bounded_f32(
                    l.white_pos_mul,
                    0.0,
                    0.0,
                    1.0,
                    &format!("{label}.white_pos_mul"),
                    diag,
                ),
                stride: bounded_u32(l.stride, 1, 1, 4, &format!("{label}.stride"), diag) as u8,
            };
            n += 1;
        }
        if n > 0 {
            b.layers = arr;
            b.layer_count = n;
        }
    }

    b.envelope = raw
        .envelope
        .as_ref()
        .map(|e| compile_envelope(e, diag))
        .unwrap_or(Envelope::Linear);

    b.scint = raw.scintillation.as_ref().map(|s| Scint {
        amp: bounded_f32(Some(s.amp), 0.0, 0.0, 1.0, "beam.scintillation.amp", diag),
        freq: bounded_f32(
            Some(s.freq),
            0.0,
            0.0,
            64.0,
            "beam.scintillation.freq",
            diag,
        ),
    });
    b
}

fn compile_envelope(raw: &RawEnvelope, diag: &mut Vec<String>) -> Envelope {
    match raw.kind.as_str() {
        "linear" => {
            forbid(&raw.hold_frac, "beam.envelope", "hold_frac", "linear", diag);
            forbid(
                &raw.front_width,
                "beam.envelope",
                "front_width",
                "linear",
                diag,
            );
            Envelope::Linear
        }
        "hold_cosine" => {
            forbid(
                &raw.front_width,
                "beam.envelope",
                "front_width",
                "hold_cosine",
                diag,
            );
            Envelope::HoldCosine {
                hold_frac: bounded_f32(
                    raw.hold_frac,
                    0.55,
                    0.0,
                    1.0,
                    "beam.envelope.hold_frac",
                    diag,
                ),
            }
        }
        "burn_out" => Envelope::BurnOut {
            hold_frac: bounded_f32(
                raw.hold_frac,
                0.45,
                0.0,
                1.0,
                "beam.envelope.hold_frac",
                diag,
            ),
            front_width: bounded_f32(
                raw.front_width,
                0.15,
                0.01,
                1.0,
                "beam.envelope.front_width",
                diag,
            ),
        },
        other => {
            diag.push(format!(
                "beam.envelope.kind: unknown {other:?}; expected linear|hold_cosine|burn_out"
            ));
            Envelope::Linear
        }
    }
}

fn compile_ramp(raw: &RawRamp, diag: &mut Vec<String>) -> RampParams {
    let hue_step = bounded_f32(
        raw.hue_step_per_move,
        0.0,
        0.0,
        0.5,
        "ramp.hue_step_per_move",
        diag,
    );
    match raw.kind.as_str() {
        "mono" => {
            forbid(&raw.stops, "ramp", "stops", "mono", diag);
            forbid(&raw.bands, "ramp", "bands", "mono", diag);
            RampParams::Mono
        }
        "hsv_sweep" => {
            forbid(&raw.stops, "ramp", "stops", "hsv_sweep", diag);
            forbid(&raw.bands, "ramp", "bands", "hsv_sweep", diag);
            RampParams::HsvSweep {
                span_deg: bounded_f32(raw.span_deg, 300.0, 0.0, 360.0, "ramp.span_deg", diag),
                sat: bounded_f32(raw.sat, 0.9, 0.0, 1.0, "ramp.sat", diag),
                val: bounded_f32(raw.val, 1.0, 0.0, 1.0, "ramp.val", diag),
                hue_step,
            }
        }
        "stops" => {
            forbid(&raw.bands, "ramp", "bands", "stops", diag);
            let mut arr = [(0.0f32, 0u32); MAX_RAMP_STOPS];
            let mut n = 0u8;
            match raw.stops.as_ref() {
                Some(stops) if !stops.is_empty() => {
                    if stops.len() > MAX_RAMP_STOPS {
                        diag.push(format!(
                            "ramp.stops: {} stops exceed the maximum {MAX_RAMP_STOPS}",
                            stops.len()
                        ));
                    }
                    for (i, s) in stops.iter().take(MAX_RAMP_STOPS).enumerate() {
                        let t = bounded_f32(
                            Some(s.t),
                            0.0,
                            0.0,
                            1.0,
                            &format!("ramp.stops[{i}].t"),
                            diag,
                        );
                        let c = strict_pack_hex(&s.color, &format!("ramp.stops[{i}]"), diag);
                        arr[i] = (t, c);
                        n += 1;
                    }
                }
                _ => diag.push("ramp.stops: at least one (t, color) stop is required".to_string()),
            }
            RampParams::Stops {
                stops: arr,
                n,
                hue_step,
            }
        }
        "bands" => {
            forbid(&raw.stops, "ramp", "stops", "bands", diag);
            let mut arr = [0u32; MAX_RAMP_BANDS];
            let mut n = 0u8;
            match raw.bands.as_ref() {
                Some(bands) if !bands.is_empty() => {
                    if bands.len() > MAX_RAMP_BANDS {
                        diag.push(format!(
                            "ramp.bands: {} bands exceed the maximum {MAX_RAMP_BANDS}",
                            bands.len()
                        ));
                    }
                    for (i, c) in bands.iter().take(MAX_RAMP_BANDS).enumerate() {
                        arr[i] = strict_pack_hex(c, &format!("ramp.bands[{i}]"), diag);
                        n += 1;
                    }
                }
                _ => diag.push("ramp.bands: at least one #RRGGBB band is required".to_string()),
            }
            RampParams::Bands { bands: arr, n }
        }
        other => {
            diag.push(format!(
                "ramp.kind: unknown {other:?}; expected mono|stops|hsv_sweep|bands"
            ));
            RampParams::Mono
        }
    }
}

fn compile_heat(raw: &RawHeat, diag: &mut Vec<String>) -> HeatParams {
    let d = TrailParams::defaults().heat;
    HeatParams {
        gain: raw
            .gain
            .map(|_| bounded_f32(raw.gain, 0.5, 0.0, 1.0, "heat.gain", diag)),
        tau: raw
            .tau
            .map(|_| bounded_f32(raw.tau, 0.9, 0.05, 4.0, "heat.tau", diag)),
        bright_floor: bounded_f32(
            raw.bright_floor,
            d.bright_floor,
            0.0,
            2.0,
            "heat.bright_floor",
            diag,
        ),
        bright_slope: bounded_f32(
            raw.bright_slope,
            d.bright_slope,
            0.0,
            2.0,
            "heat.bright_slope",
            diag,
        ),
        life_base_mul: bounded_f32(
            raw.life_base_mul,
            d.life_base_mul,
            0.05,
            2.0,
            "heat.life_base_mul",
            diag,
        ),
        life_a: bounded_f32(raw.life_a, d.life_a, 0.0, 8.0, "heat.life_a", diag),
        life_b: bounded_f32(raw.life_b, d.life_b, 0.0, 8.0, "heat.life_b", diag),
        chain_keys: bounded_f32(
            raw.chain_keys,
            d.chain_keys,
            0.0,
            6.0,
            "heat.chain_keys",
            diag,
        ),
        chain_gap_max: bounded_f32(
            raw.chain_gap_max,
            d.chain_gap_max,
            0.0,
            1.0,
            "heat.chain_gap_max",
            diag,
        ),
        chain_life_max: bounded_f32(
            raw.chain_life_max,
            d.chain_life_max,
            0.0,
            4.0,
            "heat.chain_life_max",
            diag,
        ),
    }
}

fn compile_crown(raw: &RawCrown, diag: &mut Vec<String>) -> CrownParams {
    let d = TrailParams::defaults().crown;
    CrownParams {
        enabled: raw.enabled.unwrap_or(d.enabled),
        radius_cells: bounded_f32(
            raw.radius_cells,
            d.radius_cells,
            0.0,
            4.0,
            "crown.radius_cells",
            diag,
        ),
        peak_cov: bounded_f32(raw.peak_cov, d.peak_cov, 0.0, 1.0, "crown.peak_cov", diag),
        typing_window_ms: bounded_u32(
            raw.typing_window_ms,
            d.typing_window_ms as u32,
            MIN_WINDOW_MS as u32,
            MAX_WINDOW_MS as u32,
            "crown.typing_window_ms",
            diag,
        ) as u16,
        jump_window_ms: bounded_u32(
            raw.jump_window_ms,
            d.jump_window_ms as u32,
            MIN_WINDOW_MS as u32,
            MAX_WINDOW_MS as u32,
            "crown.jump_window_ms",
            diag,
        ) as u16,
    }
}

fn compile_ring(raw: &RawRing, diag: &mut Vec<String>) -> RingParams {
    let d = TrailParams::defaults().ring;
    RingParams {
        enabled: raw.enabled.unwrap_or(d.enabled),
        life_ms: bounded_u32(
            raw.life_ms,
            d.life_ms as u32,
            MIN_WINDOW_MS as u32,
            MAX_WINDOW_MS as u32,
            "ring.life_ms",
            diag,
        ) as u16,
    }
}

fn compile_channels(raw: &RawChannels, diag: &mut Vec<String>) -> Channels {
    let d = TrailParams::defaults().channels;
    let halo = match raw.halo.as_deref() {
        None => d.halo,
        Some("off") => HaloChannel::Off,
        Some("add") => HaloChannel::Add,
        Some("over") => HaloChannel::Over,
        Some(other) => {
            diag.push(format!(
                "channels.halo: unknown {other:?}; expected off|add|over"
            ));
            d.halo
        }
    };
    Channels {
        glow_add: raw.glow_add.unwrap_or(d.glow_add),
        halo,
        bed: raw.bed.unwrap_or(d.bed),
    }
}

fn compile_theme(raw: &RawTheme, diag: &mut Vec<String>) -> ThemeArm {
    match raw.kind.as_str() {
        "auto" => ThemeArm::Auto,
        "over_veil" => ThemeArm::OverVeil,
        "darken_tints" => ThemeArm::DarkenTints,
        other => {
            diag.push(format!(
                "theme.kind: unknown {other:?}; expected auto|over_veil|darken_tints"
            ));
            ThemeArm::Auto
        }
    }
}

fn compile_pop(raw: &RawPop, index: usize, diag: &mut Vec<String>) -> ParticlePop {
    let label = format!("particles[{index}]");
    let vx = bounded_pair(
        raw.vx.as_deref(),
        (-0.5, 0.5),
        -MAX_VELOCITY_CELLS,
        MAX_VELOCITY_CELLS,
        &format!("{label}.vx"),
        diag,
    );
    let vy = bounded_pair(
        raw.vy.as_deref(),
        (-1.5, -0.4),
        -MAX_VELOCITY_CELLS,
        MAX_VELOCITY_CELLS,
        &format!("{label}.vy"),
        diag,
    );
    let life = bounded_pair(
        raw.life.as_deref(),
        (0.4, 0.9),
        0.0,
        MAX_PARTICLE_LIFE,
        &format!("{label}.life"),
        diag,
    );
    ParticlePop {
        spawn_weight: bounded_f32(
            raw.spawn_weight,
            1.0,
            0.0,
            8.0,
            &format!("{label}.spawn_weight"),
            diag,
        ),
        vx,
        vy,
        gravity: bounded_f32(
            raw.gravity,
            0.0,
            -8.0,
            8.0,
            &format!("{label}.gravity"),
            diag,
        ),
        life,
        size: bounded_f32(raw.size, 0.18, 0.05, 1.0, &format!("{label}.size"), diag),
        typing_burst_max: bounded_u32(
            raw.typing_burst_max,
            6,
            0,
            MAX_TYPING_BURST as u32,
            &format!("{label}.typing_burst_max"),
            diag,
        ) as u8,
        jump_burst_max: bounded_u32(
            raw.jump_burst_max,
            10,
            0,
            MAX_JUMP_BURST as u32,
            &format!("{label}.jump_burst_max"),
            diag,
        ) as u8,
    }
}

// ---------------------------------------------------------------------------
// Shared validators (self-contained mirror of the spec.rs Toy-Pack helpers).
// ---------------------------------------------------------------------------

/// Round a clamped `f32` onto the [`QUANT`] grid so the emitted-`u8` path and
/// the fingerprint stay bit-exact across reloads and CPU↔GPU.
fn quant(v: f32) -> f32 {
    (v * QUANT).round() / QUANT
}

fn bounded_f32(
    value: Option<f32>,
    default: f32,
    lo: f32,
    hi: f32,
    field: &str,
    diag: &mut Vec<String>,
) -> f32 {
    match value {
        Some(v) if v.is_finite() && (lo..=hi).contains(&v) => quant(v),
        Some(v) => {
            diag.push(format!(
                "{field}: {v:?} is outside {lo}..={hi} or non-finite"
            ));
            quant(default)
        }
        None => quant(default),
    }
}

fn bounded_pair(
    value: Option<&[f32]>,
    default: (f32, f32),
    lo: f32,
    hi: f32,
    field: &str,
    diag: &mut Vec<String>,
) -> (f32, f32) {
    match value {
        Some([a, b])
            if a.is_finite()
                && b.is_finite()
                && (lo..=hi).contains(a)
                && (lo..=hi).contains(b)
                && a <= b =>
        {
            (quant(*a), quant(*b))
        }
        Some([a, b]) => {
            diag.push(format!(
                "{field}: [{a:?}, {b:?}] must be finite, within {lo}..={hi}, and non-decreasing"
            ));
            (quant(default.0), quant(default.1))
        }
        // FAIL CLOSED on a wrong-length pair (the reason the raw field is a
        // `Vec`, not a truncating `[f32; 2]`): a `[min, max]` pair must have
        // EXACTLY two entries — reject one/three/… with a diagnostic.
        Some(other) => {
            diag.push(format!(
                "{field}: expected exactly 2 values [min, max], got {}",
                other.len()
            ));
            (quant(default.0), quant(default.1))
        }
        None => (quant(default.0), quant(default.1)),
    }
}

fn bounded_u32(
    value: Option<u32>,
    default: u32,
    lo: u32,
    hi: u32,
    field: &str,
    diag: &mut Vec<String>,
) -> u32 {
    match value {
        Some(v) if (lo..=hi).contains(&v) => v,
        Some(v) => {
            diag.push(format!("{field}: {v} is outside {lo}..={hi}"));
            default
        }
        None => default,
    }
}

fn strict_pack_hex(value: &str, label: &str, diag: &mut Vec<String>) -> u32 {
    if value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return u32::from_str_radix(&value[1..], 16).unwrap_or(0);
    }
    diag.push(format!(
        "{label}.color: expected exactly #RRGGBB, got {value:?}"
    ));
    0
}

fn forbid<T>(value: &Option<T>, label: &str, field: &str, kind: &str, diag: &mut Vec<String>) {
    if value.is_some() {
        diag.push(format!("{label}.{field}: not valid for kind {kind:?}"));
    }
}

fn validate_text(label: &str, value: &str, max: usize, diag: &mut Vec<String>) {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        diag.push(format!(
            "{label}: must be non-empty, trimmed, control-free, and at most {max} UTF-8 bytes"
        ));
    }
}

fn valid_id(value: &str, max: usize) -> bool {
    let bytes = value.as_bytes();
    let edge_ok = bytes
        .first()
        .zip(bytes.last())
        .is_some_and(|(&first, &last)| {
            first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric()
        });
    edge_ok
        && bytes.len() <= max
        && !value.contains("..")
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b'.' | b'_' | b'-')
        })
}

/// FNV-1a over `id` then the full source — the style-switch / pack-swap detector.
fn pack_fingerprint(id: &str, source: &str) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for &b in id.as_bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h ^= 0xFF;
    h = h.wrapping_mul(0x0100_0193);
    for &b in source.as_bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNTHWAVE: &str = include_str!("../assets/trail-packs/synthwave.toml");
    const EMBERFALL: &str = include_str!("../assets/trail-packs/emberfall.toml");

    #[test]
    fn every_checked_in_example_pack_compiles() {
        for (name, src) in [("synthwave", SYNTHWAVE), ("emberfall", EMBERFALL)] {
            compile_trail_pack_toml(src).unwrap_or_else(|e| panic!("{name}:\n{e}"));
        }
    }

    #[test]
    fn synthwave_resolves_to_expected_params() {
        let pack = compile_trail_pack_toml(SYNTHWAVE).expect("synthwave compiles");
        assert_eq!(pack.metadata().id, "synthwave");
        let p = pack.params();
        assert!(p.beam.enabled);
        assert!(matches!(p.ramp, RampParams::HsvSweep { .. }));
        assert!(p.particle_count >= 1);
        assert_ne!(p.pack_fp, 0);
        // Every stored f32 is on the quantization grid.
        assert_eq!(p.beam.cell_frac, quant(p.beam.cell_frac));
        assert_eq!(p.crown.radius_cells, quant(p.crown.radius_cells));
    }

    #[test]
    fn trail_pack_file_reader_admits_regular_and_rejects_oversize() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "aterm-trail-pack-reader-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create Trail Pack reader fixtures");
        let regular = root.join("regular.toml");
        std::fs::write(&regular, SYNTHWAVE).expect("write regular Trail Pack");
        assert_eq!(
            read_trail_pack_file(&regular).expect("read regular Trail Pack"),
            SYNTHWAVE
        );

        let oversized = root.join("oversized.toml");
        std::fs::write(&oversized, vec![b'x'; MAX_TRAIL_PACK_BYTES + 1])
            .expect("write oversized Trail Pack");
        let error = read_trail_pack_file(&oversized).expect_err("oversized Trail Pack rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains(&MAX_TRAIL_PACK_BYTES.to_string()),
            "{error}"
        );
        std::fs::remove_dir_all(root).expect("remove Trail Pack reader fixtures");
    }

    #[test]
    fn unknown_field_is_rejected() {
        let bad = SYNTHWAVE.replacen("[beam]", "[beam]\nsurprise = true", 1);
        let err = compile_trail_pack_toml(&bad).expect_err("unknown field rejected");
        assert!(
            err.to_string().contains("unknown field `surprise`"),
            "{err}"
        );
    }

    #[test]
    fn unknown_enum_strings_are_rejected() {
        let bad_env = SYNTHWAVE.replacen("kind = \"hold_cosine\"", "kind = \"confetti\"", 1);
        let err = compile_trail_pack_toml(&bad_env).expect_err("unknown envelope rejected");
        assert!(err.to_string().contains("confetti"), "{err}");

        let bad_ramp = r#"
pack = 1
id = "x"
[ramp]
kind = "kaleidoscope"
"#;
        let err = compile_trail_pack_toml(bad_ramp).expect_err("unknown ramp rejected");
        assert!(err.to_string().contains("kaleidoscope"), "{err}");

        let bad_ch = "pack = 1\nid = \"x\"\n[channels]\nhalo = \"sideways\"\n";
        let err = compile_trail_pack_toml(bad_ch).expect_err("unknown halo rejected");
        assert!(err.to_string().contains("sideways"), "{err}");

        let bad_theme = "pack = 1\nid = \"x\"\n[theme]\nkind = \"neon\"\n";
        let err = compile_trail_pack_toml(bad_theme).expect_err("unknown theme rejected");
        assert!(err.to_string().contains("neon"), "{err}");
    }

    #[test]
    fn bad_hex_color_is_rejected() {
        let src = "pack = 1\nid = \"x\"\n[ramp]\nkind = \"bands\"\nbands = [\"58D8FF\"]\n";
        let err = compile_trail_pack_toml(src).expect_err("non-canonical color rejected");
        assert!(
            err.to_string().contains("expected exactly #RRGGBB"),
            "{err}"
        );
    }

    #[test]
    fn byte_cap_precedes_toml_parse() {
        let oversized = "x".repeat(MAX_TRAIL_PACK_BYTES + 1);
        let err = compile_trail_pack_toml(&oversized).expect_err("oversize rejected");
        assert!(err.to_string().contains("maximum"), "{err}");
        assert!(
            err.to_string().contains(&MAX_TRAIL_PACK_BYTES.to_string()),
            "{err}"
        );
    }

    #[test]
    fn out_of_range_scalars_are_clamped_and_collected_in_one_pass() {
        // drift/window too small, thickness 99, a 7th layer, velocity 9, a 4th pop.
        let src = r#"
pack = 1
id = "over"
[beam]
cell_frac = 4.0
[[beam.layers]]
thickness_mul = 99.0
coverage_mul = 3.0
[crown]
typing_window_ms = 5
[[particles]]
vy = [-9.0, -0.5]
"#;
        let err = compile_trail_pack_toml(src).expect_err("clamped values diagnose");
        let joined = err.diagnostics().join("\n");
        for expected in [
            "beam.cell_frac",
            "thickness_mul",
            "coverage_mul",
            "crown.typing_window_ms",
            "particles[0].vy",
        ] {
            assert!(
                joined.contains(expected),
                "missing {expected:?} in:\n{joined}"
            );
        }
        assert!(err.diagnostics().len() >= 5, "multi-diagnostic: {joined}");
    }

    #[test]
    fn layer_and_pop_counts_are_bounded() {
        let layers = (0..=MAX_TRAIL_LAYERS)
            .map(|_| "[[beam.layers]]\nthickness_mul = 1.0\ncoverage_mul = 1.0\n")
            .collect::<String>();
        let src = format!("pack = 1\nid = \"many\"\n[beam]\n{layers}");
        let err = compile_trail_pack_toml(&src).expect_err("layer cap rejected");
        assert!(err.to_string().contains("exceed the maximum"), "{err}");

        let pops = (0..=MAX_TRAIL_POPS)
            .map(|_| "[[particles]]\n")
            .collect::<String>();
        let src = format!("pack = 1\nid = \"manypop\"\n{pops}");
        let err = compile_trail_pack_toml(&src).expect_err("pop cap rejected");
        assert!(err.to_string().contains("populations exceed"), "{err}");
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let src = "pack = 7\nid = \"x\"\n";
        let err = compile_trail_pack_toml(src).expect_err("bad schema rejected");
        assert!(
            err.to_string().contains("unsupported schema version 7"),
            "{err}"
        );
    }

    #[test]
    fn minimal_pack_inherits_defaults() {
        let pack =
            compile_trail_pack_toml("pack = 1\nid = \"bare\"\n").expect("bare pack compiles");
        let p = pack.params();
        assert!(p.beam.enabled);
        assert_eq!(p.beam.layer_count as usize, DEFAULT_BEAM_LAYERS.len());
        assert!(matches!(p.ramp, RampParams::Mono));
        assert_eq!(p.particle_count, 0);
    }

    #[test]
    fn cross_kind_fields_are_forbidden() {
        // `bands` supplied to a `mono` ramp is a fail-closed error.
        let src = "pack = 1\nid = \"x\"\n[ramp]\nkind = \"mono\"\nbands = [\"#ff0000\"]\n";
        let err = compile_trail_pack_toml(src).expect_err("cross-kind field rejected");
        assert!(
            err.to_string().contains("not valid for kind \"mono\""),
            "{err}"
        );
    }

    #[test]
    fn wrong_length_velocity_or_life_pair_is_rejected() {
        // A 3-entry velocity pair used to TRUNCATE to two via serde's fixed
        // `[f32; 2]` impl (silently dropping the 3rd) — now it fails closed with a
        // diagnostic that names the field and the actual length.
        let three = "pack = 1\nid = \"trip\"\n[[particles]]\nvy = [-1.0, -0.5, -0.2]\n";
        let err = compile_trail_pack_toml(three).expect_err("3-entry pair rejected");
        let joined = err.diagnostics().join("\n");
        assert!(joined.contains("particles[0].vy"), "{joined}");
        assert!(joined.contains("expected exactly 2 values"), "{joined}");
        assert!(joined.contains("got 3"), "{joined}");

        // A single-entry pair is equally invalid (no silent [x, default]).
        let one = "pack = 1\nid = \"solo\"\n[[particles]]\nvx = [0.5]\n";
        let err = compile_trail_pack_toml(one).expect_err("1-entry pair rejected");
        assert!(
            err.diagnostics().join("\n").contains("particles[0].vx"),
            "{err}"
        );

        // An empty `life` array is rejected too.
        let none = "pack = 1\nid = \"empty\"\n[[particles]]\nlife = []\n";
        let err = compile_trail_pack_toml(none).expect_err("0-entry pair rejected");
        assert!(
            err.diagnostics().join("\n").contains("particles[0].life"),
            "{err}"
        );

        // A correct 2-entry pair still compiles unchanged.
        let ok = "pack = 1\nid = \"ok\"\n[[particles]]\nvy = [-1.0, -0.5]\n";
        compile_trail_pack_toml(ok).expect("valid 2-entry pair compiles");
    }
}
