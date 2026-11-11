// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The committed asset passes the verifying decoder (length, dims, checksum).
#[test]
fn shipped_weights_load_and_verify() {
    let bytes: &[u8] = include_bytes!("../data/tone_model.atn");
    assert_eq!(bytes.len(), ASSET_LEN, "asset length drifted");
    ToneModel::from_bytes(bytes).expect("shipped tone model must verify");
    assert!(builtin().is_some());
}

/// Each corruption class is refused with its own error.
#[test]
fn corrupted_assets_are_refused() {
    let good = include_bytes!("../data/tone_model.atn").to_vec();
    assert_eq!(
        ToneModel::from_bytes(&good[..100]).unwrap_err(),
        ToneModelError::BadLength,
    );
    assert_eq!(
        ToneModel::from_bytes(&[0u8; 4]).unwrap_err(),
        ToneModelError::Truncated,
    );
    let mut bad_magic = good.clone();
    bad_magic[0] ^= 0xff;
    assert_eq!(
        ToneModel::from_bytes(&bad_magic).unwrap_err(),
        ToneModelError::BadMagic,
    );
    let mut flipped = good.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x01;
    assert_eq!(
        ToneModel::from_bytes(&flipped).unwrap_err(),
        ToneModelError::BadChecksum,
    );
}

/// `classify_opt` reports thin evidence as `None`, and agrees with `classify` on a real line.
#[test]
fn classify_opt_reports_abstention_as_none() {
    let m = builtin().expect("shipped model");
    let mut s = ToneScratch::default();
    assert_eq!(m.classify_opt("", &mut s), None);
    assert_eq!(m.classify_opt("a", &mut s), None);
    let line = "why is this broken again ugh";
    assert_eq!(m.classify_opt(line, &mut s), Some(m.classify(line, &mut s)));
}

/// The shipped weights clear a modest held-out floor, overall and per focus language: an asset-integrity pin, not an accuracy claim.
#[test]
fn holdout_sanity_set_clears_documented_floors() {
    let m = builtin().expect("shipped model");
    let mut s = ToneScratch::default();
    let mut total = (0usize, 0usize);
    let mut by_lang: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    for line in HOLDOUT_TSV.lines() {
        let Some((want, lang, text)) = parse_corpus_line(line) else {
            continue;
        };
        let got = m.classify(text, &mut s);
        let e = by_lang.entry(lang).or_default();
        e.1 += 1;
        total.1 += 1;
        if got == want {
            e.0 += 1;
            total.0 += 1;
        }
    }
    assert!(
        total.1 >= 60,
        "holdout set went missing ({} lines)",
        total.1
    );
    let overall = total.0 as f32 / total.1 as f32;
    assert!(
        overall >= 0.70,
        "held-out floor broken: {}/{} = {overall:.2} < 0.70",
        total.0,
        total.1
    );
    for lang in ["zh", "ja", "ko", "en", "es"] {
        let &(ok, n) = by_lang.get(lang).unwrap_or(&(0, 0));
        assert!(n >= 5, "holdout must keep covering {lang} (has {n})");
        let acc = ok as f32 / n as f32;
        assert!(
            acc >= 0.60,
            "{lang} floor broken: {ok}/{n} = {acc:.2} < 0.60"
        );
    }
}

/// Featurization is script-agnostic (CJK needs no tokenizer), collapses whitespace runs and folds ASCII case.
#[test]
fn featurization_is_script_agnostic_and_folded() {
    let count = |t: &str| for_each_ngram_bucket(t, |_| {});
    assert_eq!(count("abcd"), 9);
    assert_eq!(count("猫が好き"), 9);
    assert_eq!(count("사랑해요"), 9);
    let mut a = Vec::new();
    let mut b = Vec::new();
    for_each_ngram_bucket("Hello World", |x| a.push(x));
    for_each_ngram_bucket("hello world", |x| b.push(x));
    assert_eq!(a, b);
    a.clear();
    b.clear();
    for_each_ngram_bucket("    ls -la", |x| a.push(x));
    for_each_ngram_bucket(" ls -la", |x| b.push(x));
    assert_eq!(a, b);
    for ch in (0..=0x7f)
        .filter_map(char::from_u32)
        .filter(|ch| ch.is_whitespace())
    {
        a.clear();
        for_each_ngram_bucket(&format!("{ch}{ch}ls -la"), |x| a.push(x));
        assert_eq!(a, b, "whitespace {ch:?}");
    }
}

/// The trainer's encode/decode path round-trips onto the i8 lattice.
#[test]
fn encode_decode_round_trips_the_quantized_lattice() {
    let mut x = 0x1234_5678u32;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x >> 8) as f32 / 16_777_216.0 - 0.5
    };
    let emb: Vec<f32> = (0..EMB_LEN).map(|_| rnd()).collect();
    let w1: Vec<f32> = (0..W1_LEN).map(|_| rnd()).collect();
    let b1: Vec<f32> = (0..HIDDEN).map(|_| rnd()).collect();
    let w2: Vec<f32> = (0..W2_LEN).map(|_| rnd()).collect();
    let b2: Vec<f32> = (0..TONE_COUNT).map(|_| rnd()).collect();
    let bytes = encode_model(&emb, &w1, &b1, &w2, &b2);
    assert_eq!(bytes.len(), ASSET_LEN);
    let m = ToneModel::from_bytes(&bytes).expect("round trip");
    let step = emb.iter().fold(0.0f32, |a, &v| a.max(v.abs())) / 127.0;
    for (orig, got) in emb.iter().zip(m.emb.iter().flatten()) {
        assert!((orig - got).abs() <= step * 0.5 + 1e-6);
    }
    for (orig, got) in b1.iter().zip(&m.b1) {
        assert_eq!(orig.to_bits(), got.to_bits());
    }
}
