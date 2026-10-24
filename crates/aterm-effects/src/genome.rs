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

impl Default for VoteScratch {
    #[inline]
    fn default() -> Self {
        Self([0; 64])
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
    for w in CTX_WEIGHTS {
        let Some((s, e)) = token_right(chars, pos) else {
            break;
        };
        pos = e;
        vote_token(&chars[s..e], w, votes, tok, folded);
    }
    let mut pos = start;
    for w in CTX_WEIGHTS {
        let Some((s, e)) = token_left(chars, pos) else {
            break;
        };
        pos = s;
        vote_token(&chars[s..e], w, votes, tok, folded);
    }
    let mut fp = 0u64;
    for (i, v) in votes.iter().enumerate() {
        fp |= u64::from(*v >= 0) << i;
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
    if tok_chars.iter().all(char::is_ascii) {
        if tok_chars.len() < 2 {
            return None;
        }
        let mut h = aterm_hash::FNV64_OFFSET;
        for &c in tok_chars {
            if !c.is_ascii_alphabetic() {
                return None;
            }
            h = aterm_hash::fnv_mix_u64(h, u64::from(c.to_ascii_lowercase() as u8));
        }
        return Some(mix(h));
    }
    tok.clear();
    tok.extend(tok_chars.iter().copied());
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
#[derive(Clone, Copy, Default, Debug)]
pub struct VoterSet {
    pub hash: [u64; 9],
    pub present: u16,
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

    let mut w_total = 0i32;
    let mut i = 0;
    while i < SIMHASH_SLOT_WEIGHTS.len() {
        if v.present & (1 << i) != 0 {
            w_total += SIMHASH_SLOT_WEIGHTS[i];
        }
        i += 1;
    }
    let t = (w_total + 1) >> 1;
    let mut ge = !0u64;
    let planes = [s0, s1, s2, s3, s4];
    let mut k = 0;
    while k < 5 {
        let s_k = planes[k];
        let t_k = if (t >> k) & 1 == 1 { !0u64 } else { 0u64 };
        ge = (s_k & !t_k) | (!(s_k ^ t_k) & ge);
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
    for slot in 1..=4usize {
        let Some((s, e)) = token_right(chars, pos) else {
            break;
        };
        pos = e;
        if let Some(h) = token_vote_hash(&chars[s..e], tok, folded) {
            v.hash[slot] = h;
            v.present |= 1 << slot;
        }
    }
    let mut pos = start;
    for slot in 5..=8usize {
        let Some((s, e)) = token_left(chars, pos) else {
            break;
        };
        pos = s;
        if let Some(h) = token_vote_hash(&chars[s..e], tok, folded) {
            v.hash[slot] = h;
            v.present |= 1 << slot;
        }
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
pub const fn nova_duration_ms(g: u64) -> u32 {
    1000 + field(g, 9, 2) as u32 * 400 / 3
}

/// See [`nova_duration_ms`]: the `palette` row of [`nova_features`], alone.
#[inline]
pub const fn nova_palette(g: u64) -> u8 {
    field(g, 0, 3) as u8
}

/// See [`nova_duration_ms`]: the `radius` row of [`nova_features`], alone.
#[inline]
pub const fn nova_radius(g: u64) -> f32 {
    1.6 + field(g, 5, 2) as f32 * 0.2
}

/// The Gray-decoded ink-pair hue-nudge codes for a class.
#[inline]
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
    let roster = HEADS.len();
    let ticket = field(gkey, 0, 6) as usize;
    let primary = 64 - 64 % roster;
    if ticket < primary {
        ticket % roster
    } else {
        scale_head_spill(field(gkey, 34, 30), roster)
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
mod tests {
    use super::*;

    /// `(lo, n)` of every Gray-decoded field in the v4 cat layout: variant / coat
    /// / iris / age over bits 0..=14, plus the ink-pair hue-nudge window (bits
    /// 26..=33).
    const CAT_GRAY_FIELDS: &[(u32, u32)] = &[
        (0, 6),  // variant (index into HEADS)
        (6, 4),  // coat (COAT_RAMP index)
        (10, 3), // iris (EYE_RAMP index)
        (13, 2), // age
        (26, 4), // ink pair lo
        (30, 4), // ink pair hi
    ];

    /// `(lo, n)` of every Gray-decoded field in the NOVA layout (§3.4).
    const NOVA_GRAY_FIELDS: &[(u32, u32)] = &[
        (0, 3),
        (3, 2),
        (5, 2),
        (7, 2),
        (9, 2),
        (11, 2),
        (13, 2),
        (19, 4),
        (23, 4),
    ];

    fn all_fields() -> Vec<(u32, u32)> {
        let mut v = CAT_GRAY_FIELDS.to_vec();
        v.extend_from_slice(NOVA_GRAY_FIELDS);
        v
    }

    fn chars_of(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// Locate `word` in `chars` and return its `[start, end)` char span.
    fn span_of(chars: &[char], word: &str) -> (usize, usize) {
        let w: Vec<char> = word.chars().collect();
        for s in 0..=chars.len().saturating_sub(w.len()) {
            if chars[s..s + w.len()] == w[..] {
                return (s, s + w.len());
            }
        }
        panic!("word {word} not in text");
    }

    fn fp_of(text: &str, word: &str, form_hash: u64) -> u64 {
        let chars = chars_of(text);
        let (s, e) = span_of(&chars, word);
        let mut votes = VoteScratch::default();
        let (mut tok, mut folded) = (String::new(), String::new());
        simhash_ctx(&chars, s, e, form_hash, &mut votes, &mut tok, &mut folded)
    }

    /// Identical contexts fingerprint equal, one far-token edit moves few bits, and disjoint
    /// contexts differ by ~32.
    #[test]
    fn simhash_locality() {
        let form = mix(0xF0F0_1234);
        let a = fp_of("alpha beta gamma kitty delta epsilon zeta", "kitty", form);
        let b = fp_of("alpha beta gamma kitty delta epsilon zeta", "kitty", form);
        assert_eq!(a, b, "identical context must fingerprint equal");

        let c = fp_of("alpha beta gamma kitty delta epsilon yolk", "kitty", form);
        let near = (a ^ c).count_ones();
        assert!(
            near <= 20,
            "one far-token edit must move few bits, moved {near}"
        );

        let d = fp_of(
            "crimson harbor lantern kitty velvet orchid thunder",
            "kitty",
            mix(0x0BAD_CAFE),
        );
        let far = (a ^ d).count_ones();
        assert!(
            (16..=48).contains(&far),
            "disjoint contexts should differ by ~32 bits, got {far}"
        );
        assert!(
            near < far,
            "near edit ({near}) must beat disjoint rewrite ({far})"
        );
    }

    /// §3.2 filter: digits, punctuation, and single letters are silent voters —
    /// a ticking clock next door cannot perturb the fingerprint.
    #[test]
    fn simhash_clock_and_spinner_immunity() {
        let form = mix(0xFEED_BEEF);
        let a = fp_of("build ok 12:04:33 kitty | spinner", "kitty", form);
        let b = fp_of("build ok 23:59:59 kitty / spinner", "kitty", form);
        assert_eq!(a, b, "digit/punct churn must not move a single bit");
    }

    /// Flipping bit 0 of a field moves it exactly ±1 step, flipping bit k a bounded mean
    /// distance, and no flip leaves its field.
    #[test]
    fn gray_decode_locality() {
        let fields = all_fields();
        let gkeys: Vec<u64> = (0..512u64)
            .map(|i| mix(i ^ 0x6A09_E667_F3BC_C908))
            .collect();

        for &(lo, n) in &fields {
            for &g in &gkeys {
                let before = field(g, lo, n) as i64;
                let after = field(g ^ (1 << lo), lo, n) as i64;
                assert_eq!(
                    (before - after).abs(),
                    1,
                    "bit-0 flip at lo={lo} n={n} must move exactly 1 step"
                );
            }
            let mut total = 0i64;
            let mut count = 0i64;
            for &g in &gkeys {
                for k in 0..n {
                    let before = field(g, lo, n) as i64;
                    let after = field(g ^ (1 << (lo + k)), lo, n) as i64;
                    total += (before - after).abs();
                    count += 1;
                }
            }
            let mean = total as f64 / count as f64;
            let bound = (f64::from(2u32.pow(n)) - 1.0) / f64::from(n) * 1.2;
            assert!(
                mean <= bound,
                "mean |Δstep| {mean:.3} exceeds pinned bound {bound:.3} at lo={lo} n={n}"
            );
            for &g in &gkeys[..64] {
                for k in 0..n {
                    let flipped = g ^ (1 << (lo + k));
                    for &(olo, on) in &fields {
                        if olo == lo && on == n {
                            continue;
                        }
                        let overlap = lo + k >= olo && lo + k < olo + on;
                        if overlap {
                            continue;
                        }
                        assert_eq!(
                            field(g, olo, on),
                            field(flipped, olo, on),
                            "flip at bit {} leaked into field lo={olo} n={on}",
                            lo + k
                        );
                    }
                }
            }
        }
    }

    /// 1e6 synthetic contexts land each §3.5 magic window (cat
    /// Fortune/Nebula/Butterfly/Sakura, nova Quasar/Singularity) within 3σ of
    /// its rate, and every value of every Gray field is reachable.
    #[test]
    fn magic_windows_hit_expected_rates() {
        const N: u64 = 1_000_000;
        let (mut fortune, mut nebula, mut quasar, mut singularity) = (0u32, 0u32, 0u32, 0u32);
        let (mut butterfly, mut sakura) = (0u32, 0u32);
        let fields = all_fields();
        let mut seen: Vec<Vec<bool>> = fields.iter().map(|&(_, n)| vec![false; 1 << n]).collect();
        let mut rot_seen = [false; 16];

        for i in 0..N {
            let ctx_fp = mix(i ^ 0x243F_6A88_85A3_08D3);
            let form = mix(i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x1319_8A2E_0370_7344);
            let seed = mix(i ^ 0xA409_3822_299F_31D0);
            let magic = mix(ctx_fp ^ form ^ MAGIC_SALT);
            match cat_magic(magic) {
                Some(CatMagic::Fortune) => fortune += 1,
                Some(CatMagic::Nebula) => nebula += 1,
                Some(CatMagic::Butterfly) => butterfly += 1,
                Some(CatMagic::Sakura) => sakura += 1,
                None => {}
            }
            match nova_magic(magic) {
                Some(NovaMagic::Quasar) => quasar += 1,
                Some(NovaMagic::Singularity) => singularity += 1,
                None => {}
            }
            let gkey = seed ^ ctx_fp;
            for (fi, &(lo, n)) in fields.iter().enumerate() {
                seen[fi][field(gkey, lo, n) as usize] = true;
            }
            rot_seen[((gkey >> 15) & 0xF) as usize] = true;
        }

        let check = |name: &str, count: u32, p: f64| {
            let mu = N as f64 * p;
            let sigma = (N as f64 * p * (1.0 - p)).sqrt();
            let delta = (f64::from(count) - mu).abs();
            assert!(
                delta <= 3.0 * sigma,
                "{name}: {count} vs μ={mu:.1} (|Δ|={delta:.1} > 3σ={:.1})",
                3.0 * sigma
            );
        };
        check("fortune", fortune, 8.0 / 4096.0);
        check("nebula", nebula, 4.0 / 4096.0);
        check("quasar", quasar, 8.0 / 4096.0);
        check("singularity", singularity, 4.0 / 4096.0);
        check("butterfly", butterfly, 43.0 / 4096.0);
        check("sakura", sakura, 8.0 / 4096.0);

        for (fi, &(lo, n)) in fields.iter().enumerate() {
            for (v, hit) in seen[fi].iter().enumerate() {
                assert!(*hit, "field lo={lo} n={n} never decoded value {v}");
            }
        }
        assert!(rot_seen.iter().all(|&b| b), "nova rot raw field unreached");
    }

    /// The v4 special/accessory windows land within 3σ of their rates
    /// (accessories conditioned on `special_variant_v4 == None`), every head
    /// within 5σ of 1/HEADS, and every v4 low-field value is reachable.
    #[test]
    fn v4_magic_windows_hit_expected_rates() {
        use crate::cat_glyphs_gen::HEADS;
        const N: u64 = 1_000_000;
        let mut maneki = 0u32;
        let (mut witch, mut sleeping, mut yarn) = (0u32, 0u32, 0u32);
        let (mut stretch, mut tuxedo, mut tabbybell, mut fluffy) = (0u32, 0u32, 0u32, 0u32);
        let mut plain = 0u64;
        let (mut bow, mut crown, mut bell) = (0u32, 0u32, 0u32);
        let mut variant_counts = vec![0u32; HEADS.len()];
        let mut coat_seen = [false; 16];
        let mut iris_seen = [false; 8];
        let mut age_seen = [false; 4];

        for i in 0..N {
            let ctx_fp = mix(i ^ 0x243F_6A88_85A3_08D3);
            let form = mix(i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x1319_8A2E_0370_7344);
            let seed = mix(i ^ 0xA409_3822_299F_31D0);
            let magic = mix(ctx_fp ^ form ^ MAGIC_SALT);
            match special_variant_v4(magic) {
                Some(CatGlyphId::SpecManeki) => maneki += 1,
                Some(CatGlyphId::SpecWitch) => witch += 1,
                Some(CatGlyphId::SpecSleeping) => sleeping += 1,
                Some(CatGlyphId::SpecYarn) => yarn += 1,
                Some(CatGlyphId::SpecStretch) => stretch += 1,
                Some(CatGlyphId::SpecTuxedo) => tuxedo += 1,
                Some(CatGlyphId::SpecTabbybell) => tabbybell += 1,
                Some(CatGlyphId::SpecFluffy) => fluffy += 1,
                Some(_) => unreachable!("special_variant_v4 only yields specials"),
                None => {
                    plain += 1;
                    match accessory_variant_v4(magic) {
                        Some(CatGlyphId::AccBow) => bow += 1,
                        Some(CatGlyphId::AccCrown) => crown += 1,
                        Some(CatGlyphId::AccBell) => bell += 1,
                        Some(_) => unreachable!("accessory_variant_v4 only yields Acc*"),
                        None => {}
                    }
                }
            }
            let gkey = seed ^ ctx_fp;
            variant_counts[HEADS
                .iter()
                .position(|&h| h == cat_variant_v4(gkey))
                .unwrap()] += 1;
            let (coat, iris) = cat_fills_v4(gkey);
            coat_seen[usize::from(coat)] = true;
            iris_seen[usize::from(iris)] = true;
            age_seen[AGES.iter().position(|&a| a == cat_age_v4(gkey)).unwrap()] = true;
        }

        let check = |name: &str, count: u32, p: f64| {
            let mu = N as f64 * p;
            let sigma = (N as f64 * p * (1.0 - p)).sqrt();
            let delta = (f64::from(count) - mu).abs();
            assert!(
                delta <= 3.0 * sigma,
                "{name}: {count} vs μ={mu:.1} (|Δ|={delta:.1} > 3σ={:.1})",
                3.0 * sigma
            );
        };
        check("maneki", maneki, 8.0 / 4096.0);
        for (name, c) in [
            ("witch", witch),
            ("sleeping", sleeping),
            ("yarn", yarn),
            ("stretch", stretch),
            ("tuxedo", tuxedo),
            ("tabbybell", tabbybell),
            ("fluffy", fluffy),
        ] {
            check(name, c, 4.0 / 4096.0);
        }
        let check_cond = |name: &str, count: u32, p: f64| {
            let n = plain as f64;
            let mu = n * p;
            let sigma = (n * p * (1.0 - p)).sqrt();
            let delta = (f64::from(count) - mu).abs();
            assert!(
                delta <= 3.0 * sigma,
                "{name}: {count} vs μ={mu:.1} over n={n} plain (|Δ|={delta:.1} > 3σ={:.1})",
                3.0 * sigma
            );
        };
        check_cond("bow", bow, 128.0 / 4096.0);
        check_cond("crown", crown, 4.0 / 4096.0);
        check_cond("bell", bell, 16.0 / 4096.0);

        let head_p = 1.0 / HEADS.len() as f64;
        let head_mu = N as f64 * head_p;
        let head_sigma = (N as f64 * head_p * (1.0 - head_p)).sqrt();
        for (index, &count) in variant_counts.iter().enumerate() {
            let delta = (f64::from(count) - head_mu).abs();
            assert!(
                delta <= 5.0 * head_sigma,
                "HEADS[{index}]={count} vs μ={head_mu:.1} (|Δ|={delta:.1} > 5σ={:.1})",
                5.0 * head_sigma,
            );
        }
        assert!(
            variant_counts.iter().all(|&count| count > 0),
            "every HEAD reachable"
        );
        assert!(coat_seen.iter().all(|&b| b), "every coat index reachable");
        assert!(iris_seen.iter().all(|&b| b), "every iris index reachable");
        assert!(age_seen.iter().all(|&b| b), "every age band reachable");
    }

    #[test]
    fn v4_head_primary_tickets_are_equal_and_spill_is_bounded() {
        use crate::cat_glyphs_gen::HEADS;

        let primary = 64 - 64 % HEADS.len();
        let mut primary_counts = vec![0usize; HEADS.len()];
        let mut spill = 0usize;
        for raw in 0..64u64 {
            let ticket = field(raw, 0, 6) as usize;
            let index = cat_variant_index_v4(raw);
            assert!(index < HEADS.len());
            if ticket < primary {
                assert_eq!(
                    index,
                    ticket % HEADS.len(),
                    "accepted primary tickets preserve the old deterministic mapping"
                );
                primary_counts[index] += 1;
            } else {
                spill += 1;
            }
            assert_eq!(
                cat_variant_index_v4(raw),
                index,
                "the bounded selector is deterministic"
            );
        }
        assert_eq!(spill, 64 % HEADS.len());
        assert!(
            primary_counts
                .iter()
                .all(|&count| count == primary / HEADS.len()),
            "every head owns exactly two primary tickets"
        );
    }

    #[test]
    fn v4_head_spill_is_independent_of_coat_iris_age_motion_and_ink() {
        for raw_variant in 0..64u64 {
            if field(raw_variant, 0, 6) < 50 {
                continue;
            }
            for spill in [0, 1, (1 << 15) - 1, (1 << 30) - 1] {
                let base = raw_variant | (spill << 34);
                let expected = cat_variant_v4(base);
                for non_head in [
                    1u64 << 6,
                    1u64 << 10,
                    1u64 << 13,
                    1u64 << 15,
                    1u64 << 17,
                    1u64 << 19,
                    1u64 << 26,
                    (1u64 << 34) - (1u64 << 6),
                ] {
                    assert_eq!(
                        cat_variant_v4(base ^ non_head),
                        expected,
                        "non-head mask {non_head:#018x} changed a tail-ticket head"
                    );
                }
            }
        }
    }

    /// The v4 selectors and the nova decoder honour their ranges at the field
    /// extremes: every head, coat, iris and age band, and the nova ranges.
    #[test]
    fn feature_decoders_cover_documented_ranges() {
        use crate::cat_glyphs_gen::HEADS;
        let mut variants = vec![false; HEADS.len()];
        let mut coats = [false; 16];
        let mut irises = [false; 8];
        let mut ages = [false; 4];
        let mut rays = (u8::MAX, 0u8);
        for i in 0..8192u64 {
            let g = mix(i);
            variants[HEADS.iter().position(|&h| h == cat_variant_v4(g)).unwrap()] = true;
            let (coat, iris) = cat_fills_v4(g);
            coats[usize::from(coat)] = true;
            irises[usize::from(iris)] = true;
            ages[AGES.iter().position(|&a| a == cat_age_v4(g)).unwrap()] = true;
            let nf = nova_features(g);
            rays = (rays.0.min(nf.rays), rays.1.max(nf.rays));
            assert!(nf.palette <= 7);
            assert!((1.599..=2.201).contains(&nf.radius));
            assert!((1.499..=3.501).contains(&nf.ring_thick));
            assert!((1000..=1400).contains(&nf.duration_ms));
            assert!((8..=20).contains(&nf.debris));
            assert!((0.999..=2.501).contains(&nf.chroma));
            assert!(nf.rot <= 15);
        }
        assert!(variants.iter().all(|&b| b), "every HEAD variant reachable");
        assert!(
            coats.iter().all(|&b| b),
            "all 16 COAT_RAMP indices reachable"
        );
        assert!(irises.iter().all(|&b| b), "all 8 iris stops reachable");
        assert!(ages.iter().all(|&b| b), "all 4 age bands reachable");
        assert_eq!(rays, (5, 8), "rays span 5..=8");
    }

    /// The single-field nova accessors equal the matching [`nova_features`] rows bit-for-bit.
    #[test]
    fn nova_single_field_accessors_match_full_decode() {
        for i in 0..8192u64 {
            let g = mix(i);
            let nf = nova_features(g);
            assert_eq!(nova_duration_ms(g), nf.duration_ms);
            assert_eq!(nova_palette(g), nf.palette);
            assert_eq!(nova_radius(g).to_bits(), nf.radius.to_bits());
        }
    }

    /// Deterministic voter-set stream for the §3.2 kernel-equivalence battery.
    fn voter_set(i: u64, present: u16) -> VoterSet {
        let mut v = VoterSet {
            hash: [0; 9],
            present,
        };
        for (s, h) in v.hash.iter_mut().enumerate() {
            *h = mix(i.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (s as u64) << 56
                ^ 0x5851_F42D_4C95_7F2D);
        }
        v
    }

    /// The bit-sliced kernel equals the reference over 10k random voter sets plus edge cases:
    /// 0/1/9 voters, all-equal hashes, and ties (where a threshold off-by-one would hide).
    #[test]
    fn simhash_bitsliced_equals_reference_over_voter_sets() {
        let eq = |v: &VoterSet, label: &str| {
            assert_eq!(
                simhash_majority_bitsliced(v),
                simhash_majority_reference(v),
                "kernel divergence ({label}): present={:#011b} hashes={:x?}",
                v.present,
                v.hash
            );
        };
        for i in 0..10_000u64 {
            let present = (mix(i ^ 0xC2B2_AE3D_27D4_EB4F) & 0x1FF) as u16;
            eq(&voter_set(i, present), "random");
        }
        eq(&voter_set(1, 0), "0 voters");
        assert_eq!(simhash_majority_bitsliced(&voter_set(1, 0)), u64::MAX);
        for s in 0..9u16 {
            let v = voter_set(2 + u64::from(s), 1 << s);
            eq(&v, "1 voter");
            assert_eq!(simhash_majority_bitsliced(&v), v.hash[usize::from(s)]);
        }
        for i in 0..64u64 {
            eq(&voter_set(0x1000 + i, 0x1FF), "9 voters");
            let mut v = voter_set(0, 0x1FF);
            v.hash = [mix(0x000A_11E0 ^ i); 9];
            eq(&v, "all-equal hashes");
            assert_eq!(simhash_majority_bitsliced(&v), v.hash[0]);
        }
        for (a, b) in [(2usize, 3usize), (2, 6), (3, 7), (6, 7), (1, 5), (4, 8)] {
            for i in 0..64u64 {
                let mut v = VoterSet {
                    hash: [0; 9],
                    present: (1 << a) | (1 << b),
                };
                let p = mix(0x71E ^ i);
                v.hash[a] = p;
                v.hash[b] = !p;
                eq(&v, "complementary tie pair");
                assert_eq!(
                    simhash_majority_bitsliced(&v),
                    u64::MAX,
                    "ties must resolve to 1"
                );
            }
        }
        for i in 0..256u64 {
            let p = mix(0x4B1D ^ i);
            let mut v = VoterSet {
                hash: [0; 9],
                present: 0b0_0001_0011,
            };
            v.hash[0] = p;
            v.hash[4] = p;
            v.hash[1] = !p;
            eq(&voter_set(i, 0b1_0001_0011), "random 4-voter");
            eq(&v, "weighted tie (3+1 vs 4)");
            assert_eq!(
                simhash_majority_bitsliced(&v),
                u64::MAX,
                "2S == W must yield 1"
            );
        }
    }

    /// End to end the kernels agree over real text, including bare words,
    /// clock/spinner-silenced contexts and joiner tokens.
    #[test]
    fn simhash_ctx_bitsliced_matches_reference_kernel_on_text() {
        let texts: &[(&str, &str)] = &[
            ("alpha beta gamma kitty delta epsilon zeta", "kitty"),
            ("kitty", "kitty"),
            ("build ok 12:04:33 kitty | spinner", "kitty"),
            ("the cat's whiskers touch cat-like fur near cat here", "cat"),
            ("a kitty b", "kitty"),
        ];
        for &(text, word) in texts {
            let chars = chars_of(text);
            let (s, e) = span_of(&chars, word);
            for f in 0..32u64 {
                let form = mix(0xF0_F0 ^ f);
                let mut votes = VoteScratch::default();
                let (mut tok, mut folded) = (String::new(), String::new());
                let reference =
                    simhash_ctx_reference(&chars, s, e, form, &mut votes, &mut tok, &mut folded);
                let bitsliced = simhash_ctx_bitsliced(&chars, s, e, form, &mut tok, &mut folded);
                assert_eq!(reference, bitsliced, "kernels diverged on {text:?}");
            }
        }
    }
}
