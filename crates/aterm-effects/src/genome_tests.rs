// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
const fn voter_set(i: u64, present: u16) -> VoterSet {
    let mut v = VoterSet {
        hash: [0; 9],
        present,
    };
    let mut s = 0usize;
    while s < 9 {
        v.hash[s] =
            mix(i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (s as u64) << 56 ^ 0x5851_F42D_4C95_7F2D);
        s += 1;
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
