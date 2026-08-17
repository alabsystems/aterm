// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! v2.2 **Kitty Log vocabulary registry** (docs/sparkle-words-v2-design.md
//! §F4): the ONE CI-pinnable source shared by the recorder
//! (`word_decorations`), the persistent store's TOML keys (aterm-gui
//! `kitty_log`), the settings-page labels, and the genome decode — the
//! the shared registry idiom (`ALL` + `config_key()` + `label()`).
//!
//! Aggregation key = `(KittyType, KittyMagic, primary LangId)` — never raw
//! `Genome.magic` (raw magic would grow the log file without bound). The
//! fallback CAUSE is its own dimension ([`KittyShownAs`]), never conflated
//! with the type.
//!
//! This file also owns the kitty NAME namespace (owner ask, 2026-08-15): the
//! hand-picked [`signature_name`] table and the [`KITTY_NAME_BANK`] hash bank
//! behind the one public draw door [`kitty_name`] — the name half of the
//! pet's hover label (aterm-gui `App::pet_label_identity`).

use aterm_lexicon::LangSet;

use crate::cat_glyphs_gen::{CatGlyphId, GLYPH_IDS, GLYPHS, GlyphKind};
use crate::genome::{CatAge, CatMagic};

/// The exact, reusable visual identity of one collected cat. This is the small
/// value carried from the word renderer into the Kitty Log and back into the
/// cursor companion: semantic authored glyph ids plus integer palette/age axes,
/// never pixels or a raw word/genome.
///
/// Keeping this `Copy + Eq` means discovery handling is allocation-free and a
/// cursor frame can select a collected look with a handful of scalar copies;
/// the existing [`crate::cat_baker::CatBaker`] remains the only art cache.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KittyLook {
    /// Authored head or full-cat special.
    pub variant: CatGlyphId,
    /// Optional authored overlay (bow, crown, or bell), only on head variants.
    pub accessory: Option<CatGlyphId>,
    /// Index into `cat_baker::COAT_RAMP`.
    pub coat: u8,
    /// Index into `cat_baker::EYE_RAMP`.
    pub iris: u8,
    /// Genome age band; converted to the integer bake-key scale at emission.
    pub age: CatAge,
}

impl Default for KittyLook {
    fn default() -> Self {
        Self {
            variant: CatGlyphId::S103,
            accessory: None,
            coat: 8,
            iris: 4,
            age: CatAge::Adult,
        }
    }
}

/// Salt keeping session-derived companion identities in their own genome
/// namespace, away from the word renderer's position-bearing occurrence seeds
/// (`b"sessKit\0"` as big-endian ASCII).
const SESSION_LOOK_SALT: u64 = 0x7365_7373_4B69_7400;

/// Salt keeping app-derived kitty identities in their own genome namespace —
/// disjoint from both [`SESSION_LOOK_SALT`] and the word renderer's occurrence
/// seeds (`b"appKitt\0"` as big-endian ASCII), so "the claude cat" can never
/// collide with a session kitty by hash coincidence of the input spaces.
const APP_LOOK_SALT: u64 = 0x6170_704B_6974_7400;

/// Salt keeping app kitty NAMES in their own genome namespace (`b"appName\0"`
/// as big-endian ASCII) — disjoint from [`APP_LOOK_SALT`] on purpose: reusing
/// the look salt would make the name draw correlate bit-for-bit with the breed
/// axes (identical `gkey`), and the law here is one documented ASCII salt per
/// identity namespace.
const APP_NAME_SALT: u64 = 0x6170_704E_616D_6500;

/// THE NAME BANK for [`kitty_name`]'s hash path: 64 short cat names — exactly
/// 2⁶, so a single unbiased [`crate::genome::field`] draw indexes it with no
/// modulo skew. Wide enough that two everyday tools rarely share a name;
/// collisions between different ids are ACCEPTED (a name is a nameplate, not
/// an identity key — the breed axes already distinguish the cats).
///
/// Hygiene, pinned by `name_bank_is_wide_short_and_duplicate_free`: no
/// duplicates, nothing over 8 chars, and no overlap with the hand-picked
/// [`signature_name`] table (a flagship's name stays a flagship's). The
/// matching no-overlap law for the collection book's `glyph_label` names (a
/// different namespace — those name COLLECTED specials, not apps) cannot be
/// pinned from this crate (`glyph_label` lives DOWNSTREAM in aterm-gui's
/// `kitty_log`), so the bank is exposed read-only through [`kitty_name_bank`]
/// and that pin lives beside the label table instead
/// (`glyph_labels_stay_out_of_the_kitty_name_namespace`).
const KITTY_NAME_BANK: [&str; 64] = [
    "Pixel", "Widget", "Gizmo", "Pepper", "Olive", "Clover", "Maple", "Ginger", "Nutmeg", "Pickle",
    "Noodle", "Waffle", "Pretzel", "Bagel", "Pudding", "Tofu", "Pinto", "Cocoa", "Espresso",
    "Latte", "Chai", "Pesto", "Basil", "Sage", "Juniper", "Hazel", "Poppy", "Marble", "Domino",
    "Checkers", "Patches", "Freckle", "Smudge", "Doodle", "Scribble", "Inky", "Jinx", "Whiskers",
    "Mittens", "Slippers", "Velvet", "Satin", "Shadow", "Comet", "Nova", "Orbit", "Quark",
    "Photon", "Fig", "Plum", "Peach", "Mango", "Kiwi", "Truffle", "Wasabi", "Miso", "Ramen",
    "Sushi", "Bento", "Taco", "Churro", "Crumpet", "Scone", "Tuna",
];

/// The whole name bank, read-only — exposed ONLY so downstream name tables
/// can pin their half of the no-overlap law against it (the collection book's
/// `glyph_label` test in aterm-gui, which this crate cannot see); the bank
/// itself stays private and [`kitty_name`] stays the one draw door.
#[must_use]
pub fn kitty_name_bank() -> &'static [&'static str] {
    &KITTY_NAME_BANK
}

/// SIGNATURE NAMES — hand-picked for every id the canonical table spells out
/// ([`canonical_app_id`]), chosen from character rather than hash luck, the
/// same way [`flagship_look`] hand-picks the flagship breeds. Coverage is
/// deliberately the WHOLE canonical list: `aider`/`gemini`/`cursor` have no
/// flagship breed (their looks stay hash-derived), but a first-class id whose
/// NAME could reshuffle if the bank grew would feel like a stranger.
///
/// The picks:
///   • `shell` — **Boots**: the charcoal tuxedo that lives at the prompt; the
///     first thing you put on, every session.
///   • `claude` — **Clementine**: the bright-ginger coat, a warm terracotta
///     fruit of a cat (and it even echoes the "Cl…" of Claude).
///   • `codex` — **Vector**: the level, precise gaze — a direction and a
///     magnitude, nothing wasted.
///   • `agy` — **Sprout**: the adolescent band, the youngest tool in the
///     pack, still growing.
///   • `aider` — **Buddy**: the pair-programming aide; the cat that sits with
///     you while you work.
///   • `gemini` — **Castor**: the brighter of the Gemini twins.
///   • `cursor` — **Blink**: what a cursor does all day.
fn signature_name(app_id: &str) -> Option<&'static str> {
    Some(match app_id {
        "shell" => "Boots",
        "claude" => "Clementine",
        "codex" => "Vector",
        "agy" => "Sprout",
        "aider" => "Buddy",
        "gemini" => "Castor",
        "cursor" => "Blink",
        _ => return None,
    })
}

/// THE APP KITTY'S NAME (owner spec, 2026-08-15: hovering the pet shows "the
/// program it corresponds to and the kitty's name"): a deterministic name for
/// a canonical app id from [`canonical_app_id`] — same app ⇒ same name, on
/// every machine, forever, by construction (the exact idiom of
/// [`KittyLook::for_app`]: nothing to persist, nothing to sync).
///
/// The canonical ids wear the hand-picked [`signature_name`]s; every other
/// app draws from [`KITTY_NAME_BANK`] by `form_hash` under [`APP_NAME_SALT`]
/// — its own namespace, so the name can never correlate with the breed draw.
///
/// "Forever" is TEST-PINNED for both halves: the signature table by
/// `signature_names_are_pinned_distinct_and_cover_the_canonical_ids`, and the
/// hash path by `hash_path_names_are_pinned_forever` (golden draws — a bank
/// reorder or salt change fails loudly instead of silently renaming every
/// non-canonical app across builds).
#[must_use]
pub fn kitty_name(app_id: &str) -> &'static str {
    if let Some(name) = signature_name(app_id) {
        return name;
    }
    let gkey = crate::genome::mix(aterm_lexicon::form_hash(app_id) ^ APP_NAME_SALT);
    KITTY_NAME_BANK[crate::genome::field(gkey, 0, 6) as usize]
}

/// Launcher tokens that are TRANSPARENT to app identity: when the first word
/// of a commandline is one of these, the app is whatever comes next. The
/// documented, deliberately small list — `sudo` (privilege wrapper), `env`
/// (environment wrapper), `npx` (package runner; the tool being run is the
/// identity, not the runner). Leading `VAR=value` assignments and `-flags`
/// are likewise skipped as launcher plumbing, not apps.
const TRANSPARENT_PREFIXES: &[&str] = &["sudo", "env", "npx"];

/// The first meaningful token of a shell commandline, basename'd and
/// lowercased — the raw material for [`canonical_app_id`]. `None` when the
/// commandline holds no identifiable program (empty, only assignments/flags).
///
/// This is a whitespace-token approximation, not a shell parser: quoting is
/// not interpreted (a commandline is identity input here, never executed).
/// Both `/` and `\` count as path separators and a trailing `.exe` is
/// stripped, so the same tool yields the same id across platforms.
#[must_use]
pub fn app_basename(commandline: &str) -> Option<String> {
    for token in commandline.split_whitespace() {
        // Launcher plumbing, not the app: `FOO=bar cargo …`, `sudo -E cargo …`.
        if token.starts_with('-') || token.contains('=') {
            continue;
        }
        let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
        let mut base = base.to_ascii_lowercase();
        if let Some(stripped) = base.strip_suffix(".exe") {
            base = stripped.to_owned();
        }
        if base.is_empty() {
            continue;
        }
        if TRANSPARENT_PREFIXES.contains(&base.as_str()) {
            continue;
        }
        return Some(base);
    }
    None
}

/// THE CANONICAL APP TABLE — the ONE place basenames become app identities,
/// so every argv form of a flagship tool lands on the same kitty. Policy,
/// kept small on purpose:
///   • every shell is the SAME app ("shell") — the prompt is home, and home
///     has one cat regardless of which shell renders it;
///   • the flagship AI coding tools get their canonical ids spelled out so a
///     rename/wrapper (`claude.exe`, `/opt/bin/codex`) cannot fork the breed;
///   • everything else IS its basename — unknown tools stay distinct and get
///     the deterministic name-derived breed from [`KittyLook::for_app`].
#[must_use]
pub fn canonical_app_id(basename: &str) -> &str {
    match basename {
        "zsh" | "bash" | "fish" | "sh" | "pwsh" | "nu" => "shell",
        "claude" => "claude",
        "codex" => "codex",
        "agy" => "agy",
        "aider" => "aider",
        "gemini" => "gemini",
        "cursor" => "cursor",
        other => other,
    }
}

/// FLAGSHIP BREEDS — hand-picked `(variant, coat, iris, age)` for the apps
/// people live in, spread across distinct coat families so no two flagships
/// can be mistaken for hash luck. Everything else goes through the
/// [`APP_LOOK_SALT`] hash path in [`KittyLook::for_app`].
///
/// The picks (COAT_RAMP / EYE_RAMP stops per `cat_baker` §5.3):
///   • `shell` — S103 (the roster's default head: the baseline face for the
///     baseline tool), charcoal-black coat 1 with moss-green iris 5: phosphor
///     green on near-black, the terminal's own colours.
///   • `claude` — S108 (one of the two tallest, roundest, lowest-eyed faces:
///     open and friendly), bright-ginger coat 12 — the ramp's closest stop to
///     Anthropic's warm terracotta — with warm copper iris 0.
///   • `codex` — S117 (level, mid-set eyes: a precise, even gaze), clean
///     white coat 15 with storm-teal iris 7: the cool counterpoint to claude.
///   • `agy` — S122 (the only head in the roster with its own aspect ratio:
///     one of a kind, like the house tool), blue-slate coat 3 — the ramp's
///     blue notch — with sea-green iris 6, and the ADOLESCENT age band: the
///     youngest tool in the pack, still growing.
fn flagship_look(app_id: &str) -> Option<KittyLook> {
    let (variant, coat, iris, age) = match app_id {
        "shell" => (CatGlyphId::S103, 1, 5, CatAge::Adult),
        "claude" => (CatGlyphId::S108, 12, 0, CatAge::Adult),
        "codex" => (CatGlyphId::S117, 15, 7, CatAge::Adult),
        "agy" => (CatGlyphId::S122, 3, 6, CatAge::Adolescent),
        _ => return None,
    };
    Some(
        KittyLook {
            variant,
            accessory: None,
            coat,
            iris,
            age,
        }
        .normalized(),
    )
}

impl KittyLook {
    /// THE SESSION KITTY (owner, 2026-07-26: "I like that there is a unique
    /// kitty chosen per session and sticks with that session because that makes
    /// the session kitty special").
    ///
    /// A deterministic identity derived from the session id: unique across
    /// sessions, byte-stable for the whole life of one. Because it is a pure
    /// function of the id there is nothing to persist, nothing to restore, and
    /// a reattached session gets its own kitty back automatically.
    ///
    /// Before this existed the companion fell back to [`Self::default`]
    /// whenever the Kitty Log held no collected identity — i.e. every session
    /// on a fresh install wore the *same* cat, which is the opposite of
    /// special. An explicitly collected companion still wins over this: the
    /// user choosing a cat is a reason, and only reasons may change the kitty.
    ///
    /// The axes are decoded through the ordinary v4 genome so a session kitty
    /// is drawn from exactly the same roster (head, coat, iris, age band) the
    /// ambient word-cats roll from — no separate art path, no new bake keys.
    #[must_use]
    pub fn for_session(session: u64) -> Self {
        let gkey = crate::genome::mix(session ^ SESSION_LOOK_SALT);
        let (coat, iris) = crate::genome::cat_fills_v4(gkey);
        Self {
            variant: crate::genome::cat_variant_v4(gkey),
            // Accessories stay the Kitty Log's business: a bow or crown marks a
            // COLLECTED cat, so minting them for free here would devalue them.
            accessory: None,
            coat,
            iris,
            age: crate::genome::cat_age_v4(gkey),
        }
        .normalized()
    }

    /// THE APP KITTY (owner spec, 2026-08-07: "each major app gets its own
    /// cursor kitty"): the deterministic breed for a canonical app id from
    /// [`canonical_app_id`].
    ///
    /// Flagships (`shell`/`claude`/`codex`/`agy`) wear the hand-picked
    /// [`flagship_look`] tuples so the tools people live in look intentional;
    /// every other app derives its look from the NAME — `form_hash` (the
    /// lexicon's canonical FNV-1a-64) salted into its own namespace, decoded
    /// through the ordinary v4 genome exactly like [`Self::for_session`]. Same
    /// app ⇒ same cat, on every machine, by construction: the look is a pure
    /// function of the id, so there is nothing to persist and nothing to sync.
    ///
    /// Accessories stay `None` here for the same reason as the session kitty:
    /// a bow or crown marks a COLLECTED cat, and minting them for free would
    /// devalue them. The roster is 25 heads × 16 coats × 8 irises × 4 ages =
    /// 12,800 distinct looks.
    #[must_use]
    pub fn for_app(app_id: &str) -> Self {
        if let Some(look) = flagship_look(app_id) {
            return look;
        }
        let gkey = crate::genome::mix(aterm_lexicon::form_hash(app_id) ^ APP_LOOK_SALT);
        let (coat, iris) = crate::genome::cat_fills_v4(gkey);
        Self {
            variant: crate::genome::cat_variant_v4(gkey),
            accessory: None,
            coat,
            iris,
            age: crate::genome::cat_age_v4(gkey),
        }
        .normalized()
    }

    /// Clamp untrusted persisted indices and restore a renderable composition.
    /// An accessory can never be a base glyph: old/corrupt ledgers degrade to
    /// the default head instead of asking the baker to stretch an overlay into
    /// a face. Full-cat specials cannot wear an accessory.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.coat = self.coat.min(15);
        self.iris = self.iris.min(7);
        match GLYPHS[self.variant as usize].kind {
            GlyphKind::Head => {
                if self
                    .accessory
                    .is_some_and(|id| GLYPHS[id as usize].kind != GlyphKind::Accessory)
                {
                    self.accessory = None;
                }
            }
            GlyphKind::Special => self.accessory = None,
            GlyphKind::Accessory => {
                self.variant = Self::default().variant;
                self.accessory = None;
            }
        }
        self
    }
}

/// Stable semantic key for an authored glyph. Persist this rather than the Rust
/// enum discriminant; contributor-added art may change roster ordinals.
#[must_use]
pub fn glyph_key(id: CatGlyphId) -> &'static str {
    GLYPHS[id as usize].id
}

/// Resolve a persisted semantic glyph key through the generated roster.
#[must_use]
pub fn glyph_from_key(key: &str) -> Option<CatGlyphId> {
    GLYPHS
        .iter()
        .zip(GLYPH_IDS.iter().copied())
        .find_map(|(def, id)| (def.id == key).then_some(id))
}

/// Stable persistence key for an age band.
#[must_use]
pub fn age_key(age: CatAge) -> &'static str {
    match age {
        CatAge::Kitten => "kitten",
        CatAge::Adolescent => "adolescent",
        CatAge::Adult => "adult",
        CatAge::Elder => "elder",
    }
}

/// Resolve an age key from a ledger; unknown/legacy values use Adult.
#[must_use]
pub fn age_from_key(key: &str) -> CatAge {
    match key {
        "kitten" => CatAge::Kitten,
        "adolescent" => CatAge::Adolescent,
        "elder" => CatAge::Elder,
        _ => CatAge::Adult,
    }
}

/// The SHOWN kitty type — the Kitty Log vocabulary. cat-art v4 renders every
/// cat as the authored peeking [`KittyType::HeadPeek`]; the remaining variants are
/// FROZEN for `kitty-log.toml` serde compatibility (persisted rows key off
/// [`KittyType::config_key`]) — the recorder never emits them any more.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KittyType {
    /// Head-only HeadPeek — the ONE type v4 records.
    HeadPeek,
    /// FROZEN (serde compat): tilted head.
    HeadTilt,
    /// FROZEN (serde compat): left-paw grip.
    HeadPawLeft,
    /// FROZEN (serde compat): right-paw grip.
    HeadPawRight,
    /// FROZEN (serde compat): two-paw grip.
    HeadTwoPaws,
    /// FROZEN (serde compat): the retired PawSingle pose.
    PawSingle,
    /// FROZEN (serde compat): the retired PawDouble (kneading) pose.
    PawDouble,
    /// FROZEN (serde compat): the retired v1 paw glyph.
    PawClassic,
}

impl KittyType {
    /// Every shown type, in registry order — the single source a generic
    /// surface (log keys, settings rows, completeness denominators,
    /// introspection) iterates without hardcoding the set.
    pub const ALL: [KittyType; 8] = [
        KittyType::HeadPeek,
        KittyType::HeadTilt,
        KittyType::HeadPawLeft,
        KittyType::HeadPawRight,
        KittyType::HeadTwoPaws,
        KittyType::PawSingle,
        KittyType::PawDouble,
        KittyType::PawClassic,
    ];

    /// The stable `kitty-log.toml` / introspection key for this type.
    pub fn config_key(self) -> &'static str {
        match self {
            KittyType::HeadPeek => "head_peek",
            KittyType::HeadTilt => "head_tilt",
            KittyType::HeadPawLeft => "head_paw_left",
            KittyType::HeadPawRight => "head_paw_right",
            KittyType::HeadTwoPaws => "head_two_paws",
            KittyType::PawSingle => "paw_single",
            KittyType::PawDouble => "paw_double",
            KittyType::PawClassic => "paw_classic",
        }
    }

    /// A human label — shared by the settings collection book and dumps.
    pub fn label(self) -> &'static str {
        match self {
            KittyType::HeadPeek => "Peeking head",
            KittyType::HeadTilt => "Tilted head",
            KittyType::HeadPawLeft => "Left-paw grip",
            KittyType::HeadPawRight => "Right-paw grip",
            KittyType::HeadTwoPaws => "Two-paw grip",
            KittyType::PawSingle => "Single paw",
            KittyType::PawDouble => "Kneading paws",
            KittyType::PawClassic => "Classic paw",
        }
    }
}

/// The magic dimension of the aggregation key — [`CatMagic`] plus the
/// explicit ordinary bucket (so the log never stores raw `Genome.magic`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KittyMagic {
    /// The ordinary build (no §3.5 window hit, or `feline.magic = false`).
    None,
    /// Fortune Cat (1/512).
    Fortune,
    /// Nebula Cat (1/1024).
    Nebula,
    /// Butterfly companion (≈ 1/95, v2.2).
    Butterfly,
    /// Sakura cat (1/512, v2.2).
    Sakura,
}

impl KittyMagic {
    /// Every magic bucket, in rarity-page order.
    pub const ALL: [KittyMagic; 5] = [
        KittyMagic::None,
        KittyMagic::Fortune,
        KittyMagic::Nebula,
        KittyMagic::Butterfly,
        KittyMagic::Sakura,
    ];

    /// The stable `kitty-log.toml` / introspection key for this bucket.
    pub fn config_key(self) -> &'static str {
        match self {
            KittyMagic::None => "none",
            KittyMagic::Fortune => "fortune",
            KittyMagic::Nebula => "nebula",
            KittyMagic::Butterfly => "butterfly",
            KittyMagic::Sakura => "sakura",
        }
    }

    /// A human label for the collection book.
    pub fn label(self) -> &'static str {
        match self {
            KittyMagic::None => "Ordinary",
            KittyMagic::Fortune => "Fortune",
            KittyMagic::Nebula => "Nebula",
            KittyMagic::Butterfly => "Butterfly",
            KittyMagic::Sakura => "Sakura",
        }
    }

    /// The recorder's mapping from the (config-gated) genome decode.
    pub fn from_cat(magic: Option<CatMagic>) -> KittyMagic {
        match magic {
            None => KittyMagic::None,
            Some(CatMagic::Fortune) => KittyMagic::Fortune,
            Some(CatMagic::Nebula) => KittyMagic::Nebula,
            Some(CatMagic::Butterfly) => KittyMagic::Butterfly,
            Some(CatMagic::Sakura) => KittyMagic::Sakura,
        }
    }
}

/// HOW the sighting rendered — the fallback CAUSE as its own recorded
/// dimension (§F4.1: four causes, never conflated).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KittyShownAs {
    /// The real §5 cat (any [`KittyType`] except `PawClassic`).
    Cat,
    /// No graphic because cell floors / text clearance suppressed the cat.
    PawFallbackFloor,
    /// No graphic because the occurrence fell past `MAX_CATS`.
    PawFallbackOverflow,
    /// No graphic because legacy `style = "paw"` selects ink-only rendering.
    PawStyle,
}

impl KittyShownAs {
    /// Every cause, in registry order.
    pub const ALL: [KittyShownAs; 4] = [
        KittyShownAs::Cat,
        KittyShownAs::PawFallbackFloor,
        KittyShownAs::PawFallbackOverflow,
        KittyShownAs::PawStyle,
    ];

    /// The stable `kitty-log.toml` / introspection key for this cause.
    pub fn config_key(self) -> &'static str {
        match self {
            KittyShownAs::Cat => "cat",
            KittyShownAs::PawFallbackFloor => "paw_fallback_floor",
            KittyShownAs::PawFallbackOverflow => "paw_fallback_overflow",
            KittyShownAs::PawStyle => "paw_style",
        }
    }

    /// A human label for the collection book.
    pub fn label(self) -> &'static str {
        match self {
            KittyShownAs::Cat => "Cat",
            KittyShownAs::PawFallbackFloor => "No graphic (ineligible cat)",
            KittyShownAs::PawFallbackOverflow => "No graphic (cat limit)",
            KittyShownAs::PawStyle => "No graphic (legacy paw mode)",
        }
    }
}

/// [`KittySighting::traits`] bit: the identity carries heterochromia (§3.4,
/// ~1/64). v2.6: recorded from the genome on head poses even when the sub-16
/// cell size gate baked the plain build (§5.4 — the log is an identity
/// record, not a pixel record).
pub const TRAIT_HETEROCHROMIA: u8 = 1;
/// [`KittySighting::traits`] bit: the identity carries an ear nick (~1/8;
/// same v2.6 sub-16 note as heterochromia).
pub const TRAIT_EAR_NICK: u8 = 1 << 1;
/// [`KittySighting::traits`] bit: a forehead blaze was DISPLAYED (~1/8).
pub const TRAIT_BLAZE: u8 = 1 << 2;
/// [`KittySighting::traits`] bit: the shy build showed (bit 60; v3 texel
/// trait — smaller 0.85·r buttons + blush + lowered catch-light, no lids —
/// sparkle-words v3 design §2.1).
pub const TRAIT_SHY: u8 = 1 << 3;
/// [`KittySighting::traits`] bit (v3 §2.1): the cat wore a Bow (1/32 of
/// ordinary cats — accessories never stack on magic builds).
pub const TRAIT_BOW: u8 = 1 << 4;
/// [`KittySighting::traits`] bit (v3 §2.1): Sunglasses (1/256; replaces the
/// eyes, no gaze).
pub const TRAIT_SUNGLASSES: u8 = 1 << 5;
/// [`KittySighting::traits`] bit (v3 §2.1): the Witch Hat (1/512).
pub const TRAIT_WITCH_HAT: u8 = 1 << 6;
/// [`KittySighting::traits`] bit (v3 §2.1): the Crown (1/1024).
pub const TRAIT_CROWN: u8 = 1 << 7;

/// Fold the §2.1 accessory decode ([`genome::cat_accessory`]) into the
/// sighting's traits-u8 — the recorder ORs this into its existing trait
/// assembly, so the accessory dimension rides the established channel (the
/// Kitty Log persists it as four dedicated counters).
///
/// [`genome::cat_accessory`]: crate::genome::cat_accessory
pub fn accessory_trait_bits(acc: Option<crate::genome::Accessory>) -> u8 {
    use crate::genome::Accessory;
    match acc {
        None => 0,
        Some(Accessory::Bow) => TRAIT_BOW,
        Some(Accessory::Sunglasses) => TRAIT_SUNGLASSES,
        Some(Accessory::WitchHat) => TRAIT_WITCH_HAT,
        Some(Accessory::Crown) => TRAIT_CROWN,
    }
}

/// One recorded kitty sighting (§F4.2): everything the Kitty Log aggregates,
/// SESSION-AGNOSTIC (no clocks — the store timestamps at flush) and free of
/// raw genome words. Recorded once per episode, only on a present where the
/// output actually landed; drained by the host via
/// `WordDecorations::drain_kitty_sightings`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KittySighting {
    /// The SHOWN type (post-forcing pose × paw count, or the fallback paw).
    pub kitty_type: KittyType,
    /// The magic bucket, already `feline.magic`-gated (what was SHOWN).
    pub magic: KittyMagic,
    /// How it rendered (cat vs the three fallback causes).
    pub shown_as: KittyShownAs,
    /// Every language claiming the matched surface (LangIds are lexicon-build
    /// scoped — the store must persist lang CODE strings via
    /// `Lexicon::lang_code`, never raw ids).
    pub langs: LangSet,
    /// Displayed-trait bits (`TRAIT_*`): set only when a head pose actually
    /// drew/applied the trait this episode.
    pub traits: u8,
    /// The exact bounded visual identity that landed. The durable log persists
    /// its semantic keys so the cursor companion can wear a collected look.
    pub look: KittyLook,
    /// The episode identity — the host's `(session, ident)` dedupe key; NOT
    /// persisted (position-bearing).
    pub ident: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_normalization_never_leaves_an_overlay_as_the_base() {
        let default = KittyLook::default();
        let repaired = KittyLook {
            variant: CatGlyphId::AccBow,
            accessory: Some(CatGlyphId::AccCrown),
            coat: u8::MAX,
            iris: u8::MAX,
            age: CatAge::Kitten,
        }
        .normalized();

        assert_eq!(repaired.variant, default.variant);
        assert_eq!(GLYPHS[repaired.variant as usize].kind, GlyphKind::Head);
        assert_eq!(repaired.accessory, None);
        assert_eq!((repaired.coat, repaired.iris), (15, 7));
        assert_eq!(repaired.age, CatAge::Kitten);

        let special = KittyLook {
            variant: CatGlyphId::SpecSleeping,
            accessory: Some(CatGlyphId::AccBow),
            ..default
        }
        .normalized();
        assert_eq!(special.variant, CatGlyphId::SpecSleeping);
        assert_eq!(special.accessory, None);
    }

    /// The registry keys are unique and stable — the CI pin for log/store/UI
    /// agreement (renaming a key would orphan persisted rows).
    #[test]
    fn registry_keys_are_unique_and_pinned() {
        let type_keys: Vec<&str> = KittyType::ALL.iter().map(|t| t.config_key()).collect();
        assert_eq!(
            type_keys,
            [
                "head_peek",
                "head_tilt",
                "head_paw_left",
                "head_paw_right",
                "head_two_paws",
                "paw_single",
                "paw_double",
                "paw_classic"
            ],
            "shown-type keys are pinned"
        );
        let magic_keys: Vec<&str> = KittyMagic::ALL.iter().map(|m| m.config_key()).collect();
        assert_eq!(
            magic_keys,
            ["none", "fortune", "nebula", "butterfly", "sakura"],
            "magic keys are pinned"
        );
        let shown_keys: Vec<&str> = KittyShownAs::ALL.iter().map(|s| s.config_key()).collect();
        assert_eq!(
            shown_keys,
            [
                "cat",
                "paw_fallback_floor",
                "paw_fallback_overflow",
                "paw_style"
            ],
            "shown-as keys are pinned"
        );
        let mut dedup = type_keys.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), type_keys.len(), "keys must be unique");
    }

    /// v3 §2.1: the accessory trait bits are pinned (16/32/64/128), disjoint
    /// from the displayed-trait bits, and `accessory_trait_bits` maps the
    /// genome decode onto exactly one of them.
    #[test]
    fn accessory_trait_bits_are_pinned_and_disjoint() {
        use crate::genome::Accessory;
        assert_eq!(TRAIT_BOW, 16);
        assert_eq!(TRAIT_SUNGLASSES, 32);
        assert_eq!(TRAIT_WITCH_HAT, 64);
        assert_eq!(TRAIT_CROWN, 128);
        let low = TRAIT_HETEROCHROMIA | TRAIT_EAR_NICK | TRAIT_BLAZE | TRAIT_SHY;
        let high = TRAIT_BOW | TRAIT_SUNGLASSES | TRAIT_WITCH_HAT | TRAIT_CROWN;
        assert_eq!(low & high, 0, "accessory bits never collide with traits");
        assert_eq!(accessory_trait_bits(None), 0);
        for (acc, bit) in [
            (Accessory::Bow, TRAIT_BOW),
            (Accessory::Sunglasses, TRAIT_SUNGLASSES),
            (Accessory::WitchHat, TRAIT_WITCH_HAT),
            (Accessory::Crown, TRAIT_CROWN),
        ] {
            assert_eq!(accessory_trait_bits(Some(acc)), bit);
        }
    }

    /// The commandline → basename extraction: paths (both separators), case
    /// folding, `.exe` stripping, and the documented transparent launcher
    /// plumbing (`sudo`/`env`/`npx`, `VAR=value` assignments, `-flags`).
    #[test]
    fn app_basename_strips_paths_prefixes_and_case() {
        assert_eq!(app_basename("cargo build"), Some("cargo".into()));
        assert_eq!(
            app_basename("/usr/local/bin/Claude --resume"),
            Some("claude".into())
        );
        assert_eq!(
            app_basename(r"C:\Tools\Codex.exe run"),
            Some("codex".into())
        );
        assert_eq!(
            app_basename("sudo env FOO=bar npx somenewtool --flag"),
            Some("somenewtool".into()),
            "every transparent prefix defers to the token after it"
        );
        assert_eq!(
            app_basename("sudo -E cargo test"),
            Some("cargo".into()),
            "launcher flags are plumbing, not apps"
        );
        assert_eq!(
            app_basename("RUST_LOG=debug ./run.sh"),
            Some("run.sh".into())
        );
        assert_eq!(app_basename(""), None);
        assert_eq!(app_basename("   "), None);
        assert_eq!(
            app_basename("FOO=bar"),
            None,
            "assignments alone name no app"
        );
    }

    /// The canonical table is policy: every shell is the ONE "shell" app, the
    /// flagships keep their spelled-out ids, and an unknown basename is its
    /// own id (distinct, deterministic).
    #[test]
    fn canonical_table_folds_shells_and_pins_flagships() {
        for shell in ["zsh", "bash", "fish", "sh", "pwsh", "nu"] {
            assert_eq!(canonical_app_id(shell), "shell");
        }
        for flagship in ["claude", "codex", "agy", "aider", "gemini", "cursor"] {
            assert_eq!(canonical_app_id(flagship), flagship);
        }
        assert_eq!(canonical_app_id("somenewtool"), "somenewtool");
    }

    /// The flagship breeds are FIXED tuples (never hash luck) and pairwise
    /// distinct — pinned here so a ramp or roster refactor that would silently
    /// re-dress the claude cat fails loudly instead.
    #[test]
    fn flagship_breeds_are_fixed_and_distinct() {
        let expect = [
            ("shell", (CatGlyphId::S103, 1, 5, CatAge::Adult)),
            ("claude", (CatGlyphId::S108, 12, 0, CatAge::Adult)),
            ("codex", (CatGlyphId::S117, 15, 7, CatAge::Adult)),
            ("agy", (CatGlyphId::S122, 3, 6, CatAge::Adolescent)),
        ];
        let mut looks = Vec::new();
        for (id, (variant, coat, iris, age)) in expect {
            let look = KittyLook::for_app(id);
            assert_eq!(
                (look.variant, look.coat, look.iris, look.age),
                (variant, coat, iris, age),
                "{id} wears its hand-picked tuple"
            );
            looks.push(look);
        }
        for i in 0..looks.len() {
            for j in i + 1..looks.len() {
                assert_ne!(looks[i], looks[j], "flagships are pairwise distinct");
            }
        }
    }

    /// An unknown app derives its breed from the NAME and is stable: two
    /// resolutions yield the identical tuple (a pure function — same app,
    /// same cat, every machine), and different names may differ.
    #[test]
    fn unknown_app_breed_is_name_derived_and_stable() {
        let first = KittyLook::for_app("somenewtool");
        let second = KittyLook::for_app("somenewtool");
        assert_eq!(
            (first.variant, first.coat, first.iris, first.age),
            (second.variant, second.coat, second.iris, second.age),
            "the exact tuple resolves identically twice"
        );
        assert_eq!(first, second);
        assert_eq!(first, first.normalized(), "emitted looks are renderable");
        assert_eq!(
            GLYPHS[first.variant as usize].kind,
            GlyphKind::Head,
            "the hash path only ever deals heads"
        );
    }

    /// `for_app` never mints an accessory — bows and crowns mark COLLECTED
    /// cats (same law as the session kitty), across flagships and the hash
    /// path alike.
    #[test]
    fn for_app_never_emits_an_accessory() {
        for id in ["shell", "claude", "codex", "agy"] {
            assert_eq!(KittyLook::for_app(id).accessory, None);
        }
        for n in 0..64 {
            let name = format!("tool{n}");
            assert_eq!(KittyLook::for_app(&name).accessory, None, "{name}");
        }
    }

    /// The signature names are FIXED (never hash luck) and pairwise distinct,
    /// and they cover EXACTLY the ids the canonical table spells out — pinned
    /// so a bank refactor that would silently rename the claude cat fails
    /// loudly instead.
    #[test]
    fn signature_names_are_pinned_distinct_and_cover_the_canonical_ids() {
        let expect = [
            ("shell", "Boots"),
            ("claude", "Clementine"),
            ("codex", "Vector"),
            ("agy", "Sprout"),
            ("aider", "Buddy"),
            ("gemini", "Castor"),
            ("cursor", "Blink"),
        ];
        let mut names = Vec::new();
        for (id, name) in expect {
            assert_eq!(kitty_name(id), name, "{id} wears its hand-picked name");
            names.push(name);
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), expect.len(), "signature names are distinct");
    }

    /// Canonicalization reaches the signature name from every argv form: the
    /// same normalization pipeline that folds `claude.exe` onto the claude
    /// BREED folds it onto the claude NAME.
    #[test]
    fn canonicalized_forms_share_the_signature_name() {
        for (cmdline, name) in [
            ("/usr/local/bin/Claude --resume", "Clementine"),
            (r"C:\Tools\Codex.exe run", "Vector"),
            ("zsh -l", "Boots"),
            ("sudo env FOO=bar gemini", "Castor"),
        ] {
            let base = app_basename(cmdline).expect("fixture names a program");
            assert_eq!(
                kitty_name(canonical_app_id(&base)),
                name,
                "{cmdline:?} reaches its flagship's name"
            );
        }
    }

    /// An unknown app's name is hash-derived and STABLE (a pure function of
    /// the id), always drawn from the bank, and the draw genuinely spreads
    /// across it (the 6-bit field is not vacuously stuck).
    #[test]
    fn unknown_app_name_is_bank_drawn_and_stable() {
        assert_eq!(kitty_name("somenewtool"), kitty_name("somenewtool"));
        let mut seen = std::collections::BTreeSet::new();
        for n in 0..256 {
            let id = format!("tool{n}");
            let name = kitty_name(&id);
            assert!(
                KITTY_NAME_BANK.contains(&name),
                "{id} drew {name}, which is not in the bank"
            );
            seen.insert(name);
        }
        assert!(
            seen.len() >= 48,
            "256 ids should reach most of the 64-name bank — hit {}",
            seen.len()
        );
    }

    /// THE FOREVER PIN for the hash path: [`kitty_name`]'s doc promises
    /// "same app ⇒ same name, on every machine, forever" — a promise the
    /// other assertions cannot hold up (in-run self-equality, bank
    /// membership and spread are all invariant under a bank permutation or a
    /// salt change, either of which would reshuffle every non-canonical
    /// app's name across builds with every test green). These are today's
    /// exact draws; if this fails, the draw pipeline (bank order,
    /// [`APP_NAME_SALT`], `genome::mix`/`field`) changed and every unpinned
    /// kitty was just renamed — a BREAKING change to the promise, not a
    /// refactor.
    #[test]
    fn hash_path_names_are_pinned_forever() {
        for (id, name) in [
            // `somenewtool` and `rg` COLLIDE on Espresso today — kept as a
            // pair on purpose: the doc's "collisions are accepted" clause,
            // pinned alongside the stability it qualifies.
            ("somenewtool", "Espresso"),
            ("cargo", "Poppy"),
            ("rg", "Espresso"),
            ("python3", "Truffle"),
        ] {
            assert_eq!(kitty_name(id), name, "{id} must keep its name forever");
        }
    }

    /// Bank hygiene: no duplicates, nothing empty or over 8 chars, and no
    /// overlap with the signature table (a flagship's name stays exclusive).
    /// The bank's SIZE is compile-time pinned by its `[&str; 64]` type — 2⁶,
    /// so the single `field(gkey, 0, 6)` draw is unbiased by construction.
    #[test]
    fn name_bank_is_wide_short_and_duplicate_free() {
        let mut dedup = KITTY_NAME_BANK.to_vec();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), KITTY_NAME_BANK.len(), "no duplicate names");
        for name in KITTY_NAME_BANK {
            assert!(
                !name.is_empty() && name.chars().count() <= 8,
                "{name:?} is not a short cat name"
            );
            assert!(
                name.chars().next().is_some_and(char::is_uppercase),
                "{name:?} should read as a proper name"
            );
        }
        for id in ["shell", "claude", "codex", "agy", "aider", "gemini", "cursor"] {
            assert!(
                !KITTY_NAME_BANK.contains(&kitty_name(id)),
                "{id}'s signature name must not also be a bank ticket"
            );
        }
    }

    /// The sighting stays `Copy` and clock-free (session-agnostic) — the
    /// compile-time shape the host dedupe buffer relies on.
    #[test]
    fn sighting_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<KittySighting>();
        let s = KittySighting {
            kitty_type: KittyType::HeadPeek,
            magic: KittyMagic::None,
            shown_as: KittyShownAs::Cat,
            langs: LangSet::EMPTY,
            traits: TRAIT_SHY | TRAIT_BLAZE,
            look: KittyLook {
                coat: 3,
                ..KittyLook::default()
            },
            ident: 42,
        };
        let t = s;
        assert_eq!(s, t);
    }
}
