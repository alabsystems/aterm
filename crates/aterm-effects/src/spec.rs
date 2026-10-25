// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sparkle-words spec framework: new sparkle words are data, not code.

use aterm_hash::FxHashMap;
use aterm_lexicon::{fold, form_hash, is_no_space_surface};

/// One word's composed effect over the three axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordEffectSpec {
    /// Peek-up sprite show.
    pub graphic: Option<GraphicSpec>,
    /// Glyph colorway.
    pub ink: Option<InkSpec>,
    /// Light show.
    pub burst: Option<BurstSpec>,
}

impl WordEffectSpec {
    /// Silent effect spec with all three channels disabled (`None`).
    pub const NONE: Self = Self {
        graphic: None,
        ink: None,
        burst: None,
    };
}

impl Default for WordEffectSpec {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
}

/// The graphic axis: which sprite collection peeks, and its dwell range (ms).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphicSpec {
    pub collection: Collection,
    /// Dwell range `(lo, hi)` in ms; the genome quantizes inside it (the cat engine's 4-step dwell decode generalized).
    pub dwell_ms: (u32, u32),
}

/// Sprite collections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Collection {
    Cats,
    /// The ambient animal roster: the species-keyed peeking head.
    Animals,
}

/// The ink axis: a colorway + whether the animated intro runs once (`sweep_once = false` keeps re-sweeping while visible, the `ink_loop` semantics).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InkSpec {
    pub colorway: Colorway,
    pub sweep_once: bool,
}

/// Glyph colorways.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Colorway {
    /// The two-tone gradient + one specular sweep.
    TwoTone { c0: u32, c1: u32 },
    /// The rainbow: per-lead-cell hue over `span_deg` (clamped to `min(span_deg, 100°·(lead_cells − 1))`), one exact full-cycle temporal drift over `drift_ms`, then a byte-stable freeze at the t = 0 phase.
    Rainbow {
        sat: f32,
        val: f32,
        span_deg: f32,
        drift_ms: u32,
    },
    /// The feline self-terminating glow in the word's own fg.
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
    /// The randomized self-terminating sparkle.
    Sparkle,
    /// The classic nova (dip → flash → ring + rays → debris → ember).
    Nova,
    /// The supernova (charge → theme-branched detonation → double shockwave → rainbow debris → afterglow).
    SuperNova,
    /// A dense one-shot star burst (the sparkle machinery, burst-shaped).
    Starburst,
    /// A soft additive glow pulse over the word.
    Glow,
}

/// Rainbow defaults (the `ProfanityStyle::Rainbow` class default and the bare `colorway = "rainbow"` custom shorthand).
pub const RAINBOW_SAT: f32 = 0.85;
pub const RAINBOW_VAL: f32 = 1.0;
pub const RAINBOW_SPAN_DEG: f32 = 300.0;
pub const RAINBOW_DRIFT_MS: u32 = 2500;

/// The default rainbow ink spec.
#[inline]
#[must_use]
pub const fn rainbow_ink() -> InkSpec {
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
/// Source-size ceiling checked before TOML parsing, bounding cold-path parser work and allocation for an untrusted pack.
pub const MAX_TOY_PACK_BYTES: usize = 256 * 1024;
/// Maximum recipes in one pack.
pub const MAX_TOYS_PER_PACK: usize = 128;
/// Maximum word surfaces attached to one recipe.
pub const MAX_WORDS_PER_TOY: usize = 64;
/// Maximum word surfaces across one pack.
pub const MAX_WORDS_PER_PACK: usize = 4096;
/// Maximum UTF-8 byte length of one word surface.
pub const MAX_TOY_WORD_BYTES: usize = 128;

/// Read one Toy Pack manifest without permitting special files or unbounded allocation.
#[inline]
pub fn read_toy_pack_file(path: &std::path::Path) -> std::io::Result<String> {
    crate::file_feed::read_bounded_regular_utf8(path, MAX_TOY_PACK_BYTES)
}

/// The dispatch table on the resolved config: registered custom specs plus the `form_hash → SpecId` override map.
#[derive(Clone, Debug, Default)]
pub struct SpecTable {
    specs: Vec<WordEffectSpec>,
    /// `form_hash` as the scanner computes it (of the folded token, or of the raw text of a
    /// no-space surface) → spec index.
    overrides: FxHashMap<u64, SpecId>,
}

/// Which shared runtime tunings have at least one admitted word-effect consumer, derived from the compiled [`SpecTable`]: graphic-only, Glow, SuperNova and zero-chance recipes do not make unrelated controls effective.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpecConsumerCapabilities {
    /// A positive-chance classic Nova burst consumes the shared magic toggle.
    pub nova_burst: bool,
    /// A positive-chance Sparkle or Starburst consumes palette/density/timing/jitter.
    pub sparkle_or_starburst_burst: bool,
    /// At least one custom Rainbow ink axis consumes shared ink strength.
    pub rainbow_ink: bool,
    /// At least one custom TwoTone ink axis consumes strength and sweep timing.
    pub twotone_ink: bool,
    /// A Nova + TwoTone composition also consumes Nova magic through its ink palette, even when that recipe's burst chance is zero.
    pub nova_twotone_ink: bool,
    /// At least one scannable, non-ignored Emphasis surface reaches the class default rather than a custom override.
    pub emphasis_class_default: bool,
}

impl SpecConsumerCapabilities {
    /// Empty capability set with all consumer features disabled.
    pub const NONE: Self = Self {
        nova_burst: false,
        sparkle_or_starburst_burst: false,
        rainbow_ink: false,
        twotone_ink: false,
        nova_twotone_ink: false,
        emphasis_class_default: false,
    };
}

impl Default for SpecConsumerCapabilities {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
}

impl SpecTable {
    /// Whether any custom spec is registered: a term of the emphasis-class resolve gate (`enabled && (ink_enabled || has_custom_specs)`).
    #[inline]
    #[must_use]
    pub const fn has_custom(&self) -> bool {
        !self.specs.is_empty()
    }

    /// Exact shared-setting consumers in this compiled table, after any Toy Pack overlay.
    #[must_use]
    pub fn consumer_capabilities(&self) -> SpecConsumerCapabilities {
        let mut capabilities = SpecConsumerCapabilities::default();
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

    /// The per-word override for a scan match, if any; it wins over the class default whatever the match's class.
    #[inline]
    #[must_use]
    pub fn override_for(&self, form_hash: u64) -> Option<&WordEffectSpec> {
        self.overrides
            .get(&form_hash)
            .and_then(|id| self.specs.get(usize::from(*id)))
    }

    fn intern_spec(&mut self, spec: WordEffectSpec) -> Option<SpecId> {
        if spec == WordEffectSpec::default() {
            return None;
        }
        if let Some(i) = self.specs.iter().position(|s| *s == spec) {
            Some(i as SpecId)
        } else if self.specs.len() >= usize::from(SpecId::MAX) {
            // defensive cap; unreachable for sane configs
            None
        } else {
            self.specs.push(spec);
            Some((self.specs.len() - 1) as SpecId)
        }
    }

    fn insert_custom_with_id(&mut self, word: &str, id: SpecId) {
        if is_no_space_surface(word) {
            // CJK path: the scanner hashes the RAW compound.
            self.overrides.insert(form_hash(word), id);
        } else {
            let mut base = fold(word);
            if base.is_empty() {
                return;
            }
            self.overrides.insert(form_hash(&base), id);
            let base_len = base.len();
            for suf in ["'s", "\u{2019}s", "'", "\u{2019}"] {
                base.truncate(base_len);
                base.push_str(suf);
                self.overrides.insert(form_hash(&base), id);
            }
        }
    }

    /// Register one custom spec for `word` (all surface-hash variants).
    pub fn insert_custom(&mut self, word: &str, spec: WordEffectSpec) {
        let word = word.trim();
        if word.is_empty() {
            return;
        }
        let Some(id) = self.intern_spec(spec) else {
            return;
        };
        self.insert_custom_with_id(word, id);
    }

    /// Overlay another table onto this one.
    pub fn overlay(&mut self, other: SpecTable) {
        let mut remap = Vec::with_capacity(other.specs.len());
        for spec in other.specs {
            remap.push(self.intern_spec(spec));
        }
        for (form_hash, old_id) in other.overrides {
            if let Some(new_id) = remap.get(usize::from(old_id)).copied().flatten() {
                self.overrides.insert(form_hash, new_id);
            }
        }
    }
}

/// One raw `[[sparkle_words.custom]]` entry, serde-shaped so the native `Config` deserializer embeds it directly:
///
/// ```toml
/// [[sparkle_words.custom]]
/// words   = ["ultrathink", "gg", "猫神"]
/// ink     = { colorway = "rainbow" }        # or "twotone:#RRGGBB,#RRGGBB"
/// burst   = { kind = "starburst", chance = 10 }
/// graphic = { collection = "cats" }
/// ```
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct RawCustomEntry {
    pub words: Vec<String>,
    pub ink: Option<RawInk>,
    pub burst: Option<RawBurst>,
    pub graphic: Option<RawGraphic>,
}

/// The `ink` field: a shorthand string (`"rainbow"` / `"twotone:#RRGGBB,#RRGGBB"`) or a `{ colorway = "…" }` table.
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
#[inline]
fn parse_hex(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}

/// Whether a custom ink colorway is one of the two runtime-supported shapes.
#[inline]
#[must_use]
pub fn custom_colorway_is_valid(s: &str) -> bool {
    let s = s.trim();
    s == "rainbow" || parse_two_tone(s).is_some()
}

#[inline]
fn parse_two_tone(s: &str) -> Option<(u32, u32)> {
    let rest = s.strip_prefix("twotone:")?;
    let mut colors = rest.split(',');
    let c0 = colors.next().and_then(parse_hex)?;
    let c1 = colors.next().and_then(parse_hex)?;
    colors.next().is_none().then_some((c0, c1))
}

/// Resolve a colorway string: `"rainbow"` (default) or
/// `"twotone:#RRGGBB,#RRGGBB"`.
#[inline]
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
    /// Resolve the raw entry into a [`WordEffectSpec`] (fail-open decoding); an entry with no axes gives the all-`None` spec, which [`SpecTable::insert_custom`] skips.
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
        let burst = self.burst.as_ref().map(|raw| {
            let kind = raw.kind.trim();
            BurstSpec {
                kind: if kind.eq_ignore_ascii_case("sparkle") {
                    BurstKind::Sparkle
                } else if kind.eq_ignore_ascii_case("nova") {
                    BurstKind::Nova
                } else if kind.eq_ignore_ascii_case("supernova")
                    || kind.eq_ignore_ascii_case("super_nova")
                    || kind.eq_ignore_ascii_case("super-nova")
                {
                    BurstKind::SuperNova
                } else if kind.eq_ignore_ascii_case("glow") {
                    BurstKind::Glow
                } else {
                    // "starburst" and anything unknown: the gentlest burst.
                    BurstKind::Starburst
                },
                chance_pct: raw.chance.unwrap_or(100).min(100) as u8,
            }
        });
        let graphic = self.graphic.as_ref().map(|_| GraphicSpec {
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

/// Build the [`SpecTable`] and the synthesized emphasis-class lexicon fragment for a set of custom entries.
#[must_use]
pub fn build_custom(entries: &[RawCustomEntry]) -> (SpecTable, String) {
    let mut table = SpecTable::default();
    let mut spaced: Vec<&str> = Vec::new();
    let mut cjk: Vec<&str> = Vec::new();
    for e in entries {
        let spec = e.to_spec();
        let mut spec_id = None;
        for w in &e.words {
            let w = w.trim();
            if w.is_empty() {
                continue;
            }
            if let Some(id) = *spec_id.get_or_insert_with(|| table.intern_spec(spec)) {
                table.insert_custom_with_id(w, id);
            }
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

/// Append one scanner-ready emphasis entry. Inline customs keep their
/// control-character-dropping behaviour; strict Toy Packs reject such surfaces
/// before reaching this.
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
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
}

/// Parse the standalone `[[sparkle_words.custom]]` TOML fragment (the web `set_sparkle_custom_specs` payload, the same fragment the native config carries).
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
    let root: Root = aterm_toml::from_str(fragment).map_err(|e| e.to_string())?;
    Ok(root.sparkle_words.custom)
}

const MAX_PACK_ID_BYTES: usize = 96;
const MAX_TOY_ID_BYTES: usize = 64;
const MAX_PACK_NAME_BYTES: usize = 80;
const MAX_PACK_DESCRIPTION_BYTES: usize = 512;
const MAX_PACK_AUTHORS: usize = 8;
const MAX_AUTHOR_BYTES: usize = 80;
const MAX_LICENSE_BYTES: usize = 64;
const MAX_LANG_BYTES: usize = 35;
/// The crate's animation floor: no authored window, drift or sweep may run faster.
pub(crate) const MIN_ANIMATION_MS: u32 = 350;
const MAX_ANIMATION_MS: u32 = 6000;
const MAX_CAT_DWELL_MS: u32 = 3750;

/// Contributor-facing metadata retained with a compiled Toy Pack.
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
#[derive(Clone, Debug)]
pub struct CompiledToyPack {
    metadata: ToyPackMetadata,
    spec_table: SpecTable,
    lexicon_toml: String,
    toy_count: usize,
    word_count: usize,
}

impl CompiledToyPack {
    #[inline]
    #[must_use]
    pub const fn metadata(&self) -> &ToyPackMetadata {
        &self.metadata
    }

    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn spec_table(&self) -> &SpecTable {
        &self.spec_table
    }

    /// Scanner entries to append to the normal user lexicon override.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn lexicon_toml(&self) -> &str {
        &self.lexicon_toml
    }

    #[inline]
    #[must_use]
    pub const fn toy_count(&self) -> usize {
        self.toy_count
    }

    #[inline]
    #[must_use]
    pub const fn word_count(&self) -> usize {
        self.word_count
    }

    /// Consume the pack into the two runtime dispatch artifacts plus metadata.
    #[inline]
    #[must_use]
    pub fn into_parts(self) -> (ToyPackMetadata, SpecTable, String) {
        (self.metadata, self.spec_table, self.lexicon_toml)
    }
}

/// One or more actionable Toy Pack diagnostics.
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
    #[cfg(test)]
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

/// Parse and compile one Toy Pack manifest (cold path): source bytes are capped before TOML parsing, every collection, string and timing is validated, and the result is the same O(1) form-hash dispatch table inline customs use.
pub fn compile_toy_pack_toml(source: &str) -> Result<CompiledToyPack, ToyPackError> {
    if source.len() > MAX_TOY_PACK_BYTES {
        return Err(ToyPackError::one(format!(
            "source is {} bytes; maximum is {MAX_TOY_PACK_BYTES}",
            source.len()
        )));
    }
    let raw: RawToyPackDocument = aterm_toml::from_str(source)
        .map_err(|e| ToyPackError::one(format!("TOML schema error: {e}")))?;
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
    let mut scanner_surfaces = Vec::<(String, &str, u64)>::new();
    let word_count = raw
        .toy
        .iter()
        .fold(0usize, |n, toy| n.saturating_add(toy.words.len()));
    if word_count > MAX_WORDS_PER_PACK {
        diagnostics.push(format!(
            "words: {word_count} surfaces exceed the per-pack maximum {MAX_WORDS_PER_PACK}"
        ));
    }

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

        let spec_id = compile_toy_spec(toy, &context, &mut diagnostics)
            .and_then(|spec| table.intern_spec(spec));
        let mut spaced = Vec::new();
        let mut cjk = Vec::new();
        for (word_index, word) in toy.words.iter().take(MAX_WORDS_PER_TOY).enumerate() {
            let label = format!("{context}.words[{word_index}]");
            if !validate_toy_word(word, &label, &mut diagnostics) {
                continue;
            }
            let no_space = is_no_space_surface(word);
            let mut hashes = [0u64; 5];
            let (normalized, hashes_len) = if no_space {
                let mut key = String::with_capacity(4 + word.len());
                key.push_str("cjk:");
                key.push_str(word);
                hashes[0] = form_hash(word);
                (key, 1usize)
            } else {
                let mut base = fold(word);
                let base_len = base.len();
                let mut key = String::with_capacity(5 + base_len);
                key.push_str("word:");
                key.push_str(&base);
                hashes[0] = form_hash(&base);
                for (i, suffix) in ["'s", "\u{2019}s", "'", "\u{2019}"].into_iter().enumerate() {
                    base.truncate(base_len);
                    base.push_str(suffix);
                    hashes[i + 1] = form_hash(&base);
                }
                (key, 5usize)
            };
            let hashes = &hashes[..hashes_len];
            let has_override_key = no_space || normalized.len() > "word:".len();
            if let Some(previous) = surfaces.insert(normalized, label.clone()) {
                diagnostics.push(format!(
                    "{label}: normalized surface duplicates {previous}; dispatch order must not decide behavior"
                ));
                continue;
            }
            if let Some(previous) = hashes.iter().find_map(|hash| dispatch_hashes.get(hash)) {
                diagnostics.push(format!(
                    "{label}: dispatch key collides with {previous} (including possessive variants); dispatch order must not decide behavior"
                ));
                continue;
            }
            for &hash in hashes {
                dispatch_hashes.insert(hash, label.clone());
            }
            if let Some(id) = spec_id
                && has_override_key
            {
                for &hash in hashes {
                    table.overrides.insert(hash, id);
                }
            }
            scanner_surfaces.push((label, word.as_str(), hashes[0]));
            if no_space {
                cjk.push(word.as_str());
            } else {
                spaced.push(word.as_str());
            }
        }
        push_lexicon_entry(&mut lexicon_toml, &toy.lang, &spaced, false);
        push_lexicon_entry(&mut lexicon_toml, &toy.lang, &cjk, true);
    }

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
                for (label, surface, expected_hash) in scanner_surfaces {
                    lexicon.scan_into_with_scratch(
                        surface,
                        &options,
                        &mut scanner_chars,
                        &mut scanner_hits,
                        &mut scanner_scratch,
                    );
                    let expected_end = scanner_chars.len();
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
        compile_toy_pack_toml(&unknown_field).expect_err("unknown field rejected");

        let unknown_kind =
            TINY_TRIUMPHS.replacen("kind = \"starburst\"", "kind = \"confetti_cannon\"", 1);
        compile_toy_pack_toml(&unknown_kind).expect_err("unknown effect rejected");

        let bad_color = TINY_TRIUMPHS.replacen("#58D8FF", "58D8FF", 1);
        compile_toy_pack_toml(&bad_color).expect_err("non-canonical color rejected");

        let unscannable = TINY_TRIUMPHS.replacen("\"shipit\"", "\"ship it\"", 1);
        compile_toy_pack_toml(&unscannable).expect_err("multi-word trigger rejected");

        let cjk_exception = TINY_TRIUMPHS.replacen("\"shipit\"", "\"猫背\"", 1);
        compile_toy_pack_toml(&cjk_exception)
            .expect_err("a production-scanner CJK exception cannot become a trigger");
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
        let mut fields: Vec<&str> = error
            .diagnostics()
            .iter()
            .map(|d| d.split_once(": ").map_or(d.as_str(), |(field, _)| field))
            .collect();
        fields.sort_unstable();
        let mut want = vec![
            "schema",
            "pack.id",
            "pack.version",
            "pack.authors",
            "toy[0] (\"same\").lang",
            "toy[0] (\"same\").ink.colors",
            "toy[0] (\"same\").ink.drift_ms",
            "toy[0] (\"same\").burst.chance_pct",
            "toy[0] (\"same\").graphic.collection",
            "toy[0] (\"same\").graphic.dwell_ms",
            "toy[0] (\"same\").words[1]",
            "toy[1] (\"same\").id",
            "toy[1] (\"same\").ink.window_ms",
            "toy[1] (\"same\").words[0]",
        ];
        want.sort_unstable();
        assert_eq!(fields, want, "{:#?}", error.diagnostics());
    }

    #[test]
    fn toy_pack_source_cap_precedes_toml_parse() {
        let oversized = "x".repeat(MAX_TOY_PACK_BYTES + 1);
        let error = compile_toy_pack_toml(&oversized).expect_err("oversize source rejected");
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
        compile_toy_pack_toml(&colliding)
            .expect_err("base and possessive surface must not share a dispatch key");
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
        compile_toy_pack_toml(&source).expect_err("word cap rejected");
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
        assert!(table.override_for(form_hash("Ultrathink")).is_none());

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

        assert!(lex_frag.contains("forms = [\"Ultrathink\"]"), "{lex_frag}");
        assert!(lex_frag.contains("cjk = true"), "{lex_frag}");
        assert!(lex_frag.contains("forms = [\"猫神\"]"), "{lex_frag}");

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

    /// A single-char CJK word (needs `cjk_single_char = true`) and a mixed-script
    /// word both surface on the lexicon `conflicts` channel.
    #[test]
    fn unscannable_custom_words_warn_on_the_conflicts_channel() {
        let frag = r#"
[[sparkle_words.custom]]
words = ["犬", "abc猫"]
ink = { colorway = "rainbow" }
"#;
        let entries = parse_custom_toml(frag).expect("fragment parses");
        let (table, lex_frag) = build_custom(&entries);
        assert!(table.has_custom());
        assert!(table.override_for(form_hash("犬")).is_some());
        let lx = aterm_lexicon::Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
            .expect("synthesized fragment parses");
        for word in ["\"犬\"", "\"abc猫\""] {
            assert!(
                lx.conflicts().iter().any(|c| c.contains(word)),
                "{word} does not warn, got {:?}",
                lx.conflicts()
            );
        }
    }

    #[test]
    fn empty_spec_and_malformed_fragment_fail_safely() {
        let entries =
            parse_custom_toml("[[sparkle_words.custom]]\nwords = [\"zzz\"]\n").expect("parses");
        let (table, _) = build_custom(&entries);
        assert!(!table.has_custom());
        assert!(parse_custom_toml("[[sparkle_words.custom]").is_err());
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
