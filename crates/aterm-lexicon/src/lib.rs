// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Multilingual whole-word lexical matching for the "sparkle words" text decorations: a curated lexicon of surface forms per [`Class`] and a scanner that finds whole-word occurrences in a line of text.

mod fold;
mod raw;
pub mod tricks;

pub use fold::{
    fold, fold_into, has_foldable_marks, is_code_adjacent_punct, is_no_space_script, is_token_char,
};
pub use tricks::{RowKind, Trick, TrickLexicon, TrickRole, TrickRow};

use aterm_hash::{FxHashMap, FxHashSet};
use raw::{RawEntry, RawException, RawExceptions, RawLexicon};
use std::sync::OnceLock;

/// Which decoration family a matched word belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Class {
    /// The "fuck" expletive family — drawn as the randomized sparkle.
    Profanity,
    /// The cat / kitty / kitten family — drawn as the steady cat-paw.
    Feline,
    /// The dog / puppy family — the canine counterpart of [`Class::Feline`].
    Canine,
    /// Ambient animal nouns (monkey / camel / penguin / …), each entry tagged with a `species` id naming the sprite the renderer peeks beside the word.
    Animal,
    /// Hype / emphasis words, drawn as the animated-ink shimmer only (no sprite overlay).
    Emphasis,
}

impl Class {
    /// Canonical lowercase name of this decoration class.
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Profanity => "profanity",
            Self::Feline => "feline",
            Self::Canine => "canine",
            Self::Animal => "animal",
            Self::Emphasis => "emphasis",
        }
    }

    /// Return `true` iff this class is [`Class::Animal`].
    #[inline]
    #[must_use]
    pub const fn is_animal(self) -> bool {
        matches!(self, Self::Animal)
    }

    /// Return `true` iff matches in this class paint a sprite/particle overlay (all except [`Class::Emphasis`]).
    #[inline]
    #[must_use]
    pub const fn has_sprite_overlay(self) -> bool {
        !matches!(self, Self::Emphasis)
    }
}

/// Interned language identifier: an index into the [`Lexicon`]'s language table ([`Lexicon::lang_code`] resolves it).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct LangId(pub u16);

/// The reserved id for an empty or `"unknown"` `lang` code, and the overflow bucket once every
/// real [`LangSet`] slot is taken; the language table never grows into it.
const UNKNOWN_LANG: LangId = LangId((LangSet::CAPACITY - 1) as u16);

/// Interned species identifier for [`Class::Animal`] surfaces, in first-appearance TOML order: [`Lexicon::species_code`] resolves it to the TOML `species` id, the renderer's sprite key.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct SpeciesId(pub u16);

/// A set of interned languages, as a `u64` bitset (bit `i` ⇔ `LangId(i)`), so a match knows every language that claims its surface (`kucing` is id, ms and jv).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct LangSet(u64);

impl LangSet {
    /// Number of distinct bits (= max interned languages, incl. `"unknown"`).
    pub const CAPACITY: usize = 64;
    /// The empty set.
    pub const EMPTY: LangSet = LangSet(0);

    /// Construct a `LangSet` directly from its raw 64-bit mask.
    #[inline]
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Return the raw 64-bit mask of this set.
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// This set plus `id`.
    #[inline]
    #[must_use]
    pub const fn with(self, id: LangId) -> LangSet {
        // The `& 63` is a no-op for every id this crate hands out (ids are
        // bounded by CAPACITY at intern time); it makes the shift's
        // no-overflow obligation checkable in const contexts.
        LangSet(self.0 | (1u64 << ((id.0 as u32) & 63)))
    }

    /// Whether `id` is in the set.
    #[inline]
    #[must_use]
    pub const fn contains(self, id: LangId) -> bool {
        self.0 & (1u64 << ((id.0 as u32) & 63)) != 0
    }

    /// Set union.
    #[inline]
    #[must_use]
    pub const fn union(self, other: LangSet) -> LangSet {
        LangSet(self.0 | other.0)
    }

    /// True when no language is set.
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Number of languages in the set.
    #[inline]
    #[must_use]
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Encode the 64-bit language mask into 8 little-endian bytes.
    #[inline]
    #[must_use]
    pub const fn encode_le(self) -> [u8; 8] {
        let v = self.0;
        [
            v as u8,
            (v >> 8) as u8,
            (v >> 16) as u8,
            (v >> 24) as u8,
            (v >> 32) as u8,
            (v >> 40) as u8,
            (v >> 48) as u8,
            (v >> 56) as u8,
        ]
    }

    /// Decode 8 little-endian bytes into a `LangSet`.
    #[inline]
    #[must_use]
    pub const fn decode_le(bytes: [u8; 8]) -> Self {
        Self(
            (bytes[0] as u64)
                | ((bytes[1] as u64) << 8)
                | ((bytes[2] as u64) << 16)
                | ((bytes[3] as u64) << 24)
                | ((bytes[4] as u64) << 32)
                | ((bytes[5] as u64) << 40)
                | ((bytes[6] as u64) << 48)
                | ((bytes[7] as u64) << 56),
        )
    }

    /// Iterate the members in ascending [`LangId`] order — i.e. lexicon first-appearance order, so the first yielded id is [`primary_lang`].
    #[inline]
    pub fn iter(self) -> impl Iterator<Item = LangId> {
        let mut rest = self.0;
        std::iter::from_fn(move || {
            if rest == 0 {
                return None;
            }
            let bit = rest.trailing_zeros();
            rest &= rest.wrapping_sub(1);
            Some(LangId(bit as u16))
        })
    }
}

/// The set's primary language: the lowest set bit, i.e. the member whose entry appeared first in the lexicon TOML.
#[inline]
#[must_use]
pub const fn primary_lang(set: LangSet) -> LangId {
    if set.is_empty() {
        return UNKNOWN_LANG;
    }
    LangId(set.0.trailing_zeros() as u16)
}

/// One whole-word occurrence found by [`Lexicon::scan`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Match {
    /// First character index of the matched word (inclusive).
    pub start: usize,
    /// One past the last character index of the matched word (exclusive).
    pub end: usize,
    /// Which decoration family matched.
    pub class: Class,
    /// Every language whose entry lists the matched surface under this class (`kucing` → id ∪ ms ∪ jv).
    pub langs: LangSet,
    /// Collision-free identity of the exact compiled surface, unique within one [`Lexicon`].
    pub form_id: FormId,
    /// FNV-1a hash of the matched key: the folded whole-word surface for spaced scripts, the raw compound for CJK.
    pub form_hash: u64,
    /// Whether the winning surface is language-gated because it collides with ordinary text in another language; renderers defer such a match while it touches the live input cursor.
    pub ambiguous: bool,
    /// For [`Class::Animal`] matches, the species sprite this surface names ([`Lexicon::species_code`] resolves it); `None` for every other class.
    pub species: Option<SpeciesId>,
}

/// Collision-free identity of one exact recognized surface in a compiled [`Lexicon`]: opaque and local to that lexicon, so compare values but do not persist or interpret them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FormId(u64);

impl FormId {
    /// Sentinel for synthetic host-side test occurrences that did not come from a lexicon scan.
    pub const UNKNOWN: Self = Self(u64::MAX);

    /// Return the raw 64-bit form identifier.
    #[inline]
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Return the low-byte morphology variant tag (`0` for exact table keys).
    #[inline]
    #[must_use]
    pub const fn variant_byte(self) -> u8 {
        self.0 as u8
    }

    /// Return the base table key index (`self.0 >> 8`).
    #[inline]
    #[must_use]
    pub const fn base_id(self) -> u64 {
        self.0 >> 8
    }

    /// Encode this `FormId` into 8 little-endian bytes.
    #[inline]
    #[must_use]
    pub const fn encode_le(self) -> [u8; 8] {
        let v = self.0;
        [
            v as u8,
            (v >> 8) as u8,
            (v >> 16) as u8,
            (v >> 24) as u8,
            (v >> 32) as u8,
            (v >> 40) as u8,
            (v >> 48) as u8,
            (v >> 56) as u8,
        ]
    }

    /// Decode 8 little-endian bytes into a `FormId`.
    #[inline]
    #[must_use]
    pub const fn decode_le(bytes: [u8; 8]) -> Self {
        Self(
            (bytes[0] as u64)
                | ((bytes[1] as u64) << 8)
                | ((bytes[2] as u64) << 16)
                | ((bytes[3] as u64) << 24)
                | ((bytes[4] as u64) << 32)
                | ((bytes[5] as u64) << 40)
                | ((bytes[6] as u64) << 48)
                | ((bytes[7] as u64) << 56),
        )
    }

    /// Exact table keys use variant 0 of the low byte; the possessive and clitic fallbacks of
    /// [`Lexicon::classify_token_into`] use a nonzero code, keeping `kitty`, `kitty's` and each
    /// prefix form distinct without a probabilistic token hash.
    #[inline]
    const fn exact(base: u64) -> Self {
        Self(base << 8)
    }

    #[inline]
    const fn with_variant(self, variant: u8) -> Self {
        debug_assert!(variant != 0);
        debug_assert!((self.0 & 0xff) == 0);
        Self(self.0 | (variant as u64))
    }
}

/// Tunables that the host's config maps onto the scan.
#[derive(Clone, Debug, Default)]
pub struct ScanOptions<'a> {
    /// Decorate the bare 3-letter token `cat` (and equally short feline tokens).
    pub allow_bare_cat: bool,
    /// Decorate a lone single-character CJK surface anywhere in a run, even inside an unlisted compound.
    pub cjk_single_char: bool,
    /// Folded surfaces to never decorate (the user's `ignore_words` / global `deny`, already folded by the caller via [`fold`]).
    pub ignore: Option<&'a std::collections::HashSet<String>>,
}

/// Caller-owned buffers for [`Lexicon::scan_into_with_scratch`].
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
    Toml(aterm_toml::de::Error),
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexError::Toml(e) => {
                f.write_str("lexicon TOML parse error: ")?;
                f.write_str(&e.to_string())
            }
        }
    }
}

impl std::error::Error for LexError {}

/// Renders a value's `Debug` output through `Display`, so `.to_string()` yields the `{:?}` text
/// without expanding a std format macro (whose hardened unsafe op the verifier cannot discharge)
/// into the caller.
struct DebugText<T: std::fmt::Debug>(T);

impl<T: std::fmt::Debug> std::fmt::Display for DebugText<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

/// One `[[entry]]` of the embedded `data/lexicon.toml`, decoded by the build
/// script through the run-time shapes ([`RawEntry`]) and kept as it reads there,
/// without its reviewer `notes`.
struct BuiltinEntry {
    class: &'static str,
    lang: &'static str,
    mode: &'static str,
    stems: &'static [&'static str],
    suffixes: &'static [&'static str],
    forms: &'static [&'static str],
    cjk: bool,
    ambiguous: bool,
    species: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/builtin_lexicon.rs"));

/// The embedded documents' rows, as decoding `data/lexicon.toml` and
/// `data/exceptions.toml` would give them: the build script decoded them, so a
/// launch parses no TOML for the builtin lexicon.
fn builtin_rows() -> (RawLexicon, RawExceptions) {
    let owned = |list: &[&str]| -> Vec<String> { list.iter().map(|s| (*s).to_owned()).collect() };
    let entry = BUILTIN_ENTRIES
        .iter()
        .map(|e| RawEntry {
            class: e.class.to_owned(),
            lang: e.lang.to_owned(),
            mode: e.mode.to_owned(),
            stems: owned(e.stems),
            suffixes: owned(e.suffixes),
            forms: owned(e.forms),
            cjk: e.cjk,
            ambiguous: e.ambiguous,
            species: e.species.to_owned(),
            _notes: String::new(),
            user: false,
        })
        .collect();
    let exception = BUILTIN_EXCEPTION_WORDS
        .iter()
        .map(|word| RawException {
            word: (*word).to_owned(),
            _lang: String::new(),
            _meaning: String::new(),
        })
        .collect();
    (RawLexicon { entry }, RawExceptions { exception })
}

/// Cap on surface forms materialized across all entries in [`Lexicon::build`]. The builtin makes
/// about 1,500; the cap stops an override with huge `stems`/`suffixes`/`forms` arrays from driving
/// O(stems·suffixes) memory and build time.
const MAX_TOTAL_SURFACES: usize = 1 << 20;

/// Longest no-space surface, in characters, that [`Lexicon::build`] compiles: the bound on the
/// maximal-run probe window ([`Lexicon::max_cjk`]).
const MAX_CJK_SURFACE_CHARS: usize = 32;

/// A compiled lexicon: folded surface forms mapped to their decoration class, plus the CJK compound tables.
#[derive(Debug, Clone)]
pub struct Lexicon {
    /// Folded whole-word surface → (class, claiming languages), for
    /// whitespace-delimited scripts.
    spaced: FxHashMap<String, LexiconHit>,
    /// Raw CJK compound → (class, claiming languages), for no-space scripts
    /// (matched by maximal run).
    cjk_forms: FxHashMap<String, LexiconHit>,
    /// Raw CJK compounds that contain a surface but are not that word (山猫 lynx, 猫車
    /// wheelbarrow, カメラ), suppressed during the maximal-run match.
    cjk_exceptions: FxHashSet<String>,
    /// Longest CJK key (chars), bounding the maximal-run search window.
    max_cjk: usize,
    /// First character of every [`Self::cjk_forms`] key and [`Self::cjk_exceptions`] member → the
    /// length in chars of the longest surface starting with it, built by [`note_cjk_head`] from
    /// the finished tables.
    cjk_heads: FxHashMap<char, usize>,
    /// Interned language codes, in first-appearance TOML order ([`LangId`] `i`
    /// ⇔ `langs[i]`).
    langs: Vec<String>,
    /// Interned [`Class::Animal`] species ids, in first-appearance TOML order
    /// ([`SpeciesId`] `i` ⇔ `species[i]`).
    species: Vec<String>,
    /// Data problems found during build; see [`Self::conflicts`].
    conflicts: Vec<String>,
    /// Folded surfaces from user config (override TOML, `extra_words`, custom specs): the scanner's
    /// short-word emphasis guard exempts them, so a short custom word ("gg") still matches.
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
    species: Option<SpeciesId>,
    /// The marks-required gate: every claiming surface lost diacritics in folding into a
    /// bare-ASCII key, which plain ASCII text spells as something else (Vietnamese `đm` → `dm`, as
    /// in `dmg`; Spanish `coño` → `cono` in `conoid`).
    marks_required: bool,
}

/// The set of languages whose `ambiguous` entries are enabled.
struct Langs {
    all: bool,
    set: FxHashSet<String>,
    /// The codes as the caller listed them, so an unknown one is reported in order and as written.
    listed: Vec<String>,
}

impl Langs {
    fn new(langs: &[&str]) -> Self {
        let all = langs.contains(&"all");
        let listed: Vec<String> = langs.iter().map(|l| (*l).to_string()).collect();
        let set = listed.iter().cloned().collect();
        Langs { all, set, listed }
    }
    fn enabled(&self, lang: &str) -> bool {
        self.all || self.set.contains(lang)
    }
    /// Report every configured code the loaded data never names: it un-gates nothing (`"DA"`,
    /// `"da-DK"`, `"dk"`, `"ALL"`), and the person who set it must be told.
    fn note_unknown(&self, known: &FxHashSet<String>, conflicts: &mut Vec<String>) {
        for lang in &self.listed {
            if lang == "all" || known.contains(lang) {
                continue;
            }
            let mut msg = String::from("configured language ");
            msg.push_str(&DebugText(lang.as_str()).to_string());
            msg.push_str(
                " names nothing in this vocabulary: it un-gated nothing (the codes \
                 are the bare lowercase ones the data uses)",
            );
            conflicts.push(msg);
        }
    }
}

impl Lexicon {
    /// The embedded lexicon with only English `ambiguous` entries un-gated (the default `languages = ["en"]`).
    #[must_use]
    pub fn builtin() -> &'static Lexicon {
        static L: OnceLock<Lexicon> = OnceLock::new();
        L.get_or_init(|| Self::with_languages(&["en"]))
    }

    /// The embedded lexicon, un-gating `ambiguous` entries for `langs` (`["all"]` un-gates everything).
    #[must_use]
    pub fn with_languages(langs: &[&str]) -> Lexicon {
        let (raw, raw_exc) = builtin_rows();
        Self::build(raw, raw_exc, &Langs::new(langs))
    }

    /// The embedded lexicon with an optional user `[[entry]]` TOML appended, so it extends coverage under the same folding and conflict detection.
    pub fn with_languages_and_override(
        langs: &[&str],
        extra_toml: Option<&str>,
    ) -> Result<Lexicon, LexError> {
        let (mut raw, raw_exc) = builtin_rows();
        if let Some(t) = extra_toml {
            let user: RawLexicon = aterm_toml::from_str(t).map_err(LexError::Toml)?;
            raw.entry.extend(user.entry.into_iter().map(|mut e| {
                e.user = true;
                e
            }));
        }
        Ok(Self::build(raw, raw_exc, &Langs::new(langs)))
    }

    /// Build a lexicon from TOML sources (a lexicon document and an exceptions document, decoded here).
    pub fn from_sources(
        lexicon_toml: &str,
        exceptions_toml: &str,
        langs: &[&str],
    ) -> Result<Lexicon, LexError> {
        let raw: RawLexicon = aterm_toml::from_str(lexicon_toml).map_err(LexError::Toml)?;
        let raw_exc: RawExceptions =
            aterm_toml::from_str(exceptions_toml).map_err(LexError::Toml)?;
        Ok(Self::build(raw, raw_exc, &Langs::new(langs)))
    }

    fn build(raw: RawLexicon, raw_exc: RawExceptions, langs: &Langs) -> Lexicon {
        let mut spaced: FxHashMap<String, PendingHit> = FxHashMap::default();
        let mut cjk_forms: FxHashMap<String, PendingHit> = FxHashMap::default();
        let mut cjk_exceptions: FxHashSet<String> = FxHashSet::default();
        let mut user_surfaces: FxHashSet<String> = FxHashSet::default();
        let mut conflicts = Vec::new();
        let mut max_cjk = 1usize;
        let mut lang_table: Vec<String> = Vec::new();
        let mut species_table: Vec<String> = Vec::new();
        let mut total: usize = 0;
        let mut data_langs: FxHashSet<String> = FxHashSet::default();

        for e in raw.entry {
            if !data_langs.contains(e.lang.as_str()) {
                data_langs.insert(e.lang.clone());
            }
            let class = match e.class.as_str() {
                "profanity" => Class::Profanity,
                "feline" => Class::Feline,
                "canine" => Class::Canine,
                "orca" => continue,
                "animal" => Class::Animal,
                "emphasis" => Class::Emphasis,
                other => {
                    let mut msg = String::from("unknown class ");
                    msg.push_str(&DebugText(other).to_string());
                    msg.push_str(" (lang ");
                    msg.push_str(&e.lang);
                    msg.push(')');
                    conflicts.push(msg);
                    continue;
                }
            };
            let lset = LangSet::EMPTY.with(intern_lang(&mut lang_table, &e.lang));
            let species = match (class, e.species.trim()) {
                (Class::Animal, "") => {
                    let mut msg = String::from("animal entry without species (lang ");
                    msg.push_str(&e.lang);
                    msg.push_str("): entry skipped");
                    conflicts.push(msg);
                    continue;
                }
                (Class::Animal, sp) => Some(intern_species(&mut species_table, sp)),
                (_, "") => None,
                (_, sp) => {
                    let mut msg = String::from("species ");
                    msg.push_str(&DebugText(sp).to_string());
                    msg.push_str(" on non-animal entry (lang ");
                    msg.push_str(&e.lang);
                    msg.push_str("): field ignored");
                    conflicts.push(msg);
                    None
                }
            };
            if e.ambiguous && !langs.enabled(&e.lang) {
                continue;
            }

            let mut surfaces: Vec<String> = Vec::new();
            if e.mode != "suffix" && e.mode != "forms" {
                let mut msg = String::from("unknown mode ");
                msg.push_str(&DebugText(e.mode.as_str()).to_string());
                msg.push_str(" (lang ");
                msg.push_str(&e.lang);
                msg.push_str("): expected \"suffix\" or \"forms\"; `stems` and `suffixes` ignored");
                conflicts.push(msg);
            }
            if e.mode == "suffix" {
                let prod = e.stems.len().saturating_mul(e.suffixes.len().max(1));
                if prod > MAX_TOTAL_SURFACES || total.saturating_add(prod) > MAX_TOTAL_SURFACES {
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
            let forms = e.forms.len();
            if total.saturating_add(forms) > MAX_TOTAL_SURFACES {
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

            let offered = surfaces.len();
            let mut inserted = 0usize;

            for s in surfaces {
                let s = s.trim();
                if s.is_empty() {
                    continue;
                }
                if e.cjk {
                    let key: String = s.to_string();
                    let len = key.chars().count();
                    if len > MAX_CJK_SURFACE_CHARS {
                        push_overlong_surface_conflict(&mut conflicts, len, Some(&e.lang));
                        continue;
                    }
                    if len > max_cjk {
                        max_cjk = len;
                    }
                    if e.user && len == 1 {
                        let mut msg = String::from("single-char CJK surface ");
                        msg.push_str(&DebugText(&key).to_string());
                        msg.push_str(" (lang ");
                        msg.push_str(&e.lang);
                        msg.push_str(") requires cjk_single_char = true to scan");
                        conflicts.push(msg);
                    }
                    inserted = inserted.saturating_add(1);
                    insert_class(
                        &mut cjk_forms,
                        key,
                        PendingHit {
                            class,
                            langs: lset,
                            ambiguous: e.ambiguous,
                            species,
                            marks_required: false,
                        },
                        &mut conflicts,
                    );
                } else {
                    let key = fold(s);
                    if key.is_empty() || !is_matchable_token(&key) {
                        if e.user {
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
                        // Explicit config is consent: the short-word guards exempt this surface.
                        user_surfaces.insert(key.clone());
                    }
                    let marks_required = key.is_ascii() && fold::has_foldable_marks(s);
                    inserted = inserted.saturating_add(1);
                    insert_class(
                        &mut spaced,
                        key,
                        PendingHit {
                            class,
                            langs: lset,
                            ambiguous: e.ambiguous,
                            species,
                            marks_required,
                        },
                        &mut conflicts,
                    );
                }
            }

            if offered > 0 && inserted == 0 {
                let mut msg = String::from("entry (class ");
                msg.push_str(&e.class);
                msg.push_str(", lang ");
                msg.push_str(&e.lang);
                msg.push_str(") compiled to NO scannable surface: all ");
                msg.push_str(&offered.to_string());
                msg.push_str(" are multi-word, mixed-script, overlong or empty");
                conflicts.push(msg);
            }
        }

        langs.note_unknown(&data_langs, &mut conflicts);

        for x in raw_exc.exception {
            let key = x.word.trim().to_string();
            if key.is_empty() {
                continue;
            }
            let len = key.chars().count();
            if len > MAX_CJK_SURFACE_CHARS {
                push_overlong_surface_conflict(&mut conflicts, len, None);
                continue;
            }
            if len > max_cjk {
                max_cjk = len;
            }
            cjk_exceptions.insert(key);
        }

        let mut next_form_id = 0u64;
        let spaced = finalize_hits(spaced, &mut next_form_id);
        let cjk_forms = finalize_hits(cjk_forms, &mut next_form_id);

        let mut cjk_heads: FxHashMap<char, usize> = FxHashMap::default();
        for key in cjk_forms.keys() {
            note_cjk_head(&mut cjk_heads, key);
        }
        for key in &cjk_exceptions {
            note_cjk_head(&mut cjk_heads, key);
        }

        Lexicon {
            spaced,
            cjk_forms,
            cjk_exceptions,
            max_cjk,
            cjk_heads,
            langs: lang_table,
            species: species_table,
            conflicts,
            user_surfaces,
        }
    }

    /// The interned code for `id` (`"en"`, `"ja"`, …); the reserved [`UNKNOWN_LANG`] slot — and any id this lexicon never handed out — renders as `"unknown"`.
    #[inline]
    #[must_use]
    pub fn lang_code(&self, id: LangId) -> &str {
        self.langs
            .get(usize::from(id.0))
            .map_or("unknown", String::as_str)
    }

    /// The interned species id for an animal match (`"monkey"`, `"camel"`, …) — the renderer's authored-sprite key.
    #[inline]
    #[must_use]
    pub fn species_code(&self, id: SpeciesId) -> &str {
        self.species
            .get(usize::from(id.0))
            .map_or("unknown", String::as_str)
    }

    /// Every interned species id, in [`SpeciesId`] order: what `animal_art_quality` checks against the authored art, both ways.
    #[inline]
    #[must_use]
    pub const fn species_codes(&self) -> &[String] {
        self.species.as_slice()
    }

    /// Data problems found at build time: a cross-class or cross-species claim, an unknown class or mode, a surface cap hit, a user surface that can never scan as written.
    #[inline]
    #[must_use]
    pub const fn conflicts(&self) -> &[String] {
        self.conflicts.as_slice()
    }

    /// Total number of folded single-word surfaces.
    #[inline]
    #[must_use]
    pub fn spaced_form_count(&self) -> usize {
        self.spaced.len()
    }

    /// Iterate the folded whitespace-script surfaces and their class (validator).
    #[inline]
    pub fn iter_spaced(&self) -> impl Iterator<Item = (&str, Class)> {
        self.spaced.iter().map(|(k, v)| (k.as_str(), v.class))
    }

    /// Iterate the CJK compound surfaces and their class (validator).
    #[inline]
    pub fn iter_cjk(&self) -> impl Iterator<Item = (&str, Class)> {
        self.cjk_forms.iter().map(|(k, v)| (k.as_str(), v.class))
    }

    /// Whether the spaced surface behind `folded_key` matches only tokens that themselves carry folded-away marks ([`has_foldable_marks`]); `false` for an unknown key.
    #[inline]
    #[must_use]
    pub fn surface_requires_marks(&self, folded_key: &str) -> bool {
        self.spaced
            .get(folded_key)
            .is_some_and(|hit| hit.marks_required)
    }

    /// Whether at least one compiled surface in `class` passes the scanner's static policy gates and is not claimed by an overriding runtime table (`overridden` gets each surface's form hash).
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
                && !(class == Class::Feline
                    && !opts.allow_bare_cat
                    && surface.chars().count() <= 3
                    && !hit.marks_required)
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

    /// Classify an already-isolated whole token (no surrounding context).
    #[must_use]
    pub fn classify_token(&self, token: &str) -> Option<Class> {
        let mut folded = String::new();
        let mut candidate_folded = String::new();
        self.classify_token_into(token, &mut folded, &mut candidate_folded)
            .map(|hit| hit.class)
    }

    /// [`classify_token`](Self::classify_token) into caller scratch, returning the whole hit. On
    /// return `folded` holds the full-token fold (the first lookup key), which the scanner reuses
    /// for its guards; the possessive and clitic fallbacks fold into `candidate_folded`.
    fn classify_token_into(
        &self,
        token: &str,
        folded: &mut String,
        candidate_folded: &mut String,
    ) -> Option<LexiconHit> {
        let admits = |c: &LexiconHit| !c.marks_required || fold::has_foldable_marks(token);
        fold::fold_into(token, folded);
        if let Some(c) = self.spaced.get(folded.as_str()).copied()
            && admits(&c)
        {
            return Some(c);
        }
        if let Some((stripped, variant)) = strip_possessive(token) {
            fold::fold_into(stripped, candidate_folded);
            if let Some(mut c) = self.spaced.get(candidate_folded.as_str()).copied()
                && admits(&c)
            {
                c.form_id = c.form_id.with_variant(variant);
                return Some(c);
            }
        }

        if let Some(rest) = token.strip_prefix("\u{0627}\u{0644}")
            && rest.chars().count() >= 2
        {
            fold::fold_into(rest, candidate_folded);
            if let Some(mut c) = self.spaced.get(candidate_folded.as_str()).copied()
                && admits(&c)
            {
                c.form_id = c.form_id.with_variant(5);
                return Some(c);
            }
        }

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
            if let Some(mut c) = self.spaced.get(candidate_folded.as_str()).copied()
                && admits(&c)
            {
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
    #[must_use]
    pub fn scan(&self, text: &str, opts: &ScanOptions) -> Vec<Match> {
        let mut chars = Vec::new();
        let mut out = Vec::new();
        let mut scratch = ScanScratch::default();
        self.scan_into_with_scratch(text, opts, &mut chars, &mut out, &mut scratch);
        out
    }

    /// Like [`scan`](Self::scan), but reuses every caller-owned buffer (cleared first).
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
                if !(left_suppresses(chars, i) || right_suppresses(chars, k)) {
                    self.scan_cjk_run(chars, i, k, opts, window, bounds, out);
                }
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
        let hi = if j < chars.len() { j } else { chars.len() };
        let lo = if i < hi { i } else { hi };
        token.extend(chars[lo..hi].iter());
        let Some(hit) = self.classify_token_into(token.as_str(), folded, candidate_folded) else {
            return;
        };
        if let Some(ignore) = opts.ignore
            && ignore.contains(folded.as_str())
        {
            return;
        }
        if hit.class == Class::Feline
            && !opts.allow_bare_cat
            && folded.chars().count() <= 3
            && !hit.marks_required
        {
            return;
        }
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
            species: hit.species,
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
        let end = if end < chars.len() { end } else { chars.len() };
        let mut p = start;
        while p < end {
            let Some(&head_max) = self.cjk_heads.get(&chars[p]) else {
                p = p.saturating_add(1);
                continue;
            };
            let remaining = end - p;
            let head_max = if head_max < self.max_cjk {
                head_max
            } else {
                self.max_cjk
            };
            let max_l = if head_max < remaining {
                head_max
            } else {
                remaining
            };
            window.clear();
            bounds.clear();
            let mut off = 0;
            while off < max_l {
                window.push(chars[p + off]);
                bounds.push(window.len());
                off += 1;
            }
            let mut hit: Option<(usize, usize, CjkHit)> = None;
            let mut l = max_l;
            while l >= 1 {
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
                                species: hit.species,
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

/// Intern an animal `species` id into `table` (first-appearance order). A plain index table with
/// no reserved slot; ids saturate at `u16::MAX`, far above the roster.
fn intern_species(table: &mut Vec<String>, code: &str) -> SpeciesId {
    if let Some(i) = table.iter().position(|s| s == code) {
        return SpeciesId(i as u16);
    }
    let i = table.len().min(usize::from(u16::MAX));
    if i == usize::from(u16::MAX) {
        return SpeciesId(u16::MAX);
    }
    table.push(code.to_string());
    SpeciesId(i as u16)
}
/// Cross-class homograph precedence: `profanity > feline > canine > animal > emphasis`. A surface
/// that is an expletive in any enabled language ("poes": Dutch pussycat, Afrikaans vulgar) is
/// profanity, so it sparkles rather than getting a paw.
#[inline]
const fn class_rank(class: Class) -> u8 {
    match class {
        Class::Profanity => 5,
        Class::Feline => 4,
        Class::Canine => 3,
        Class::Animal => 1,
        Class::Emphasis => 0,
    }
}

/// A surface's precedence-in-progress record: [`LexiconHit`] minus the
/// [`FormId`], which is assigned only after every class collision has settled
/// ([`finalize_hits`]).
#[derive(Clone, Copy, Debug)]
struct PendingHit {
    class: Class,
    langs: LangSet,
    ambiguous: bool,
    species: Option<SpeciesId>,
    marks_required: bool,
}

/// Report a no-space surface [`Lexicon::build`] dropped for exceeding
/// [`MAX_CJK_SURFACE_CHARS`]: from an entry of language `lang`, or from the
/// exception list (`None`).
fn push_overlong_surface_conflict(conflicts: &mut Vec<String>, len: usize, lang: Option<&str>) {
    let mut msg = String::from("no-space surface of ");
    msg.push_str(&len.to_string());
    match lang {
        Some(lang) => {
            msg.push_str(" characters (lang ");
            msg.push_str(lang);
            msg.push(')');
        }
        None => msg.push_str(" characters (exception list)"),
    }
    msg.push_str(" dropped: longer than the ");
    msg.push_str(&MAX_CJK_SURFACE_CHARS.to_string());
    msg.push_str("-character maximal-run scan window");
    conflicts.push(msg);
}

/// Record `surface`'s first character in the CJK prefilter, keeping the longest surface length
/// (chars) seen for it.
fn note_cjk_head(heads: &mut FxHashMap<char, usize>, surface: &str) {
    let mut it = surface.chars();
    let Some(first) = it.next() else {
        return;
    };
    let len = it.count().saturating_add(1);
    let slot = heads.entry(first).or_insert(0);
    if *slot < len {
        *slot = len;
    }
}

/// Insert a surface, resolving collisions: a higher-ranked class REPLACES the
/// entry and keeps ONLY its own languages (the surface is displayed as the
/// winning class, so only that class's claimants may be attributed); the SAME
/// class UNIONS the language sets (`kucing` = id ∪ ms ∪ jv); a lower-ranked
/// class is dropped entirely, languages included.
fn insert_class(
    map: &mut FxHashMap<String, PendingHit>,
    key: String,
    new: PendingHit,
    conflicts: &mut Vec<String>,
) {
    match map.get_mut(&key) {
        Some(prev) if class_rank(new.class) > class_rank(prev.class) => {
            let marks_required = prev.marks_required && new.marks_required;
            *prev = new;
            prev.marks_required = marks_required;
        }
        Some(prev) if prev.class == new.class => {
            prev.langs = prev.langs.union(new.langs);
            prev.ambiguous &= new.ambiguous;
            prev.marks_required &= new.marks_required;
            match (prev.species, new.species) {
                (Some(a), Some(b)) if a != b => {
                    let mut msg = String::from("surface ");
                    msg.push_str(&DebugText(&key).to_string());
                    msg.push_str(" claimed by two animal species: kept the first");
                    conflicts.push(msg);
                }
                (None, sp @ Some(_)) => prev.species = sp,
                _ => {}
            }
        }
        Some(prev) => {
            // The loser is dropped whole except its marks testimony (as above), so the merge does
            // not depend on claimant order.
            prev.marks_required &= new.marks_required;
        }
        None => {
            map.insert(key, new);
        }
    }
}

/// Freeze a precedence-resolved surface table, giving each exact key a deterministic,
/// collision-free ID.
fn finalize_hits(
    map: FxHashMap<String, PendingHit>,
    next_form_id: &mut u64,
) -> FxHashMap<String, LexiconHit> {
    let mut entries: Vec<_> = map.into_iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut out = FxHashMap::default();
    out.reserve(entries.len());
    for (key, pending) in entries {
        let form_id = FormId::exact(*next_form_id);
        *next_form_id = next_form_id
            .checked_add(1)
            .expect("compiled lexicon exhausted the FormId namespace");
        out.insert(
            key,
            LexiconHit {
                class: pending.class,
                langs: pending.langs,
                form_id,
                ambiguous: pending.ambiguous,
                species: pending.species,
                marks_required: pending.marks_required,
            },
        );
    }
    out
}

/// The [`Match::form_hash`] a scan reports for `surface`, so a host (the sparkle-words custom-spec resolver) keys per-word tables by the scanner's own hash.
#[inline]
#[must_use]
pub fn form_hash(surface: &str) -> u64 {
    fnv1a64(surface)
}

/// True when every char of `s` is in a no-space script (CJK, kana, Hangul, Thai, Lao, Khmer): such a surface must be a `cjk = true` entry with a raw (unfolded) key, or the build drops it as not a scannable token.
#[inline]
#[must_use]
pub fn is_no_space_surface(s: &str) -> bool {
    !s.is_empty() && s.chars().all(fold::is_no_space_script)
}

/// FNV-1a over the key's UTF-8 bytes — the [`Match::form_hash`] source. Kept
/// byte-level (not char-level) so the hash of a `&str` slice never allocates.
#[inline]
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
#[inline]
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
            // consume the joiner, keep scanning
            j += 1;
        } else {
            break;
        }
    }
    j
}

/// An apostrophe or hyphen, which the scanner joins into a token when it sits between two token characters (`cat's`, `cat-like`).
#[inline]
#[must_use]
pub const fn is_interior_joiner(c: char) -> bool {
    c == '\'' || c == '\u{2019}' || c == '-'
}

/// True if `s` is a single whole-word token: it begins and ends with a token
/// character and contains only token characters and interior joiners.
fn is_matchable_token(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
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
#[inline]
fn left_suppresses(chars: &[char], i: usize) -> bool {
    if i == 0 {
        return false;
    }
    let Some(&c) = chars.get(i - 1) else {
        return false;
    };
    fold::is_code_adjacent_punct(c)
        || c == '-'
        || (c == '.' && i >= 2 && matches!(chars.get(i - 2), Some(p) if p.is_alphanumeric()))
}

/// True if the character just RIGHT of the token ending at `j` puts it in a
/// code/path context.
#[inline]
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
#[inline]
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

    /// Every surface the maximal-run scan can match is reachable through `cjk_heads` at a stored
    /// length no shorter than its own, over both tables and each language set that changes which
    /// entries load.
    #[test]
    fn cjk_prefilter_admits_every_surface_it_gates() {
        for langs in [&["en"][..], &["all"][..], &["ja", "th"][..]] {
            let lx = Lexicon::with_languages(langs);
            let surfaces = lx
                .cjk_forms
                .keys()
                .chain(lx.cjk_exceptions.iter())
                .map(String::as_str);
            for surface in surfaces {
                let first = surface
                    .chars()
                    .next()
                    .expect("builder skips empty CJK surfaces");
                let len = surface.chars().count();
                let head_max = lx.cjk_heads.get(&first).copied().unwrap_or(0);
                assert!(
                    head_max >= len,
                    "{langs:?}: surface {surface:?} ({len} chars) is gated behind \
                     first character {first:?} whose stored maximum is {head_max} \
                     — the scan would clamp its window below this surface and \
                     never probe it"
                );
                assert!(
                    head_max <= lx.max_cjk,
                    "{langs:?}: first character {first:?} stores {head_max}, \
                     above max_cjk {}",
                    lx.max_cjk
                );
            }
            assert!(
                lx.cjk_heads.len() < 5_000,
                "{langs:?}: {} distinct first characters — this is no longer a \
                 selective prefilter",
                lx.cjk_heads.len()
            );
            assert!(
                lx.cjk_heads.len() > 50,
                "{langs:?}: only {} first characters; the CJK data has collapsed",
                lx.cjk_heads.len()
            );
        }
    }

    /// Language-collision regression: Romanian `fut` / `futu` must remain
    /// recognizable in Romanian/all-language configurations, but every scan
    /// reports them as cursor-deferrable collisions.
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
        let text = "kitty's 子猫 القطة وقطة fuck";
        let compat_chars: Vec<char> = text.chars().collect();
        let compat_hits = lx.scan(text, &opts);

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

    #[test]
    fn cjk_run_scratch_reuse_preserves_matches_and_ignore() {
        use std::collections::HashSet;
        let lx = lex();
        let o = ScanOptions::default();
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

    #[test]
    fn ignore_guard_keys_on_full_token_fold_not_stripped_stem() {
        use std::collections::HashSet;
        let lx = lex();
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

    #[test]
    fn suffix_expansion_is_globally_bounded() {
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

        assert!(
            lx.spaced_form_count() <= MAX_TOTAL_SURFACES,
            "surface count {} exceeded cap {MAX_TOTAL_SURFACES}",
            lx.spaced_form_count(),
        );
        assert!(
            lx.conflicts().iter().any(|c| c.contains("surface cap")),
            "expected a surface-cap conflict, got {:?}",
            lx.conflicts(),
        );
        assert_eq!(lx.classify_token("qwxfeline"), Some(Class::Feline));
        assert_eq!(lx.classify_token("qwxfelinez"), Some(Class::Feline));
        assert_eq!(lx.classify_token("qwxkittenz"), Some(Class::Feline));
        assert_eq!(lx.classify_token("qwxstem0qwxsuf0"), None);
    }

    #[test]
    fn no_space_surface_length_is_bounded() {
        let long = "\u{9F98}".repeat(4096);
        let fits = format!("{}\u{9F8D}", "\u{9F98}".repeat(MAX_CJK_SURFACE_CHARS - 1));
        let toml = format!(
            "[[entry]]\nclass = \"feline\"\nlang = \"zh\"\nmode = \"forms\"\ncjk = true\n\
             forms = [\"{long}\", \"{fits}\"]\n"
        );
        let lx = Lexicon::with_languages_and_override(&["all"], Some(&toml))
            .expect("override lexicon must parse");
        assert!(
            lx.max_cjk <= MAX_CJK_SURFACE_CHARS,
            "the scan window is {} characters, above the {MAX_CJK_SURFACE_CHARS} bound",
            lx.max_cjk
        );
        assert!(
            lx.cjk_heads
                .values()
                .all(|&len| len <= MAX_CJK_SURFACE_CHARS)
        );
        assert!(
            lx.conflicts().iter().any(|c| c.contains("4096 characters")),
            "the dropped surface must be reported: {:?}",
            lx.conflicts()
        );
        let line = format!("{}{fits}", "\u{9F98}".repeat(20_000));
        let hits = lx.scan(&line, &ScanOptions::default());
        let spans: Vec<(usize, usize)> = hits.iter().map(|m| (m.start, m.end)).collect();
        assert_eq!(spans, [(20_000, 20_000 + MAX_CJK_SURFACE_CHARS)]);
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
        assert_eq!(
            u.iter().collect::<Vec<_>>(),
            vec![LangId(1), LangId(3), LangId(7)]
        );
        assert_eq!(primary_lang(u), LangId(1));
        assert_eq!(primary_lang(LangSet::EMPTY), UNKNOWN_LANG);
        assert_eq!(lex().lang_code(primary_lang(LangSet::EMPTY)), "unknown");
    }

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
        for i in [0usize, 62] {
            let ms = lx.scan(&format!("qwxlang{i}"), &o);
            assert_eq!(ms.len(), 1);
            assert_eq!(lx.lang_code(primary_lang(ms[0].langs)), format!("l{i}"));
        }
        for i in [63usize, 69] {
            let ms = lx.scan(&format!("qwxlang{i}"), &o);
            assert_eq!(ms.len(), 1);
            assert_eq!(lx.lang_code(primary_lang(ms[0].langs)), "unknown");
        }
    }
}
