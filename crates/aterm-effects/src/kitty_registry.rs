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

impl KittyLook {
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
