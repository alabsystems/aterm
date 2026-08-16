// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sparkle Words v3 §6 — **the spec framework**: new sparkle words are data,
//! not code. A [`WordEffectSpec`] composes three orthogonal axes:
//!
//! * **graphic** — a peek-up sprite show ([`Collection::Cats`]; the orca redo
//!   adds `Orcas` later),
//! * **ink** — a glyph colorway ([`Colorway`]: two-tone gradient, the
//!   ultrathink-style rainbow, or the feline self-glow),
//! * **burst** — a light show ([`BurstKind`]: sparkle, nova, FUCK SUPER NOVA,
//!   starburst, glow).
//!
//! The engine's per-class behavior is expressed as class-default specs
//! (resolved from the live [`DecoConfig`](crate::word_decorations::DecoConfig)
//! knobs), and user `[[sparkle_words.custom]]` entries become per-word
//! overrides keyed by the scanner's `form_hash`. **Overrides win over class
//! defaults regardless of the match's class, and an overridden word bypasses
//! the per-class enable gate** (a custom spec on a builtin profanity form
//! fires with `[profanity] enabled = false`) — normative, design §6.
//!
//! ## form_hash correctness (normative)
//!
//! The scanner reports `form_hash = FNV-1a(folded full token)` for spaced
//! surfaces and `FNV-1a(RAW compound)` for no-space (CJK) scripts, so the
//! override table is keyed with EXACTLY those hashes:
//!
//! * spaced surfaces hash `aterm_lexicon::fold(word)` (hashing raw TOML text
//!   would silently no-op on any cased/accented spelling);
//! * the four possessive-variant hashes (`w's`, `w’s`, `w'`, `w’`) are also
//!   inserted, because possessive hits carry the FULL-token hash;
//! * no-space-script surfaces are detected ([`aterm_lexicon::is_no_space_surface`])
//!   and synthesized as `cjk = true` lexicon entries with RAW keys + RAW
//!   hashes — a spaced entry containing a CJK surface is silently dropped at
//!   lexicon insert (the `append_extra_words_entry` gap this module closes).
//!
//! Dropped/conflicting surfaces keep surfacing through the existing lexicon
//! `conflicts` channel (custom words are auto-appended to the emphasis class;
//! the builtin still ships zero emphasis forms). In particular, a custom
//! surface that can never scan as written is NOT silently accepted: a
//! single-char CJK word warns that it requires `cjk_single_char = true`, and
//! a mixed-script word (dropped at lexicon insert) warns it was dropped —
//! both recorded at lexicon build for the resolver's warning log.
//!
//! ## Toy Packs (strict contribution lane)
//!
//! [`compile_toy_pack_toml`] compiles a versioned, bounded Toy Pack into the
//! same [`SpecTable`] + lexicon fragment as the inline compatibility surface.
//! Parsing and validation happen only when a host loads/reloads a pack; the
//! render path still sees only copied [`WordEffectSpec`] values and compact
//! enum dispatch. Unlike `[[sparkle_words.custom]]`, Toy Packs fail closed on
//! unknown fields, unknown effect names, malformed colours, unsafe timings,
//! duplicate ids/surfaces, or an unscannable word.

use aterm_hash::FxHashMap;
use aterm_lexicon::{fold, form_hash, is_no_space_surface};

/// One word's composed effect over the three §6 axes. All-`None` renders
/// nothing (such customs are not registered as overrides).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WordEffectSpec {
    /// Peek-up sprite show.
    pub graphic: Option<GraphicSpec>,
    /// Glyph colorway.
    pub ink: Option<InkSpec>,
    /// Light show.
    pub burst: Option<BurstSpec>,
}

/// The graphic axis: which sprite collection peeks, and its dwell range (ms).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphicSpec {
    pub collection: Collection,
    /// Dwell range `(lo, hi)` in ms; the genome quantizes inside it (the cat
    /// engine's 4-step dwell decode generalized).
    pub dwell_ms: (u32, u32),
}

/// Sprite collections. The orca redo adds `Orcas` later (design §10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Collection {
    Cats,
    /// The ambient animal roster: the SPECIES-keyed peeking head. Which
    /// species rides the occurrence (resolved from the lexicon's `species`
    /// tag at rescan), not this spec — one collection covers every animal.
    /// Builtin-only for now: Toy Pack schema v1 still accepts only `"cats"`
    /// (a pack word cannot name a species), so packs fail closed exactly as
    /// before.
    Animals,
}

/// The ink axis: a colorway + whether the animated intro runs once
/// (`sweep_once = false` keeps re-sweeping while visible, the `ink_loop`
/// semantics).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InkSpec {
    pub colorway: Colorway,
    pub sweep_once: bool,
}

/// Glyph colorways (§3.1/§6).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Colorway {
    /// The v2 two-tone gradient + one specular sweep.
    TwoTone { c0: u32, c1: u32 },
    /// The §3.1 rainbow: per-lead-cell hue over `span_deg` (span clamps to
    /// `min(span_deg, 100°·(lead_cells − 1))`), one EXACT-full-cycle temporal
    /// drift over `drift_ms`, then byte-stable freeze at the t = 0 phase.
    /// `sat`/`val` are the dark-theme parameters; light backgrounds
    /// (`relative_luminance(bg) > 0.5`) re-resolve to `s = 1.0, v = 0.62`
    /// (deep candy tones that clear 3.5:1 on white) at emission.
    Rainbow {
        sat: f32,
        val: f32,
        span_deg: f32,
        drift_ms: u32,
    },
    /// The feline v2.9 self-terminating glow in the word's own fg.
    SelfGlow { lift: f32, amp: f32, window_ms: u32 },
}

/// The burst axis: a one-shot light show with a per-appearance chance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BurstSpec {
    pub kind: BurstKind,
    /// Per-appearance escalation chance, percent (`0..=100`; 0 disables).
    pub chance_pct: u8,
}

/// Burst light shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BurstKind {
    /// The v1 randomized self-terminating sparkle.
    Sparkle,
    /// The v2 §6 classic nova (dip → flash → ring + rays → debris → ember).
    Nova,
    /// The v3 §3.2 FUCK SUPER NOVA (charge → theme-branched detonation →
    /// double shockwave → rainbow debris → afterglow).
    SuperNova,
    /// A dense one-shot star burst (the sparkle machinery, burst-shaped).
    Starburst,
    /// A soft additive glow pulse over the word.
    Glow,
}

/// §3.1 rainbow defaults (the `ProfanityStyle::Rainbow` class default and the
/// bare `colorway = "rainbow"` custom shorthand).
pub const RAINBOW_SAT: f32 = 0.85;
pub const RAINBOW_VAL: f32 = 1.0;
pub const RAINBOW_SPAN_DEG: f32 = 300.0;
pub const RAINBOW_DRIFT_MS: u32 = 2500;

/// The default rainbow ink spec.
#[must_use]
pub fn rainbow_ink() -> InkSpec {
    InkSpec {
        colorway: Colorway::Rainbow {
            sat: RAINBOW_SAT,
            val: RAINBOW_VAL,
            span_deg: RAINBOW_SPAN_DEG,
            drift_ms: RAINBOW_DRIFT_MS,
        },
        sweep_once: true,
    }
}

/// Spec id: an index into [`SpecTable::specs`].
pub type SpecId = u16;

/// Toy Pack schema version accepted by [`compile_toy_pack_toml`].
pub const TOY_PACK_SCHEMA_V1: u32 = 1;
/// Source-size ceiling checked before TOML parsing, bounding cold-path parser
/// work and allocation for an untrusted pack.
pub const MAX_TOY_PACK_BYTES: usize = 256 * 1024;
/// Maximum recipes in one pack.
pub const MAX_TOYS_PER_PACK: usize = 128;
/// Maximum word surfaces attached to one recipe.
pub const MAX_WORDS_PER_TOY: usize = 64;
/// Maximum word surfaces across one pack.
pub const MAX_WORDS_PER_PACK: usize = 4096;
/// Maximum UTF-8 byte length of one word surface.
pub const MAX_TOY_WORD_BYTES: usize = 128;

/// Read one Toy Pack manifest without permitting special files or unbounded
/// allocation. Hosts should use this before [`compile_toy_pack_toml`] so a
/// FIFO/device cannot stall startup and an oversized file costs at most the
/// compiler ceiling plus one sentinel byte.
pub fn read_toy_pack_file(path: &std::path::Path) -> std::io::Result<String> {
    crate::file_feed::read_bounded_regular_utf8(path, MAX_TOY_PACK_BYTES)
}

/// The §6 dispatch table carried on the resolved config: registered custom
/// specs plus the `form_hash → SpecId` override map. Class defaults are
/// resolved from the live `DecoConfig` knobs (they are behavior, not data),
/// so the table stores only the user's custom specs.
#[derive(Clone, Debug, Default)]
pub struct SpecTable {
    specs: Vec<WordEffectSpec>,
    /// `form_hash` (scan-time semantics, see the module doc) → spec index.
    overrides: FxHashMap<u64, SpecId>,
}

/// Which shared runtime tunings have at least one admitted word-effect
/// consumer. The custom-axis fields are derived from the compiled [`SpecTable`], not from the
/// mere presence of a `[[sparkle_words.custom]]` or Toy Pack path: graphic-only,
/// Glow, SuperNova, and zero-chance recipes do not make unrelated controls
/// effective.
///
/// Custom TwoTone recipes own their repeat choice through
/// [`InkSpec::sweep_once`]. Consequently there is deliberately no
/// "global ink loop" capability here; the global `ink.loop` setting affects
/// class-default specs only. `twotone_ink` does identify consumers of the
/// shared sweep duration and strength.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpecConsumerCapabilities {
    /// A positive-chance classic Nova burst consumes the shared magic toggle.
    pub nova_burst: bool,
    /// A positive-chance Sparkle or Starburst consumes palette/density/timing/jitter.
    pub sparkle_or_starburst_burst: bool,
    /// At least one custom Rainbow ink axis consumes shared ink strength.
    pub rainbow_ink: bool,
    /// At least one custom TwoTone ink axis consumes strength and sweep timing.
    pub twotone_ink: bool,
    /// A Nova + TwoTone composition also consumes Nova magic through its ink
    /// palette, even when that recipe's burst chance is zero.
    pub nova_twotone_ink: bool,
    /// At least one scannable, non-ignored Emphasis surface reaches the class
    /// default rather than a custom override. Filled by the host's prepared
    /// lexicon+spec projection; a bare `SpecTable` leaves this false.
    pub emphasis_class_default: bool,
}

impl SpecTable {
    /// Whether any custom spec is registered — the §6 emphasis-class resolve
    /// gate term (`enabled && (ink_enabled || has_custom_specs)`).
    #[must_use]
    pub fn has_custom(&self) -> bool {
        !self.specs.is_empty()
    }

    /// Exact shared-setting consumers in this already-compiled table.
    ///
    /// `specs` contains only recipes that were attached to at least one
    /// non-empty word by [`Self::insert_custom`], so empty declarations cannot
    /// manufacture a live consumer. Calling this after Toy Pack overlay yields
    /// the same admitted inline + pack generation the renderer uses.
    #[must_use]
    pub fn consumer_capabilities(&self) -> SpecConsumerCapabilities {
        let mut capabilities = SpecConsumerCapabilities::default();
        // Overlay keeps the compact spec arena resident and rewrites only the
        // dispatch map. Walk reachable ids, not the arena, so a Toy Pack
        // recipe fully shadowed by a later inline override cannot keep a
        // setting falsely live.
        for id in self.overrides.values() {
            let Some(spec) = self.specs.get(usize::from(*id)) else {
                continue;
            };
            let rainbow = matches!(
                spec.ink.map(|ink| ink.colorway),
                Some(Colorway::Rainbow { .. })
            );
            let twotone = matches!(
                spec.ink.map(|ink| ink.colorway),
                Some(Colorway::TwoTone { .. })
            );
            capabilities.rainbow_ink |= rainbow;
            capabilities.twotone_ink |= twotone;

            if let Some(burst) = spec.burst {
                let rolls = burst.chance_pct > 0;
                capabilities.nova_burst |= rolls && burst.kind == BurstKind::Nova;
                capabilities.sparkle_or_starburst_burst |=
                    rolls && matches!(burst.kind, BurstKind::Sparkle | BurstKind::Starburst);
                capabilities.nova_twotone_ink |= burst.kind == BurstKind::Nova && twotone;
            }
        }
        capabilities
    }

    /// The per-word override for a scan match, if any. Wins over the class
    /// default regardless of the match's class (design §6, normative).
    #[must_use]
    pub fn override_for(&self, form_hash: u64) -> Option<&WordEffectSpec> {
        self.overrides
            .get(&form_hash)
            .and_then(|id| self.specs.get(usize::from(*id)))
    }

    /// Register one custom spec for `word` (all surface-hash variants). No-op
    /// for an all-`None` spec (nothing to dispatch) or an empty word.
    pub fn insert_custom(&mut self, word: &str, spec: WordEffectSpec) {
        if spec == WordEffectSpec::default() {
            return;
        }
        let word = word.trim();
        if word.is_empty() {
            return;
        }
        let id = if let Some(i) = self.specs.iter().position(|s| *s == spec) {
            i as SpecId
        } else {
            if self.specs.len() >= usize::from(SpecId::MAX) {
                return; // defensive cap; unreachable for sane configs
            }
            self.specs.push(spec);
            (self.specs.len() - 1) as SpecId
        };
        if is_no_space_surface(word) {
            // CJK path: the scanner hashes the RAW compound.
            self.overrides.insert(form_hash(word), id);
        } else {
            // Spaced path: the scanner hashes the FOLDED full token — and a
            // possessive hit carries the FULL-token hash, so the four
            // possessive variants register too (fold is per-char, so
            // `fold(w + suf) == fold(w) + suf` for these ASCII/U+2019 tails).
            let base = fold(word);
            if base.is_empty() {
                return;
            }
            for suf in ["", "'s", "\u{2019}s", "'", "\u{2019}"] {
                let mut k = base.clone();
                k.push_str(suf);
                self.overrides.insert(form_hash(&k), id);
            }
        }
    }

    /// Overlay another table onto this one. Existing specs are deduplicated;
    /// when both tables claim the same scanner `form_hash`, `other` wins.
    /// This is the deterministic composition law used by ordered Toy Packs
    /// and the final inline-custom override.
    pub fn overlay(&mut self, other: SpecTable) {
        let mut remap = Vec::with_capacity(other.specs.len());
        for spec in other.specs {
            let id = if let Some(index) = self.specs.iter().position(|current| *current == spec) {
                Some(index as SpecId)
            } else if self.specs.len() >= usize::from(SpecId::MAX) {
                None
            } else {
                self.specs.push(spec);
                Some((self.specs.len() - 1) as SpecId)
            };
            remap.push(id);
        }
        for (form_hash, old_id) in other.overrides {
            if let Some(new_id) = remap.get(usize::from(old_id)).copied().flatten() {
                self.overrides.insert(form_hash, new_id);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `[[sparkle_words.custom]]` parsing — shared by the native config resolver
// and the web `set_sparkle_custom_specs(toml)` surface (the SAME fragment).
// ---------------------------------------------------------------------------

/// One raw `[[sparkle_words.custom]]` entry, serde-shaped so the native
/// `Config` deserializer embeds it directly:
///
/// ```toml
/// [[sparkle_words.custom]]
/// words   = ["ultrathink", "gg", "猫神"]
/// ink     = { colorway = "rainbow" }        # or "twotone:#RRGGBB,#RRGGBB"
/// burst   = { kind = "starburst", chance = 10 }
/// graphic = { collection = "cats" }
/// ```
///
/// Any combination of axes; unknown values fail open to the nearest default.
///
/// `PartialEq` (with the sub-field types below): the GUI's config hot-reload
/// dedupe compares the freshly parsed `Config` — which embeds these raw
/// entries verbatim — against the currently applied one, so an mtime bump
/// with unchanged content skips the reload side-effect storm.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct RawCustomEntry {
    pub words: Vec<String>,
    pub ink: Option<RawInk>,
    pub burst: Option<RawBurst>,
    pub graphic: Option<RawGraphic>,
}

/// The `ink` field: a shorthand string (`"rainbow"` /
/// `"twotone:#RRGGBB,#RRGGBB"`) or a `{ colorway = "…" }` table.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(untagged)]
pub enum RawInk {
    Shorthand(String),
    Table {
        colorway: String,
        #[serde(default)]
        sweep_once: Option<bool>,
    },
}

/// The `burst` field.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct RawBurst {
    pub kind: String,
    /// Percent 0..=100 (clamped); default 100 (always).
    pub chance: Option<u32>,
}

/// The `graphic` field.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct RawGraphic {
    pub collection: String,
}

/// Parse a `#RRGGBB` / `RRGGBB` color into packed `0x00RRGGBB`.
fn parse_hex(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}

/// Whether a custom ink colorway is one of the two runtime-supported shapes.
/// Two-tone is deliberately exact: a third comma-separated color is not an
/// ignored extension but an invalid value that falls back to rainbow.
#[must_use]
pub fn custom_colorway_is_valid(s: &str) -> bool {
    let s = s.trim();
    s == "rainbow" || parse_two_tone(s).is_some()
}

fn parse_two_tone(s: &str) -> Option<(u32, u32)> {
    let rest = s.strip_prefix("twotone:")?;
    let mut colors = rest.split(',');
    let c0 = colors.next().and_then(parse_hex)?;
    let c1 = colors.next().and_then(parse_hex)?;
    colors.next().is_none().then_some((c0, c1))
}

/// Resolve a colorway string: `"rainbow"` (default) or
/// `"twotone:#RRGGBB,#RRGGBB"`. Unknown values fail open to rainbow — the
/// custom framework's flagship default.
fn parse_colorway(s: &str) -> Colorway {
    let s = s.trim();
    if let Some((c0, c1)) = parse_two_tone(s) {
        return Colorway::TwoTone { c0, c1 };
    }
    match rainbow_ink().colorway {
        c @ Colorway::Rainbow { .. } => c,
        _ => unreachable!(),
    }
}

impl RawCustomEntry {
    /// Resolve the raw entry into a [`WordEffectSpec`] (fail-open decoding;
    /// an entry with no axes resolves to the all-`None` spec, which
    /// [`SpecTable::insert_custom`] skips).
    #[must_use]
    pub fn to_spec(&self) -> WordEffectSpec {
        let ink = self.ink.as_ref().map(|raw| {
            let (colorway, sweep_once) = match raw {
                RawInk::Shorthand(s) => (parse_colorway(s), true),
                RawInk::Table {
                    colorway,
                    sweep_once,
                } => (parse_colorway(colorway), sweep_once.unwrap_or(true)),
            };
            InkSpec {
                colorway,
                sweep_once,
            }
        });
        let burst = self.burst.as_ref().map(|raw| BurstSpec {
            kind: match raw.kind.trim().to_ascii_lowercase().as_str() {
                "sparkle" => BurstKind::Sparkle,
                "nova" => BurstKind::Nova,
                "supernova" | "super_nova" | "super-nova" => BurstKind::SuperNova,
                "glow" => BurstKind::Glow,
                // "starburst" and anything unknown: the gentlest burst.
                _ => BurstKind::Starburst,
            },
            chance_pct: raw.chance.unwrap_or(100).min(100) as u8,
        });
        let graphic = self.graphic.as_ref().map(|_| GraphicSpec {
            // Only Cats exists in v3; unknown collections fail open to it.
            collection: Collection::Cats,
            dwell_ms: (2200, 3598),
        });
        WordEffectSpec {
            graphic,
            ink,
            burst,
        }
    }
}

/// Build the [`SpecTable`] + the synthesized emphasis-class lexicon fragment
/// for a set of custom entries. The fragment is appended to the user's
/// lexicon-override TOML so custom words actually scan (spaced surfaces as a
/// plain forms entry; no-space-script surfaces as a `cjk = true` entry —
/// closing the silent-drop gap called out in design §6).
#[must_use]
pub fn build_custom(entries: &[RawCustomEntry]) -> (SpecTable, String) {
    let mut table = SpecTable::default();
    let mut spaced: Vec<&str> = Vec::new();
    let mut cjk: Vec<&str> = Vec::new();
    for e in entries {
        let spec = e.to_spec();
        for w in &e.words {
            let w = w.trim();
            if w.is_empty() {
                continue;
            }
            table.insert_custom(w, spec);
            if is_no_space_surface(w) {
                cjk.push(w);
            } else {
                spaced.push(w);
            }
        }
    }
    let mut out = String::new();
    push_lexicon_entry(&mut out, "en", &spaced, false);
    push_lexicon_entry(&mut out, "en", &cjk, true);
    (table, out)
}

/// Append one scanner-ready emphasis entry. Legacy inline customs deliberately
/// retain their control-character-dropping behavior; strict Toy Packs reject
/// such surfaces before reaching this helper.
fn push_lexicon_entry(out: &mut String, lang: &str, forms: &[&str], is_cjk: bool) {
    if forms.is_empty() {
        return;
    }
    out.push_str("\n[[entry]]\nclass = \"emphasis\"\nlang = \"");
    push_toml_basic_string_body(out, lang);
    out.push_str("\"\nmode = \"forms\"\n");
    if is_cjk {
        out.push_str("cjk = true\n");
    }
    out.push_str("forms = [");
    for (i, w) in forms.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('"');
        push_toml_basic_string_body(out, w);
        out.push('"');
    }
    out.push_str("]\n");
}

fn push_toml_basic_string_body(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {} // legacy inline customs drop controls
            c => out.push(c),
        }
    }
}

/// Parse the standalone `[[sparkle_words.custom]]` TOML fragment (the web
/// `set_sparkle_custom_specs` payload — the SAME fragment the native config
/// carries). Fail-closed on malformed TOML so the caller can warn and keep
/// the previous specs (the lexicon-override posture).
pub fn parse_custom_toml(fragment: &str) -> Result<Vec<RawCustomEntry>, String> {
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct SparkleWords {
        custom: Vec<RawCustomEntry>,
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct Root {
        sparkle_words: SparkleWords,
    }
    let root: Root = toml::from_str(fragment).map_err(|e| e.to_string())?;
    Ok(root.sparkle_words.custom)
}

// ---------------------------------------------------------------------------
// Toy Pack v1 — strict, versioned, bounded cold-path compilation.
// ---------------------------------------------------------------------------

const MAX_PACK_ID_BYTES: usize = 96;
const MAX_TOY_ID_BYTES: usize = 64;
const MAX_PACK_NAME_BYTES: usize = 80;
const MAX_PACK_DESCRIPTION_BYTES: usize = 512;
const MAX_PACK_AUTHORS: usize = 8;
const MAX_AUTHOR_BYTES: usize = 80;
const MAX_LICENSE_BYTES: usize = 64;
const MAX_LANG_BYTES: usize = 35;
const MIN_ANIMATION_MS: u32 = 350;
const MAX_ANIMATION_MS: u32 = 6000;
const MAX_CAT_DWELL_MS: u32 = 3750;

/// Contributor-facing metadata retained with a compiled Toy Pack. `id` is a
/// stable, namespaced key (for example `community.example.tiny-triumphs`), not
/// display text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToyPackMetadata {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub authors: Vec<String>,
    pub license: String,
    pub description: Option<String>,
}

/// A validated Toy Pack lowered to the engine's existing dispatch artifacts.
/// Hosts parse this on startup/config reload, then retain only these values;
/// no manifest strings or TOML decisions enter the frame loop.
#[derive(Clone, Debug)]
pub struct CompiledToyPack {
    metadata: ToyPackMetadata,
    spec_table: SpecTable,
    lexicon_toml: String,
    toy_count: usize,
    word_count: usize,
}

impl CompiledToyPack {
    #[must_use]
    pub fn metadata(&self) -> &ToyPackMetadata {
        &self.metadata
    }

    #[must_use]
    pub fn spec_table(&self) -> &SpecTable {
        &self.spec_table
    }

    /// Scanner entries to append to the normal user lexicon override.
    #[must_use]
    pub fn lexicon_toml(&self) -> &str {
        &self.lexicon_toml
    }

    #[must_use]
    pub fn toy_count(&self) -> usize {
        self.toy_count
    }

    #[must_use]
    pub fn word_count(&self) -> usize {
        self.word_count
    }

    /// Consume the pack into the two runtime dispatch artifacts plus metadata.
    #[must_use]
    pub fn into_parts(self) -> (ToyPackMetadata, SpecTable, String) {
        (self.metadata, self.spec_table, self.lexicon_toml)
    }
}

/// One or more actionable Toy Pack diagnostics. Semantic validation collects
/// independent errors in one pass so an artist does not have to fix/re-run one
/// field at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToyPackError {
    diagnostics: Vec<String>,
}

impl ToyPackError {
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

impl std::fmt::Display for ToyPackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.diagnostics.len() == 1 {
            return f.write_str(&self.diagnostics[0]);
        }
        writeln!(f, "toy pack has {} errors:", self.diagnostics.len())?;
        for diagnostic in &self.diagnostics {
            writeln!(f, "- {diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ToyPackError {}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToyPackDocument {
    schema: u32,
    pack: RawToyPackMetadata,
    #[serde(default)]
    toy: Vec<RawToy>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToyPackMetadata {
    id: String,
    name: String,
    version: u32,
    authors: Vec<String>,
    license: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToy {
    id: String,
    lang: String,
    words: Vec<String>,
    #[serde(default)]
    ink: Option<RawToyPackInk>,
    #[serde(default)]
    burst: Option<RawToyPackBurst>,
    #[serde(default)]
    graphic: Option<RawToyPackGraphic>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToyPackInk {
    kind: String,
    #[serde(default)]
    colors: Option<[String; 2]>,
    #[serde(default)]
    saturation: Option<f32>,
    #[serde(default)]
    value: Option<f32>,
    #[serde(default)]
    span_degrees: Option<f32>,
    #[serde(default)]
    drift_ms: Option<u32>,
    #[serde(default)]
    lift: Option<f32>,
    #[serde(default)]
    amplitude: Option<f32>,
    #[serde(default)]
    window_ms: Option<u32>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToyPackBurst {
    kind: String,
    #[serde(default)]
    chance_pct: Option<u32>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToyPackGraphic {
    collection: String,
    #[serde(default)]
    dwell_ms: Option<[u32; 2]>,
}

/// Parse and compile one Toy Pack manifest. This is a cold-path operation:
/// source bytes are capped before TOML parsing, every collection/string/timing
/// is validated, and the result is the same O(1) form-hash dispatch table used
/// by legacy inline customs.
pub fn compile_toy_pack_toml(source: &str) -> Result<CompiledToyPack, ToyPackError> {
    if source.len() > MAX_TOY_PACK_BYTES {
        return Err(ToyPackError::one(format!(
            "source is {} bytes; maximum is {MAX_TOY_PACK_BYTES}",
            source.len()
        )));
    }
    let raw: RawToyPackDocument =
        toml::from_str(source).map_err(|e| ToyPackError::one(format!("TOML schema error: {e}")))?;
    let mut diagnostics = Vec::new();

    if raw.schema != TOY_PACK_SCHEMA_V1 {
        diagnostics.push(format!(
            "schema: unsupported version {}; expected {TOY_PACK_SCHEMA_V1}",
            raw.schema
        ));
    }
    validate_pack_metadata(&raw.pack, &mut diagnostics);
    if raw.toy.is_empty() {
        diagnostics.push("toy: at least one [[toy]] recipe is required".to_string());
    }
    if raw.toy.len() > MAX_TOYS_PER_PACK {
        diagnostics.push(format!(
            "toy: {} recipes exceed the per-pack maximum {MAX_TOYS_PER_PACK}",
            raw.toy.len()
        ));
    }

    let mut table = SpecTable::default();
    let mut lexicon_toml = String::new();
    let mut toy_ids = std::collections::BTreeSet::new();
    let mut surfaces = std::collections::BTreeMap::<String, String>::new();
    let mut dispatch_hashes = std::collections::BTreeMap::<u64, String>::new();
    let mut scanner_surfaces = Vec::<(String, &str)>::new();
    let word_count = raw
        .toy
        .iter()
        .fold(0usize, |n, toy| n.saturating_add(toy.words.len()));
    if word_count > MAX_WORDS_PER_PACK {
        diagnostics.push(format!(
            "words: {word_count} surfaces exceed the per-pack maximum {MAX_WORDS_PER_PACK}"
        ));
    }

    // The `.take` bounds semantic compilation even after the source-size cap:
    // oversize arrays get one diagnostic, never unbounded validation work.
    for (index, toy) in raw.toy.iter().take(MAX_TOYS_PER_PACK).enumerate() {
        let context = format!("toy[{index}] ({:?})", toy.id);
        if !valid_id(&toy.id, MAX_TOY_ID_BYTES, false) {
            diagnostics.push(format!(
                "{context}.id: expected a lowercase ASCII id (letters/digits plus ._-), \
                 1..={MAX_TOY_ID_BYTES} bytes"
            ));
        }
        if !toy_ids.insert(toy.id.as_str()) {
            diagnostics.push(format!("{context}.id: duplicate recipe id"));
        }
        if !valid_lang(&toy.lang) {
            diagnostics.push(format!(
                "{context}.lang: expected a BCP47-like ASCII tag, 2..={MAX_LANG_BYTES} bytes"
            ));
        }
        if toy.words.is_empty() {
            diagnostics.push(format!("{context}.words: at least one surface is required"));
        }
        if toy.words.len() > MAX_WORDS_PER_TOY {
            diagnostics.push(format!(
                "{context}.words: {} surfaces exceed the per-recipe maximum {MAX_WORDS_PER_TOY}",
                toy.words.len()
            ));
        }

        let spec = compile_toy_spec(toy, &context, &mut diagnostics);
        let mut spaced = Vec::new();
        let mut cjk = Vec::new();
        for (word_index, word) in toy.words.iter().take(MAX_WORDS_PER_TOY).enumerate() {
            let label = format!("{context}.words[{word_index}]");
            if !validate_toy_word(word, &label, &mut diagnostics) {
                continue;
            }
            let no_space = is_no_space_surface(word);
            let normalized = if no_space {
                let mut key = String::from("cjk:");
                key.push_str(word);
                key
            } else {
                let mut key = String::from("word:");
                key.push_str(&fold(word));
                key
            };
            if let Some(previous) = surfaces.insert(normalized, label.clone()) {
                diagnostics.push(format!(
                    "{label}: normalized surface duplicates {previous}; dispatch order must not decide behavior"
                ));
                continue;
            }
            let hashes = toy_dispatch_hashes(word);
            if let Some(previous) = hashes.iter().find_map(|hash| dispatch_hashes.get(hash)) {
                diagnostics.push(format!(
                    "{label}: dispatch key collides with {previous} (including possessive variants); dispatch order must not decide behavior"
                ));
                continue;
            }
            for hash in hashes {
                dispatch_hashes.insert(hash, label.clone());
            }
            if let Some(spec) = spec {
                table.insert_custom(word, spec);
            }
            scanner_surfaces.push((label, word));
            if no_space {
                cjk.push(word.as_str());
            } else {
                spaced.push(word.as_str());
            }
        }
        push_lexicon_entry(&mut lexicon_toml, &toy.lang, &spaced, false);
        push_lexicon_entry(&mut lexicon_toml, &toy.lang, &cjk, true);
    }

    // Round-trip through the real scanner builder, then scan every accepted
    // surface in isolation. This catches mixed-script/punctuation/multi-word
    // surfaces, CJK exception precedence, and any future scanner rule the pack
    // compiler would otherwise duplicate and drift from.
    if diagnostics.is_empty() {
        match aterm_lexicon::Lexicon::with_languages_and_override(&["all"], Some(&lexicon_toml)) {
            Ok(lexicon) => {
                diagnostics.extend(
                    lexicon
                        .conflicts()
                        .iter()
                        .map(|conflict| format!("word surface rejected by scanner: {conflict}")),
                );
                let options = aterm_lexicon::ScanOptions {
                    allow_bare_cat: true,
                    cjk_single_char: true,
                    ignore: None,
                };
                let mut scanner_chars = Vec::new();
                let mut scanner_hits = Vec::new();
                let mut scanner_scratch = aterm_lexicon::ScanScratch::default();
                for (label, surface) in scanner_surfaces {
                    let expected_hash = if is_no_space_surface(surface) {
                        form_hash(surface)
                    } else {
                        form_hash(&fold(surface))
                    };
                    let expected_end = surface.chars().count();
                    lexicon.scan_into_with_scratch(
                        surface,
                        &options,
                        &mut scanner_chars,
                        &mut scanner_hits,
                        &mut scanner_scratch,
                    );
                    let reaches_dispatch = scanner_hits.iter().any(|hit| {
                        hit.start == 0 && hit.end == expected_end && hit.form_hash == expected_hash
                    });
                    if !reaches_dispatch {
                        diagnostics.push(format!(
                            "{label}: {surface:?} does not produce a whole-surface scanner match"
                        ));
                    }
                }
            }
            Err(e) => diagnostics.push(format!(
                "internal lexicon fragment did not parse (report this as aterm bug): {e}"
            )),
        }
    }

    if !diagnostics.is_empty() {
        return Err(ToyPackError { diagnostics });
    }
    Ok(CompiledToyPack {
        metadata: ToyPackMetadata {
            id: raw.pack.id,
            name: raw.pack.name,
            version: raw.pack.version,
            authors: raw.pack.authors,
            license: raw.pack.license,
            description: raw.pack.description,
        },
        spec_table: table,
        lexicon_toml,
        toy_count: raw.toy.len(),
        word_count,
    })
}

fn toy_dispatch_hashes(word: &str) -> Vec<u64> {
    if is_no_space_surface(word) {
        return vec![form_hash(word)];
    }
    let base = fold(word);
    ["", "'s", "\u{2019}s", "'", "\u{2019}"]
        .into_iter()
        .map(|suffix| {
            let mut key = base.clone();
            key.push_str(suffix);
            form_hash(&key)
        })
        .collect()
}

fn validate_pack_metadata(raw: &RawToyPackMetadata, diagnostics: &mut Vec<String>) {
    if !valid_id(&raw.id, MAX_PACK_ID_BYTES, true) {
        diagnostics.push(format!(
            "pack.id: expected a namespaced lowercase ASCII id (for example \
             community.name.pack), 1..={MAX_PACK_ID_BYTES} bytes"
        ));
    }
    validate_text("pack.name", &raw.name, MAX_PACK_NAME_BYTES, diagnostics);
    if raw.version == 0 {
        diagnostics.push("pack.version: must be at least 1".to_string());
    }
    if raw.authors.is_empty() {
        diagnostics.push("pack.authors: at least one credit is required".to_string());
    }
    if raw.authors.len() > MAX_PACK_AUTHORS {
        diagnostics.push(format!(
            "pack.authors: {} credits exceed the maximum {MAX_PACK_AUTHORS}",
            raw.authors.len()
        ));
    }
    for (index, author) in raw.authors.iter().take(MAX_PACK_AUTHORS).enumerate() {
        validate_text(
            &format!("pack.authors[{index}]"),
            author,
            MAX_AUTHOR_BYTES,
            diagnostics,
        );
    }
    validate_text("pack.license", &raw.license, MAX_LICENSE_BYTES, diagnostics);
    if !raw
        .license
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '+' | '(' | ')' | ' '))
    {
        diagnostics.push(
            "pack.license: use an ASCII SPDX expression (for example Apache-2.0 or CC0-1.0)"
                .to_string(),
        );
    }
    if let Some(description) = raw.description.as_deref() {
        validate_text(
            "pack.description",
            description,
            MAX_PACK_DESCRIPTION_BYTES,
            diagnostics,
        );
    }
}

fn validate_text(label: &str, value: &str, max: usize, diagnostics: &mut Vec<String>) {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        diagnostics.push(format!(
            "{label}: must be non-empty, trimmed, control-free, and at most {max} UTF-8 bytes"
        ));
    }
}

fn valid_id(value: &str, max: usize, namespaced: bool) -> bool {
    let bytes = value.as_bytes();
    let edge_ok = bytes
        .first()
        .zip(bytes.last())
        .is_some_and(|(&first, &last)| {
            first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric()
        });
    edge_ok
        && bytes.len() <= max
        && (!namespaced || value.contains('.'))
        && !value.contains("..")
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b'.' | b'_' | b'-')
        })
}

fn valid_lang(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=MAX_LANG_BYTES).contains(&bytes.len())
        && bytes
            .first()
            .zip(bytes.last())
            .is_some_and(|(&first, &last)| {
                first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric()
            })
        && !value.contains("--")
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

fn validate_toy_word(word: &str, label: &str, diagnostics: &mut Vec<String>) -> bool {
    let mut valid = true;
    if word.is_empty() || word.trim() != word {
        diagnostics.push(format!(
            "{label}: must be non-empty with no leading/trailing whitespace"
        ));
        valid = false;
    }
    if word.len() > MAX_TOY_WORD_BYTES {
        diagnostics.push(format!(
            "{label}: {} bytes exceed the maximum {MAX_TOY_WORD_BYTES}",
            word.len()
        ));
        valid = false;
    }
    if word.chars().any(char::is_control) {
        diagnostics.push(format!("{label}: control characters are not allowed"));
        valid = false;
    }
    if is_no_space_surface(word) && word.chars().count() == 1 {
        diagnostics.push(format!(
            "{label}: single-character no-space words require a host scan opt-in; \
             strict packs must use a compound of at least two characters"
        ));
        valid = false;
    }
    valid
}

fn compile_toy_spec(
    toy: &RawToy,
    context: &str,
    diagnostics: &mut Vec<String>,
) -> Option<WordEffectSpec> {
    let before = diagnostics.len();
    let ink = toy
        .ink
        .as_ref()
        .map(|raw| compile_toy_ink(raw, context, diagnostics));
    let burst = toy
        .burst
        .as_ref()
        .map(|raw| compile_toy_burst(raw, context, diagnostics));
    let graphic = toy
        .graphic
        .as_ref()
        .map(|raw| compile_toy_graphic(raw, context, diagnostics));
    let spec = WordEffectSpec {
        graphic,
        ink,
        burst,
    };
    if spec == WordEffectSpec::default() {
        diagnostics.push(format!(
            "{context}: at least one of ink, burst, or graphic is required"
        ));
    }
    (diagnostics.len() == before).then_some(spec)
}

fn compile_toy_ink(raw: &RawToyPackInk, context: &str, diagnostics: &mut Vec<String>) -> InkSpec {
    let label = format!("{context}.ink");
    let colorway = match raw.kind.as_str() {
        "rainbow" => {
            forbid(&raw.colors, &label, "colors", "rainbow", diagnostics);
            forbid(&raw.lift, &label, "lift", "rainbow", diagnostics);
            forbid(&raw.amplitude, &label, "amplitude", "rainbow", diagnostics);
            forbid(&raw.window_ms, &label, "window_ms", "rainbow", diagnostics);
            Colorway::Rainbow {
                sat: bounded_f32(
                    raw.saturation,
                    RAINBOW_SAT,
                    0.0,
                    1.0,
                    &label,
                    "saturation",
                    diagnostics,
                ),
                val: bounded_f32(
                    raw.value,
                    RAINBOW_VAL,
                    0.0,
                    1.0,
                    &label,
                    "value",
                    diagnostics,
                ),
                span_deg: bounded_f32(
                    raw.span_degrees,
                    RAINBOW_SPAN_DEG,
                    0.0,
                    360.0,
                    &label,
                    "span_degrees",
                    diagnostics,
                ),
                drift_ms: bounded_u32(
                    raw.drift_ms,
                    RAINBOW_DRIFT_MS,
                    MIN_ANIMATION_MS,
                    MAX_ANIMATION_MS,
                    &label,
                    "drift_ms",
                    diagnostics,
                ),
            }
        }
        "two_tone" => {
            forbid(
                &raw.saturation,
                &label,
                "saturation",
                "two_tone",
                diagnostics,
            );
            forbid(&raw.value, &label, "value", "two_tone", diagnostics);
            forbid(
                &raw.span_degrees,
                &label,
                "span_degrees",
                "two_tone",
                diagnostics,
            );
            forbid(&raw.drift_ms, &label, "drift_ms", "two_tone", diagnostics);
            forbid(&raw.lift, &label, "lift", "two_tone", diagnostics);
            forbid(&raw.amplitude, &label, "amplitude", "two_tone", diagnostics);
            forbid(&raw.window_ms, &label, "window_ms", "two_tone", diagnostics);
            let (c0, c1) = match raw.colors.as_ref() {
                None => {
                    diagnostics.push(format!(
                        "{label}.colors: two_tone requires exactly two #RRGGBB colours"
                    ));
                    (0, 0)
                }
                Some(colors) => (
                    strict_pack_hex(&colors[0], &label, 0, diagnostics),
                    strict_pack_hex(&colors[1], &label, 1, diagnostics),
                ),
            };
            Colorway::TwoTone { c0, c1 }
        }
        "self_glow" => {
            forbid(&raw.colors, &label, "colors", "self_glow", diagnostics);
            forbid(
                &raw.saturation,
                &label,
                "saturation",
                "self_glow",
                diagnostics,
            );
            forbid(&raw.value, &label, "value", "self_glow", diagnostics);
            forbid(
                &raw.span_degrees,
                &label,
                "span_degrees",
                "self_glow",
                diagnostics,
            );
            forbid(&raw.drift_ms, &label, "drift_ms", "self_glow", diagnostics);
            Colorway::SelfGlow {
                lift: bounded_f32(raw.lift, 0.72, 0.0, 1.0, &label, "lift", diagnostics),
                amp: bounded_f32(
                    raw.amplitude,
                    0.55,
                    0.0,
                    1.0,
                    &label,
                    "amplitude",
                    diagnostics,
                ),
                window_ms: bounded_u32(
                    raw.window_ms,
                    1400,
                    MIN_ANIMATION_MS,
                    MAX_ANIMATION_MS,
                    &label,
                    "window_ms",
                    diagnostics,
                ),
            }
        }
        other => {
            diagnostics.push(format!(
                "{label}.kind: unknown {other:?}; expected rainbow|two_tone|self_glow"
            ));
            rainbow_ink().colorway
        }
    };
    // Pack recipes are structurally one-shot. A future schema may add looping
    // ink with an explicit wake/performance contract; v1 always settles.
    InkSpec {
        colorway,
        sweep_once: true,
    }
}

fn compile_toy_burst(
    raw: &RawToyPackBurst,
    context: &str,
    diagnostics: &mut Vec<String>,
) -> BurstSpec {
    let label = format!("{context}.burst");
    let kind = match raw.kind.as_str() {
        "sparkle" => BurstKind::Sparkle,
        "nova" => BurstKind::Nova,
        "supernova" => BurstKind::SuperNova,
        "starburst" => BurstKind::Starburst,
        "glow" => BurstKind::Glow,
        other => {
            diagnostics.push(format!(
                "{label}.kind: unknown {other:?}; expected sparkle|nova|supernova|starburst|glow"
            ));
            BurstKind::Glow
        }
    };
    let chance = raw.chance_pct.unwrap_or(100);
    if chance > 100 {
        diagnostics.push(format!("{label}.chance_pct: {chance} is outside 0..=100"));
    }
    BurstSpec {
        kind,
        chance_pct: chance.min(100) as u8,
    }
}

fn compile_toy_graphic(
    raw: &RawToyPackGraphic,
    context: &str,
    diagnostics: &mut Vec<String>,
) -> GraphicSpec {
    let label = format!("{context}.graphic");
    if raw.collection != "cats" {
        diagnostics.push(format!(
            "{label}.collection: unknown {:?}; schema v1 supports only cats",
            raw.collection
        ));
    }
    let [lo, hi] = raw.dwell_ms.unwrap_or([2200, 3598]);
    if lo < MIN_ANIMATION_MS || hi > MAX_CAT_DWELL_MS || lo > hi {
        diagnostics.push(format!(
            "{label}.dwell_ms: expected [lo, hi] with {MIN_ANIMATION_MS} <= lo <= hi <= {MAX_CAT_DWELL_MS}"
        ));
    }
    GraphicSpec {
        collection: Collection::Cats,
        dwell_ms: (lo, hi),
    }
}

fn forbid<T>(
    value: &Option<T>,
    label: &str,
    field: &str,
    kind: &str,
    diagnostics: &mut Vec<String>,
) {
    if value.is_some() {
        diagnostics.push(format!("{label}.{field}: not valid for kind {kind:?}"));
    }
}

#[allow(clippy::too_many_arguments)]
fn bounded_f32(
    value: Option<f32>,
    default: f32,
    lo: f32,
    hi: f32,
    label: &str,
    field: &str,
    diagnostics: &mut Vec<String>,
) -> f32 {
    match value {
        Some(value) if value.is_finite() && (lo..=hi).contains(&value) => value,
        Some(value) => {
            diagnostics.push(format!(
                "{label}.{field}: {value:?} is outside {lo}..={hi} or non-finite"
            ));
            default
        }
        None => default,
    }
}

#[allow(clippy::too_many_arguments)]
fn bounded_u32(
    value: Option<u32>,
    default: u32,
    lo: u32,
    hi: u32,
    label: &str,
    field: &str,
    diagnostics: &mut Vec<String>,
) -> u32 {
    match value {
        Some(value) if (lo..=hi).contains(&value) => value,
        Some(value) => {
            diagnostics.push(format!("{label}.{field}: {value} is outside {lo}..={hi}"));
            default
        }
        None => default,
    }
}

fn strict_pack_hex(value: &str, label: &str, index: usize, diagnostics: &mut Vec<String>) -> u32 {
    if value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return u32::from_str_radix(&value[1..], 16).unwrap_or(0);
    }
    diagnostics.push(format!(
        "{label}.colors[{index}]: expected exactly #RRGGBB, got {value:?}"
    ));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_TRIUMPHS: &str = include_str!("../toy-packs/community/tiny-triumphs/pack.toml");

    #[test]
    fn every_checked_in_community_toy_pack_compiles() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("toy-packs")
            .join("community");
        let mut manifests = Vec::new();
        for entry in std::fs::read_dir(&root).expect("read community toy-pack directory") {
            let path = entry.expect("read community toy-pack entry").path();
            if !path.is_dir() {
                continue;
            }
            let manifest = path.join("pack.toml");
            assert!(
                manifest.is_file(),
                "community Toy Pack directory {} must contain a regular pack.toml",
                path.display()
            );
            manifests.push(manifest);
        }
        manifests.sort();
        assert!(
            !manifests.is_empty(),
            "the community gallery has an example"
        );
        for manifest in manifests {
            let source = read_toy_pack_file(&manifest).expect("read pack manifest");
            compile_toy_pack_toml(&source)
                .unwrap_or_else(|error| panic!("{}:\n{error}", manifest.display()));
        }
    }

    #[test]
    fn checked_in_toy_pack_compiles_through_real_dispatch_and_scanner() {
        let pack = compile_toy_pack_toml(TINY_TRIUMPHS).expect("example pack compiles");
        assert_eq!(pack.metadata().id, "community.aterm.tiny-triumphs");
        assert_eq!(pack.toy_count(), 3);
        assert_eq!(pack.word_count(), 5);

        let shipped = pack
            .spec_table()
            .override_for(form_hash("shipped"))
            .expect("shipped recipe registered");
        assert!(matches!(
            shipped.ink,
            Some(InkSpec {
                colorway: Colorway::TwoTone {
                    c0: 0x0058_D8FF,
                    c1: 0x00FF_73B9
                },
                sweep_once: true
            })
        ));
        assert_eq!(
            shipped.burst,
            Some(BurstSpec {
                kind: BurstKind::Starburst,
                chance_pct: 35
            })
        );
        let purrfect = pack
            .spec_table()
            .override_for(form_hash("purrfect"))
            .expect("cat recipe registered");
        assert!(purrfect.graphic.is_some());
        assert!(matches!(
            purrfect.ink,
            Some(InkSpec {
                colorway: Colorway::SelfGlow {
                    window_ms: 1350,
                    ..
                },
                sweep_once: true
            })
        ));

        let lexicon =
            aterm_lexicon::Lexicon::with_languages_and_override(&["en"], Some(pack.lexicon_toml()))
                .expect("compiled fragment parses");
        let matches = lexicon.scan(
            "shipit, then deepthink, that review was purrfect",
            &aterm_lexicon::ScanOptions::default(),
        );
        assert_eq!(
            matches.len(),
            3,
            "all pack surfaces reach the real scanner: {matches:?}"
        );
    }

    #[test]
    fn toy_pack_schema_is_strict_about_unknown_fields_and_values() {
        let unknown_field =
            TINY_TRIUMPHS.replacen("chance_pct = 35", "chance_pct = 35, surprise = true", 1);
        let error = compile_toy_pack_toml(&unknown_field).expect_err("unknown field rejected");
        assert!(
            error.to_string().contains("unknown field `surprise`"),
            "{error}"
        );

        let unknown_kind =
            TINY_TRIUMPHS.replacen("kind = \"starburst\"", "kind = \"confetti_cannon\"", 1);
        let error = compile_toy_pack_toml(&unknown_kind).expect_err("unknown effect rejected");
        assert!(error.to_string().contains("confetti_cannon"), "{error}");

        let bad_color = TINY_TRIUMPHS.replacen("#58D8FF", "58D8FF", 1);
        let error = compile_toy_pack_toml(&bad_color).expect_err("non-canonical color rejected");
        assert!(
            error.to_string().contains("expected exactly #RRGGBB"),
            "{error}"
        );

        let unscannable = TINY_TRIUMPHS.replacen("\"shipit\"", "\"ship it\"", 1);
        let error = compile_toy_pack_toml(&unscannable).expect_err("multi-word trigger rejected");
        assert!(
            error
                .to_string()
                .contains("not a single scannable whole-word"),
            "{error}"
        );

        // 猫背 (stoop) is a production exceptions.toml suppression. (This
        // fixture used 熊猫 until 2026-08-10, when the panda graduated from
        // exception to first-class animal form and stopped erroring here.)
        let cjk_exception = TINY_TRIUMPHS.replacen("\"shipit\"", "\"猫背\"", 1);
        let error = compile_toy_pack_toml(&cjk_exception)
            .expect_err("a production-scanner CJK exception cannot become a trigger");
        assert!(
            error
                .to_string()
                .contains("does not produce a whole-surface scanner match"),
            "{error}"
        );
    }

    #[test]
    fn toy_pack_semantics_collect_version_identity_budget_and_timing_errors() {
        let bad = r##"
schema = 7
[pack]
id = "not_namespaced"
name = "Bad Pack"
version = 0
authors = []
license = "Apache-2.0"

[[toy]]
id = "same"
lang = "x"
words = ["duplicate", " trailing "]
ink = { kind = "rainbow", drift_ms = 20, colors = ["#000000", "#ffffff"] }
burst = { kind = "glow", chance_pct = 101 }
graphic = { collection = "dragons", dwell_ms = [4000, 3000] }

[[toy]]
id = "same"
lang = "en"
words = ["DUPLICATE"]
ink = { kind = "self_glow", window_ms = 9000 }
"##;
        let error = compile_toy_pack_toml(bad).expect_err("semantic errors rejected");
        let joined = error.diagnostics().join("\n");
        for expected in [
            "unsupported version 7",
            "namespaced lowercase ASCII id",
            "pack.version: must be at least 1",
            "pack.authors: at least one",
            "duplicate recipe id",
            "normalized surface duplicates",
            "BCP47-like",
            "leading/trailing whitespace",
            "not valid for kind \"rainbow\"",
            "drift_ms: 20 is outside",
            "chance_pct: 101",
            "supports only cats",
            "expected [lo, hi]",
            "window_ms: 9000 is outside",
        ] {
            assert!(
                joined.contains(expected),
                "missing {expected:?} in:\n{joined}"
            );
        }
    }

    #[test]
    fn toy_pack_source_cap_precedes_toml_parse() {
        let oversized = "x".repeat(MAX_TOY_PACK_BYTES + 1);
        let error = compile_toy_pack_toml(&oversized).expect_err("oversize source rejected");
        assert!(error.to_string().contains("maximum"), "{error}");
        assert!(
            error.to_string().contains(&MAX_TOY_PACK_BYTES.to_string()),
            "{error}"
        );
    }

    #[test]
    fn toy_pack_file_read_is_regular_utf8_and_bounded() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "aterm-toy-pack-reader-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create reader fixture directory");

        let regular = root.join("regular.toml");
        std::fs::write(&regular, TINY_TRIUMPHS).expect("write regular fixture");
        assert_eq!(
            read_toy_pack_file(&regular).expect("read regular manifest"),
            TINY_TRIUMPHS
        );

        let directory_error = read_toy_pack_file(&root).expect_err("directory rejected");
        assert_eq!(directory_error.kind(), std::io::ErrorKind::InvalidInput);

        let invalid_utf8 = root.join("invalid-utf8.toml");
        std::fs::write(&invalid_utf8, [0xff]).expect("write invalid UTF-8 fixture");
        let utf8_error = read_toy_pack_file(&invalid_utf8).expect_err("invalid UTF-8 rejected");
        assert_eq!(utf8_error.kind(), std::io::ErrorKind::InvalidData);

        let oversized = root.join("oversized.toml");
        std::fs::write(&oversized, vec![b'x'; MAX_TOY_PACK_BYTES + 32])
            .expect("write oversized fixture");
        let oversized_error =
            read_toy_pack_file(&oversized).expect_err("oversized manifest rejected by reader");
        assert_eq!(oversized_error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            oversized_error
                .to_string()
                .contains(&MAX_TOY_PACK_BYTES.to_string()),
            "{oversized_error}"
        );

        std::fs::remove_dir_all(root).expect("remove reader fixtures");
    }

    #[test]
    fn toy_pack_rejects_possessive_dispatch_collisions() {
        let colliding = TINY_TRIUMPHS.replacen(
            "words = [\"shipit\", \"shipped\"]",
            "words = [\"shipit\", \"shipit's\"]",
            1,
        );
        let error = compile_toy_pack_toml(&colliding)
            .expect_err("base and possessive surface must not share a dispatch key");
        assert!(
            error.to_string().contains("dispatch key collides")
                && error.to_string().contains("possessive variants"),
            "{error}"
        );
    }

    #[test]
    fn toy_pack_recipe_word_count_is_bounded() {
        let words = (0..=MAX_WORDS_PER_TOY)
            .map(|index| format!("\"toyword{index}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!(
            "schema = 1\n\
             [pack]\n\
             id = \"community.test.bounded\"\n\
             name = \"Bounded\"\n\
             version = 1\n\
             authors = [\"Test Author\"]\n\
             license = \"Apache-2.0\"\n\
             [[toy]]\n\
             id = \"too-many\"\n\
             lang = \"en\"\n\
             words = [{words}]\n\
             burst = {{ kind = \"glow\" }}\n"
        );
        let error = compile_toy_pack_toml(&source).expect_err("word cap rejected");
        assert!(error.to_string().contains("per-recipe maximum"), "{error}");
    }

    #[test]
    fn custom_fragment_parses_and_keys_all_hash_variants() {
        let frag = r#"
[[sparkle_words.custom]]
words = ["Ultrathink"]
ink = { colorway = "rainbow" }

[[sparkle_words.custom]]
words = ["猫神"]
ink = "twotone:#ff0000,#0000ff"
burst = { kind = "starburst", chance = 250 }
graphic = { collection = "cats" }
"#;
        let entries = parse_custom_toml(frag).expect("fragment parses");
        let (table, lex_frag) = build_custom(&entries);
        assert!(table.has_custom());

        // Spaced word: keyed by the FOLDED hash + 4 possessive variants.
        let base = table
            .override_for(form_hash("ultrathink"))
            .expect("folded base hash registered");
        assert!(matches!(
            base.ink,
            Some(InkSpec {
                colorway: Colorway::Rainbow { .. },
                ..
            })
        ));
        for suf in ["'s", "\u{2019}s", "'", "\u{2019}"] {
            let mut k = String::from("ultrathink");
            k.push_str(suf);
            assert!(
                table.override_for(form_hash(&k)).is_some(),
                "possessive variant {k:?} registered"
            );
        }
        // Raw (cased) TOML text is NOT the key — hashing it would no-op.
        assert!(table.override_for(form_hash("Ultrathink")).is_none());

        // CJK word: RAW hash; chance clamps to 100.
        let cjk = table
            .override_for(form_hash("猫神"))
            .expect("raw CJK hash registered");
        assert_eq!(
            cjk.ink,
            Some(InkSpec {
                colorway: Colorway::TwoTone {
                    c0: 0x00FF_0000,
                    c1: 0x0000_00FF
                },
                sweep_once: true
            })
        );
        assert_eq!(
            cjk.burst,
            Some(BurstSpec {
                kind: BurstKind::Starburst,
                chance_pct: 100
            })
        );
        assert!(cjk.graphic.is_some());

        // The synthesized lexicon fragment carries both entry shapes.
        assert!(lex_frag.contains("forms = [\"Ultrathink\"]"), "{lex_frag}");
        assert!(lex_frag.contains("cjk = true"), "{lex_frag}");
        assert!(lex_frag.contains("forms = [\"猫神\"]"), "{lex_frag}");

        // The fragment round-trips through the REAL lexicon builder and both
        // surfaces scan (the CJK one via the cjk=true entry — the silent-drop
        // gap this module closes).
        let lx = aterm_lexicon::Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
            .expect("synthesized fragment parses");
        let o = aterm_lexicon::ScanOptions::default();
        assert_eq!(lx.scan("go ultrathink now", &o).len(), 1);
        assert_eq!(lx.scan("これは猫神です", &o).len(), 1);
    }

    #[test]
    fn custom_twotone_requires_exactly_two_colors_and_runtime_falls_back() {
        let valid = "twotone:#112233,#445566";
        let extra = "twotone:#112233,#445566,#778899";
        assert!(custom_colorway_is_valid(valid));
        assert!(!custom_colorway_is_valid(extra));
        assert!(!custom_colorway_is_valid("twotone:#112233"));

        assert!(matches!(
            parse_colorway(valid),
            Colorway::TwoTone {
                c0: 0x0011_2233,
                c1: 0x0044_5566,
            }
        ));
        assert!(matches!(parse_colorway(extra), Colorway::Rainbow { .. }));
    }

    #[test]
    fn spec_table_overlay_is_deterministic_and_later_wins() {
        let spec = |kind| WordEffectSpec {
            burst: Some(BurstSpec {
                kind,
                chance_pct: 100,
            }),
            ..WordEffectSpec::default()
        };
        let mut base = SpecTable::default();
        base.insert_custom("shared", spec(BurstKind::Sparkle));
        base.insert_custom("base-only", spec(BurstKind::Nova));
        let mut later = SpecTable::default();
        later.insert_custom("shared", spec(BurstKind::Glow));
        later.insert_custom("later-only", spec(BurstKind::Starburst));

        base.overlay(later);
        let kind = |word: &str| {
            base.override_for(form_hash(&fold(word)))
                .and_then(|resolved| resolved.burst)
                .map(|burst| burst.kind)
        };
        assert_eq!(kind("shared"), Some(BurstKind::Glow));
        assert_eq!(kind("shared's"), Some(BurstKind::Glow));
        assert_eq!(kind("base-only"), Some(BurstKind::Nova));
        assert_eq!(kind("later-only"), Some(BurstKind::Starburst));
    }

    /// FIX III regression: unscannable custom words are not silently
    /// accepted. A single-char CJK surface (dropped at scan unless
    /// `cjk_single_char = true`) and a mixed-script surface (dropped at
    /// lexicon insert) both come back on the lexicon `conflicts` channel
    /// when the synthesized fragment is built — the channel the native
    /// resolver logs.
    #[test]
    fn unscannable_custom_words_warn_on_the_conflicts_channel() {
        let frag = r#"
[[sparkle_words.custom]]
words = ["犬", "abc猫"]
ink = { colorway = "rainbow" }
"#;
        let entries = parse_custom_toml(frag).expect("fragment parses");
        let (table, lex_frag) = build_custom(&entries);
        // Both words still register override hashes (the single-char one
        // scans under the opt-in; fail-open posture for the other).
        assert!(table.has_custom());
        assert!(table.override_for(form_hash("犬")).is_some());
        let lx = aterm_lexicon::Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
            .expect("synthesized fragment parses");
        assert!(
            lx.conflicts()
                .iter()
                .any(|c| c.contains("\"犬\"") && c.contains("requires cjk_single_char = true")),
            "single-char CJK custom word warns, got {:?}",
            lx.conflicts()
        );
        assert!(
            lx.conflicts()
                .iter()
                .any(|c| c.contains("\"abc猫\"") && c.contains("dropped")),
            "mixed-script custom word warns dropped, got {:?}",
            lx.conflicts()
        );
    }

    #[test]
    fn empty_spec_and_malformed_fragment_fail_safely() {
        // An entry with no axes registers nothing.
        let entries =
            parse_custom_toml("[[sparkle_words.custom]]\nwords = [\"zzz\"]\n").expect("parses");
        let (table, _) = build_custom(&entries);
        assert!(!table.has_custom());
        // Malformed TOML is an Err (caller warns and keeps previous specs).
        assert!(parse_custom_toml("[[sparkle_words.custom]").is_err());
        // A missing table is just zero entries.
        assert!(parse_custom_toml("").expect("empty ok").is_empty());
    }

    #[test]
    fn legacy_inline_customs_keep_their_lenient_compatibility_semantics() {
        let entries = parse_custom_toml(
            "[[sparkle_words.custom]]\nwords = [\"legacy\"]\n\
             ink = { colorway = \"unknown-old-value\" }\n\
             burst = { kind = \"unknown-old-value\", chance = 900 }\n\
             graphic = { collection = \"unknown-old-value\" }\n",
        )
        .expect("legacy fragment still parses");
        let spec = entries[0].to_spec();
        assert!(matches!(
            spec.ink,
            Some(InkSpec {
                colorway: Colorway::Rainbow { .. },
                ..
            })
        ));
        assert_eq!(
            spec.burst,
            Some(BurstSpec {
                kind: BurstKind::Starburst,
                chance_pct: 100
            })
        );
        assert_eq!(
            spec.graphic,
            Some(GraphicSpec {
                collection: Collection::Cats,
                dwell_ms: (2200, 3598)
            })
        );
    }

    #[test]
    fn consumer_capabilities_name_only_actual_shared_tuning_consumers() {
        let mut table = SpecTable::default();
        let burst = |kind, chance_pct| WordEffectSpec {
            burst: Some(BurstSpec { kind, chance_pct }),
            ..WordEffectSpec::default()
        };

        // Negative controls: these are genuine admitted custom recipes, but
        // none consumes Nova magic, legacy sparkle tuning, or global ink
        // strength/sweep timing.
        table.insert_custom(
            "graphic-only",
            WordEffectSpec {
                graphic: Some(GraphicSpec {
                    collection: Collection::Cats,
                    dwell_ms: (2_200, 3_598),
                }),
                ..WordEffectSpec::default()
            },
        );
        table.insert_custom("glow", burst(BurstKind::Glow, 100));
        table.insert_custom("supernova", burst(BurstKind::SuperNova, 100));
        table.insert_custom("rolled-off-sparkle", burst(BurstKind::Sparkle, 0));
        table.insert_custom(
            "self-glow",
            WordEffectSpec {
                ink: Some(InkSpec {
                    colorway: Colorway::SelfGlow {
                        lift: 0.2,
                        amp: 0.3,
                        window_ms: 800,
                    },
                    sweep_once: true,
                }),
                ..WordEffectSpec::default()
            },
        );
        assert_eq!(
            table.consumer_capabilities(),
            SpecConsumerCapabilities::default(),
            "unrelated admitted recipes must not make shared controls look live"
        );

        table.insert_custom("nova", burst(BurstKind::Nova, 1));
        table.insert_custom("sparkle", burst(BurstKind::Sparkle, 1));
        table.insert_custom("starburst", burst(BurstKind::Starburst, 100));
        table.insert_custom(
            "rainbow",
            WordEffectSpec {
                ink: Some(rainbow_ink()),
                ..WordEffectSpec::default()
            },
        );
        table.insert_custom(
            "nova-twotone-no-burst",
            WordEffectSpec {
                ink: Some(InkSpec {
                    colorway: Colorway::TwoTone {
                        c0: 0x0011_2233,
                        c1: 0x0044_5566,
                    },
                    // A custom recipe owns this choice. It does not become a
                    // consumer of the global `ink.loop` setting.
                    sweep_once: false,
                }),
                burst: Some(BurstSpec {
                    kind: BurstKind::Nova,
                    chance_pct: 0,
                }),
                ..WordEffectSpec::default()
            },
        );
        assert_eq!(
            table.consumer_capabilities(),
            SpecConsumerCapabilities {
                nova_burst: true,
                sparkle_or_starburst_burst: true,
                rainbow_ink: true,
                twotone_ink: true,
                nova_twotone_ink: true,
                emphasis_class_default: false,
            }
        );

        // An inline override can make an imported pack recipe unreachable.
        // The arena keeps both specs for compact ids, but capability projection
        // follows the live dispatch map and therefore drops the shadowed burst.
        let mut imported = SpecTable::default();
        imported.insert_custom("shared", burst(BurstKind::Sparkle, 100));
        let mut inline = SpecTable::default();
        inline.insert_custom(
            "shared",
            WordEffectSpec {
                graphic: Some(GraphicSpec {
                    collection: Collection::Cats,
                    dwell_ms: (2_200, 3_598),
                }),
                ..WordEffectSpec::default()
            },
        );
        imported.overlay(inline);
        assert_eq!(
            imported.consumer_capabilities(),
            SpecConsumerCapabilities::default(),
            "a fully shadowed Toy Pack burst is not an admitted consumer"
        );
    }
}
