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

/// Salt keeping the launch kitty's identity in its own genome namespace,
/// away from the word renderer's position-bearing occurrence seeds
/// (`b"lnchKit\0"` as big-endian ASCII). One documented ASCII salt per
/// identity namespace is the law here, so a future identity lane can never
/// collide with this one by hash coincidence of the input spaces.
const LAUNCH_LOOK_SALT: u64 = 0x6C6E_6368_4B69_7400;

/// Salt keeping app-derived kitty identities in their own genome namespace —
/// disjoint from both [`LAUNCH_LOOK_SALT`] and the word renderer's occurrence
/// seeds (`b"appKitt\0"` as big-endian ASCII), so "the claude cat" can never
/// collide with a launch kitty by hash coincidence of the input spaces.
const APP_LOOK_SALT: u64 = 0x6170_704B_6974_7400;

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

/// The canonical id every SHELL folds onto ([`canonical_app_id`]). A shell
/// is not a program cat: at the prompt (and inside a nested shell) the pane
/// wears the base cat — the launch kitty — so the host treats this id as
/// "no program claim" rather than looking up a breed for it.
pub const SHELL_APP_ID: &str = "shell";

/// THE CANONICAL APP TABLE — the ONE place basenames become app identities,
/// so every argv form of a flagship tool lands on the same kitty. Policy,
/// kept small on purpose:
///   • every shell is the SAME id ([`SHELL_APP_ID`]) — the prompt is home,
///     and home wears the launch kitty regardless of which shell renders it;
///   • the flagship AI coding tools get their canonical ids spelled out so a
///     rename/wrapper (`claude.exe`, `/opt/bin/codex`) cannot fork the breed;
///   • everything else IS its basename — unknown tools stay distinct and get
///     the deterministic name-derived breed from [`KittyLook::for_app`].
#[must_use]
pub fn canonical_app_id(basename: &str) -> &str {
    match basename {
        "zsh" | "bash" | "fish" | "sh" | "pwsh" | "nu" => SHELL_APP_ID,
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
///   • `claude` — S108 (one of the two tallest, roundest, lowest-eyed faces:
///     open and friendly), bright-ginger coat 12 — the ramp's closest stop to
///     Anthropic's warm terracotta — with warm copper iris 0.
///   • `codex` — S117 (level, mid-set eyes: a precise, even gaze), clean
///     white coat 15 with storm-teal iris 7: the cool counterpoint to claude.
///   • `agy` — S122 (the only head in the roster with its own aspect ratio:
///     one of a kind, like the house tool), blue-slate coat 3 — the ramp's
///     blue notch — with sea-green iris 6, and the ADOLESCENT age band: the
///     youngest tool in the pack, still growing.
///
/// (The shell's old charcoal tuxedo is gone: since the launch-kitty ruling the
/// prompt wears the launch kitty, not a program cat.)
fn flagship_look(app_id: &str) -> Option<KittyLook> {
    let (variant, coat, iris, age) = match app_id {
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
    /// THE LAUNCH KITTY (owner ruling, 2026-08-17: *"it's too hard to have
    /// the cat changing too much — let's have the cat generated at aterm
    /// launch and per computer"*): the BASE companion breed for the whole
    /// aterm process, decoded from a seed the host mints exactly once at
    /// launch. It is what the prompt wears in every window and session until
    /// aterm is launched again — the cat you see when no program has earned
    /// the cursor.
    ///
    /// It replaces the per-session kitty (2026-07-26, a different breed per
    /// tab) and the old "shell" program cat. The per-app kitties
    /// ([`Self::for_app`], "the claude cat, the codex cat") stay — the owner
    /// likes them — but ride ABOVE this base only after a program has held
    /// the pane long enough (the host's tenure gate; the switch was what
    /// felt abrupt, not the cats), and a pinned favourite outranks both: the
    /// user choosing a cat is a reason, and only reasons may change the kitty.
    ///
    /// A pure function of the seed: same seed ⇒ same cat, byte-stable, on
    /// every machine — nothing to persist, nothing to sync. The host owns
    /// the seed's entropy (aterm-gui mints it from the audited CSPRNG at
    /// `App` construction; tests pass a fixed seed), so this crate stays
    /// clockless and dieless like every other engine in it.
    ///
    /// The axes are decoded through the ordinary v4 genome so the launch
    /// kitty is drawn from exactly the same roster (head, coat, iris, age
    /// band) the ambient word-cats roll from — no separate art path, no new
    /// bake keys. `cat_variant_v4` indexes HEADS only, so the launch kitty
    /// can never be a full-cat special (the maneki stays a discovery).
    /// Accessories stay `None`: a bow or crown marks a COLLECTED cat, and
    /// minting them for free here would devalue them.
    #[must_use]
    pub fn for_launch(seed: u64) -> Self {
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

    /// THE APP KITTY (owner spec, 2026-08-07: "each major app gets its own
    /// cursor kitty"; kept under the 2026-08-17 ruling — *"I like the
    /// different cats"* — with the host's tenure gate deciding WHEN one is
    /// worn): the deterministic breed for a canonical app id from
    /// [`canonical_app_id`].
    ///
    /// Flagships (`claude`/`codex`/`agy`) wear the hand-picked
    /// [`flagship_look`] tuples so the tools people live in look intentional;
    /// every other app derives its look from the NAME — `form_hash` (the
    /// lexicon's canonical FNV-1a-64) salted into its own namespace, decoded
    /// through the ordinary v4 genome exactly like [`Self::for_launch`]. Same
    /// app ⇒ same cat, on every machine, by construction — "the claude cat"
    /// is recognizable everywhere — so there is nothing to persist and
    /// nothing to sync. (Only the BASE cat is per launch/computer.)
    ///
    /// Accessories stay `None` here for the same reason as the launch kitty:
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

/// THE SUFFICIENT-DIFFERENCE FLOOR for two coats, in [`coat_distance`]
/// units (resolved-RGB Euclidean, 0–255 channels): below it, two coats are
/// close enough that a costume swap between them reads as noise, and an
/// arrival ceremony would announce nothing a viewer can see.
///
/// Derivation — measured over the live ramp and dark-background lift, not
/// taste. 40 is the knee that:
///   • catches all six dark-collapse pairs among coats {0,1,2,3} (post-lift
///     they sit within 2.9 of each other, while raw ramp RGB claims up to
///     105) and the six adjacent warm-ramp pairs (6,7) (7,8) (8,9) (9,10)
///     (10,11) (11,12) — the similarity-ordered ramp's deliberate
///     near-twins;
///   • keeps every genuine family crossing: ginger-vs-cream (12,13) at
///     59.8 stays sufficient (a floor of 60 would demote it);
///   • demotes 22 of the 120 coat pairs — 1 856 of the 8 128 unordered
///     pairs of the 128-cell `(coat, iris)` space, 22.8 %;
///   • never touches a flagship: the claude/codex/agy trio's minimum
///     pairwise [`coat_distance`] is 163.5 (claude–agy, on a dark
///     background), 4× this floor — no flagship pair demotes under any
///     candidate floor up to ~160.
pub const SUFFICIENT_DIFFERENCE: f32 = 40.0;

/// The precomputed [`coat_distance`] table. Each cell is
/// `min(dist(COAT_RAMP[a], COAT_RAMP[b]), dist(lift(COAT_RAMP[a]),
/// lift(COAT_RAMP[b])))` — the minimum over the two background classes of
/// the Euclidean RGB distance (0–255 channels) between the RESOLVED coat
/// stops, where `lift` is `cat_baker`'s dark-background luminance rescue
/// (bands 0|4, floor `COAT_MIN_LUM_DARK_BG`). Precomputed because the lift
/// is bake-time float math (HSV + a luminance bisection), not
/// const-evaluable; `coat_distance_table_is_pinned_to_the_live_lift`
/// re-derives every cell from the living math, so the table cannot drift
/// from the baker silently.
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

/// THE SUFFICIENT-DIFFERENCE METRIC: how far apart two coat stops actually
/// look on glass, as the worst case over the two background classes —
/// `min` of the raw-ramp RGB distance and the distance after `cat_baker`'s
/// dark-background luminance rescue. The `min` is the law: the raw ramp
/// LIES on dark themes (the rescue collapses coats 0–3 to within 2.9 of
/// each other while their raw distances run 20.9–105.3), and a viewer on a
/// dark background is the common case, so "visibly different" must hold on
/// the background where the two coats are CLOSEST.
///
/// Deliberately NO iris term: the walking pet culls `GlyphRole::Iris`
/// entirely below `pet_baker::FACE_DETAIL_MIN_H` (56 px tile height —
/// every 1× display), so an iris term would certify pixel-identical pets
/// as "visibly different". The pet's whole legible identity is its coat.
///
/// Symmetric, zero on the diagonal; out-of-range indices clamp exactly as
/// [`KittyLook::normalized`] clamps `coat`. Compare against
/// [`SUFFICIENT_DIFFERENCE`].
#[must_use]
pub fn coat_distance(a: u8, b: u8) -> f32 {
    COAT_DISTANCE[usize::from(a.min(15))][usize::from(b.min(15))]
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

    /// The raw ramp stop `i` as 0–255 float channels.
    fn raw_stop(i: u8) -> (f32, f32, f32) {
        let hex = crate::cat_baker::COAT_RAMP[usize::from(i)];
        (
            ((hex >> 16) & 0xff) as f32,
            ((hex >> 8) & 0xff) as f32,
            (hex & 0xff) as f32,
        )
    }

    /// Euclidean RGB distance over 0–255 channels — the [`coat_distance`]
    /// unit.
    fn dist(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
    }

    /// One cell of the metric, re-derived from the LIVING colour math: the
    /// min over the raw ramp and `cat_baker`'s dark-background lift.
    fn live_coat_distance(a: u8, b: u8) -> f32 {
        let lifted = |i: u8| {
            let (r, g, b) = crate::cat_baker::dark_bg_coat_stop(i);
            (r * 255.0, g * 255.0, b * 255.0)
        };
        dist(raw_stop(a), raw_stop(b)).min(dist(lifted(a), lifted(b)))
    }

    /// THE TABLE PIN: every cell of the precomputed [`COAT_DISTANCE`] table
    /// equals the metric re-derived from the live ramp and the live
    /// dark-background lift (`cat_baker::dark_bg_coat_stop`). If this
    /// fails, `COAT_RAMP`, `COAT_MIN_LUM_DARK_BG` or the lift itself moved
    /// and the table must be regenerated WITH it — the table is a cache of
    /// the baker's law, never a second law. (Tolerance 0.01: the lift's
    /// luminance bisection rides `powf`, which libm does not promise
    /// correctly rounded across platforms; a real drift moves cells by
    /// whole units.)
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

    /// THE DARK-COLLAPSE TRUTH: on dark backgrounds the luminance rescue
    /// collapses coats {0,1,2,3} onto near-identical pale tones, so every
    /// pair among them is insufficient — even where the raw ramp claims a
    /// large distance ((0,3) is 105 raw and ~1 on glass). A raw-only
    /// metric would certify exactly these swaps as worth a ceremony.
    #[test]
    fn dark_collapse_pairs_are_insufficient_where_raw_rgb_would_pass() {
        for (a, b) in [(0u8, 1u8), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)] {
            assert!(
                coat_distance(a, b) < SUFFICIENT_DIFFERENCE,
                "({a},{b}): dark-collapsed pairs are never sufficient (got {})",
                coat_distance(a, b)
            );
        }
        assert!(
            dist(raw_stop(0), raw_stop(3)) > SUFFICIENT_DIFFERENCE,
            "the raw ramp would have passed (0,3) — the min over backgrounds is the point"
        );
    }

    /// The flagship trio (claude 12, codex 15, agy 3) never demotes: every
    /// pair clears [`SUFFICIENT_DIFFERENCE`] with 4× headroom, so the rate
    /// law's sufficient-difference check changes nothing for the cats
    /// people live in — by measurement, not by exemption.
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

    /// The metric's shape: symmetric, zero on the diagonal, and out-of-range
    /// indices clamp to the last stop exactly like `normalized()` clamps
    /// `coat`.
    #[test]
    fn coat_distance_is_symmetric_with_a_zero_diagonal() {
        for a in 0..16u8 {
            assert_eq!(coat_distance(a, a), 0.0, "d({a},{a})");
            for b in 0..16u8 {
                assert_eq!(coat_distance(a, b), coat_distance(b, a), "d({a},{b})");
            }
        }
        assert_eq!(coat_distance(u8::MAX, 15), 0.0, "clamped like normalized()");
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

    /// THE LAUNCH KITTY is a pure function of its seed: the same seed decodes
    /// to the identical breed (byte-stable, on every machine — nothing to
    /// persist), the result is already normalized (renderable by
    /// construction), it is a HEAD (never a full-cat special — the maneki
    /// stays a discovery), and it never mints an accessory (bows and crowns
    /// mark COLLECTED cats).
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

    /// The seed genuinely SPREADS the roster: across a modest sweep the
    /// breeds are well distributed (not vacuously stuck on a handful), and
    /// no seed in the sweep lands on the one shared `KittyLook::default()` —
    /// the "every install wears the same cat" regression the derived floor
    /// exists to prevent.
    #[test]
    fn launch_kitties_spread_the_roster_and_avoid_the_shared_default() {
        let looks: Vec<KittyLook> = (0..64u64).map(KittyLook::for_launch).collect();
        let mut distinct = looks.clone();
        distinct.sort_by_key(|l| (l.variant as u8, l.coat, l.iris, l.age as u8));
        distinct.dedup();
        assert!(
            distinct.len() >= 32,
            "64 seeds should yield at least 32 distinct breeds (got {})",
            distinct.len()
        );
        assert!(
            looks.iter().all(|l| *l != KittyLook::default()),
            "the derived floor must not collapse onto the shared default cat"
        );
    }

    /// THE FOREVER PIN for the launch draw: [`KittyLook::for_launch`]
    /// promises "same seed ⇒ same cat, on every machine". The purity and
    /// spread assertions above cannot hold that promise up on their own —
    /// they are invariant under a salt change or a genome-field reshuffle,
    /// either of which would silently re-dress every launch kitty across
    /// builds with every test green. These are today's exact draws (the
    /// literal `(variant, coat, iris, age)` tuples); if this fails, the draw
    /// pipeline ([`LAUNCH_LOOK_SALT`], `genome::mix`, the `cat_*_v4`
    /// decoders) changed and the promise broke — a BREAKING change, not a
    /// refactor. (A pinned favourite survives such a change; an unpinned
    /// launch cat does not.)
    #[test]
    fn launch_draws_are_pinned_forever() {
        for (seed, (variant, coat, iris, age)) in [
            (0u64, (CatGlyphId::S123, 2u8, 7u8, CatAge::Adult)),
            (1, (CatGlyphId::S111, 0u8, 4u8, CatAge::Elder)),
            (0x5EED, (CatGlyphId::S118, 0u8, 4u8, CatAge::Adolescent)),
        ] {
            let got = KittyLook::for_launch(seed);
            assert_eq!(
                (got.variant, got.coat, got.iris, got.age),
                (variant, coat, iris, age),
                "seed {seed:#x} must keep its breed forever"
            );
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

    /// The canonical table is policy: every shell is the ONE shell id (which
    /// the host reads as "no program claim"), the flagships keep their
    /// spelled-out ids, and an unknown basename is its own id (distinct,
    /// deterministic).
    #[test]
    fn canonical_table_folds_shells_and_pins_flagships() {
        for shell in ["zsh", "bash", "fish", "sh", "pwsh", "nu"] {
            assert_eq!(canonical_app_id(shell), SHELL_APP_ID);
        }
        for flagship in ["claude", "codex", "agy", "aider", "gemini", "cursor"] {
            assert_eq!(canonical_app_id(flagship), flagship);
        }
        assert_eq!(canonical_app_id("somenewtool"), "somenewtool");
    }

    /// The flagship breeds are FIXED tuples (never hash luck) and pairwise
    /// distinct — pinned here so a ramp or roster refactor that would silently
    /// re-dress the claude cat fails loudly instead. The shell has NO flagship
    /// tuple any more: the prompt wears the launch kitty.
    #[test]
    fn flagship_breeds_are_fixed_and_distinct() {
        let expect = [
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
        assert!(
            flagship_look(SHELL_APP_ID).is_none(),
            "the shell is the base cat's business, not a flagship"
        );
    }

    /// An unknown app derives its breed from the NAME and is stable: two
    /// resolutions yield the identical tuple (a pure function — same app,
    /// same cat, every machine), heads only, renderable.
    #[test]
    fn unknown_app_breed_is_name_derived_and_stable() {
        let first = KittyLook::for_app("somenewtool");
        let second = KittyLook::for_app("somenewtool");
        assert_eq!(first, second, "the exact tuple resolves identically twice");
        assert_eq!(first, first.normalized(), "emitted looks are renderable");
        assert_eq!(
            GLYPHS[first.variant as usize].kind,
            GlyphKind::Head,
            "the hash path only ever deals heads"
        );
    }

    /// `for_app` never mints an accessory — bows and crowns mark COLLECTED
    /// cats (same law as the launch kitty), across flagships and the hash
    /// path alike.
    #[test]
    fn for_app_never_emits_an_accessory() {
        for id in ["claude", "codex", "agy"] {
            assert_eq!(KittyLook::for_app(id).accessory, None);
        }
        for n in 0..64 {
            let name = format!("tool{n}");
            assert_eq!(KittyLook::for_app(&name).accessory, None, "{name}");
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
