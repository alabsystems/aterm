// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tone of typing: a small fastText-style classifier that guesses the coarse
//! mood of the line being typed, so the trail synth's melody can lean with the
//! writer (`trail_sound`'s tone tables). [`Tone::Technical`] doubles as the
//! neutral verdict: its melody mapping is the identity, so "no idea" and "shell
//! command" leave the sound unchanged.
//!
//! Hashed character n-grams (1–3) → embedding bag → mean pool → one 64-unit
//! `tanh` hidden layer → 5-way softmax. Features are Unicode chars, so the model
//! is script-agnostic (CJK needs no segmenter), and there is no vocabulary file:
//! every n-gram hashes (FNV-1a, per-order salt) into [`BUCKETS`] embedding rows.
//! ~68k parameters, i8-quantized into a ~67 KiB [`include_bytes!`] asset and
//! dequantized once at load. Inference is two fixed-size matmuls, inside the
//! <100 µs/line input-path budget and allocation-free given a [`ToneScratch`].
//!
//! Trained offline (`examples/tone_train.rs`) on a small curated corpus
//! (`data/tone_corpus.tsv`: short lines in ten languages plus code-flavoured
//! Technical lines), it learns loud surface markers (laughter tokens,
//! exclamation shapes, complaint vocabulary, shell/code syntax), not real
//! sentiment. The tests pin asset integrity and a modest held-out floor, not
//! accuracy; the verdict is only an aesthetic hint, never shown to the user.

use std::sync::OnceLock;

/// Number of tone classes. Fixed by the model asset format.
pub const TONE_COUNT: usize = 5;

/// The coarse mood vocabulary. Order is the model's output index order, fixed
/// by the trained weight asset.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tone {
    /// Even, unhurried prose ("ok sounds good", "no rush").
    Calm,
    /// High-energy anticipation/celebration ("it works!!", "let's go!").
    Excited,
    /// Complaint/exasperation ("why is this broken again", "ugh").
    Frustrated,
    /// Laughter, teasing, whimsy ("lol", "ㅋㅋㅋ", ":3").
    Playful,
    /// Code, commands, logs, technical prose, and the neutral fallback (its
    /// melody mapping is the identity), hence the default.
    #[default]
    Technical,
}

impl Tone {
    /// All tones in model-index order.
    pub const ALL: [Tone; TONE_COUNT] = [
        Tone::Calm,
        Tone::Excited,
        Tone::Frustrated,
        Tone::Playful,
        Tone::Technical,
    ];

    /// The model output index (softmax slot) of this tone.
    #[inline]
    pub const fn index(self) -> usize {
        match self {
            Tone::Calm => 0,
            Tone::Excited => 1,
            Tone::Frustrated => 2,
            Tone::Playful => 3,
            Tone::Technical => 4,
        }
    }

    /// Inverse of [`Tone::index`]; out-of-range folds to the neutral
    /// [`Tone::Technical`] rather than panicking on an audio-adjacent path.
    #[inline]
    pub const fn from_index(i: usize) -> Tone {
        if i < Tone::ALL.len() {
            Tone::ALL[i]
        } else {
            Tone::Technical
        }
    }

    /// Corpus label string (the TSV files' first column).
    #[inline]
    pub const fn label(self) -> &'static str {
        match self {
            Tone::Calm => "calm",
            Tone::Excited => "excited",
            Tone::Frustrated => "frustrated",
            Tone::Playful => "playful",
            Tone::Technical => "technical",
        }
    }

    /// Parse a corpus label. Unknown labels are `None` (the trainer treats
    /// that as a data error, not a silent class).
    #[inline]
    pub fn parse_label(s: &str) -> Option<Tone> {
        Tone::ALL.iter().copied().find(|t| t.label() == s)
    }
}

/// Hashed embedding rows; a power of two so the bucket fold is a mask. 2048 × 32
/// is deliberately small: collisions regularize a corpus this size.
pub const BUCKETS: usize = 2048;
/// Embedding (and mean-pool) width.
pub const EMBED: usize = 32;
/// Hidden layer width.
pub const HIDDEN: usize = 64;

/// Minimum n-gram count before the model gives an opinion (three typed chars
/// emit six). Below it [`ToneModel::scores`] returns `None` and classification
/// stays [`Tone::Technical`], so the melody does not lurch on a two-key line.
pub const MIN_NGRAMS: usize = 6;

/// Feed every hashed n-gram bucket of `text` to `f`; returns how many were
/// emitted. The one featurization for inference and the trainer, so the weights
/// match the runtime's view.
///
/// Chars are ASCII-lowercased (full Unicode folding adds little for mood markers
/// and brings locale questions), whitespace runs collapse to one space (so
/// indentation does not dominate code), and each position emits its 1-gram plus
/// the 2- and 3-grams ending at it.
pub fn for_each_ngram_bucket(text: &str, mut f: impl FnMut(usize)) -> usize {
    use aterm_hash::{FNV64_OFFSET, fnv1a_64_extend};
    /// Per-order salt (odd constant × order) so n-gram orders occupy
    /// distinct hash streams.
    const ORDER_SALT: u64 = 0x9e37_79b9_7f4a_7c15;

    #[inline]
    const fn fold(h: u64, cb: [u8; 4]) -> u64 {
        fnv1a_64_extend(h, &cb)
    }
    #[inline]
    const fn bucket(h: u64) -> usize {
        (h as usize) & (BUCKETS - 1)
    }

    let mut count = 0usize;
    // Rolling partial hashes of the previous one and two (folded) chars.
    let mut prev1_h2: Option<u64> = None;
    let mut prev1_h3: Option<u64> = None;
    let mut prev2_h3: Option<u64> = None;
    let mut last_was_space = false;
    for raw in text.chars() {
        let c = if matches!(raw, ' ' | '\t'..='\r') || (!raw.is_ascii() && raw.is_whitespace()) {
            if last_was_space {
                continue; // collapse runs — indentation is not a mood
            }
            last_was_space = true;
            ' '
        } else {
            last_was_space = false;
            if raw.is_ascii() {
                raw.to_ascii_lowercase()
            } else {
                raw
            }
        };
        let cb = (c as u32).to_le_bytes();
        // 1-gram.
        f(bucket(fold(FNV64_OFFSET ^ ORDER_SALT, cb)));
        count += 1;
        // 2-gram ending here.
        if let Some(h2_p1) = prev1_h2 {
            f(bucket(fold(h2_p1, cb)));
            count += 1;
            // 3-gram ending here.
            if let Some(h3_p2_p1) = prev2_h3 {
                f(bucket(fold(h3_p2_p1, cb)));
                count += 1;
            }
        }
        prev2_h3 = prev1_h3.map(|h3_p1| fold(h3_p1, cb));
        prev1_h3 = Some(fold(FNV64_OFFSET ^ ORDER_SALT.wrapping_mul(3), cb));
        prev1_h2 = Some(fold(FNV64_OFFSET ^ ORDER_SALT.wrapping_mul(2), cb));
    }
    count
}

/// Fixed-size inference scratch: construct once and reuse; `classify` allocates
/// nothing.
#[derive(Clone, Debug)]
pub struct ToneScratch {
    sum: [f32; EMBED],
    hid: [f32; HIDDEN],
    logits: [f32; TONE_COUNT],
}

impl Default for ToneScratch {
    #[inline]
    fn default() -> Self {
        Self {
            sum: [0.0; EMBED],
            hid: [0.0; HIDDEN],
            logits: [0.0; TONE_COUNT],
        }
    }
}

/// Why a weight asset was refused; the loader never panics.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToneModelError {
    /// Too short to even hold the header.
    Truncated,
    /// Magic bytes are not `ATONEv1\0`.
    BadMagic,
    /// Header dims disagree with the compiled BUCKETS/EMBED/HIDDEN/TONE_COUNT.
    DimensionMismatch,
    /// Total length is not exactly header + tensors + checksum.
    BadLength,
    /// FNV-1a payload checksum mismatch (bit rot / partial write).
    BadChecksum,
    /// A quantization scale is non-finite or non-positive.
    BadScale,
}

/// 8-byte asset magic. The trailing version byte is part of the magic: a
/// format change mints `ATONEv2\0` rather than reinterpreting old bytes.
const MAGIC: [u8; 8] = *b"ATONEv1\0";

const EMB_LEN: usize = BUCKETS * EMBED;
const W1_LEN: usize = EMBED * HIDDEN;
const W2_LEN: usize = HIDDEN * TONE_COUNT;
/// header: magic + 4×u32 dims + 3×f32 scales.
const HEADER_LEN: usize = 8 + 16 + 12;
/// Exact asset length: header, i8 tensors, f32 biases, u32 checksum.
const ASSET_LEN: usize = HEADER_LEN + EMB_LEN + W1_LEN + W2_LEN + (HIDDEN + TONE_COUNT) * 4 + 4;

#[inline]
const fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u32;
        h = h.wrapping_mul(0x0100_0193);
        i += 1;
    }
    h
}

/// The loaded (dequantized) classifier. Layouts: `emb[bucket*EMBED + i]`,
/// `w1[i*HIDDEN + j]`, `w2[j*TONE_COUNT + c]`.
#[derive(Clone, Debug)]
pub struct ToneModel {
    emb: Vec<f32>,
    w1: Vec<f32>,
    b1: Vec<f32>,
    w2: Vec<f32>,
    b2: Vec<f32>,
}

impl ToneModel {
    /// Decode and verify a weight asset. Any length, dim or checksum mismatch is
    /// refused: a half-read model would classify garbage with full confidence.
    pub fn from_bytes(bytes: &[u8]) -> Result<ToneModel, ToneModelError> {
        if bytes.len() < HEADER_LEN {
            return Err(ToneModelError::Truncated);
        }
        if bytes.len() != ASSET_LEN {
            // Magic first for a friendlier error on foreign files.
            if bytes.get(..8) != Some(&MAGIC[..]) {
                return Err(ToneModelError::BadMagic);
            }
            return Err(ToneModelError::BadLength);
        }
        if bytes.get(..8) != Some(&MAGIC[..]) {
            return Err(ToneModelError::BadMagic);
        }
        let payload_end = ASSET_LEN - 4;
        let stored = u32::from_le_bytes(
            bytes[payload_end..]
                .try_into()
                .map_err(|_| ToneModelError::Truncated)?,
        );
        if fnv1a32(&bytes[..payload_end]) != stored {
            return Err(ToneModelError::BadChecksum);
        }
        let u32_at = |off: usize| -> u32 {
            let mut b = [0u8; 4];
            b.copy_from_slice(&bytes[off..off + 4]);
            u32::from_le_bytes(b)
        };
        let f32_at = |off: usize| -> f32 {
            let mut b = [0u8; 4];
            b.copy_from_slice(&bytes[off..off + 4]);
            f32::from_le_bytes(b)
        };
        if u32_at(8) as usize != BUCKETS
            || u32_at(12) as usize != EMBED
            || u32_at(16) as usize != HIDDEN
            || u32_at(20) as usize != TONE_COUNT
        {
            return Err(ToneModelError::DimensionMismatch);
        }
        let emb_scale = f32_at(24);
        let w1_scale = f32_at(28);
        let w2_scale = f32_at(32);
        for s in [emb_scale, w1_scale, w2_scale] {
            if !s.is_finite() || s <= 0.0 {
                return Err(ToneModelError::BadScale);
            }
        }
        let mut off = HEADER_LEN;
        let deq = |len: usize, scale: f32, off: &mut usize| -> Vec<f32> {
            let out = bytes[*off..*off + len]
                .iter()
                .map(|&b| f32::from(b as i8) * scale)
                .collect();
            *off += len;
            out
        };
        let emb = deq(EMB_LEN, emb_scale, &mut off);
        let w1 = deq(W1_LEN, w1_scale, &mut off);
        let w2 = deq(W2_LEN, w2_scale, &mut off);
        let floats = |len: usize, off: &mut usize| -> Result<Vec<f32>, ToneModelError> {
            let mut v = Vec::with_capacity(len);
            for k in 0..len {
                let x = f32_at(*off + k * 4);
                if !x.is_finite() {
                    return Err(ToneModelError::BadScale);
                }
                v.push(x);
            }
            *off += len * 4;
            Ok(v)
        };
        let b1 = floats(HIDDEN, &mut off)?;
        let b2 = floats(TONE_COUNT, &mut off)?;
        Ok(ToneModel {
            emb,
            w1,
            b1,
            w2,
            b2,
        })
    }

    /// Compute the raw output logits into `s.logits`, returning `false` when
    /// the text has fewer than [`MIN_NGRAMS`] character n-grams.
    fn compute_logits(&self, text: &str, s: &mut ToneScratch) -> bool {
        s.sum = [0.0; EMBED];
        let emb_rows = self.emb.as_chunks::<EMBED>().0;
        let n = for_each_ngram_bucket(text, |bucket| {
            // Masking keeps `bucket < BUCKETS`, so this never misses.
            if let Some(chunk) = emb_rows.get(bucket) {
                for (acc, &w) in s.sum.iter_mut().zip(chunk) {
                    *acc += w;
                }
            }
        });
        if n < MIN_NGRAMS {
            return false;
        }
        let inv = 1.0 / n as f32;
        for e in &mut s.sum {
            *e *= inv;
        }
        // Hidden layer: tanh(b1 + e·W1), accumulated row-wise for cache locality
        // and auto-vectorization in the exact `i = 0..EMBED` addition order.
        s.hid.copy_from_slice(&self.b1);
        for (row, &e) in self.w1.as_chunks::<HIDDEN>().0.iter().zip(&s.sum) {
            for (h, &w) in s.hid.iter_mut().zip(row) {
                *h += e * w;
            }
        }
        for h in &mut s.hid {
            *h = h.tanh();
        }
        // Output logits, accumulated row-wise in the exact `j = 0..HIDDEN` order.
        s.logits.copy_from_slice(&self.b2);
        for (row, &h) in self.w2.as_chunks::<TONE_COUNT>().0.iter().zip(&s.hid) {
            for (l, &w) in s.logits.iter_mut().zip(row) {
                *l += h * w;
            }
        }
        true
    }

    /// Softmax scores in [`Tone::ALL`] order, or `None` when the text is too
    /// short ([`MIN_NGRAMS`]). Pure, deterministic, allocation-free (state lives
    /// in `s`).
    pub fn scores(&self, text: &str, s: &mut ToneScratch) -> Option<[f32; TONE_COUNT]> {
        if !self.compute_logits(text, s) {
            return None;
        }
        // Softmax (max-shifted for stability).
        let mut mx = f32::NEG_INFINITY;
        for &l in &s.logits {
            mx = mx.max(l);
        }
        let mut out = [0.0f32; TONE_COUNT];
        let mut z = 0.0f32;
        for (o, &l) in out.iter_mut().zip(&s.logits) {
            *o = (l - mx).exp();
            z += *o;
        }
        if z > 0.0 {
            for o in &mut out {
                *o /= z;
            }
        }
        Some(out)
    }

    /// The verdict with abstention explicit: the argmax tone, or `None` when the
    /// evidence is too thin ([`MIN_NGRAMS`]). Ties resolve to the lowest index.
    /// A host that caches a mood uses this, so an evidence-thin window (a
    /// one-char line after Enter) leaves the prior verdict standing.
    #[inline]
    pub fn classify_opt(&self, text: &str, s: &mut ToneScratch) -> Option<Tone> {
        if !self.compute_logits(text, s) {
            return None;
        }
        let mut best = 0usize;
        for (i, &l) in s.logits.iter().enumerate() {
            if l > s.logits[best] {
                best = i;
            }
        }
        Some(Tone::from_index(best))
    }

    /// The verdict: argmax tone, or the neutral [`Tone::Technical`] when the
    /// evidence is too thin. Ties resolve to the lowest index.
    #[inline]
    pub fn classify(&self, text: &str, s: &mut ToneScratch) -> Tone {
        self.classify_opt(text, s).unwrap_or(Tone::Technical)
    }
}

/// Quantize and serialize full-precision weights into the asset format, beside
/// the decoder so the byte layout has one author. The offline trainer
/// round-trips the bytes through [`ToneModel::from_bytes`] before reporting
/// holdout accuracy, so what ships is what was measured.
///
/// Panics unless the slices have the compiled lengths (trainer-only code).
pub fn encode_model(emb: &[f32], w1: &[f32], b1: &[f32], w2: &[f32], b2: &[f32]) -> Vec<u8> {
    assert_eq!(emb.len(), EMB_LEN);
    assert_eq!(w1.len(), W1_LEN);
    assert_eq!(b1.len(), HIDDEN);
    assert_eq!(w2.len(), W2_LEN);
    assert_eq!(b2.len(), TONE_COUNT);
    fn scale_of(t: &[f32]) -> f32 {
        let mx = t.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        if mx > 0.0 { mx / 127.0 } else { 1.0 / 127.0 }
    }
    fn quant(out: &mut Vec<u8>, t: &[f32], scale: f32) {
        for &x in t {
            let q = (x / scale).round().clamp(-127.0, 127.0) as i8;
            out.push(q as u8);
        }
    }
    let (es, s1, s2) = (scale_of(emb), scale_of(w1), scale_of(w2));
    let mut out = Vec::with_capacity(ASSET_LEN);
    out.extend_from_slice(&MAGIC);
    for dim in [BUCKETS, EMBED, HIDDEN, TONE_COUNT] {
        out.extend_from_slice(&(dim as u32).to_le_bytes());
    }
    for s in [es, s1, s2] {
        out.extend_from_slice(&s.to_le_bytes());
    }
    quant(&mut out, emb, es);
    quant(&mut out, w1, s1);
    quant(&mut out, w2, s2);
    for &x in b1 {
        out.extend_from_slice(&x.to_le_bytes());
    }
    for &x in b2 {
        out.extend_from_slice(&x.to_le_bytes());
    }
    let ck = fnv1a32(&out);
    out.extend_from_slice(&ck.to_le_bytes());
    debug_assert_eq!(out.len(), ASSET_LEN);
    out
}

/// The shipped weights (`data/tone_model.atn`, from `targo --unverified run -p
/// aterm-effects --example tone_train`), loaded once. `None` if the embedded
/// asset fails verification; callers fall back to the neutral tone.
#[inline]
pub fn builtin() -> Option<&'static ToneModel> {
    static MODEL: OnceLock<Option<ToneModel>> = OnceLock::new();
    MODEL
        .get_or_init(|| ToneModel::from_bytes(include_bytes!("../data/tone_model.atn")).ok())
        .as_ref()
}

/// The committed held-out sanity set (never trained on), read by the tests.
/// Format: `label \t lang \t text`.
#[cfg(test)]
pub const HOLDOUT_TSV: &str = include_str!("../data/tone_holdout.tsv");

/// Parse one corpus/holdout TSV line (shared by the trainer and the
/// conformance tests). Blank lines and `#` comments yield `None`.
#[inline]
pub fn parse_corpus_line(line: &str) -> Option<(Tone, &str, &str)> {
    let line = line.trim_end_matches('\r');
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut parts = line.splitn(3, '\t');
    let label = parts.next()?;
    let lang = parts.next()?;
    let text = parts.next()?;
    let tone = Tone::parse_label(label)?;
    Some((tone, lang, text))
}

#[cfg(test)]
mod tests {
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

    /// `classify_opt` reports thin evidence as `None`, and agrees with
    /// `classify` on a real line.
    #[test]
    fn classify_opt_reports_abstention_as_none() {
        let m = builtin().expect("shipped model");
        let mut s = ToneScratch::default();
        assert_eq!(m.classify_opt("", &mut s), None);
        assert_eq!(m.classify_opt("a", &mut s), None);
        let line = "why is this broken again ugh";
        assert_eq!(m.classify_opt(line, &mut s), Some(m.classify(line, &mut s)));
    }

    /// The shipped weights clear a modest held-out floor, overall and per focus
    /// language: an asset-integrity pin, not an accuracy claim.
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

    /// Featurization is script-agnostic (CJK needs no tokenizer), collapses
    /// whitespace runs and folds ASCII case.
    #[test]
    fn featurization_is_script_agnostic_and_folded() {
        let count = |t: &str| for_each_ngram_bucket(t, |_| {});
        // 4 chars ⇒ 4 + 3 + 2 n-grams, same for any script.
        assert_eq!(count("abcd"), 9);
        assert_eq!(count("猫が好き"), 9);
        assert_eq!(count("사랑해요"), 9);
        // Case folding: identical bucket streams.
        let mut a = Vec::new();
        let mut b = Vec::new();
        for_each_ngram_bucket("Hello World", |x| a.push(x));
        for_each_ngram_bucket("hello world", |x| b.push(x));
        assert_eq!(a, b);
        // Whitespace collapsing: indentation does not change the stream.
        a.clear();
        b.clear();
        for_each_ngram_bucket("    ls -la", |x| a.push(x));
        for_each_ngram_bucket(" ls -la", |x| b.push(x));
        assert_eq!(a, b);
        // Unicode White_Space also includes ASCII vertical tab, which
        // is_ascii_whitespace deliberately excludes. Keep every ASCII
        // whitespace character equivalent to the normalized space.
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
        // Quantization error bounded by half a step per weight.
        let step = emb.iter().fold(0.0f32, |a, &v| a.max(v.abs())) / 127.0;
        for (orig, got) in emb.iter().zip(&m.emb) {
            assert!((orig - got).abs() <= step * 0.5 + 1e-6);
        }
        // Biases are stored full-precision.
        for (orig, got) in b1.iter().zip(&m.b1) {
            assert_eq!(orig.to_bits(), got.to_bits());
        }
    }
}
