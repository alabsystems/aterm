// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kitty Log vocabulary registry and visual identity keys.

use aterm_lexicon::LangSet;

use crate::cat_glyphs_gen::{CatGlyphId, GLYPH_IDS, GLYPHS, GlyphKind};
use crate::genome::{CatAge, CatMagic};

/// Visual identity of a collected or companion cat.
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
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Salt for the launch kitty's genome namespace (`b"lnchKit\0"` as big-endian
/// ASCII), apart from the word renderer's occurrence seeds.
const LAUNCH_LOOK_SALT: u64 = 0x6C6E_6368_4B69_7400;
/// Salt for the app kitties' genome namespace (`b"appKitt\0"` as big-endian
/// ASCII), disjoint from [`LAUNCH_LOOK_SALT`] and the occurrence seeds.
const APP_LOOK_SALT: u64 = 0x6170_704B_6974_7400;
/// Launcher tokens transparent to app identity: the app is the token after them
/// (leading `VAR=value` assignments and `-flags` are skipped too).
const TRANSPARENT_PREFIXES: &[&str] = &["sudo", "env", "npx"];

/// Extract the first meaningful program token of a shell commandline, basenamed and lowercased.
#[must_use]
pub fn app_basename(commandline: &str) -> Option<String> {
    #[inline]
    const fn is_transparent_prefix(base: &str) -> bool {
        let mut i = 0;
        while i < TRANSPARENT_PREFIXES.len() {
            if base.eq_ignore_ascii_case(TRANSPARENT_PREFIXES[i]) {
                return true;
            }
            i += 1;
        }
        false
    }
    for token in commandline.split_whitespace() {
        // Launcher plumbing, not the app: `FOO=bar cargo …`, `sudo -E cargo …`.
        if token.starts_with('-') || token.contains('=') {
            continue;
        }
        let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
        let base = if base
            .get(base.len().saturating_sub(4)..)
            .is_some_and(|ext| ext.eq_ignore_ascii_case(".exe"))
        {
            &base[..base.len() - 4]
        } else {
            base
        };
        if base.is_empty() || is_transparent_prefix(base) {
            continue;
        }
        return Some(base.to_ascii_lowercase());
    }
    None
}

/// The canonical id every shell folds onto.
pub const SHELL_APP_ID: &str = "shell";

/// The canonical app id for a basename: every shell is [`SHELL_APP_ID`]; anything else is its basename.
#[inline]
#[must_use]
pub const fn canonical_app_id(basename: &str) -> &str {
    match basename.as_bytes() {
        b"zsh" | b"bash" | b"fish" | b"sh" | b"pwsh" | b"nu" => SHELL_APP_ID,
        _ => basename,
    }
}

/// Flagship breeds: hand-picked `(variant, coat, iris, age)` for the main apps,
/// in distinct coat families (claude's coat 12 is the stop closest to
/// terracotta).
#[inline]
const fn flagship_look(app_id: &str) -> Option<KittyLook> {
    let (variant, coat, iris, age) = match app_id.as_bytes() {
        b"claude" => (CatGlyphId::S108, 12, 0, CatAge::Adult),
        b"codex" => (CatGlyphId::S117, 15, 7, CatAge::Adult),
        b"agy" => (CatGlyphId::S122, 3, 6, CatAge::Adolescent),
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
    /// Default look (`CatGlyphId::S103`, coat 8, iris 4, adult).
    pub const DEFAULT: Self = Self {
        variant: CatGlyphId::S103,
        accessory: None,
        coat: 8,
        iris: 4,
        age: CatAge::Adult,
    };

    /// Const-friendly structural equality check.
    #[inline]
    #[must_use]
    pub const fn eq_const(&self, other: &Self) -> bool {
        let acc_eq = match (self.accessory, other.accessory) {
            (None, None) => true,
            (Some(a), Some(b)) => a as usize == b as usize,
            _ => false,
        };
        self.variant as usize == other.variant as usize
            && acc_eq
            && self.coat == other.coat
            && self.iris == other.iris
            && self.age as u8 == other.age as u8
    }

    /// Derive the deterministic launch kitty breed from the host-minted seed.
    #[inline]
    #[must_use]
    pub const fn for_launch(seed: u64) -> Self {
        let gkey = crate::genome::mix(seed ^ LAUNCH_LOOK_SALT);
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

    /// Derive the deterministic app kitty breed for a canonical app id.
    #[inline]
    #[must_use]
    pub const fn for_app(app_id: &str) -> Self {
        if let Some(look) = flagship_look(app_id) {
            return look;
        }
        let gkey = crate::genome::mix(aterm_hash::fnv1a_64(app_id.as_bytes()) ^ APP_LOOK_SALT);
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
    #[inline]
    #[must_use]
    pub const fn normalized(mut self) -> Self {
        if self.coat > 15 {
            self.coat = 15;
        }
        if self.iris > 7 {
            self.iris = 7;
        }
        match GLYPHS[self.variant as usize].kind {
            GlyphKind::Head => {
                if let Some(id) = self.accessory
                    && !matches!(GLYPHS[id as usize].kind, GlyphKind::Accessory)
                {
                    self.accessory = None;
                }
            }
            GlyphKind::Special => self.accessory = None,
            GlyphKind::Accessory => {
                self.variant = Self::DEFAULT.variant;
                self.accessory = None;
            }
        }
        self
    }
}

/// Minimum [`coat_distance`] threshold for a visibly distinct costume swap.
pub const SUFFICIENT_DIFFERENCE: f32 = 40.0;

/// The precomputed [`coat_distance`] table: each cell is the min of the RGB
/// distance between two `COAT_RAMP` stops and between the same stops after
/// `cat_baker`'s dark-background lift (bands 0|4, floor `COAT_MIN_LUM_DARK_BG`).
#[rustfmt::skip]
const COAT_DISTANCE: [[f32; 16]; 16] = [
    [
        0.0, 2.8093781, 2.0568104, 1.068686, 35.253334, 84.35241, 38.284103, 60.10213,
        81.077415, 101.84432, 120.05253, 145.46869, 164.0952, 184.17421, 216.38258,
        259.0521,
    ],
    [
        2.8093781, 0.0, 0.7539005, 1.8053502, 32.862465, 81.92891, 37.79307, 59.51762,
        79.92146, 100.79885, 119.089264, 144.68791, 163.30513, 182.58707, 214.32759,
        256.68756,
    ],
    [
        2.0568104, 0.7539005, 0.0, 1.0595087, 33.476418, 82.55651, 37.868793, 59.626038,
        80.189896, 101.03843, 119.307304, 144.8573, 163.47739, 182.97853, 214.85063,
        257.29965,
    ],
    [
        1.068686, 1.8053502, 1.0595087, 0.0, 34.22008, 83.31593, 37.80726, 59.601395,
        80.39209, 101.19558, 119.431786, 144.91122, 163.53357, 183.35428, 215.43098,
        258.02838,
    ],
    [
        35.253334, 32.862465, 33.476418, 34.22008, 0.0, 49.101936, 30.724583, 44.955532,
        56.780277, 77.672386, 96.18732, 123.081276, 141.18782, 153.26122, 182.18672,
        223.83029,
    ],
    [
        84.35241, 81.92891, 82.55651, 83.31593, 49.101936, 0.0, 66.3551, 63.356136,
        52.64029, 65.00769, 79.517296, 105.214066, 120.45331, 115.00435, 135.7461,
        174.84564,
    ],
    [
        38.284103, 37.79307, 37.868793, 37.80726, 30.724583, 66.3551, 0.0, 21.84033,
        44.63183, 64.567795, 82.38932, 107.35455, 125.97619, 149.74979, 186.91174,
        234.58047,
    ],
    [
        60.10213, 59.51762, 59.626038, 59.601395, 44.955532, 63.356136, 21.84033, 0.0,
        25.47548, 43.611923, 60.93439, 85.557, 104.16814, 130.43773, 170.78934, 221.3617,
    ],
    [
        81.077415, 79.92146, 80.189896, 80.39209, 56.780277, 52.64029, 44.63183, 25.47548,
        0.0, 21.189621, 39.749214, 66.4003, 84.723076, 105.68349, 145.93149, 197.43353,
    ],
    [
        101.84432, 100.79885, 101.03843, 101.19558, 77.672386, 65.00769, 64.567795,
        43.611923, 21.189621, 0.0, 18.574175, 45.453274, 63.600315, 88.09086, 132.81943,
        187.58731,
    ],
    [
        120.05253, 119.089264, 119.307304, 119.431786, 96.18732, 79.517296, 82.38932,
        60.93439, 39.749214, 18.574175, 0.0, 27.147743, 45.055523, 74.89326, 123.98387,
        181.39459,
    ],
    [
        145.46869, 144.68791, 144.8573, 144.91122, 123.081276, 105.214066, 107.35455,
        85.557, 66.4003, 45.453274, 27.147743, 0.0, 18.841444, 66.64833, 121.19818,
        181.39735,
    ],
    [
        164.0952, 163.30513, 163.47739, 163.53357, 141.18782, 120.45331, 125.97619,
        104.16814, 84.723076, 63.600315, 45.055523, 18.841444, 0.0, 59.841457, 115.628716,
        176.67484,
    ],
    [
        184.17421, 182.58707, 182.97853, 183.35428, 153.26122, 115.00435, 149.74979,
        130.43773, 105.68349, 88.09086, 74.89326, 66.64833, 59.841457, 0.0, 55.794266,
        116.9145,
    ],
    [
        216.38258, 214.32759, 214.85063, 215.43098, 182.18672, 135.7461, 186.91174,
        170.78934, 145.93149, 132.81943, 123.98387, 121.19818, 115.628716, 55.794266, 0.0,
        61.220913,
    ],
    [
        259.0521, 256.68756, 257.29965, 258.02838, 223.83029, 174.84564, 234.58047,
        221.3617, 197.43353, 187.58731, 181.39459, 181.39735, 176.67484, 116.9145,
        61.220913, 0.0,
    ],
];

/// Minimum RGB distance between two coat stops across light and dark backgrounds.
#[inline]
#[must_use]
pub const fn coat_distance(a: u8, b: u8) -> f32 {
    let ai = if a > 15 { 15 } else { a as usize };
    let bi = if b > 15 { 15 } else { b as usize };
    COAT_DISTANCE[ai][bi]
}

/// Stable semantic key for an authored glyph.
#[inline]
#[must_use]
pub const fn glyph_key(id: CatGlyphId) -> &'static str {
    GLYPHS[id as usize].id
}

#[inline]
const fn str_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Resolve a persisted semantic glyph key through the generated roster.
#[inline]
#[must_use]
pub const fn glyph_from_key(key: &str) -> Option<CatGlyphId> {
    let mut i = 0;
    while i < GLYPHS.len() {
        if str_eq(GLYPHS[i].id, key) {
            return Some(GLYPH_IDS[i]);
        }
        i += 1;
    }
    None
}

/// Stable persistence key for an age band.
#[inline]
#[must_use]
pub const fn age_key(age: CatAge) -> &'static str {
    match age {
        CatAge::Kitten => "kitten",
        CatAge::Adolescent => "adolescent",
        CatAge::Adult => "adult",
        CatAge::Elder => "elder",
    }
}

/// Resolve an age key from a ledger; unknown/legacy values use Adult.
#[inline]
#[must_use]
pub const fn age_from_key(key: &str) -> CatAge {
    match key.as_bytes() {
        b"kitten" => CatAge::Kitten,
        b"adolescent" => CatAge::Adolescent,
        b"elder" => CatAge::Elder,
        _ => CatAge::Adult,
    }
}

/// Shown kitty type in the Kitty Log vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KittyType {
    /// Head-only peek, the one type recorded.
    HeadPeek,
    HeadTilt,
    HeadPawLeft,
    HeadPawRight,
    HeadTwoPaws,
    PawSingle,
    PawDouble,
    PawClassic,
}

impl KittyType {
    /// Every shown type, in registry order.
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
    #[inline]
    pub const fn config_key(self) -> &'static str {
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
}

/// Magic dimension of the aggregation key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KittyMagic {
    /// The ordinary build (no magic-channel hit, or `feline.magic = false`).
    None,
    /// Fortune Cat (1/512).
    Fortune,
    /// Nebula Cat (1/1024).
    Nebula,
    /// Butterfly companion (≈ 1/95).
    Butterfly,
    /// Sakura cat (1/512).
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
    #[inline]
    pub const fn config_key(self) -> &'static str {
        match self {
            KittyMagic::None => "none",
            KittyMagic::Fortune => "fortune",
            KittyMagic::Nebula => "nebula",
            KittyMagic::Butterfly => "butterfly",
            KittyMagic::Sakura => "sakura",
        }
    }

    /// The recorder's mapping from the (config-gated) genome decode.
    #[inline]
    pub const fn from_cat(magic: Option<CatMagic>) -> KittyMagic {
        match magic {
            None => KittyMagic::None,
            Some(CatMagic::Fortune) => KittyMagic::Fortune,
            Some(CatMagic::Nebula) => KittyMagic::Nebula,
            Some(CatMagic::Butterfly) => KittyMagic::Butterfly,
            Some(CatMagic::Sakura) => KittyMagic::Sakura,
        }
    }
}

/// How the sighting rendered.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KittyShownAs {
    /// The real cat graphic.
    Cat,
    /// No graphic because cell floors / text clearance suppressed the cat.
    PawFallbackFloor,
    /// No graphic because the occurrence fell past `MAX_CATS`.
    PawFallbackOverflow,
    /// No graphic because legacy `style = "paw"` selects ink-only rendering.
    PawStyle,
}

/// [`KittySighting::traits`] bit: heterochromia.
pub const TRAIT_HETEROCHROMIA: u8 = 1;
/// [`KittySighting::traits`] bit: an ear nick.
pub const TRAIT_EAR_NICK: u8 = 1 << 1;
/// [`KittySighting::traits`] bit: a forehead blaze.
pub const TRAIT_BLAZE: u8 = 1 << 2;
/// [`KittySighting::traits`] bit: the shy build.
pub const TRAIT_SHY: u8 = 1 << 3;
/// [`KittySighting::traits`] bit: a bow (1/32 of ordinary cats).
pub const TRAIT_BOW: u8 = 1 << 4;
/// [`KittySighting::traits`] bit: sunglasses (1/256).
pub const TRAIT_SUNGLASSES: u8 = 1 << 5;
/// [`KittySighting::traits`] bit: a witch hat (1/512).
pub const TRAIT_WITCH_HAT: u8 = 1 << 6;
/// [`KittySighting::traits`] bit: a crown (1/1024).
pub const TRAIT_CROWN: u8 = 1 << 7;

/// Fold the accessory decode into the sighting's traits byte.
#[cfg(test)]
pub const fn accessory_trait_bits(acc: Option<crate::genome::Accessory>) -> u8 {
    use crate::genome::Accessory;
    match acc {
        None => 0,
        Some(Accessory::Bow) => TRAIT_BOW,
        Some(Accessory::Sunglasses) => TRAIT_SUNGLASSES,
        Some(Accessory::WitchHat) => TRAIT_WITCH_HAT,
        Some(Accessory::Crown) => TRAIT_CROWN,
    }
}

/// One recorded kitty sighting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KittySighting {
    /// The shown type; the recorder writes only [`KittyType::HeadPeek`].
    pub kitty_type: KittyType,
    /// The magic bucket, already `feline.magic`-gated.
    pub magic: KittyMagic,
    /// How it rendered (cat vs the three fallback causes).
    pub shown_as: KittyShownAs,
    /// Every language claiming the matched surface.
    pub langs: LangSet,
    /// Displayed-trait bits (`TRAIT_*`).
    pub traits: u8,
    /// The exact bounded visual identity that landed.
    pub look: KittyLook,
    /// The episode identity, the host's `(session, ident)` dedupe key.
    pub ident: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The raw ramp stop `i` as 0–255 float channels.
    const fn raw_stop(i: u8) -> (f32, f32, f32) {
        let hex = crate::cat_baker::COAT_RAMP[i as usize];
        (
            ((hex >> 16) & 0xff) as f32,
            ((hex >> 8) & 0xff) as f32,
            (hex & 0xff) as f32,
        )
    }

    /// Euclidean RGB distance over 0–255 channels, the [`coat_distance`] unit.
    fn dist(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
    }

    /// One cell of the metric, re-derived from the live colour math: the min over
    /// the raw ramp and `cat_baker`'s dark-background lift.
    fn live_coat_distance(a: u8, b: u8) -> f32 {
        let lifted = |i: u8| {
            let (r, g, b) = crate::cat_baker::dark_bg_coat_stop(i);
            (r * 255.0, g * 255.0, b * 255.0)
        };
        dist(raw_stop(a), raw_stop(b)).min(dist(lifted(a), lifted(b)))
    }

    /// Tolerance 0.01: the lift's bisection uses `powf`, which is not correctly rounded on
    /// every platform.
    #[test]
    fn coat_distance_table_is_pinned_to_the_live_lift() {
        for a in 0..16u8 {
            for b in 0..16u8 {
                let live = live_coat_distance(a, b);
                assert!(
                    (coat_distance(a, b) - live).abs() < 1e-2,
                    "cell ({a},{b}): table {} vs live {live}",
                    coat_distance(a, b)
                );
            }
        }
    }

    /// The flagship coats clear [`SUFFICIENT_DIFFERENCE`] by measurement, with 4× headroom.
    #[test]
    fn flagship_trio_is_always_sufficiently_different() {
        let coats = [
            ("claude", KittyLook::for_app("claude").coat),
            ("codex", KittyLook::for_app("codex").coat),
            ("agy", KittyLook::for_app("agy").coat),
        ];
        for i in 0..coats.len() {
            for j in i + 1..coats.len() {
                let d = coat_distance(coats[i].1, coats[j].1);
                assert!(
                    d >= 4.0 * SUFFICIENT_DIFFERENCE,
                    "{}–{}: flagship coats keep 4x headroom (got {d})",
                    coats[i].0,
                    coats[j].0
                );
            }
        }
    }

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

    /// The registry keys are unique and stable: renaming one orphans persisted rows.
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
        let mut dedup = type_keys.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), type_keys.len(), "keys must be unique");
    }

    #[test]
    fn launch_kitty_is_seed_pure_normalized_head_and_bare() {
        for seed in (0..4096u64).chain([u64::MAX, 0x8000_0000_0000_0000]) {
            let a = KittyLook::for_launch(seed);
            assert_eq!(a, KittyLook::for_launch(seed), "seed {seed}: pure");
            assert_eq!(a, a.normalized(), "seed {seed}: normalized");
            assert_eq!(
                GLYPHS[a.variant as usize].kind,
                GlyphKind::Head,
                "seed {seed}: the launch floor rolls heads only"
            );
            assert!(a.accessory.is_none(), "seed {seed}: no free accessory");
        }
    }

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
            app_basename(r"SUDO.EXE ENV.exe NPX.EXE C:\Bin\Agy.EXE --version"),
            Some("agy".into()),
            "case-insensitive .exe and launcher prefixes strip cleanly"
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

    /// `for_app` never mints an accessory, for flagships and the hash path;
    /// the hash path is stable and deals renderable heads only.
    #[test]
    fn for_app_never_emits_an_accessory() {
        for id in ["claude", "codex", "agy"] {
            assert_eq!(KittyLook::for_app(id).accessory, None);
        }
        for n in 0..64 {
            let name = format!("tool{n}");
            let look = KittyLook::for_app(&name);
            assert_eq!(look.accessory, None, "{name}");
            assert_eq!(look, KittyLook::for_app(&name), "{name}: stable");
            assert_eq!(look, look.normalized(), "{name}: renderable");
            assert_eq!(
                GLYPHS[look.variant as usize].kind,
                GlyphKind::Head,
                "{name}"
            );
        }
    }
}
