// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sparkle-words v2 **genome** (docs/sparkle-words-v2-design.md §3): the
//! deterministic (word form, surrounding words) → effect-parameters channel.
//!
//! * **`simhash_ctx`** (§3.2) — a weighted SimHash over the ±4 reading-order
//!   neighbor tokens plus the matched word's own `form_hash`. Locality by
//!   construction: editing one nearby word flips only the bits where that
//!   voter was pivotal, so similar sentences yield similar genomes (a plain
//!   avalanche hash fails this). Allocation-free: the caller passes resident
//!   scratch ([`VoteScratch`] + two `String`s); it runs **only on a persist-map
//!   miss** (word_decorations.rs §3.6), never on the per-frame path.
//! * **`gray_decode` / `field`** (§3.3, verbatim) — features are disjoint bit
//!   fields of `gkey = seed ^ ctx_fp`, each decoded through inverse Gray, so a
//!   context edit never carries across a field boundary and a bit-0 flip moves
//!   a feature exactly ±1 table step (bit k reflects within its 2^(k+1) block —
//!   the honest §3.3 property, pinned by `gray_decode_locality`).
//! * **`cat_features` / `nova_features`** (§3.4, authoritative bit layout) —
//!   decoded on demand, never stored. The table/ramp *contents* (COAT_RAMP,
//!   EYE_RAMP, NOVA_PALETTES) are P3/P4 art; the decoders return indices and
//!   scalar parameters.
//! * **`cat_magic` / `nova_magic`** (§3.5) — the rare-variant channel:
//!   `magic = mix(ctx_fp ^ form_hash ^ SALT)` folds in NO ident/column/row, so
//!   the same sentence yields the same magic outcome at any indentation.

use aterm_lexicon::{Class, fold_into, is_interior_joiner, is_token_char};

use crate::cat_glyphs_gen::{CatGlyphId, HEADS};

/// §3.3: the frozen 16-byte genome, computed once on a persist-map miss and
/// copied forward on every hit (word_decorations.rs). `gkey` folds in the
/// occurrence seed (position-bearing, §15.2); `magic` deliberately does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Genome {
    /// `seed ^ ctx_fp` — the feature-field source (§3.4 layouts).
    pub gkey: u64,
    /// `mix(ctx_fp ^ form_hash ^ MAGIC_SALT)` — the position-independent
    /// rare-variant stream (§3.5).
    pub magic: u64,
}

/// §3.5 magic-channel salt (`0x5EED_CA71_5EED_CA71` — "seed cat, seed cat").
pub const MAGIC_SALT: u64 = 0x5EED_CA71_5EED_CA71;

/// splitmix64 finalizer — decorrelates adjacent seeds and never returns 0.
pub fn mix(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let r = z ^ (z >> 31);
    if r == 0 { 0xD1B5_4A32_D192_ED03 } else { r }
}

// ─────────────────────────── §3.2 weighted SimHash ───────────────────────────

/// Distance weights `w(d) = [4, 2, 2, 1][d - 1]` for the d-th token on each side.
const CTX_WEIGHTS: [i32; 4] = [4, 2, 2, 1];
/// The matched word's own `form_hash` vote weight (§3.2: `cat` and `kitten` in
/// identical surroundings are siblings, not twins; a bare word with no
/// neighbors still gets a locality-bearing vote).
const FORM_WEIGHT: i32 = 3;

/// §3.2 vote accumulator, newtyped so the owning struct keeps
/// `#[derive(Default)]` (`[i32; 64]` itself carries no `Default` impl).
/// Resident scratch on `WordDecorations` — zero allocation, ever.
pub struct VoteScratch(pub [i32; 64]);

impl Default for VoteScratch {
    fn default() -> Self {
        Self([0; 64])
    }
}

/// Which §3.2 kernel computes `ctx_fp` (the §7.5 gating rule, restated at the
/// switch): the bit-sliced kernel ([`simhash_ctx_bitsliced`]) is SHIPPED,
/// licensed by its green equivalence certificate — the ay QF_BV single-column
/// miter + lane-independence lift
/// (`crates/aterm-spec-models/proofs/ay/sparkle_v2/`, re-checked fail-closed
/// by `aterm-spec/tests/sparkle_v2_ay_certificates.rs`) plus the in-tree
/// 10k-voter-set property battery below (measured 13.4× on the persist-miss
/// path; see PROOF_CARRYING_PERFORMANCE.md §7.5). The REFERENCE vote loop
/// stays the oracle the battery compares against. Refuse, don't silently
/// pass: if that gate ever goes red, flip this const back to `Reference` in
/// the same change that reds it (§15.15).
pub const SIMHASH_KERNEL: SimhashKernel = SimhashKernel::BitSliced;

/// §3.2 kernel selector — see [`SIMHASH_KERNEL`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SimhashKernel {
    /// The §3.2 vote loop (`votes: [i32; 64]`) — the oracle. Selected only if
    /// [`SIMHASH_KERNEL`] is flipped back (certificate-red fallback).
    #[allow(
        dead_code,
        reason = "the equivalence battery's oracle; constructed only when \
                  SIMHASH_KERNEL is flipped back to Reference on a red \
                  §7.5 certificate (sparkle-v2 design §3.2/§15.15)"
    )]
    Reference,
    /// The v2.1 carry-save-adder network (§3.2 bit-sliced note) — shipped
    /// under its green §7.5 equivalence certificate.
    BitSliced,
}

/// §3.2 context fingerprint — the live entry point. Dispatches on
/// [`SIMHASH_KERNEL`]; the reference kernel is the default (§7.5 gating rule).
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

/// §3.2 context fingerprint, REFERENCE kernel: weighted SimHash over up to 4
/// tokens on each side of the match span `[start, end)` (char indices into
/// `chars`, one physical row — the walk never crosses a row by construction)
/// plus the match's own `form_hash` at weight 3. `tok`/`folded` are resident
/// caller scratch, so the walk allocates nothing after warmup. Runs only on a
/// persist-map miss.
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
    // Right side: d = 1..=4 counts COLLECTED tokens; the alphabetic/length
    // filter then decides which of them actually vote (§3.2 order of clauses).
    let mut pos = end;
    for w in CTX_WEIGHTS {
        let Some((s, e)) = token_right(chars, pos) else {
            break;
        };
        pos = e;
        vote_token(&chars[s..e], w, votes, tok, folded);
    }
    // Left side, mirrored.
    let mut pos = start;
    for w in CTX_WEIGHTS {
        let Some((s, e)) = token_left(chars, pos) else {
            break;
        };
        pos = s;
        vote_token(&chars[s..e], w, votes, tok, folded);
    }
    // Majority sign per bit; ties resolve to 1, deterministically (§3.2).
    let mut fp = 0u64;
    for (i, v) in votes.iter().enumerate() {
        fp |= u64::from(*v >= 0) << i;
    }
    fp
}

/// Accumulate one voter's 64 bit votes at weight `w`.
fn cast_vote(votes: &mut [i32; 64], h: u64, w: i32) {
    for (i, v) in votes.iter_mut().enumerate() {
        if (h >> i) & 1 == 1 {
            *v += w;
        } else {
            *v -= w;
        }
    }
}

/// Fold one candidate token, apply the §3.2 filter (votes only if alphabetic
/// and ≥ 2 folded chars — digits, punctuation, and single letters are silent,
/// so ticking clocks and spinners contribute nothing even at birth), then cast
/// `h_t = mix(fnv(folded chars))` at weight `w`.
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

/// The fold + §3.2 filter + FNV/mix shared by BOTH kernels: `Some(h_t)` when
/// the token votes, `None` when it is silent (non-alphabetic or < 2 folded
/// chars). One implementation, so the kernels cannot drift on the filter.
fn token_vote_hash(tok_chars: &[char], tok: &mut String, folded: &mut String) -> Option<u64> {
    tok.clear();
    tok.extend(tok_chars.iter().copied());
    // The scanner's own fold (aterm-lexicon), buffer-reusing — no drift, no alloc.
    fold_into(tok, folded);
    let mut len = 0usize;
    for c in folded.chars() {
        if !c.is_alphabetic() {
            return None;
        }
        len += 1;
    }
    if len < 2 {
        return None;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in folded.chars() {
        h ^= c as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    Some(mix(h))
}

/// Next token to the right of char index `pos`: skip non-token chars, then take
/// the maximal token run using the scanner's own predicate + interior-joiner
/// rule (`cat's` / `cat-like` are single tokens — aterm-lexicon `token_end`
/// semantics). Returns `[s, e)` or `None` past end-of-row.
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
            j += 1; // consume the joiner, keep scanning (scanner parity)
        } else {
            break;
        }
    }
    Some((s, j))
}

/// Next token to the left of char index `pos` (mirror of [`token_right`]).
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
            s -= 1; // interior joiner between two token chars
        } else {
            break;
        }
    }
    Some((s, e))
}

// ──────────── §3.2 bit-sliced kernel (v2.1, certificate-gated) ────────────
//
// The reference vote loop re-expressed as a carry-save-adder network over u64
// lanes: lane i of every word is bit i of the fingerprint, so all 64 majority
// columns are computed at once. The weights {4, 3, 2, 1} are binary-decomposed
// into three bit-planes (§3.2: ≤ 3 / ≤ 5 / ≤ 2 inputs feeding a 5-bit-per-lane
// CSA), and the signed majority `vote[i] >= 0` is taken in its equivalent
// unsigned threshold form `2·S ≥ W` (S = per-lane sum of PRESENT set-bit
// weights, W = present weight total — a scalar function of the presence mask;
// ties → 1, matching the reference exactly).
//
// LANE-INDEPENDENCE SIDE CONDITION (the §7.5 lift obligation, checked by
// review + the equivalence battery): every op on voter data below is LANE-WISE
// bitwise (XOR/AND/OR/NOT) — no u64 add/sub/shift ever touches voter words, so
// no carry can cross lanes and the single-column certificate
// (`proofs/ay/sparkle_v2/simhash_column_lemma.smt2`) lifts to all 64 lanes.
// (`W`/`T` are scalar controls derived from the presence mask only; the
// threshold bits enter the lanes as broadcast constant masks.)

/// The §3.2 voter slots in fixed order: `[form(3), right d1..4 (4,2,2,1),
/// left d1..4 (4,2,2,1)]`.
pub const SIMHASH_SLOT_WEIGHTS: [i32; 9] = [FORM_WEIGHT, 4, 2, 2, 1, 4, 2, 2, 1];

/// A fully-collected §3.2 voter set: slot `i`'s token hash + a presence mask
/// (bit `i` set ⇒ slot `i` votes at `SIMHASH_SLOT_WEIGHTS[i]`). Absent slots
/// (no token at that distance, or a filter-silenced token) do not vote.
#[derive(Clone, Copy, Default, Debug)]
pub struct VoterSet {
    pub hash: [u64; 9],
    pub present: u16,
}

/// Reference majority over a collected [`VoterSet`] — byte-for-byte the
/// `cast_vote` + signed-majority fold of [`simhash_ctx_reference`], factored
/// so the equivalence battery and `bench_simhash_bitsliced` compare the two
/// kernels on identical inputs.
#[allow(
    dead_code,
    reason = "the §7.5 equivalence battery's oracle (tests + bench_simhash_bitsliced); \
              the live reference path stays simhash_ctx_reference"
)]
pub fn simhash_majority_reference(v: &VoterSet) -> u64 {
    let mut votes = [0i32; 64];
    for (i, &w) in SIMHASH_SLOT_WEIGHTS.iter().enumerate() {
        if v.present & (1 << i) != 0 {
            cast_vote(&mut votes, v.hash[i], w);
        }
    }
    let mut fp = 0u64;
    for (i, vv) in votes.iter().enumerate() {
        fp |= u64::from(*vv >= 0) << i;
    }
    fp
}

/// One lane-wise full adder: `(sum, carry)` — all ops bitwise, carry-free
/// across lanes (the lift side condition).
#[inline]
fn csa(a: u64, b: u64, c: u64) -> (u64, u64) {
    (a ^ b ^ c, (a & b) | (c & (a ^ b)))
}

/// §3.2 bit-sliced majority: the carry-save adder network. Pure function of
/// the voter set; equals [`simhash_majority_reference`] on every input (the
/// §7.5 certificate + the 10k-set property battery below).
pub fn simhash_majority_bitsliced(v: &VoterSet) -> u64 {
    // Masked lanes: an absent voter contributes no set bits (and, below, no
    // weight to W) — exactly the reference's "does not vote".
    let h = |i: usize| {
        if v.present & (1 << i) != 0 {
            v.hash[i]
        } else {
            0
        }
    };

    // Weight bit-planes ({4,3,2,1} binary-decomposed; slot order as in
    // SIMHASH_SLOT_WEIGHTS):
    //   plane 1 (weight bit 0): form(3), right d4(1), left d4(1)   — 3 inputs
    //   plane 2 (weight bit 1): form(3), right/left d2(2) d3(2)    — 5 inputs
    //   plane 4 (weight bit 2): right d1(4), left d1(4)            — 2 inputs
    let (p1_s, p1_c) = csa(h(0), h(4), h(8));
    let (p2_a, p2_ca) = csa(h(0), h(2), h(3));
    let (p2_s, p2_cb) = csa(p2_a, h(6), h(7));
    let (p2_t1, p2_t2) = (p2_ca ^ p2_cb, p2_ca & p2_cb); // carries: 2·(ca+cb)
    let (p4_s, p4_c) = (h(1) ^ h(5), h(1) & h(5));

    // S = p1 + 2·p2 + 4·p4 as five bit-sliced digits (S ≤ 3 + 2·5 + 4·2 = 21):
    //   value 1: p1_s          value 2: p1_c, p2_s
    //   value 4: p2_t1, p4_s   value 8: p2_t2, p4_c
    let s0 = p1_s;
    let (s1, k4) = (p1_c ^ p2_s, p1_c & p2_s);
    let (s2, k8) = csa(p2_t1, p4_s, k4);
    let (s3, k16) = csa(p2_t2, p4_c, k8);
    let s4 = k16;

    // Signed majority as the unsigned threshold 2·S ≥ W ⇔ S ≥ ⌈W/2⌉ = T
    // (ties → 1 exactly like the reference: W even, S = W/2 passes).
    let mut w_total = 0i32;
    for (i, &w) in SIMHASH_SLOT_WEIGHTS.iter().enumerate() {
        if v.present & (1 << i) != 0 {
            w_total += w;
        }
    }
    let t = (w_total + 1) >> 1; // 0..=11
    // Lane-wise "S ≥ T" comparator, LSB→MSB: ge = (s_k ∧ ¬t_k) ∨ (¬(s_k ⊕ t_k) ∧ ge).
    // The threshold bits are broadcast constant masks — no cross-lane carries.
    let mut ge = !0u64;
    for (k, s_k) in [s0, s1, s2, s3, s4].into_iter().enumerate() {
        let t_k = if (t >> k) & 1 == 1 { !0u64 } else { 0u64 };
        ge = (s_k & !t_k) | (!(s_k ^ t_k) & ge);
    }
    ge
}

/// Collect the §3.2 voter set for a match span — the same token walk, filter,
/// and hash as [`simhash_ctx_reference`] (shared via [`token_vote_hash`]),
/// materialized instead of cast.
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
        present: 1, // slot 0: the match's own form_hash always votes
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

/// §3.2 context fingerprint, BIT-SLICED kernel (v2.1): the same walk + filter
/// as the reference, majority taken by [`simhash_majority_bitsliced`]. NOT the
/// live default — see [`SIMHASH_KERNEL`] and the §7.5 gating rule.
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

// ───────────────────────────── §3.3 Gray decode ──────────────────────────────

/// §3.3 inverse-Gray decode, verbatim: `b_i = ⊕_{j ≥ i} g_j`.
pub fn gray_decode(mut g: u64) -> u64 {
    let mut b = g;
    while g > 0 {
        g >>= 1;
        b ^= g;
    }
    b
}

/// §3.3 field extractor, verbatim: the `n`-bit field of `gkey` at bit `lo`,
/// inverse-Gray decoded. No flip ever crosses a field boundary.
pub fn field(gkey: u64, lo: u32, n: u32) -> u64 {
    gray_decode((gkey >> lo) & ((1u64 << n) - 1))
}

/// Gray ENCODE (the exact inverse of [`gray_decode`]): `g = b ^ (b >> 1)`.
/// v2.2 uses it to write the POST-§5.2-forcing paw count back into a bake art
/// key, so forced and rolled identities that DRAW the same share a tile.
pub fn gray_encode(b: u64) -> u64 {
    b ^ (b >> 1)
}

// ───────────────────── §3.4 feature tables (authoritative) ────────────────────

/// Coat overlay pattern, Gray → 8-class similarity-ordered table (§3.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(
    dead_code,
    reason = "consumed by sparkle-words P3 (the CatBaker art pass)"
)]
pub enum CoatPattern {
    Solid,
    Smoke,
    Bicolor,
    Tuxedo,
    Calico,
    Colorpoint,
    TabbyMackerel,
    TabbyClassic,
}

/// Cat age band (§3.4 bits 10–11); `scale()` is the §3.4 body-scale column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(
    dead_code,
    reason = "consumed by sparkle-words P3 (the CatBaker art pass)"
)]
pub enum CatAge {
    Kitten,
    Adolescent,
    Adult,
    Elder,
}

impl CatAge {
    /// §3.4 body scale: 0.82 / 0.93 / 1.04 / 1.15.
    #[allow(
        dead_code,
        reason = "consumed by sparkle-words P3 (the CatBaker art pass)"
    )]
    pub fn scale(self) -> f32 {
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

/// Decoded nova features (§3.4 NOVA layout; a nova and a cat never share a
/// class, so low-bit field reuse is safe). Decoded on demand, never stored.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct NovaFeatures {
    /// NOVA_PALETTES index (8 entries, Gray-indexed so neighbors are similar
    /// tints, §6.2).
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
    /// Two Gray-decoded 4-bit hue-nudge codes (bits 19..26) for the ember/ink
    /// gradient anchors.
    pub ink_pair: (u8, u8),
}

/// §3.4 NOVA layout decode.
pub fn nova_features(g: u64) -> NovaFeatures {
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

/// §3.4 single-field decodes, for the per-frame sites that read ONE feature
/// of a genome that never changes for the life of its episode (audit
/// driver-04): the burst-mutex predicate and the residual-fade windows read
/// `duration_ms`, the ember/ink tints read `palette`, and the ignition-grant
/// radius reads `radius`. Each used to pay the whole 9-field [`nova_features`]
/// decode per occurrence per frame. The expressions are copied VERBATIM from
/// the corresponding rows above and pinned bit-for-bit by
/// `nova_single_field_accessors_match_full_decode` — a drift here is a silent
/// visual change (a wrong window or a wrong tint), not a perf bug.
pub fn nova_duration_ms(g: u64) -> u32 {
    1000 + field(g, 9, 2) as u32 * 400 / 3
}

/// See [`nova_duration_ms`]: the `palette` row of [`nova_features`], alone.
pub fn nova_palette(g: u64) -> u8 {
    field(g, 0, 3) as u8
}

/// See [`nova_duration_ms`]: the `radius` row of [`nova_features`], alone.
pub fn nova_radius(g: u64) -> f32 {
    1.6 + field(g, 5, 2) as f32 * 0.2
}

/// §4.2/§3.4: the Gray-decoded ink-pair hue-nudge codes for a class. Profanity
/// reads the NOVA layout window (bits 19..26); every other ink-bearing class —
/// including `emphasis`, pinned by §4.2 — reads the CAT layout window
/// (bits 26..33). Each 4-bit code maps to a hue rotation in `-18°..=+18°`.
pub fn ink_pair_nudges(class: Class, gkey: u64) -> (u8, u8) {
    let lo = if class == Class::Profanity { 19 } else { 26 };
    (field(gkey, lo, 4) as u8, field(gkey, lo + 4, 4) as u8)
}

// ─────────────────────────── §3.5 magic channel ──────────────────────────────

/// Rare cat variant (§3.5, low window of `magic`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[allow(
    dead_code,
    reason = "consumed by sparkle-words P3 (Fortune/Nebula cat builds)"
)]
pub enum CatMagic {
    /// `magic % 4096 < 8` — 1/512.
    Fortune,
    /// `magic % 4096 ∈ 8..12` — 1/1024.
    Nebula,
    /// `magic % 4096 ∈ 12..=54` — 43/4096 ≈ 1/95 (v2.2). A COMPANION, not a
    /// coat build: the genome coat AND pattern survive (§5.4 carve-out).
    Butterfly,
    /// `magic % 4096 ∈ 55..=62` — 8/4096 = 1/512 (v2.2): pale-pink coat,
    /// Solid, baked drifting petal specks.
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

/// §3.5 cat window. Cats consume the LOW window; a genome is never two magics
/// at once because novas read the shifted window and the classes are disjoint.
#[allow(
    dead_code,
    reason = "consumed by sparkle-words P3 (Fortune/Nebula cat builds)"
)]
pub fn cat_magic(magic: u64) -> Option<CatMagic> {
    match magic % 4096 {
        0..=7 => Some(CatMagic::Fortune),
        8..=11 => Some(CatMagic::Nebula),
        // v2.2 windows, retuned against the usage model documented in §3.5
        // (5–20 distinct rolls/day): butterfly ≈ 1/95, sakura = 1/512.
        12..=54 => Some(CatMagic::Butterfly),
        55..=62 => Some(CatMagic::Sakura),
        _ => None,
    }
}

/// §3.5 nova window (shifted 12 bits so it is independent of the cat window).
pub fn nova_magic(magic: u64) -> Option<NovaMagic> {
    match (magic >> 12) % 4096 {
        0..=7 => Some(NovaMagic::Quasar),
        8..=11 => Some(NovaMagic::Singularity),
        _ => None,
    }
}

/// v3 rare accessory (sparkle-words v3 design §2.1) — worn only by ordinary
/// ([`cat_magic`] `== None`) cats; the gate is structural in
/// [`cat_accessory`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Accessory {
    /// `(magic >> 24) % 4096 ∈ 0..=127` — 1/32, the common-delight rung.
    Bow,
    /// `∈ 128..=143` — 1/256. Replaces the eyes; no gaze patch.
    Sunglasses,
    /// `∈ 144..=151` — 1/512.
    WitchHat,
    /// `∈ 152..=155` — 1/1024.
    Crown,
}

/// §2.1 accessory window: `(magic >> 24) % 4096` — a fresh window,
/// independent of the `% 4096` cat window and the `>> 12` nova window.
/// Structurally `None` for any magic-build cat (accessories never stack on
/// Fortune/Nebula/Butterfly/Sakura); rates tests therefore CONDITION on
/// `cat_magic == None` (the marginal is `(4033/4096)·rate`).
pub fn cat_accessory(magic: u64) -> Option<Accessory> {
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

// ──────────────────── cat-art v4 genome selectors (design §3) ────────────────────
//
// The v4 gkey field layout (docs/cat-art-v4-design.md §3) reindexes the low
// bits onto the authored-glyph roster: a variant picks a HEAD, the coat/iris
// ramps recolor the `Recolor::Coat`/`Recolor::Iris` layers, and the age band
// scales the body. These decode ALONGSIDE the v2/v3 `cat_features` (kept live
// for the procedural path until the Cleanup phase) — same `field`/Gray decode,
// a fresh bit assignment:
//
//   | bits  | field   | maps to                                   |
//   |-------|---------|-------------------------------------------|
//   | 0–5   | variant | index into HEADS (25 authored heads)      |
//   | 6–9   | coat    | COAT_RAMP index (Recolor::Coat layers)    |
//   | 10–12 | iris    | EYE_RAMP index  (Recolor::Iris layers)    |
//   | 13–14 | age     | body scale (0.82/0.93/1.04/1.15)          |
//   | 34–63 | head spill | independent entropy for tail tickets   |

/// Reduce the independent 30-bit head-spill field onto `[0, upper)` with
/// multiply-high. This is constant-time, allocation-free, and its finite-domain
/// discrepancy is at most one ticket out of 2^30.
#[inline]
fn scale_head_spill(entropy: u64, upper: usize) -> usize {
    ((u128::from(entropy) * upper as u128) >> 30) as usize
}

/// v4 §3: the authored HEAD this genome wears. The largest complete multiple
/// of the roster in the Gray-decoded 6-bit field supplies an equal number of
/// primary tickets to every head. A tail ticket takes exactly one draw from the
/// dedicated Gray-decoded bits 34–63; coat, iris, age, motion, and ink fields
/// therefore cannot change the head. There is no loop, allocation, or unbounded
/// rejection.
#[inline]
fn cat_variant_index_v4(gkey: u64) -> usize {
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
#[must_use]
pub fn cat_variant_v4(gkey: u64) -> CatGlyphId {
    HEADS[cat_variant_index_v4(gkey)]
}

/// v4 §3: the `(coat, iris)` ramp indices for this genome — `coat = field(6,4)`
/// (0..=15, a `COAT_RAMP` stop) and `iris = field(10,3)` (0..=7, an `EYE_RAMP`
/// stop). Returns the raw indices (the `Eq`-safe `BakeKeyV4` fields); the baker
/// resolves them to colours through `ResolvedFills::from_indices`.
#[must_use]
pub fn cat_fills_v4(gkey: u64) -> (u8, u8) {
    (field(gkey, 6, 4) as u8, field(gkey, 10, 3) as u8)
}

/// v4 §3: the age band — `AGES[field(13,2)]`, whose `scale()` drives the body
/// size (0.82 / 0.93 / 1.04 / 1.15). Unchanged role from v2/v3.
#[must_use]
pub fn cat_age_v4(gkey: u64) -> CatAge {
    AGES[field(gkey, 13, 2) as usize]
}

/// v4 §3 special window (`magic % 4096`): a special glyph REPLACES the head
/// (its own `word_top` anchor drives the peek). Per the resolved-blocker
/// remap, Fortune → the maneki-neko; the other rare windows fold onto the seven
/// remaining authored specials (witch / sleeping / yarn / Toastbyte / tuxedo /
/// tabby-bell / fluffy). `None` = an ordinary head.
///
/// Windows over `magic % 4096`: Maneki `0..=7` (8/4096 = 1/512), then seven
/// specials at 4/4096 (1/1024) each in `8..=35`.
#[must_use]
pub fn special_variant_v4(magic: u64) -> Option<CatGlyphId> {
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

/// v4 §3 overlay-accessory window (`(magic >> 24) % 4096`): the three authored
/// accessory glyphs OVERLAY the chosen head (Bow left-ear, Crown dome crest,
/// Bell collar/chin). The v2/v3 Sunglasses + WitchHat windows are RETIRED
/// (folded into no-accessory — no hat/glasses overlay glyph exists; the witch
/// LOOK ships as [`CatGlyphId::SpecWitch`]). Structurally `None` for any
/// special-build cat (an accessory never stacks on a special), so callers
/// condition rates on `special_variant_v4 == None`.
///
/// Windows over `(magic >> 24) % 4096`: Bow `0..=127` (128/4096 = 1/32),
/// Crown `128..=131` (4/4096 = 1/1024), Bell `132..=147` (16/4096 = 1/256).
#[must_use]
pub fn accessory_variant_v4(magic: u64) -> Option<CatGlyphId> {
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

    /// `(lo, n)` of every Gray-decoded field in the cat-art v4 CAT layout (§3):
    /// variant / coat / iris / age over bits 0..=14, plus the still-live ink-pair
    /// hue-nudge window (bits 26..=33).
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
        (0, 3),  // palette
        (3, 2),  // rays
        (5, 2),  // radius
        (7, 2),  // ring_thick
        (9, 2),  // duration
        (11, 2), // debris
        (13, 2), // chroma
        (19, 4), // ink pair lo
        (23, 4), // ink pair hi
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

    /// §13 `simhash_locality`: identical contexts ⇒ equal; editing one FAR
    /// token ⇒ small Hamming distance; disjoint contexts ⇒ ~32 bits.
    #[test]
    fn simhash_locality() {
        let form = mix(0xF0F0_1234);
        let a = fp_of("alpha beta gamma kitty delta epsilon zeta", "kitty", form);
        let b = fp_of("alpha beta gamma kitty delta epsilon zeta", "kitty", form);
        assert_eq!(a, b, "identical context must fingerprint equal");

        // Edit the farthest right token (d=3, weight 2): a low-weight voter
        // changes only the bits where it was pivotal.
        let c = fp_of("alpha beta gamma kitty delta epsilon yolk", "kitty", form);
        let near = (a ^ c).count_ones();
        assert!(
            near <= 20,
            "one far-token edit must move few bits, moved {near}"
        );

        // A fully disjoint context (different neighbors AND different form)
        // decorrelates toward the 32-bit expectation.
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
        // Locality is graded: the one-token edit moves strictly fewer bits
        // than the disjoint rewrite.
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

    /// §13 `gray_decode_locality` (the honest §3.3 property): flipping bit 0 of
    /// each field moves that feature EXACTLY ±1 step; flipping bit k stays a
    /// bounded block reflection (mean |Δstep| under the pinned bound); and every
    /// flip stays inside its own field (no carry into a neighbor).
    #[test]
    fn gray_decode_locality() {
        let fields = all_fields();
        // Deterministic gkey stream.
        let gkeys: Vec<u64> = (0..512u64)
            .map(|i| mix(i ^ 0x6A09_E667_F3BC_C908))
            .collect();

        for &(lo, n) in &fields {
            // (a) bit-0 flips: exactly ±1 table step, for every sampled gkey.
            for &g in &gkeys {
                let before = field(g, lo, n) as i64;
                let after = field(g ^ (1 << lo), lo, n) as i64;
                assert_eq!(
                    (before - after).abs(),
                    1,
                    "bit-0 flip at lo={lo} n={n} must move exactly 1 step"
                );
            }
            // (b) mean |Δstep| over random single-bit flips within the field is
            // under the pinned bound. Exact expectation over uniform values is
            // (2^n − 1)/n (bit k reflects a 2^(k+1) block ⇒ mean 2^k); pin at
            // 1.2× to absorb sampling noise from the deterministic stream.
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
            // (c) no flip escapes its field: every OTHER field decodes unchanged.
            for &g in &gkeys[..64] {
                for k in 0..n {
                    let flipped = g ^ (1 << (lo + k));
                    for &(olo, on) in &fields {
                        if olo == lo && on == n {
                            continue;
                        }
                        // Overlapping windows (cat vs nova layouts share low bits)
                        // are exempt — the invariant is within-layout disjointness.
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

    /// §13 `magic_windows_hit_expected_rates`: 1e6 synthetic contexts land each
    /// still-live §3.5 magic window (cat Fortune/Nebula/Butterfly/Sakura — the
    /// Kitty-Log magic bucket + dwell bonus — and nova Quasar/Singularity) within
    /// 3σ of its design rate, and every value of every Gray field is observed
    /// (reachability). The v4 special/accessory windows are covered by
    /// [`v4_magic_windows_hit_expected_rates`].
    #[test]
    fn magic_windows_hit_expected_rates() {
        const N: u64 = 1_000_000;
        let (mut fortune, mut nebula, mut quasar, mut singularity) = (0u32, 0u32, 0u32, 0u32);
        let (mut butterfly, mut sakura) = (0u32, 0u32);
        let fields = all_fields();
        // seen[fi][value] for reachability.
        let mut seen: Vec<Vec<bool>> = fields.iter().map(|&(_, n)| vec![false; 1 << n]).collect();
        let mut rot_seen = [false; 16];

        for i in 0..N {
            // Synthetic context: independent well-mixed ctx_fp / form / seed.
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

        // 3σ windows: p = 8/4096 ⇒ μ = 1953.1, σ = 44.2; p = 4/4096 ⇒ μ = 976.6, σ = 31.2.
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
        // v2.2 windows (§3.5): butterfly 43/4096 ≈ 1/95, sakura 8/4096 = 1/512.
        check("butterfly", butterfly, 43.0 / 4096.0);
        check("sakura", sakura, 8.0 / 4096.0);

        // Reachability: every value of every Gray field, plus the nova rot raw field.
        for (fi, &(lo, n)) in fields.iter().enumerate() {
            for (v, hit) in seen[fi].iter().enumerate() {
                assert!(*hit, "field lo={lo} n={n} never decoded value {v}");
            }
        }
        assert!(rot_seen.iter().all(|&b| b), "nova rot raw field unreached");
    }

    /// cat-art v4 §3: the special/accessory magic windows land within 3σ of
    /// their remapped design rates, accessories conditioned on the ordinary
    /// (`special_variant_v4 == None`) build, every head lands within 5σ of its
    /// uniform 1/HEADS rate, and every v4 low-field value is reachable. The
    /// Sunglasses + WitchHat windows are retired — folded into no-accessory.
    #[test]
    fn v4_magic_windows_hit_expected_rates() {
        use crate::cat_glyphs_gen::HEADS;
        const N: u64 = 1_000_000;
        let mut maneki = 0u32;
        // Seven remaining specials at 1/1024 each.
        let (mut witch, mut sleeping, mut yarn) = (0u32, 0u32, 0u32);
        let (mut stretch, mut tuxedo, mut tabbybell, mut fluffy) = (0u32, 0u32, 0u32, 0u32);
        let mut plain = 0u64; // special_variant_v4 == None
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
        // Accessories, 3σ conditioned on the ordinary (non-special) build.
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
        // Every possible raw 6-bit Gray code occurs exactly once here, hence
        // every decoded ticket occurs exactly once too.
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
        // Tail tickets are the only ones that consult spill entropy. For every
        // one, changing any documented non-head field below bit 34 must leave
        // the selected head unchanged.
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

    /// The cat-art v4 §3 selectors + the nova decoder honor their documented
    /// ranges/reachability at the field extremes: every HEAD variant, every coat
    /// / iris / age band, and the nova feature ranges.
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

    /// The single-field nova accessors ARE the corresponding rows of
    /// [`nova_features`] — bit-for-bit, over the same `mix` stream the range
    /// test sweeps. This equality is what makes substituting them at the hot
    /// per-frame sites invisible on the glass: same duration ⇒ same burst and
    /// fade windows, same palette ⇒ same tints, same radius ⇒ same rings.
    /// (`to_bits` on the radius so the pin is exact, not epsilon-close.)
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

    /// §3.2/§7.5 equivalence battery: the bit-sliced CSA kernel equals the
    /// reference vote loop over 10k random voter sets PLUS the edge cases —
    /// 0/1/9 voters, all-equal hashes, and deliberate tie patterns (the
    /// ties-→-1 clause is exactly where a threshold off-by-one would hide).
    /// This is the runnable-now half of the certificate; the ay QF_BV column
    /// lemma (proofs/ay/sparkle_v2/) is the full-domain half.
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
        // 10k random voter sets: random hashes × random presence masks.
        for i in 0..10_000u64 {
            let present = (mix(i ^ 0xC2B2_AE3D_27D4_EB4F) & 0x1FF) as u16;
            eq(&voter_set(i, present), "random");
        }
        // 0 voters (all bits tie → all-ones on both kernels).
        eq(&voter_set(1, 0), "0 voters");
        assert_eq!(simhash_majority_bitsliced(&voter_set(1, 0)), u64::MAX);
        // 1 voter: each slot alone (fp == that voter's hash on both kernels).
        for s in 0..9u16 {
            let v = voter_set(2 + u64::from(s), 1 << s);
            eq(&v, "1 voter");
            assert_eq!(simhash_majority_bitsliced(&v), v.hash[usize::from(s)]);
        }
        // 9 voters, distinct hashes; and 9 voters, ALL-EQUAL hashes.
        for i in 0..64u64 {
            eq(&voter_set(0x1000 + i, 0x1FF), "9 voters");
            let mut v = voter_set(0, 0x1FF);
            v.hash = [mix(0x000A_11E0 ^ i); 9];
            eq(&v, "all-equal hashes");
            assert_eq!(simhash_majority_bitsliced(&v), v.hash[0]);
        }
        // Tie patterns: two equal-weight voters with complementary hashes
        // (per-bit sum 0 ⇒ ties resolve to 1 on BOTH kernels), across every
        // equal-weight slot pair and hash pattern.
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
        // Four-voter tie: form(3) + d4(1) against d1(4) with the form pair
        // opposed — W = 8, per-bit sums hit exactly 2S = W where hashes align.
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

    /// The end-to-end siblings agree: same token walk, same filter, same
    /// majority — over real text incl. bare words, clock/spinner-silenced
    /// contexts, and joiner tokens.
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

    /// §7.4 `bench_simhash_bitsliced`: reference vs bit-sliced kernel on
    /// identical voter sets. The delta is honestly labeled sub-µs (§3.2 — the
    /// row exists to keep the certified-optimization claim measured, §15.15).
    /// Timing-sensitive, so it follows the repo's manual-timing idiom:
    ///
    /// ```sh
    /// cargo test -p aterm-effects --release bench_simhash_bitsliced -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "perf gate (design §7.4): run manually in --release with --ignored --nocapture"]
    fn bench_simhash_bitsliced() {
        use std::hint::black_box;
        use std::time::{Duration, Instant};
        const SETS: usize = 1024;
        const ITERS: usize = 200;
        let sets: Vec<VoterSet> = (0..SETS as u64)
            .map(|i| voter_set(i, ((mix(i) & 0x1FF) as u16) | 1))
            .collect();
        let mut t_ref = Vec::with_capacity(ITERS);
        let mut t_bs = Vec::with_capacity(ITERS);
        let (mut acc_r, mut acc_b) = (0u64, 0u64);
        for _ in 0..ITERS {
            let s = Instant::now();
            for v in &sets {
                acc_r ^= simhash_majority_reference(black_box(v));
            }
            t_ref.push(s.elapsed());
            let s = Instant::now();
            for v in &sets {
                acc_b ^= simhash_majority_bitsliced(black_box(v));
            }
            t_bs.push(s.elapsed());
        }
        assert_eq!(acc_r, acc_b, "kernels must agree on the benched inputs");
        t_ref.sort();
        t_bs.sort();
        let (mr, mb) = (t_ref[ITERS / 2], t_bs[ITERS / 2]);
        let per = |d: Duration| d.as_nanos() as f64 / SETS as f64;
        println!(
            "bench_simhash_bitsliced: reference median {mr:?} ({:.1} ns/set), \
             bit-sliced median {mb:?} ({:.1} ns/set) over {ITERS} x {SETS}-set passes \
             (speedup {:.1}x; both kernels honestly sub-µs per set)",
            per(mr),
            per(mb),
            per(mr) / per(mb)
        );
        assert!(
            mb <= mr,
            "the bit-sliced kernel must not be slower than the reference (bs {mb:?} vs ref {mr:?})"
        );
    }

    #[test]
    fn mix_decorrelates_and_is_nonzero() {
        let mut prev = 0;
        for f in 0..64u64 {
            let v = mix(0xDEAD ^ f.rotate_left(17));
            assert_ne!(v, 0);
            assert_ne!(v, prev, "adjacent frames must differ");
            prev = v;
        }
    }
}
