// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Multilingual whole-word lexical matching for the "sparkle words" text
//! decorations.
//!
//! The terminal renderer decorates two families of words with distinct visual
//! effects: a **profanity** family (the "fuck" expletive family across major
//! languages) and a **feline** family (cat / kitty / kitten across major
//! languages). This crate owns the language model: a curated lexicon of surface
//! forms plus a scanner that finds *whole-word* occurrences in a line of text.
//!
//! # Why whole-word
//!
//! Naive substring matching produces the infamous "Scunthorpe problem":
//! `scatter`, `concatenate`, `category` all contain `cat`. The scanner here
//! matches only complete tokens (delimited by Unicode word boundaries) against
//! folded lexicon keys, and additionally suppresses tokens that sit in a code /
//! path / URL context (`cat.txt`, `/api/cat`). See [`Lexicon::scan`].
//!
//! # Coverage is honest, not total
//!
//! "Every major language, all forms" is not achievable with a finite list.
//! Concatenative languages (English, Romance, Germanic) use `stems × suffixes`;
//! fusional / agglutinative / non-concatenative languages use curated explicit
//! `forms`. Everything is best-effort and extensible — see `data/lexicon.toml`.
//!
//! # Output is render-only
//!
//! Matches are positions used to paint a render-time overlay. They never alter
//! terminal state, copied text, or recordings.

mod fold;

pub use fold::{fold, fold_into, is_no_space_script, is_token_char};

use aterm_hash::{FxHashMap, FxHashSet};
use std::sync::OnceLock;

/// Which decoration family a matched word belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Class {
    /// The "fuck" expletive family — drawn as the randomized sparkle.
    Profanity,
    /// The cat / kitty / kitten family — drawn as the steady cat-paw.
    Feline,
    /// The orca / cetacean family — drawn as the randomized "splash" of water droplets.
    Orca,
    /// Hype / emphasis words — drawn as the animated-ink shimmer only (no
    /// sprite overlay). The builtin lexicon ships NO emphasis forms: the class
    /// is populated entirely by the user's `extra_words` / lexicon override.
    Emphasis,
}

/// Interned language identifier: an index into the [`Lexicon`]'s language
/// table ([`Lexicon::lang_code`] resolves it back to the TOML `lang` code).
/// Ids are assigned in first-appearance TOML order across ALL well-formed
/// entries — before the `ambiguous` language gating — so a given build input
/// yields the same ids regardless of the configured `languages`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct LangId(pub u16);

/// The reserved id for surfaces whose entry carried an empty / `"unknown"`
/// `lang` code, and the deterministic overflow bucket once every real slot of
/// the [`LangSet`] bitset is taken. [`Lexicon::lang_code`] renders it as
/// `"unknown"` (the language table never grows into this slot).
const UNKNOWN_LANG: LangId = LangId((LangSet::CAPACITY - 1) as u16);

/// A set of interned languages, as a `u64` bitset (bit `i` ⇔ `LangId(i)`).
///
/// The compiled maps carry one per surface so a match knows every language
/// that claims its surface form (`kucing` is id **and** ms **and** jv). The
/// last bit is reserved for the `"unknown"` bucket, so a lexicon can intern
/// [`CAPACITY`](Self::CAPACITY)` - 1` real language codes; further codes
/// collapse into `"unknown"` deterministically.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct LangSet(u64);

impl LangSet {
    /// Number of distinct bits (= max interned languages, incl. `"unknown"`).
    pub const CAPACITY: usize = 64;
    /// The empty set.
    pub const EMPTY: LangSet = LangSet(0);

    /// This set plus `id`.
    #[must_use]
    pub fn with(self, id: LangId) -> LangSet {
        // The `% 64` is a no-op for every id this crate hands out (ids are
        // bounded by CAPACITY at intern time); it makes the shift's
        // no-overflow obligation checkable.
        LangSet(self.0 | (1u64 << (u32::from(id.0) % 64)))
    }

    /// Whether `id` is in the set.
    #[must_use]
    pub fn contains(self, id: LangId) -> bool {
        self.0 & (1u64 << (u32::from(id.0) % 64)) != 0
    }

    /// Set union.
    #[must_use]
    pub fn union(self, other: LangSet) -> LangSet {
        LangSet(self.0 | other.0)
    }

    /// True when no language is set.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Number of languages in the set.
    #[must_use]
    pub fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Iterate the members in ascending [`LangId`] order — i.e. lexicon
    /// first-appearance order, so the first yielded id is [`primary_lang`].
    pub fn iter(self) -> impl Iterator<Item = LangId> {
        let mut rest = self.0;
        std::iter::from_fn(move || {
            if rest == 0 {
                return None;
            }
            let bit = rest.trailing_zeros();
            rest &= rest.wrapping_sub(1); // clear the lowest set bit
            // `bit < 64` always fits u16.
            Some(LangId(bit as u16))
        })
    }
}

/// The set's primary language: the lowest set bit, i.e. the member whose entry
/// appeared FIRST in the lexicon TOML. An empty set yields the reserved
/// `"unknown"` id.
#[must_use]
pub fn primary_lang(set: LangSet) -> LangId {
    if set.is_empty() {
        return UNKNOWN_LANG;
    }
    // Non-empty ⇒ trailing_zeros < 64, which fits u16.
    LangId(set.0.trailing_zeros() as u16)
}

/// One whole-word occurrence found by [`Lexicon::scan`].
///
/// `start`/`end` are **character** indices into the scanned `&str` (i.e. indices
/// into `text.chars()`), half-open `[start, end)`. The caller maps these back to
/// terminal columns using its own per-character column table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Match {
    /// First character index of the matched word (inclusive).
    pub start: usize,
    /// One past the last character index of the matched word (exclusive).
    pub end: usize,
    /// Which decoration family matched.
    pub class: Class,
    /// Every language whose entry lists the matched surface under this class
    /// (`kucing` → id ∪ ms ∪ jv). Resolve members via [`Lexicon::lang_code`];
    /// [`primary_lang`] picks the first-appearance member.
    pub langs: LangSet,
    /// Collision-free identity of the exact compiled lexicon surface.
    ///
    /// IDs are unique within one [`Lexicon`] instance and survive every scan
    /// performed with that instance. Hosts use this when a redraw moves an
    /// occurrence: equality means the scanner matched the same exact folded
    /// (or raw CJK) key, rather than merely a 64-bit hash collision.
    pub form_id: FormId,
    /// FNV-1a hash of the matched lexicon key — the *folded* whole-word surface
    /// for spaced scripts, the *raw* compound slice for CJK. Position- and
    /// case-independent by construction (`Cat` at column 3 and `cat` at column
    /// 70 hash equal), so the host can use it as the form term of a
    /// context-derived seed without re-folding the token.
    pub form_hash: u64,
    /// Whether the winning surface is language-gated because it collides with
    /// ordinary text in another language.
    ///
    /// Renderers use this bit to defer a match that still touches the live
    /// input cursor. Once a delimiter moves the cursor away, the same exact
    /// surface remains a normal match. A surface claimed by any non-ambiguous
    /// entry of the winning class is non-ambiguous here.
    pub ambiguous: bool,
}

/// Collision-free identity of one exact recognized surface in a compiled
/// [`Lexicon`].
///
/// The numeric value is intentionally opaque and local to the lexicon that
/// produced it; compare values, but do not persist or interpret them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FormId(u64);

impl FormId {
    /// Sentinel for synthetic host-side test occurrences that did not come
    /// from a lexicon scan. Real [`Match`] values never carry this ID.
    pub const UNKNOWN: Self = Self(u64::MAX);

    /// Reserve the low byte for the finite, allocation-free morphology variants
    /// recognized by [`Lexicon::classify_token_into`]. Exact table keys use
    /// variant zero; possessive and clitic fallbacks use a nonzero code. This
    /// keeps `kitty`, `kitty's`, and every supported prefix form distinct without
    /// falling back to a probabilistic token hash.
    fn exact(base: u64) -> Self {
        Self(base << 8)
    }

    fn with_variant(self, variant: u8) -> Self {
        debug_assert_ne!(variant, 0);
        debug_assert_eq!(self.0 & 0xff, 0);
        Self(self.0 | u64::from(variant))
    }
}

/// Tunables that the host's config maps onto the scan. Defaults are the
/// false-positive-safe choices.
#[derive(Clone, Debug, Default)]
pub struct ScanOptions<'a> {
    /// Decorate the bare 3-letter token `cat` (and equally short feline tokens).
    /// Off by default because `cat` is also the ubiquitous shell command and a
    /// common English substring-of-intent; longer forms (`cats`, `kitty`,
    /// `gato`, …) are always eligible.
    pub allow_bare_cat: bool,
    /// Decorate a lone CJK cat ideograph (`猫`) anywhere, even inside an
    /// un-listed compound. Off by default (high false-positive rate).
    pub cjk_single_char: bool,
    /// Folded surfaces to never decorate (the user's `ignore_words` / global
    /// `deny`, already folded by the caller via [`fold`]). A plain `HashSet` so
    /// callers needn't depend on `aterm-hash`.
    pub ignore: Option<&'a std::collections::HashSet<String>>,
}

/// Caller-owned auxiliary buffers for [`Lexicon::scan_into_with_scratch`].
///
/// Keep one beside the caller's `chars` and `out` vectors and reuse all three
/// across lines. After their capacities have warmed, scanning performs no heap
/// allocation, including possessive/clitic fallbacks and CJK maximal-run
/// probes. The fields are intentionally private: they are workspace owned by
/// the scanner and carry no result state.
#[derive(Debug, Default)]
pub struct ScanScratch {
    token: String,
    folded: String,
    candidate_folded: String,
    window: String,
    bounds: Vec<usize>,
}

/// Error building a [`Lexicon`] from TOML sources.
#[derive(Debug)]
pub enum LexError {
    /// A TOML document failed to parse.
    Toml(toml::de::Error),
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Composed without `write!` so no std format-macro expansion (which
            // contains a hardened unsafe op the verifier cannot discharge) lands
            // in this body. `e.to_string()` renders with default options exactly
            // like the former `{e}`.
            LexError::Toml(e) => {
                f.write_str("lexicon TOML parse error: ")?;
                f.write_str(&e.to_string())
            }
        }
    }
}

/// Renders a value's `Debug` output through `Display`, so `.to_string()` can
/// produce the exact `{:?}` text without expanding a std format macro (and its
/// hardened unsafe op) into the calling function's body.
struct DebugText<T: std::fmt::Debug>(T);

impl<T: std::fmt::Debug> std::fmt::Display for DebugText<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl std::error::Error for LexError {}

const BUILTIN_LEXICON: &str = include_str!("../data/lexicon.toml");
const BUILTIN_EXCEPTIONS: &str = include_str!("../data/exceptions.toml");

/// Hard cap on the number of surface forms materialized across *all* lexicon
/// entries combined during [`Lexicon::build`]. The embedded builtin produces only
/// a few hundred (max stems×suffixes product ~8), so this bound (2^20) never
/// touches it; it exists solely to stop a config/override lexicon with huge
/// `stems` / `suffixes` / `forms` arrays (or many entries) from driving
/// O(stems·suffixes) peak memory and build time — an unbounded-allocation DoS.
/// The cap is a GLOBAL running total, not per-entry, because a per-entry cap is
/// bypassable by many individually-small entries.
const MAX_TOTAL_SURFACES: usize = 1 << 20;

/// A compiled lexicon: folded surface forms mapped to their decoration class,
/// plus the CJK compound tables.
#[derive(Debug, Clone)]
pub struct Lexicon {
    /// Folded whole-word surface → (class, claiming languages), for
    /// whitespace-delimited scripts.
    spaced: FxHashMap<String, LexiconHit>,
    /// Raw CJK compound → (class, claiming languages), for no-space scripts
    /// (matched by maximal run).
    cjk_forms: FxHashMap<String, LexiconHit>,
    /// Raw CJK compounds that CONTAIN a cat ideograph but are NOT a pet cat
    /// (panda, lynx, wheelbarrow…), suppressed during the maximal-run match.
    cjk_exceptions: FxHashSet<String>,
    /// Longest CJK key (chars), bounding the maximal-run search window.
    max_cjk: usize,
    /// Interned language codes, in first-appearance TOML order ([`LangId`] `i`
    /// ⇔ `langs[i]`). The reserved [`UNKNOWN_LANG`] slot is never stored here;
    /// [`Self::lang_code`] renders it as `"unknown"`.
    langs: Vec<String>,
    /// Data problems found during build (same surface in both classes, unknown
    /// class). Empty for a well-formed lexicon; surfaced by the CI validator.
    conflicts: Vec<String>,
    /// Folded surfaces that came from USER config (override TOML /
    /// `extra_words` / custom specs), not the builtin data. Consulted by the
    /// scanner's unconditional ≤3-folded-chars emphasis guard: explicit config
    /// is consent, so a short custom word ("gg") still matches (v3 §6).
    user_surfaces: FxHashSet<String>,
}

/// Compiled value stored behind an exact surface key. `form_id` is assigned
/// only after cross-class precedence has settled, so one winning key has one
/// collision-free identity regardless of how many language entries claimed it.
#[derive(Clone, Copy, Debug)]
struct LexiconHit {
    class: Class,
    langs: LangSet,
    form_id: FormId,
    ambiguous: bool,
}

// ---- TOML shapes ----

#[derive(serde::Deserialize)]
struct RawLexicon {
    #[serde(default)]
    entry: Vec<RawEntry>,
}

#[derive(serde::Deserialize)]
struct RawEntry {
    class: String,
    #[serde(default)]
    lang: String,
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default)]
    stems: Vec<String>,
    #[serde(default)]
    suffixes: Vec<String>,
    #[serde(default)]
    forms: Vec<String>,
    #[serde(default)]
    cjk: bool,
    #[serde(default)]
    ambiguous: bool,
    #[serde(default)]
    #[allow(dead_code, reason = "documentation field for human reviewers")]
    notes: String,
    /// NOT a TOML field: set by [`Lexicon::with_languages_and_override`] on
    /// every entry that came from the user's override TOML, so the build can
    /// record its surfaces in [`Lexicon::user_surfaces`] (v3 §6 short-word
    /// exemption).
    #[serde(skip)]
    user: bool,
}

fn default_mode() -> String {
    "suffix".to_string()
}

#[derive(serde::Deserialize)]
struct RawExceptions {
    #[serde(default)]
    exception: Vec<RawException>,
}

#[derive(serde::Deserialize)]
struct RawException {
    word: String,
    #[serde(default)]
    #[allow(dead_code, reason = "documentation fields for human reviewers")]
    lang: String,
    #[serde(default)]
    #[allow(dead_code, reason = "documentation fields for human reviewers")]
    meaning: String,
}

/// The set of languages whose `ambiguous` entries are enabled.
struct Langs {
    all: bool,
    set: FxHashSet<String>,
}

impl Langs {
    fn new(langs: &[&str]) -> Self {
        let all = langs.contains(&"all");
        let set = langs.iter().map(|l| (*l).to_string()).collect();
        Langs { all, set }
    }
    fn enabled(&self, lang: &str) -> bool {
        self.all || self.set.contains(lang)
    }
}

impl Lexicon {
    /// The embedded lexicon with only English `ambiguous` entries un-gated (the
    /// default `languages = ["en"]`). Built once and cached.
    #[must_use]
    pub fn builtin() -> &'static Lexicon {
        static L: OnceLock<Lexicon> = OnceLock::new();
        L.get_or_init(|| Self::with_languages(&["en"]))
    }

    /// The embedded lexicon, un-gating `ambiguous` entries for `langs`
    /// (`["all"]` un-gates everything). Panics only if the *embedded* data fails
    /// to parse, which the test suite forbids.
    #[must_use]
    pub fn with_languages(langs: &[&str]) -> Lexicon {
        // Explicit match instead of `.expect(..)`: `expect`'s panic-freedom is
        // not modeled by the verifier; the panic path itself (unreachable for
        // the embedded data — the test suite forbids it) still reports the same
        // "<msg>: <err:?>" text `expect` would have produced.
        match Self::from_sources(BUILTIN_LEXICON, BUILTIN_EXCEPTIONS, langs) {
            Ok(lx) => lx,
            Err(e) => {
                let mut msg = String::from("embedded lexicon must parse: ");
                msg.push_str(&DebugText(&e).to_string());
                std::panic::panic_any(msg)
            }
        }
    }

    /// The embedded lexicon with an optional user `[[entry]]` TOML merged OVER it
    /// (user entries are appended, so they extend coverage; the builder's
    /// fold-normalization and cross-class conflict detection apply to them too).
    /// `langs` selects which `ambiguous` entries load. Returns a parse error for a
    /// malformed override so the caller can warn and fall back to [`with_languages`].
    pub fn with_languages_and_override(
        langs: &[&str],
        extra_toml: Option<&str>,
    ) -> Result<Lexicon, LexError> {
        let mut raw: RawLexicon = toml::from_str(BUILTIN_LEXICON).map_err(LexError::Toml)?;
        let raw_exc: RawExceptions = toml::from_str(BUILTIN_EXCEPTIONS).map_err(LexError::Toml)?;
        if let Some(t) = extra_toml {
            let user: RawLexicon = toml::from_str(t).map_err(LexError::Toml)?;
            // v3 §6: user entries are marked so their surfaces land in the
            // `user_surfaces` set (the short-emphasis-guard exemption).
            raw.entry.extend(user.entry.into_iter().map(|mut e| {
                e.user = true;
                e
            }));
        }
        Ok(Self::build(raw, raw_exc, &Langs::new(langs)))
    }

    /// Build a lexicon from TOML sources (e.g. the embedded data, or a user
    /// override merged in). `langs` selects which `ambiguous` entries load.
    pub fn from_sources(
        lexicon_toml: &str,
        exceptions_toml: &str,
        langs: &[&str],
    ) -> Result<Lexicon, LexError> {
        let raw: RawLexicon = toml::from_str(lexicon_toml).map_err(LexError::Toml)?;
        let raw_exc: RawExceptions = toml::from_str(exceptions_toml).map_err(LexError::Toml)?;
        Ok(Self::build(raw, raw_exc, &Langs::new(langs)))
    }

    fn build(raw: RawLexicon, raw_exc: RawExceptions, langs: &Langs) -> Lexicon {
        let mut spaced: FxHashMap<String, (Class, LangSet, bool)> = FxHashMap::default();
        let mut cjk_forms: FxHashMap<String, (Class, LangSet, bool)> = FxHashMap::default();
        let mut cjk_exceptions: FxHashSet<String> = FxHashSet::default();
        let mut user_surfaces: FxHashSet<String> = FxHashSet::default();
        let mut conflicts = Vec::new();
        let mut max_cjk = 1usize;
        // Interned language codes, first-appearance order (see the field doc).
        let mut lang_table: Vec<String> = Vec::new();
        // Running count of surface forms materialized so far, shared across every
        // entry, to bound the total against MAX_TOTAL_SURFACES (see its docs).
        let mut total: usize = 0;

        for e in raw.entry {
            let class = match e.class.as_str() {
                "profanity" => Class::Profanity,
                "feline" => Class::Feline,
                "orca" => Class::Orca,
                "emphasis" => Class::Emphasis,
                other => {
                    // Composed without `format!` (see `DebugText`); byte-for-byte
                    // the former "unknown class {other:?} (lang {})" text.
                    let mut msg = String::from("unknown class ");
                    msg.push_str(&DebugText(other).to_string());
                    msg.push_str(" (lang ");
                    msg.push_str(&e.lang);
                    msg.push(')');
                    conflicts.push(msg);
                    continue;
                }
            };
            // Intern the entry's language BEFORE the ambiguous gating below, so
            // the id table (and thus every LangId) depends only on the TOML
            // input, never on the configured `languages`. The gating itself
            // still keys on the RAW entry code — its semantics are unchanged.
            let lset = LangSet::EMPTY.with(intern_lang(&mut lang_table, &e.lang));
            if e.ambiguous && !langs.enabled(&e.lang) {
                continue;
            }

            // Collect surface forms: stems × suffixes (suffix mode) + explicit forms.
            // Both come from attacker-controlled config/override TOML, so the
            // expansion is bounded against the GLOBAL running `total`: an entry whose
            // surfaces would push the total past MAX_TOTAL_SURFACES is skipped (and
            // recorded as a conflict) rather than materialized.
            let mut surfaces: Vec<String> = Vec::new();
            if e.mode == "suffix" {
                // `prod` is exactly the surface count the loop below would push
                // (stems × suffixes, or stems alone when there are no suffixes);
                // `saturating_mul`/`saturating_add` keep pathological input from
                // overflowing usize.
                let prod = e.stems.len().saturating_mul(e.suffixes.len().max(1));
                if prod > MAX_TOTAL_SURFACES || total.saturating_add(prod) > MAX_TOTAL_SURFACES {
                    // Composed without `format!` (see `DebugText`'s doc for why);
                    // byte-for-byte the former message text.
                    let mut msg = String::from("suffix expansion skipped (lang ");
                    msg.push_str(&e.lang);
                    msg.push_str("): ");
                    msg.push_str(&e.stems.len().to_string());
                    msg.push_str(" stems × ");
                    msg.push_str(&e.suffixes.len().to_string());
                    msg.push_str(" suffixes = ");
                    msg.push_str(&prod.to_string());
                    msg.push_str(" would exceed surface cap ");
                    msg.push_str(&MAX_TOTAL_SURFACES.to_string());
                    conflicts.push(msg);
                } else {
                    for st in &e.stems {
                        if e.suffixes.is_empty() {
                            surfaces.push(st.clone());
                        } else {
                            for sf in &e.suffixes {
                                // `format!("{st}{sf}")` without the macro.
                                let mut surface =
                                    String::with_capacity(st.len().saturating_add(sf.len()));
                                surface.push_str(st);
                                surface.push_str(sf);
                                surfaces.push(surface);
                            }
                        }
                    }
                    total = total.saturating_add(prod);
                }
            }
            // Explicit forms are attacker-controlled too (linear in file size); count
            // them against the same global cap before materializing them.
            let forms = e.forms.len();
            if total.saturating_add(forms) > MAX_TOTAL_SURFACES {
                // Composed without `format!`; byte-for-byte the former text.
                let mut msg = String::from("explicit forms skipped (lang ");
                msg.push_str(&e.lang);
                msg.push_str("): ");
                msg.push_str(&forms.to_string());
                msg.push_str(" forms would exceed surface cap ");
                msg.push_str(&MAX_TOTAL_SURFACES.to_string());
                conflicts.push(msg);
            } else {
                surfaces.extend(e.forms.iter().cloned());
                total = total.saturating_add(forms);
            }

            for s in surfaces {
                let s = s.trim();
                if s.is_empty() {
                    continue;
                }
                if e.cjk {
                    let key: String = s.to_string();
                    let len = key.chars().count();
                    if len > max_cjk {
                        max_cjk = len;
                    }
                    if e.user && len == 1 {
                        // v3 §6: the scanner drops single-char CJK hits unless
                        // `cjk_single_char = true` — a USER surface that can
                        // only ever scan under that opt-in must say so instead
                        // of silently never firing. Builtin single-char entries
                        // (操/草/干, …) are a deliberate data choice and stay
                        // quiet. Composed without `format!` (see `DebugText`);
                        // the word "requires" is load-bearing for callers that
                        // surface these to the user.
                        let mut msg = String::from("single-char CJK surface ");
                        msg.push_str(&DebugText(&key).to_string());
                        msg.push_str(" (lang ");
                        msg.push_str(&e.lang);
                        msg.push_str(") requires cjk_single_char = true to scan");
                        conflicts.push(msg);
                    }
                    insert_class(&mut cjk_forms, key, class, lset, e.ambiguous);
                } else {
                    let key = fold(s);
                    // A surface that is not a single scannable token (contains a
                    // space, dot, slash, …) can never match whole-word; skip it.
                    // Its head noun, if listed separately, still matches.
                    if key.is_empty() || !is_matchable_token(&key) {
                        if e.user {
                            // v3 §6: a USER surface dropped here (mixed-script
                            // like "abc猫", multi-word, punctuation) would
                            // otherwise be silently accepted by the config and
                            // never scan — record it on the conflicts channel.
                            // Builtin multi-word forms ("con mèo", "đ.m") are a
                            // deliberate data choice and stay quiet.
                            let mut msg = String::from("surface ");
                            msg.push_str(&DebugText(s).to_string());
                            msg.push_str(" (lang ");
                            msg.push_str(&e.lang);
                            msg.push_str(
                                ") dropped: not a single scannable whole-word \
                                 token (mixed-script and multi-word surfaces \
                                 never match)",
                            );
                            conflicts.push(msg);
                        }
                        continue;
                    }
                    if e.user {
                        // v3 §6: explicit config is consent — record the
                        // folded surface so the scanner's short-word guards
                        // exempt it.
                        user_surfaces.insert(key.clone());
                    }
                    insert_class(&mut spaced, key, class, lset, e.ambiguous);
                }
            }
        }

        for x in raw_exc.exception {
            let key = x.word.trim().to_string();
            if key.is_empty() {
                continue;
            }
            let len = key.chars().count();
            if len > max_cjk {
                max_cjk = len;
            }
            cjk_exceptions.insert(key);
        }

        // Assign collision-free form identities only after precedence merging
        // has selected one winning class per exact key. Sorting makes the IDs
        // reproducible across processes without relying on hash-map iteration
        // order; the two namespaces share one counter, so their IDs cannot
        // alias either.
        let mut next_form_id = 0u64;
        let spaced = finalize_hits(spaced, &mut next_form_id);
        let cjk_forms = finalize_hits(cjk_forms, &mut next_form_id);

        Lexicon {
            spaced,
            cjk_forms,
            cjk_exceptions,
            max_cjk,
            langs: lang_table,
            conflicts,
            user_surfaces,
        }
    }

    /// The interned code for `id` (`"en"`, `"ja"`, …); the reserved
    /// [`UNKNOWN_LANG`] slot — and any id this lexicon never handed out —
    /// renders as `"unknown"`.
    #[must_use]
    pub fn lang_code(&self, id: LangId) -> &str {
        self.langs
            .get(usize::from(id.0))
            .map_or("unknown", String::as_str)
    }

    /// Data problems found at build time (a surface claimed by both classes, an
    /// unknown class string, a USER surface that can never scan as written —
    /// single-char CJK without the `cjk_single_char` opt-in, mixed-script).
    /// Empty for well-formed data; the CI validator asserts this is empty for
    /// the embedded lexicon.
    #[must_use]
    pub fn conflicts(&self) -> &[String] {
        &self.conflicts
    }

    /// Total number of folded single-word surfaces.
    #[must_use]
    pub fn spaced_form_count(&self) -> usize {
        self.spaced.len()
    }

    /// Iterate the folded whitespace-script surfaces and their class (validator).
    pub fn iter_spaced(&self) -> impl Iterator<Item = (&str, Class)> {
        self.spaced.iter().map(|(k, v)| (k.as_str(), v.class))
    }

    /// Iterate the CJK compound surfaces and their class (validator).
    pub fn iter_cjk(&self) -> impl Iterator<Item = (&str, Class)> {
        self.cjk_forms.iter().map(|(k, v)| (k.as_str(), v.class))
    }

    /// Whether at least one compiled surface in `class` can pass the scanner's
    /// static policy gates and is not claimed by an overriding runtime table.
    /// This is a cold-path capability projection for hosts; it follows the same
    /// folded keys, ignore policy, short-word rules, CJK exceptions, and
    /// single-character opt-in as [`Self::scan`], without synthesizing text or
    /// allocating one scan per surface.
    pub fn has_scannable_class_surface(
        &self,
        class: Class,
        opts: &ScanOptions<'_>,
        mut overridden: impl FnMut(u64) -> bool,
    ) -> bool {
        let spaced = self.spaced.iter().any(|(surface, hit)| {
            hit.class == class
                && opts
                    .ignore
                    .is_none_or(|ignore| !ignore.contains(surface.as_str()))
                && !(class == Class::Feline && !opts.allow_bare_cat && surface.chars().count() <= 3)
                && !(class == Class::Emphasis
                    && surface.chars().count() <= 3
                    && !self.user_surfaces.contains(surface.as_str()))
                && !overridden(fnv1a64(surface))
        });
        spaced
            || self.cjk_forms.iter().any(|(surface, hit)| {
                hit.class == class
                    && !self.cjk_exceptions.contains(surface.as_str())
                    && (surface.chars().count() != 1 || opts.cjk_single_char)
                    && opts
                        .ignore
                        .is_none_or(|ignore| !ignore.contains(surface.as_str()))
                    && !overridden(fnv1a64(surface))
            })
    }

    /// Classify an already-isolated whole token (no surrounding context). Applies
    /// possessive and clitic stripping but **not** the bare-`cat` / code-context
    /// policy guards (those are [`Lexicon::scan`]'s job). Returns the family if
    /// the token is a known surface form.
    #[must_use]
    pub fn classify_token(&self, token: &str) -> Option<Class> {
        let mut folded = String::new();
        let mut candidate_folded = String::new();
        self.classify_token_into(token, &mut folded, &mut candidate_folded)
            .map(|hit| hit.class)
    }

    /// [`classify_token`](Self::classify_token) reusing `folded` as scratch for
    /// the folded full-token lookup key, and also returning the surface's
    /// claiming-language set. On return `folded` always holds the
    /// **full-token** fold (the first lookup key), so the per-row scanner can
    /// reuse it for the ignore-list and bare-`cat` guards without re-folding. The
    /// rare possessive / clitic fallbacks fold into `candidate_folded`, so they
    /// never clobber `folded` and allocate nothing after scratch warmup.
    fn classify_token_into(
        &self,
        token: &str,
        folded: &mut String,
        candidate_folded: &mut String,
    ) -> Option<LexiconHit> {
        fold::fold_into(token, folded);
        if let Some(c) = self.spaced.get(folded.as_str()).copied() {
            return Some(c);
        }
        if let Some((stripped, variant)) = strip_possessive(token) {
            fold::fold_into(stripped, candidate_folded);
            if let Some(mut c) = self.spaced.get(candidate_folded.as_str()).copied() {
                c.form_id = c.form_id.with_variant(variant);
                return Some(c);
            }
        }

        // Arabic definite article ال. The old `clitic_candidates` helper
        // materialized a Vec<String>; the candidate is already a valid slice.
        if let Some(rest) = token.strip_prefix("\u{0627}\u{0644}")
            && rest.chars().count() >= 2
        {
            fold::fold_into(rest, candidate_folded);
            if let Some(mut c) = self.spaced.get(candidate_folded.as_str()).copied() {
                c.form_id = c.form_id.with_variant(5);
                return Some(c);
            }
        }

        // Arabic / Hebrew single-letter proclitics. UTF-8 width of the first
        // scalar gives the allocation-free suffix slice formerly collected
        // with `token.chars().skip(1).collect::<String>()`.
        let arabic_clitics = ['\u{0648}', '\u{0641}', '\u{0628}', '\u{0643}', '\u{0644}'];
        let hebrew_clitics = [
            '\u{05D4}', '\u{05D5}', '\u{05D1}', '\u{05DB}', '\u{05DC}', '\u{05DE}', '\u{05E9}',
        ];
        if let Some(first) = token.chars().next()
            && (arabic_clitics.contains(&first) || hebrew_clitics.contains(&first))
            && token.chars().count() >= 3
        {
            let rest = &token[first.len_utf8()..];
            fold::fold_into(rest, candidate_folded);
            if let Some(mut c) = self.spaced.get(candidate_folded.as_str()).copied() {
                let variant = arabic_clitics
                    .iter()
                    .position(|c| *c == first)
                    .map(|i| 6 + i as u8)
                    .or_else(|| {
                        hebrew_clitics
                            .iter()
                            .position(|c| *c == first)
                            .map(|i| 6 + arabic_clitics.len() as u8 + i as u8)
                    })
                    .expect("the guard established a supported clitic");
                c.form_id = c.form_id.with_variant(variant);
                return Some(c);
            }
        }
        None
    }

    /// Find every whole-word occurrence of a lexicon form in `text`.
    ///
    /// `text` is one logical line (soft-wrapped physical rows already joined by
    /// the caller). Matches are returned in left-to-right order with character
    /// indices into `text.chars()`.
    #[must_use]
    pub fn scan(&self, text: &str, opts: &ScanOptions) -> Vec<Match> {
        let mut chars = Vec::new();
        let mut out = Vec::new();
        let mut scratch = ScanScratch::default();
        self.scan_into_with_scratch(text, opts, &mut chars, &mut out, &mut scratch);
        out
    }

    /// Compatibility form of [`scan_into_with_scratch`](Self::scan_into_with_scratch).
    /// `chars` and `out` are reused, but this convenience entry point creates
    /// fresh auxiliary scratch. Hot row-by-row callers should own one
    /// [`ScanScratch`] and call `scan_into_with_scratch` instead.
    pub fn scan_into(
        &self,
        text: &str,
        opts: &ScanOptions,
        chars: &mut Vec<char>,
        out: &mut Vec<Match>,
    ) {
        let mut scratch = ScanScratch::default();
        self.scan_into_with_scratch(text, opts, chars, out, &mut scratch);
    }

    /// Like [`scan`](Self::scan), but reuses every caller-owned buffer (cleared
    /// first). After `chars`, `out`, and `scratch` reach their high-water
    /// capacities, repeated scans allocate nothing. `out` holds the matches on
    /// return; `chars` remains the reconstructed character stream for callers
    /// that need match-relative context.
    pub fn scan_into_with_scratch(
        &self,
        text: &str,
        opts: &ScanOptions,
        chars: &mut Vec<char>,
        out: &mut Vec<Match>,
        scratch: &mut ScanScratch,
    ) {
        chars.clear();
        chars.extend(text.chars());
        out.clear();
        let ScanScratch {
            token,
            folded,
            candidate_folded,
            window,
            bounds,
        } = scratch;
        let n = chars.len();
        let mut i = 0;
        while i < n {
            let c = chars[i];
            if fold::is_no_space_script(c) {
                let mut k = i;
                while k < n && fold::is_no_space_script(chars[k]) {
                    // `k < n` makes the plain `+ 1` overflow-free; the saturating
                    // form is identical and lets the verifier discharge it.
                    k = k.saturating_add(1);
                }
                self.scan_cjk_run(chars, i, k, opts, window, bounds, out);
                i = k;
            } else if fold::is_token_char(c) {
                let j = token_end(chars, i);
                self.try_spaced_token(chars, i, j, opts, token, folded, candidate_folded, out);
                i = j;
            } else {
                // `i < n` makes the plain `+ 1` overflow-free; same value.
                i = i.saturating_add(1);
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "threads reusable scratch buffers (token/fold/window/bounds) \
                  through the scanner to keep the hot path allocation-free"
    )]
    fn try_spaced_token(
        &self,
        chars: &[char],
        i: usize,
        j: usize,
        opts: &ScanOptions,
        token: &mut String,
        folded: &mut String,
        candidate_folded: &mut String,
        out: &mut Vec<Match>,
    ) {
        // Code / path / URL context guard.
        if left_suppresses(chars, i) || right_suppresses(chars, j) {
            return;
        }
        token.clear();
        // Clamp the token span to the buffer: identical when the caller's
        // `i <= j <= chars.len()` invariant holds (the verifier cannot carry
        // that invariant across the method boundary).
        let hi = if j < chars.len() { j } else { chars.len() };
        let lo = if i < hi { i } else { hi };
        token.extend(chars[lo..hi].iter());
        // `classify_token_into` leaves the full-token fold in `folded` (its first
        // lookup key), so the ignore / bare-`cat` guards below reuse it instead of
        // re-folding the same token.
        let Some(hit) = self.classify_token_into(token.as_str(), folded, candidate_folded) else {
            return;
        };
        if let Some(ignore) = opts.ignore
            && ignore.contains(folded.as_str())
        {
            return;
        }
        // Bare-`cat` policy: short feline tokens are opt-in.
        if hit.class == Class::Feline && !opts.allow_bare_cat && folded.chars().count() <= 3 {
            return;
        }
        // Emphasis policy: hype words must be >= 4 folded chars — EXCEPT
        // user-supplied surfaces (v3 §6: explicit config is consent; a custom
        // 2-char word must scan). Builtin/derived short surfaces stay
        // suppressed: a 3-char emphasis surface ("wow") would fire on far too
        // much ordinary prose.
        if hit.class == Class::Emphasis
            && folded.chars().count() <= 3
            && !self.user_surfaces.contains(folded.as_str())
        {
            return;
        }
        out.push(Match {
            start: i,
            end: j,
            class: hit.class,
            langs: hit.langs,
            form_id: hit.form_id,
            form_hash: fnv1a64(folded.as_str()),
            ambiguous: hit.ambiguous,
        });
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "threads reusable scratch buffers (window/bounds and outputs) \
                  through the CJK scan to keep the hot path allocation-free"
    )]
    fn scan_cjk_run(
        &self,
        chars: &[char],
        start: usize,
        end: usize,
        opts: &ScanOptions,
        window: &mut String,
        bounds: &mut Vec<usize>,
        out: &mut Vec<Match>,
    ) {
        // Clamp the run's upper bound to the buffer: identical when the caller's
        // `end <= chars.len()` invariant holds (the verifier cannot carry that
        // invariant across the method boundary).
        let end = if end < chars.len() { end } else { chars.len() };
        let mut p = start;
        while p < end {
            let remaining = end - p;
            let max_l = if self.max_cjk < remaining {
                self.max_cjk
            } else {
                remaining
            };
            // Build the longest candidate once into `window`, recording the
            // cumulative byte length after each pushed char (`bounds[k]` = byte
            // length of the first `k + 1` chars). A shorter candidate is then a
            // cheap prefix slice rather than a fresh per-length allocation.
            window.clear();
            bounds.clear();
            let mut off = 0;
            while off < max_l {
                // `off < max_l <= end - p` keeps `p + off` in bounds; the plain
                // while-loop (vs a `0..max_l` range) keeps that visible to the
                // verifier.
                window.push(chars[p + off]);
                bounds.push(window.len());
                off += 1;
            }
            // (chars matched, byte length of that prefix of `window`, what hit)
            let mut hit: Option<(usize, usize, CjkHit)> = None;
            let mut l = max_l;
            while l >= 1 {
                // `&str` borrow into `FxHashSet<String>` / `FxHashMap<String, _>`
                // (valid: `String: Borrow<str>`), preserving longest-match-first.
                // `bounds` holds `max_l` char-boundary byte lengths, so both
                // `get`s always succeed; they only make the index / boundary
                // obligations checkable without changing behavior.
                let Some(&cand_bytes) = bounds.get(l - 1) else {
                    break;
                };
                let Some(cand) = window.get(..cand_bytes) else {
                    break;
                };
                if self.cjk_exceptions.contains(cand) {
                    hit = Some((l, cand_bytes, CjkHit::Exception));
                    break;
                }
                if let Some(form) = self.cjk_forms.get(cand).copied() {
                    hit = Some((l, cand_bytes, CjkHit::Form(form)));
                    break;
                }
                l -= 1;
            }
            // `l <= max_l <= end - p` keeps every `p + l` below `end`, so the
            // saturating adds below are exactly `+`; they only discharge the
            // verifier's overflow obligations.
            match hit {
                Some((l, _, CjkHit::Exception)) => p = p.saturating_add(l),
                Some((l, cand_bytes, CjkHit::Form(hit))) => {
                    if l == 1 && !opts.cjk_single_char {
                        p = p.saturating_add(1);
                    } else {
                        let ignored = match (opts.ignore, window.get(..cand_bytes)) {
                            (Some(ig), Some(cand)) => ig.contains(cand),
                            _ => false,
                        };
                        if !ignored {
                            out.push(Match {
                                start: p,
                                end: p.saturating_add(l),
                                class: hit.class,
                                langs: hit.langs,
                                form_id: hit.form_id,
                                form_hash: fnv1a64(&window[..bounds[l - 1]]),
                                ambiguous: hit.ambiguous,
                            });
                        }
                        p = p.saturating_add(l);
                    }
                }
                None => p = p.saturating_add(1),
            }
        }
    }
}

enum CjkHit {
    Form(LexiconHit),
    Exception,
}

/// Intern `code` into `table` (first-appearance order). Empty / `"unknown"`
/// codes — and, deterministically, every NEW code once the table's
/// [`LangSet`]-addressable real slots are exhausted — land in the reserved
/// [`UNKNOWN_LANG`] slot, which [`Lexicon::lang_code`] renders as `"unknown"`.
fn intern_lang(table: &mut Vec<String>, code: &str) -> LangId {
    let code = code.trim();
    if code.is_empty() || code == "unknown" {
        return UNKNOWN_LANG;
    }
    if let Some(i) = table.iter().position(|l| l == code) {
        // Table length is bounded below UNKNOWN_LANG (< 2^16), so the cast is
        // lossless.
        return LangId(i as u16);
    }
    if table.len() >= usize::from(UNKNOWN_LANG.0) {
        return UNKNOWN_LANG;
    }
    table.push(code.to_string());
    LangId((table.len() - 1) as u16)
}

/// Cross-class homograph precedence, TOTAL ORDER: profanity > feline > orca >
/// emphasis. A surface that is an expletive in *any* enabled language (e.g.
/// "poes": Dutch pussycat, Afrikaans vulgar) is treated as profanity, so it
/// sparkles rather than getting a friendly paw; likewise a user emphasis word
/// colliding with a builtin feline form keeps the cat.
fn class_rank(class: Class) -> u8 {
    match class {
        Class::Profanity => 3,
        Class::Feline => 2,
        Class::Orca => 1,
        Class::Emphasis => 0,
    }
}

/// Insert a surface, resolving collisions: a higher-ranked class REPLACES the
/// entry and keeps ONLY its own languages (the surface is displayed as the
/// winning class, so only that class's claimants may be attributed); the SAME
/// class UNIONS the language sets (`kucing` = id ∪ ms ∪ jv); a lower-ranked
/// class is dropped entirely, languages included.
fn insert_class(
    map: &mut FxHashMap<String, (Class, LangSet, bool)>,
    key: String,
    class: Class,
    langs: LangSet,
    ambiguous: bool,
) {
    match map.get_mut(&key) {
        Some((prev, set, prev_ambiguous)) if class_rank(class) > class_rank(*prev) => {
            *prev = class;
            *set = langs;
            *prev_ambiguous = ambiguous;
        }
        Some((prev, set, prev_ambiguous)) if *prev == class => {
            *set = set.union(langs);
            // One unambiguous claimant is sufficient to make the winning
            // surface safe for immediate live-cursor decoration.
            *prev_ambiguous &= ambiguous;
        }
        Some(_) => {}
        None => {
            map.insert(key, (class, langs, ambiguous));
        }
    }
}

/// Freeze a precedence-resolved surface table and assign one collision-free,
/// deterministic ID to every exact key. This is build-time work; scans retain
/// the compact `Copy` value and perform no extra lookup or allocation.
fn finalize_hits(
    map: FxHashMap<String, (Class, LangSet, bool)>,
    next_form_id: &mut u64,
) -> FxHashMap<String, LexiconHit> {
    let mut entries: Vec<_> = map.into_iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut out = FxHashMap::default();
    out.reserve(entries.len());
    for (key, (class, langs, ambiguous)) in entries {
        let form_id = FormId::exact(*next_form_id);
        *next_form_id = next_form_id
            .checked_add(1)
            .expect("compiled lexicon exhausted the FormId namespace");
        out.insert(
            key,
            LexiconHit {
                class,
                langs,
                form_id,
                ambiguous,
            },
        );
    }
    out
}

/// The [`Match::form_hash`] a scan would report for a surface string, exposed
/// so the sparkle-words custom-spec resolver (v3 §6) can key its per-word
/// override table with EXACTLY the scanner's hash. Callers must pass the same
/// text a match carries: the [`fold`]ed full token for spaced surfaces (fold
/// possessive variants whole: `fold(w) + "'s"` etc.), the RAW compound for
/// no-space (CJK) surfaces.
#[must_use]
pub fn form_hash(surface: &str) -> u64 {
    fnv1a64(surface)
}

/// True when `s` is a no-space-script surface (every char is CJK/kana/Hangul/
/// Thai/Lao/Khmer): the custom-spec resolver must register it as a `cjk = true`
/// entry with a RAW (unfolded) key, or the lexicon insert silently drops it.
#[must_use]
pub fn is_no_space_surface(s: &str) -> bool {
    !s.is_empty() && s.chars().all(fold::is_no_space_script)
}

/// FNV-1a over the key's UTF-8 bytes — the [`Match::form_hash`] source. Kept
/// byte-level (not char-level) so the hash of a `&str` slice never allocates.
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Maximal run of token characters starting at `i`, treating an apostrophe or
/// hyphen between two token characters as an interior joiner (so `cat's` and
/// `cat-like` are single tokens).
fn token_end(chars: &[char], i: usize) -> usize {
    let n = chars.len();
    let mut j = i;
    loop {
        while j < n && fold::is_token_char(chars[j]) {
            j += 1;
        }
        if j > i
            && j < n
            && is_interior_joiner(chars[j])
            && j + 1 < n
            && fold::is_token_char(chars[j + 1])
        {
            j += 1; // consume the joiner, keep scanning
        } else {
            break;
        }
    }
    j
}

/// True for a character the scanner treats as an *interior joiner* — an
/// apostrophe or hyphen sitting between two token characters (`cat's`,
/// `cat-like` are single tokens). Public for the same reason as
/// [`is_token_char`]: the host's context tokenizer (the sparkle-words genome's
/// ±K neighbor-token walk) must segment tokens exactly like the scanner, and a
/// re-implemented look-alike joiner set would drift.
#[must_use]
pub fn is_interior_joiner(c: char) -> bool {
    c == '\'' || c == '\u{2019}' || c == '-'
}

/// True if `s` is a single whole-word token: it begins and ends with a token
/// character and contains only token characters and interior joiners. Used to
/// reject lexicon surfaces (`d.m`, `anak kucing`) that the whole-word scanner
/// could never match.
fn is_matchable_token(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    // `first()`/`last()` instead of `chars[0]` / `chars[chars.len() - 1]`: the
    // verifier cannot link `is_empty()` to `len()` across the method boundary;
    // `None` here is exactly the old `is_empty()` early-out.
    let (Some(&first), Some(&last)) = (chars.first(), chars.last()) else {
        return false;
    };
    if !fold::is_token_char(first) || !fold::is_token_char(last) {
        return false;
    }
    for (k, &c) in chars.iter().enumerate() {
        if fold::is_token_char(c) {
            continue;
        }
        let interior_ok = is_interior_joiner(c)
            && k > 0
            && k + 1 < chars.len()
            && fold::is_token_char(chars[k - 1])
            && fold::is_token_char(chars[k + 1]);
        if !interior_ok {
            return false;
        }
    }
    true
}

/// True if the character just LEFT of the token at `i` puts it in a code/path
/// context.
fn left_suppresses(chars: &[char], i: usize) -> bool {
    if i == 0 {
        return false;
    }
    // `get` instead of `chars[i - 1]`: the caller always passes a token start
    // `i <= chars.len()`, but the verifier cannot see that invariant across the
    // call; the `None` arm is unreachable under it.
    let Some(&c) = chars.get(i - 1) else {
        return false;
    };
    // A leading '-' is a CLI flag (`--cat`); interior hyphens are consumed into
    // the token by `token_end`, so a '-' at the left boundary is never prose.
    fold::is_code_adjacent_punct(c)
        || c == '-'
        || (c == '.' && i >= 2 && matches!(chars.get(i - 2), Some(p) if p.is_alphanumeric()))
}

/// True if the character just RIGHT of the token ending at `j` puts it in a
/// code/path context.
fn right_suppresses(chars: &[char], j: usize) -> bool {
    let n = chars.len();
    if j >= n {
        return false;
    }
    let c = chars[j];
    fold::is_code_adjacent_punct(c)
        || ((c == '.' || c == '-') && j + 1 < n && chars[j + 1].is_alphanumeric())
}

/// Strip a trailing English possessive (`'s` / `'`) from a token, if present.
fn strip_possessive(token: &str) -> Option<(&str, u8)> {
    for (variant, suf) in ["'s", "\u{2019}s", "'", "\u{2019}"].into_iter().enumerate() {
        if let Some(stem) = token.strip_suffix(suf)
            && !stem.is_empty()
        {
            return Some((stem, variant as u8 + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex() -> Lexicon {
        Lexicon::with_languages(&["all"])
    }

    #[test]
    fn embedded_lexicon_parses_and_has_no_conflicts() {
        let lx = lex();
        assert!(
            lx.conflicts().is_empty(),
            "lexicon conflicts: {:?}",
            lx.conflicts()
        );
        assert!(lx.spaced_form_count() > 50);
    }

    /// LANGUAGE-COLLISION REGRESSION: Romanian `fut` / `futu` must remain
    /// recognizable in Romanian/all-language configurations, but every scan
    /// reports them as cursor-deferrable collisions. The renderer can therefore
    /// distinguish a provisional `future` prefix from a delimiter-settled word.
    #[test]
    fn romanian_fut_forms_are_ambiguous_in_ro_and_all_but_not_default_english() {
        let en = Lexicon::with_languages(&["en"]);
        let opts = ScanOptions::default();

        for prefix in ["f", "fu", "fut", "futu", "futur", "future"] {
            assert!(
                en.scan(prefix, &opts).is_empty(),
                "default English lexicon classified future prefix {prefix:?}"
            );
        }
        assert!(en.scan("fuc", &opts).is_empty());

        for language in ["ro", "all"] {
            let lexicon = Lexicon::with_languages(&[language]);
            for prefix in ["f", "fu"] {
                assert!(lexicon.scan(prefix, &opts).is_empty());
            }
            for prefix in ["fut", "futu"] {
                let hits = lexicon.scan(prefix, &opts);
                assert_eq!(hits.len(), 1, "{language} lost exact form {prefix:?}");
                assert_eq!(hits[0].class, Class::Profanity);
                assert!(
                    hits[0].ambiguous,
                    "{language} {prefix:?} must defer at a caret"
                );

                let delimited = lexicon.scan(&format!("{prefix} "), &opts);
                assert_eq!(delimited.len(), 1, "{language} lost delimited {prefix:?}");
                assert!(delimited[0].ambiguous);
            }
            for completed in ["futur", "future", "fuc", "fix", "fix!", "fixes"] {
                assert!(
                    lexicon.scan(completed, &opts).is_empty(),
                    "{language} classified harmless token {completed:?}"
                );
            }
            let fuck = lexicon.scan("fuck", &opts);
            assert_eq!(fuck.len(), 1);
            assert_eq!(fuck[0].class, Class::Profanity);
            assert!(
                !fuck[0].ambiguous,
                "fuck must remain an immediate exact match"
            );
        }
    }

    #[test]
    fn resident_scan_scratch_matches_compatibility_and_stops_growing() {
        let lx = lex();
        let opts = ScanOptions {
            allow_bare_cat: true,
            cjk_single_char: true,
            ignore: None,
        };
        // Exercises every resident lane: ordinary and possessive spaced
        // tokens, CJK window/bounds, Arabic definite-article and one-letter
        // clitic fallbacks, and multiple result entries.
        let text = "kitty's 子猫 القطة وقطة fuck";
        let mut compat_chars = Vec::new();
        let mut compat_hits = Vec::new();
        lx.scan_into(text, &opts, &mut compat_chars, &mut compat_hits);

        let mut chars = Vec::new();
        let mut hits = Vec::new();
        let mut scratch = ScanScratch::default();
        lx.scan_into_with_scratch(text, &opts, &mut chars, &mut hits, &mut scratch);
        assert_eq!(chars, compat_chars);
        assert_eq!(hits, compat_hits);
        assert_eq!(hits.len(), 5, "all scratch lanes must reach a real match");

        let capacities = |scratch: &ScanScratch, chars: &Vec<char>, hits: &Vec<Match>| {
            [
                chars.capacity(),
                hits.capacity(),
                scratch.token.capacity(),
                scratch.folded.capacity(),
                scratch.candidate_folded.capacity(),
                scratch.window.capacity(),
                scratch.bounds.capacity(),
            ]
        };
        let warm = capacities(&scratch, &chars, &hits);
        for _ in 0..1000 {
            lx.scan_into_with_scratch(text, &opts, &mut chars, &mut hits, &mut scratch);
            assert_eq!(hits, compat_hits);
        }
        assert_eq!(
            capacities(&scratch, &chars, &hits),
            warm,
            "resident scanner buffers must stop growing after warmup"
        );
    }

    // Regression for the CJK maximal-run scratch reuse (scan_cjk_run builds the
    // longest candidate once into `window`/`bounds` and probes prefix slices
    // instead of allocating a fresh `String` per candidate length): longest
    // match still wins, offsets are correct across multiple runs on one line,
    // and the CJK ignore guard still keys on the raw compound substring.
    #[test]
    fn cjk_run_scratch_reuse_preserves_matches_and_ignore() {
        use std::collections::HashSet;
        let lx = lex();
        let o = ScanOptions::default();
        // Two CJK runs separated by an ASCII space share the same window/bounds
        // scratch; each whole compound (length > 1) matches at the right chars.
        let got = lx.scan("子猫 ねこ", &o);
        assert_eq!(got.len(), 2, "got {got:?}");
        assert_eq!(
            (got[0].start, got[0].end, got[0].class),
            (0, 2, Class::Feline)
        );
        assert_eq!(
            (got[1].start, got[1].end, got[1].class),
            (3, 5, Class::Feline)
        );
        // The CJK ignore guard reuses the `window` prefix slice (the raw compound
        // substring), so ignoring the raw form suppresses the match.
        let ignore: HashSet<String> = ["子猫".to_string()].into_iter().collect();
        let oi = ScanOptions {
            ignore: Some(&ignore),
            ..ScanOptions::default()
        };
        assert!(
            lx.scan("子猫", &oi).is_empty(),
            "ignored CJK compound must not match"
        );
    }

    // Regression for the per-token fold reuse (try_spaced_token threads a `folded`
    // scratch that classify_token_into fills with the FULL-token fold for its
    // first lookup, reused by the ignore / bare-cat guards). A possessive token
    // classifies via stem stripping, but the guards must still key on the full
    // surface fold, exactly as the pre-refactor `fold(&token)` did.
    #[test]
    fn ignore_guard_keys_on_full_token_fold_not_stripped_stem() {
        use std::collections::HashSet;
        let lx = lex();
        // "kitty's" -> possessive-stripped to "kitty" (Feline), full fold "kitty's".
        assert_eq!(lx.scan("kitty's", &ScanOptions::default()).len(), 1);

        let full: HashSet<String> = ["kitty's".to_string()].into_iter().collect();
        let opt_full = ScanOptions {
            ignore: Some(&full),
            ..ScanOptions::default()
        };
        assert!(
            lx.scan("kitty's", &opt_full).is_empty(),
            "ignoring the full surface must suppress the match"
        );

        let stem: HashSet<String> = ["kitty".to_string()].into_iter().collect();
        let opt_stem = ScanOptions {
            ignore: Some(&stem),
            ..ScanOptions::default()
        };
        assert_eq!(
            lx.scan("kitty's", &opt_stem).len(),
            1,
            "ignoring only the stripped stem must NOT suppress (full-token fold is the key)"
        );
    }

    // Regression for the unbounded-alloc / quadratic-memory DoS: a config/override
    // lexicon with a pathological suffix entry (stems × suffixes far above the cap)
    // must be skipped instead of materializing the full cartesian product, while a
    // normal small entry still expands fully and the tiny builtin is untouched.
    #[test]
    fn suffix_expansion_is_globally_bounded() {
        // Build the override TOML programmatically: a normal 2×2 entry (must expand
        // fully) plus a pathological 2000×2000 = 4_000_000-surface entry (product
        // alone > MAX_TOTAL_SURFACES, so it must be dropped).
        let mut toml = String::new();
        toml.push_str(
            "[[entry]]\nclass = \"feline\"\nlang = \"xx\"\nmode = \"suffix\"\n\
             stems = [\"qwxfeline\", \"qwxkitten\"]\nsuffixes = [\"\", \"z\"]\n\n",
        );
        toml.push_str("[[entry]]\nclass = \"feline\"\nlang = \"yy\"\nmode = \"suffix\"\nstems = [");
        for i in 0..2000 {
            if i > 0 {
                toml.push_str(", ");
            }
            toml.push_str(&format!("\"qwxstem{i}\""));
        }
        toml.push_str("]\nsuffixes = [");
        for i in 0..2000 {
            if i > 0 {
                toml.push_str(", ");
            }
            toml.push_str(&format!("\"qwxsuf{i}\""));
        }
        toml.push_str("]\n");

        let lx = Lexicon::with_languages_and_override(&["all"], Some(&toml))
            .expect("override lexicon must parse");

        // The build completed without exploding and the total stays bounded.
        assert!(
            lx.spaced_form_count() <= MAX_TOTAL_SURFACES,
            "surface count {} exceeded cap {MAX_TOTAL_SURFACES}",
            lx.spaced_form_count(),
        );
        // The pathological expansion was skipped and recorded as a build conflict.
        assert!(
            lx.conflicts().iter().any(|c| c.contains("surface cap")),
            "expected a surface-cap conflict, got {:?}",
            lx.conflicts(),
        );
        // The normal small entry still expanded fully (stems × suffixes present).
        assert_eq!(lx.classify_token("qwxfeline"), Some(Class::Feline));
        assert_eq!(lx.classify_token("qwxfelinez"), Some(Class::Feline));
        assert_eq!(lx.classify_token("qwxkittenz"), Some(Class::Feline));
        // No surface from the dropped pathological entry made it in.
        assert_eq!(lx.classify_token("qwxstem0qwxsuf0"), None);

        // The tiny embedded builtin is unaffected: it still builds cleanly and its
        // known surfaces remain present (mirrors embedded_lexicon_parses_* above).
        let builtin = Lexicon::with_languages(&["all"]);
        assert!(
            builtin.conflicts().is_empty(),
            "builtin conflicts: {:?}",
            builtin.conflicts(),
        );
        assert!(builtin.spaced_form_count() > 50);
        assert_eq!(builtin.classify_token("kitty"), Some(Class::Feline));
    }

    #[test]
    fn langset_bitset_mechanics() {
        let a = LangSet::EMPTY.with(LangId(3)).with(LangId(7));
        let b = LangSet::EMPTY.with(LangId(1));
        let u = a.union(b);
        assert!(u.contains(LangId(1)) && u.contains(LangId(3)) && u.contains(LangId(7)));
        assert!(!u.contains(LangId(0)));
        assert_eq!(u.len(), 3);
        assert!(!u.is_empty());
        assert!(LangSet::EMPTY.is_empty());
        // Iteration is ascending id order; primary is the lowest set bit.
        assert_eq!(
            u.iter().collect::<Vec<_>>(),
            vec![LangId(1), LangId(3), LangId(7)]
        );
        assert_eq!(primary_lang(u), LangId(1));
        // The empty set resolves to the reserved "unknown" slot.
        assert_eq!(primary_lang(LangSet::EMPTY), UNKNOWN_LANG);
        assert_eq!(lex().lang_code(primary_lang(LangSet::EMPTY)), "unknown");
    }

    // The LangSet bitset holds 63 real codes + the reserved "unknown" slot;
    // an (override-sized) lexicon with more distinct languages must
    // deterministically collapse the overflow into "unknown" instead of
    // wrapping or panicking.
    #[test]
    fn lang_interning_overflow_collapses_to_unknown() {
        let mut toml = String::new();
        for i in 0..70 {
            toml.push_str(&format!(
                "[[entry]]\nclass=\"feline\"\nlang=\"l{i}\"\nmode=\"forms\"\nforms=[\"qwxlang{i}\"]\n\n"
            ));
        }
        let lx = Lexicon::from_sources(&toml, "", &["all"]).expect("sources parse");
        let o = ScanOptions::default();
        // The first 63 codes intern real slots…
        for i in [0usize, 62] {
            let ms = lx.scan(&format!("qwxlang{i}"), &o);
            assert_eq!(ms.len(), 1);
            assert_eq!(lx.lang_code(primary_lang(ms[0].langs)), format!("l{i}"));
        }
        // …every later code lands in the reserved "unknown" bucket.
        for i in [63usize, 69] {
            let ms = lx.scan(&format!("qwxlang{i}"), &o);
            assert_eq!(ms.len(), 1);
            assert_eq!(lx.lang_code(primary_lang(ms[0].langs)), "unknown");
        }
    }
}
