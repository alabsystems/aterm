// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sparkle-words genome (docs/sparkle-words-v2-design.md §3): the deterministic (word form, surrounding words) → effect-parameters channel.

use aterm_lexicon::{Class, fold_into, is_interior_joiner, is_token_char};

use crate::cat_glyphs_gen::{CatGlyphId, HEADS};

/// The frozen 16-byte genome, computed once on a persist-map miss and copied forward on every hit (word_decorations.rs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Genome {
    /// `seed ^ ctx_fp` — the feature-field source (§3.4 layouts).
    pub gkey: u64,
    /// `mix(ctx_fp ^ form_hash ^ MAGIC_SALT)` — the position-independent rare-variant stream (§3.5).
    pub magic: u64,
}

/// §3.5 magic-channel salt ("seed cat, seed cat").
pub const MAGIC_SALT: u64 = 0x5EED_CA71_5EED_CA71;

/// splitmix64 finalizer — decorrelates adjacent seeds and never returns 0.
#[inline]
#[must_use]
pub const fn mix(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let r = z ^ (z >> 31);
    if r == 0 { 0xD1B5_4A32_D192_ED03 } else { r }
}

/// Distance weights `w(d) = [4, 2, 2, 1][d - 1]` for the d-th token on each side.
const CTX_WEIGHTS: [i32; 4] = [4, 2, 2, 1];
/// The matched word's own `form_hash` vote weight: `cat` and `kitten` in the
/// same surroundings differ, and a bare word still gets a vote.
const FORM_WEIGHT: i32 = 3;

/// The vote accumulator, newtyped so the owning struct can derive `Default`.
pub struct VoteScratch(pub [i32; 64]);

impl VoteScratch {
    /// Zeroed 64-bin hue tally.
    pub const ZERO: Self = Self([0; 64]);

    /// Construct a zeroed 64-bin hue tally.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self::ZERO
    }
}

impl Default for VoteScratch {
    #[inline]
    fn default() -> Self {
        Self::ZERO
    }
}

/// Which kernel computes `ctx_fp`.
pub const SIMHASH_KERNEL: SimhashKernel = SimhashKernel::BitSliced;

/// §3.2 kernel selector — see [`SIMHASH_KERNEL`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SimhashKernel {
    /// The §3.2 vote loop (`votes: [i32; 64]`): the oracle and the certificate-red fallback.
    Reference,
    /// The carry-save-adder network, shipped under its §7.5 equivalence certificate.
    BitSliced,
}

/// §3.2 context fingerprint, the live entry point: dispatches on [`SIMHASH_KERNEL`].
#[inline]
pub fn simhash_ctx(
    chars: &[char],
    start: usize,
    end: usize,
    form_hash: u64,
    votes: &mut VoteScratch,
    tok: &mut String,
    folded: &mut String,
) -> u64 {
    match SIMHASH_KERNEL {
        SimhashKernel::Reference => {
            simhash_ctx_reference(chars, start, end, form_hash, votes, tok, folded)
        }
        SimhashKernel::BitSliced => {
            simhash_ctx_bitsliced(chars, start, end, form_hash, tok, folded)
        }
    }
}

/// Context fingerprint, reference kernel: weighted SimHash over up to 4 tokens each side of the match span `start, end)` (char indices into `chars`, one physical row) plus the match's `form_hash` at weight 3.
#[inline]
pub fn simhash_ctx_reference(
    chars: &[char],
    start: usize,
    end: usize,
    form_hash: u64,
    votes: &mut VoteScratch,
    tok: &mut String,
    folded: &mut String,
) -> u64 {
    let votes = &mut votes.0;
    votes.fill(0);
    cast_vote(votes, form_hash, FORM_WEIGHT);
    let mut pos = end;
    let mut wi = 0;
    while wi < CTX_WEIGHTS.len() {
        let Some((s, e)) = token_right(chars, pos) else {
            break;
        };
        pos = e;
        vote_token(&chars[s..e], CTX_WEIGHTS[wi], votes, tok, folded);
        wi += 1;
    }
    let mut pos = start;
    let mut wi = 0;
    while wi < CTX_WEIGHTS.len() {
        let Some((s, e)) = token_left(chars, pos) else {
            break;
        };
        pos = s;
        vote_token(&chars[s..e], CTX_WEIGHTS[wi], votes, tok, folded);
        wi += 1;
    }
    let mut fp = 0u64;
    let mut i = 0;
    while i < 64 {
        fp |= u64::from(votes[i] >= 0) << i;
        i += 1;
    }
    fp
}

/// Accumulate one voter's 64 bit votes at weight `w`.
#[inline]
const fn cast_vote(votes: &mut [i32; 64], h: u64, w: i32) {
    let mut i = 0;
    while i < 64 {
        if (h >> i) & 1 == 1 {
            votes[i] += w;
        } else {
            votes[i] -= w;
        }
        i += 1;
    }
}

/// Cast one token's vote at weight `w` if it passes the filter: alphabetic with ≥ 2 folded
/// chars, so digits, punctuation, clocks and spinners are silent.
#[inline]
fn vote_token(
    tok_chars: &[char],
    w: i32,
    votes: &mut [i32; 64],
    tok: &mut String,
    folded: &mut String,
) {
    if let Some(h) = token_vote_hash(tok_chars, tok, folded) {
        cast_vote(votes, h, w);
    }
}

/// The fold, filter and `mix(fnv(folded))` hash shared by both kernels: `Some(h_t)` when the
/// token votes, `None` when silent.
#[inline]
fn token_vote_hash(tok_chars: &[char], tok: &mut String, folded: &mut String) -> Option<u64> {
    if tok_chars.is_empty() || (tok_chars.len() == 1 && tok_chars[0].is_ascii()) {
        return None;
    }
    let mut h = aterm_hash::FNV64_OFFSET;
    let mut ascii_ok = true;
    let mut i = 0;
    while i < tok_chars.len() {
        let c = tok_chars[i];
        if c.is_ascii_alphabetic() {
            h = aterm_hash::fnv_mix_u64(h, u64::from(c.to_ascii_lowercase() as u8));
        } else if c.is_ascii() {
            let mut j = i + 1;
            let mut rest_ascii = true;
            while j < tok_chars.len() {
                if !tok_chars[j].is_ascii() {
                    rest_ascii = false;
                    break;
                }
                j += 1;
            }
            if rest_ascii {
                return None;
            }
            ascii_ok = false;
            break;
        } else {
            ascii_ok = false;
            break;
        }
        i += 1;
    }
    if ascii_ok {
        return if tok_chars.len() >= 2 {
            Some(mix(h))
        } else {
            None
        };
    }
    tok.clear();
    let mut ci = 0;
    while ci < tok_chars.len() {
        tok.push(tok_chars[ci]);
        ci += 1;
    }
    fold_into(tok, folded);
    let mut len = 0usize;
    let mut h = aterm_hash::FNV64_OFFSET;
    for c in folded.chars() {
        if !c.is_ascii_alphabetic() && (c.is_ascii() || !c.is_alphabetic()) {
            return None;
        }
        len += 1;
        h = aterm_hash::fnv_mix_u64(h, c as u64);
    }
    if len < 2 {
        return None;
    }
    Some(mix(h))
}

/// Next token right of char index `pos`: skip non-token chars, then take the
/// maximal token run using the scanner's predicate and interior-joiner rule
/// (`cat's` / `cat-like` are one token, as in aterm-lexicon's `token_end`).
#[inline]
fn token_right(chars: &[char], mut pos: usize) -> Option<(usize, usize)> {
    let n = chars.len();
    while pos < n && !is_token_char(chars[pos]) {
        pos += 1;
    }
    if pos >= n {
        return None;
    }
    let s = pos;
    let mut j = pos;
    loop {
        while j < n && is_token_char(chars[j]) {
            j += 1;
        }
        if j < n && is_interior_joiner(chars[j]) && j + 1 < n && is_token_char(chars[j + 1]) {
            // consume the joiner, keep scanning (scanner parity)
            j += 1;
        } else {
            break;
        }
    }
    Some((s, j))
}

/// Next token to the left of char index `pos` (mirror of [`token_right`]).
#[inline]
fn token_left(chars: &[char], mut pos: usize) -> Option<(usize, usize)> {
    while pos > 0 && !is_token_char(chars[pos - 1]) {
        pos -= 1;
    }
    if pos == 0 {
        return None;
    }
    let e = pos;
    let mut s = pos;
    loop {
        while s > 0 && is_token_char(chars[s - 1]) {
            s -= 1;
        }
        if s >= 2 && is_interior_joiner(chars[s - 1]) && is_token_char(chars[s - 2]) {
            // interior joiner between two token chars
            s -= 1;
        } else {
            break;
        }
    }
    Some((s, e))
}

/// The §3.2 voter slots in fixed order: `[form(3), right d1..4 (4,2,2,1), left d1..4 (4,2,2,1)]`.
pub const SIMHASH_SLOT_WEIGHTS: [i32; 9] = [FORM_WEIGHT, 4, 2, 2, 1, 4, 2, 2, 1];

/// A collected voter set: slot `i`'s token hash and a presence mask (bit `i` set means slot `i` votes at `SIMHASH_SLOT_WEIGHTS[i]`).
#[derive(Clone, Copy, Debug)]
pub struct VoterSet {
    pub hash: [u64; 9],
    pub present: u16,
}

impl VoterSet {
    /// Empty voter set with no populated slots.
    pub const EMPTY: Self = Self {
        hash: [0; 9],
        present: 0,
    };
}

impl Default for VoterSet {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Reference majority over a [`VoterSet`]: the `cast_vote` + signed-majority fold of [`simhash_ctx_reference`], factored so the battery compares both kernels on identical inputs.
#[cfg(test)]
pub const fn simhash_majority_reference(v: &VoterSet) -> u64 {
    let mut votes = [0i32; 64];
    let mut i = 0;
    while i < SIMHASH_SLOT_WEIGHTS.len() {
        if v.present & (1 << i) != 0 {
            cast_vote(&mut votes, v.hash[i], SIMHASH_SLOT_WEIGHTS[i]);
        }
        i += 1;
    }
    let mut fp = 0u64;
    let mut i = 0;
    while i < 64 {
        if votes[i] >= 0 {
            fp |= 1u64 << i;
        }
        i += 1;
    }
    fp
}

/// One lane-wise full adder: `(sum, carry)` — all ops bitwise, carry-free
/// across lanes (the lift side condition).
#[inline]
const fn csa(a: u64, b: u64, c: u64) -> (u64, u64) {
    (a ^ b ^ c, (a & b) | (c & (a ^ b)))
}

/// Bit-sliced majority via the carry-save adder network.
#[inline]
#[must_use]
pub const fn simhash_majority_bitsliced(v: &VoterSet) -> u64 {
    const fn h(v: &VoterSet, i: usize) -> u64 {
        if v.present & (1 << i) != 0 {
            v.hash[i]
        } else {
            0
        }
    }

    let (p1_s, p1_c) = csa(h(v, 0), h(v, 4), h(v, 8));
    let (p2_a, p2_ca) = csa(h(v, 0), h(v, 2), h(v, 3));
    let (p2_s, p2_cb) = csa(p2_a, h(v, 6), h(v, 7));
    let (p2_t1, p2_t2) = (p2_ca ^ p2_cb, p2_ca & p2_cb);
    let (p4_s, p4_c) = (h(v, 1) ^ h(v, 5), h(v, 1) & h(v, 5));

    let s0 = p1_s;
    let (s1, k4) = (p1_c ^ p2_s, p1_c & p2_s);
    let (s2, k8) = csa(p2_t1, p4_s, k4);
    let (s3, k16) = csa(p2_t2, p4_c, k8);
    let s4 = k16;

    let p = v.present;
    let w_total = (p & 0b0_0000_0001).count_ones() as i32 * 3
        + (p & 0b0_0010_0010).count_ones() as i32 * 4
        + (p & 0b0_1100_1100).count_ones() as i32 * 2
        + (p & 0b1_0001_0000).count_ones() as i32;
    let t = (w_total + 1) >> 1;
    let mut ge = !0u64;
    let planes = [s0, s1, s2, s3, s4];
    let mut k = 0;
    while k < 5 {
        let s_k = planes[k];
        ge = if (t >> k) & 1 != 0 {
            ge & s_k
        } else {
            ge | s_k
        };
        k += 1;
    }
    ge
}

/// Collect the voter set for a match span: the same walk, filter and hash as
/// [`simhash_ctx_reference`] (via [`token_vote_hash`]), materialized instead of
/// cast.
#[inline]
fn collect_ctx_voters(
    chars: &[char],
    start: usize,
    end: usize,
    form_hash: u64,
    tok: &mut String,
    folded: &mut String,
) -> VoterSet {
    let mut v = VoterSet {
        hash: [0; 9],
        present: 1,
    };
    v.hash[0] = form_hash;
    let mut pos = end;
    let mut slot = 1usize;
    while slot <= 4 {
        let Some((s, e)) = token_right(chars, pos) else {
            break;
        };
        pos = e;
        if let Some(h) = token_vote_hash(&chars[s..e], tok, folded) {
            v.hash[slot] = h;
            v.present |= 1 << slot;
        }
        slot += 1;
    }
    let mut pos = start;
    let mut slot = 5usize;
    while slot <= 8 {
        let Some((s, e)) = token_left(chars, pos) else {
            break;
        };
        pos = s;
        if let Some(h) = token_vote_hash(&chars[s..e], tok, folded) {
            v.hash[slot] = h;
            v.present |= 1 << slot;
        }
        slot += 1;
    }
    v
}

/// Context fingerprint, bit-sliced kernel: the reference's walk and filter, majority by [`simhash_majority_bitsliced`].
#[inline]
pub fn simhash_ctx_bitsliced(
    chars: &[char],
    start: usize,
    end: usize,
    form_hash: u64,
    tok: &mut String,
    folded: &mut String,
) -> u64 {
    simhash_majority_bitsliced(&collect_ctx_voters(
        chars, start, end, form_hash, tok, folded,
    ))
}

/// §3.3 inverse-Gray decode, verbatim: `b_i = ⊕_{j ≥ i} g_j`.
#[inline]
#[must_use]
pub const fn gray_decode(mut g: u64) -> u64 {
    g ^= g >> 1;
    g ^= g >> 2;
    g ^= g >> 4;
    g ^= g >> 8;
    g ^= g >> 16;
    g ^= g >> 32;
    g
}

/// §3.3 field extractor, verbatim: the `n`-bit field of `gkey` at bit `lo`, inverse-Gray decoded.
#[inline]
#[must_use]
pub const fn field(gkey: u64, lo: u32, n: u32) -> u64 {
    gray_decode((gkey >> lo) & ((1u64 << n) - 1))
}

/// Gray encode, the inverse of [`gray_decode`]: `g = b ^ (b >> 1)`.
#[cfg(test)]
pub const fn gray_encode(b: u64) -> u64 {
    b ^ (b >> 1)
}

/// Cat age band (v4 bits 13–14, [`cat_age_v4`]); `scale()` is the body scale.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatAge {
    Kitten,
    Adolescent,
    Adult,
    Elder,
}

impl CatAge {
    /// §3.4 body scale: 0.82 / 0.93 / 1.04 / 1.15.
    #[inline]
    #[must_use]
    pub const fn scale(self) -> f32 {
        match self {
            CatAge::Kitten => 0.82,
            CatAge::Adolescent => 0.93,
            CatAge::Adult => 1.04,
            CatAge::Elder => 1.15,
        }
    }
}

const AGES: [CatAge; 4] = [
    CatAge::Kitten,
    CatAge::Adolescent,
    CatAge::Adult,
    CatAge::Elder,
];

/// Decoded nova features (§3.4 NOVA layout; a nova and a cat never share a class, so low-bit field reuse is safe).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct NovaFeatures {
    /// NOVA_PALETTES index (8 entries, Gray-indexed so neighbors are similar tints, §6.2).
    pub palette: u8,
    /// Ray count, `5..=8`.
    pub rays: u8,
    /// Ring radius, `1.6..=2.2` rows.
    pub radius: f32,
    /// Ring thickness, `1.5..=3.5` px.
    pub ring_thick: f32,
    /// Total nova window, `1000..=1400` ms (≤ 1.4 s, D3).
    pub duration_ms: u32,
    /// Debris mote count, `8..=20`.
    pub debris: u8,
    /// Chromatic-fringe offset, `1.0..=2.5` px.
    pub chroma: f32,
    /// Raw 4-bit ray rotation phase (pure phase; locality-irrelevant).
    pub rot: u8,
    /// Two Gray-decoded 4-bit hue-nudge codes (bits 19..26) for the ember/ink gradient anchors.
    pub ink_pair: (u8, u8),
}

/// §3.4 NOVA layout decode.
#[inline]
#[must_use]
pub const fn nova_features(g: u64) -> NovaFeatures {
    NovaFeatures {
        palette: field(g, 0, 3) as u8,
        rays: 5 + field(g, 3, 2) as u8,
        radius: 1.6 + field(g, 5, 2) as f32 * 0.2,
        ring_thick: 1.5 + field(g, 7, 2) as f32 * (2.0 / 3.0),
        duration_ms: 1000 + field(g, 9, 2) as u32 * 400 / 3,
        debris: 8 + field(g, 11, 2) as u8 * 4,
        chroma: 1.0 + field(g, 13, 2) as f32 * 0.5,
        rot: ((g >> 15) & 0xF) as u8,
        ink_pair: (field(g, 19, 4) as u8, field(g, 23, 4) as u8),
    }
}

/// Single-field nova decodes for per-frame sites that read one feature of an episode's fixed genome, instead of the full [`nova_features`] decode.
#[inline]
#[must_use]
pub const fn nova_duration_ms(g: u64) -> u32 {
    1000 + field(g, 9, 2) as u32 * 400 / 3
}

/// See [`nova_duration_ms`]: the `palette` row of [`nova_features`], alone.
#[inline]
#[must_use]
pub const fn nova_palette(g: u64) -> u8 {
    field(g, 0, 3) as u8
}

/// See [`nova_duration_ms`]: the `radius` row of [`nova_features`], alone.
#[inline]
#[must_use]
pub const fn nova_radius(g: u64) -> f32 {
    1.6 + field(g, 5, 2) as f32 * 0.2
}

/// The Gray-decoded ink-pair hue-nudge codes for a class.
#[inline]
#[must_use]
pub const fn ink_pair_nudges(class: Class, gkey: u64) -> (u8, u8) {
    let lo = if matches!(class, Class::Profanity) {
        19
    } else {
        26
    };
    (field(gkey, lo, 4) as u8, field(gkey, lo + 4, 4) as u8)
}

/// Rare cat variant (§3.5, low window of `magic`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CatMagic {
    /// `magic % 4096 < 8` — 1/512.
    Fortune,
    /// `magic % 4096 ∈ 8..12` — 1/1024.
    Nebula,
    /// `magic % 4096 ∈ 12..=54` — 43/4096 ≈ 1/95.
    Butterfly,
    /// `magic % 4096 ∈ 55..=62` — 8/4096 = 1/512: pale-pink coat, Solid, baked drifting petal specks.
    Sakura,
}

/// Rare nova variant (§3.5, shifted window of `magic`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NovaMagic {
    /// `(magic >> 12) % 4096 < 8` — 1/512.
    Quasar,
    /// `(magic >> 12) % 4096 ∈ 8..12` — 1/1024.
    Singularity,
}

/// §3.5 cat window.
#[inline]
#[must_use]
pub const fn cat_magic(magic: u64) -> Option<CatMagic> {
    match magic % 4096 {
        0..=7 => Some(CatMagic::Fortune),
        8..=11 => Some(CatMagic::Nebula),
        12..=54 => Some(CatMagic::Butterfly),
        55..=62 => Some(CatMagic::Sakura),
        _ => None,
    }
}

/// §3.5 nova window (shifted 12 bits so it is independent of the cat window).
#[inline]
#[must_use]
pub const fn nova_magic(magic: u64) -> Option<NovaMagic> {
    match (magic >> 12) % 4096 {
        0..=7 => Some(NovaMagic::Quasar),
        8..=11 => Some(NovaMagic::Singularity),
        _ => None,
    }
}

/// A rare accessory, worn only by ordinary ([`cat_magic`] `== None`) cats; the gate is structural in [`cat_accessory`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg(test)]
pub enum Accessory {
    /// `(magic >> 24) % 4096 ∈ 0..=127` — 1/32, the common-delight rung.
    Bow,
    /// `∈ 128..=143` — 1/256.
    Sunglasses,
    /// `∈ 144..=151` — 1/512.
    WitchHat,
    /// `∈ 152..=155` — 1/1024.
    Crown,
}

/// Accessory window `(magic >> 24) % 4096`, independent of the `% 4096` cat window and the `>> 12` nova window.
#[cfg(test)]
pub const fn cat_accessory(magic: u64) -> Option<Accessory> {
    if cat_magic(magic).is_some() {
        return None;
    }
    match (magic >> 24) % 4096 {
        0..=127 => Some(Accessory::Bow),
        128..=143 => Some(Accessory::Sunglasses),
        144..=151 => Some(Accessory::WitchHat),
        152..=155 => Some(Accessory::Crown),
        _ => None,
    }
}

/// Reduce the 30-bit head-spill field onto `[0, upper)` by multiply-high:
/// constant-time, allocation-free, discrepancy at most one ticket in 2^30.
#[inline]
const fn scale_head_spill(entropy: u64, upper: usize) -> usize {
    ((entropy as u128 * upper as u128) >> 30) as usize
}

/// The authored head this genome wears. The largest complete multiple of the
/// roster in the Gray-decoded 6-bit field gives every head an equal number of
/// primary tickets; a tail ticket takes one draw from Gray-decoded bits 34–63,
/// so coat, iris, age, motion and ink fields cannot change the head.
#[inline]
const fn cat_variant_index_v4(gkey: u64) -> usize {
    const ROSTER: usize = HEADS.len();
    const PRIMARY: usize = 64 - 64 % ROSTER;
    let ticket = field(gkey, 0, 6) as usize;
    if ticket < PRIMARY {
        ticket % ROSTER
    } else {
        scale_head_spill(field(gkey, 34, 30), ROSTER)
    }
}

/// v4 §3: resolve the bounded unbiased head index to its authored glyph.
#[inline]
#[must_use]
pub const fn cat_variant_v4(gkey: u64) -> CatGlyphId {
    HEADS[cat_variant_index_v4(gkey)]
}

/// The `(coat, iris)` ramp indices: `coat = field(6,4)` (0..=15, a `COAT_RAMP` stop) and `iris = field(10,3)` (0..=7, an `EYE_RAMP` stop), resolved by `ResolvedFills::from_context`.
#[inline]
#[must_use]
pub const fn cat_fills_v4(gkey: u64) -> (u8, u8) {
    (field(gkey, 6, 4) as u8, field(gkey, 10, 3) as u8)
}

/// v4 §3: the age band, `AGES[field(13,2)]`, whose `scale()` drives the body size.
#[inline]
#[must_use]
pub const fn cat_age_v4(gkey: u64) -> CatAge {
    AGES[field(gkey, 13, 2) as usize]
}

/// The special window (`magic % 4096`): a special glyph replaces the head (its `word_top` anchor drives the peek).
#[inline]
#[must_use]
pub const fn special_variant_v4(magic: u64) -> Option<CatGlyphId> {
    match magic % 4096 {
        0..=7 => Some(CatGlyphId::SpecManeki),
        8..=11 => Some(CatGlyphId::SpecWitch),
        12..=15 => Some(CatGlyphId::SpecSleeping),
        16..=19 => Some(CatGlyphId::SpecYarn),
        20..=23 => Some(CatGlyphId::SpecStretch),
        24..=27 => Some(CatGlyphId::SpecTuxedo),
        28..=31 => Some(CatGlyphId::SpecTabbybell),
        32..=35 => Some(CatGlyphId::SpecFluffy),
        _ => None,
    }
}

/// The overlay-accessory window (`(magic >> 24) % 4096`): Bow (left ear), Crown (dome crest) or Bell (collar) overlaid on the head.
#[inline]
#[must_use]
pub const fn accessory_variant_v4(magic: u64) -> Option<CatGlyphId> {
    if special_variant_v4(magic).is_some() {
        return None;
    }
    match (magic >> 24) % 4096 {
        0..=127 => Some(CatGlyphId::AccBow),
        128..=131 => Some(CatGlyphId::AccCrown),
        132..=147 => Some(CatGlyphId::AccBell),
        _ => None,
    }
}

#[cfg(test)]
#[path = "genome_tests.rs"]
mod tests;
