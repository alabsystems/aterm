// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TONE-OF-TYPING — a tiny fastText-style neural classifier that guesses the
//! coarse MOOD of the line currently being typed, so the trail synth's melody
//! can lean with the writer (`trail_sound`'s tone tables). Five classes:
//! [`Tone::Calm`], [`Tone::Excited`], [`Tone::Frustrated`], [`Tone::Playful`],
//! [`Tone::Technical`] — where Technical doubles as the NEUTRAL verdict (its
//! melody mapping is the identity), so "no idea" and "shell command" both
//! leave the sound exactly as it ships today.
//!
//! # Architecture (and why this shape)
//!
//! Hashed character n-gram (1–3) embedding bag → mean pool → one 64-unit
//! `tanh` hidden layer → 5-way softmax. The fastText recipe, sized way down:
//!
//! - **Character n-grams, not words.** The features are Unicode CHARS, so the
//!   model is script-agnostic by construction — Chinese/Japanese/Korean text
//!   needs no segmenter (a CJK char IS a meaningful unigram; pairs are
//!   compounds), and Latin/Cyrillic/Arabic scripts ride the same path. This
//!   is the whole reason for the fastText shape over a word-token model.
//! - **Hashed buckets, fixed size.** No vocabulary file: every n-gram hashes
//!   (FNV-1a, per-order salt) into [`BUCKETS`] embedding rows. Collisions are
//!   part of the training objective, exactly as in fastText.
//! - **Tiny on purpose.** ~68k parameters, i8-quantized to a ~70 KiB
//!   [`include_bytes!`] asset, dequantized to f32 once at load. Inference is
//!   two fixed-size matmuls — comfortably inside the <100 µs/line budget the
//!   input path demands, with ZERO allocation after [`ToneScratch`] exists.
//! - **Hand-rolled, no ML deps.** The house style (see the lexicon's
//!   "vendoring norm" stance and aterm-grapheme's baked tables): fixed-size
//!   loops + a checked binary weight asset, not a tensor crate.
//!
//! # Honesty note (this is a mood squint, not sentiment science)
//!
//! The model is trained (offline, `examples/tone_train.rs`, never in the
//! build) on a small curated seed corpus authored for this feature —
//! `data/tone_corpus.tsv`, a few hundred short lines spanning
//! en/zh/ja/ko/es/de/fr/ru/ar/hi plus code-flavoured Technical lines. That is
//! enough to learn the loud surface markers of each mood (laughter tokens,
//! exclamation shapes, complaint vocabulary, shell/code syntax) across those
//! scripts; it is nowhere near enough for real sentiment analysis, sarcasm,
//! or dialect coverage. The conformance test pins ASSET INTEGRITY — the
//! shipped weights load and clear a documented, deliberately modest floor on
//! a held-out sanity set — not scientific accuracy. Everything downstream
//! treats the verdict as a gentle aesthetic hint (a scale-table swap), never
//! as a user-visible judgement.

use std::sync::OnceLock;

/// Number of tone classes. Fixed by the model asset format.
pub const TONE_COUNT: usize = 5;

/// The coarse mood vocabulary. Order is the model's output index order and
/// is FROZEN — the trained weight asset encodes it.
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
    /// Code, commands, logs, technical prose — AND the neutral fallback.
    /// Its melody mapping is the identity, so an uncertain classifier and a
    /// shell session both sound exactly like today's aterm. Default for the
    /// same reason.
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
    pub fn index(self) -> usize {
        match self {
            Tone::Calm => 0,
            Tone::Excited => 1,
            Tone::Frustrated => 2,
            Tone::Playful => 3,
            Tone::Technical => 4,
        }
    }

    /// Inverse of [`Tone::index`]; out-of-range folds to the neutral
    /// [`Tone::Technical`] (fail-closed — an impossible index must not panic
    /// an audio-adjacent path).
    pub fn from_index(i: usize) -> Tone {
        *Tone::ALL.get(i).unwrap_or(&Tone::Technical)
    }

    /// Corpus label string (the TSV files' first column).
    pub fn label(self) -> &'static str {
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
    pub fn parse_label(s: &str) -> Option<Tone> {
        Tone::ALL.iter().copied().find(|t| t.label() == s)
    }
}

/// Hashed embedding rows. Power of two so the bucket fold is a mask, not a
/// modulo. 2048 rows × 32 dims is deliberately small: collisions regularize
/// a corpus this size rather than starve it.
pub const BUCKETS: usize = 2048;
/// Embedding (and mean-pool) width.
pub const EMBED: usize = 32;
/// Hidden layer width.
pub const HIDDEN: usize = 64;

/// Minimum n-gram count before the model ventures ANY opinion. Below this
/// (roughly three typed chars) [`ToneModel::scores`] returns `None` and
/// classification stays [`Tone::Technical`] — a two-key line is evidence of
/// nothing, and the melody must not lurch on it.
pub const MIN_NGRAMS: usize = 6;

/// Feed every hashed n-gram bucket of `text` to `f`; returns how many were
/// emitted. THE single featurization used by inference, training, and tests —
/// the trainer (`examples/tone_train.rs`) calls this exact function, so the
/// shipped weights can never drift from the runtime's view of the text.
///
/// Shape: chars are ASCII-lowercased (full Unicode case-folding buys little
/// for mood markers and would drag locale questions in — the corpus is folded
/// through the same function, so training and inference always agree),
/// whitespace runs collapse to one space (indentation must not dominate a
/// code line's features), and each position emits its 1-gram plus the 2- and
/// 3-grams ending at it. Per-order hash salts keep a lone char and the same
/// char as a trigram tail from aliasing systematically.
pub fn for_each_ngram_bucket(text: &str, mut f: impl FnMut(usize)) -> usize {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    /// Per-order salt (odd constant × order) so n-gram orders occupy
    /// distinct hash streams.
    const ORDER_SALT: u64 = 0x9e37_79b9_7f4a_7c15;

    fn fold(h: u64, c: char) -> u64 {
        let mut h = h;
        for b in (c as u32).to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(FNV_PRIME);
        }
        h
    }
    fn bucket(h: u64) -> usize {
        (h as usize) & (BUCKETS - 1)
    }

    let mut count = 0usize;
    // Rolling window of the previous two (folded) chars.
    let mut prev1: Option<char> = None;
    let mut prev2: Option<char> = None;
    let mut last_was_space = false;
    for raw in text.chars() {
        let c = if raw.is_whitespace() {
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
        // 1-gram.
        f(bucket(fold(FNV_OFFSET ^ ORDER_SALT, c)));
        count += 1;
        // 2-gram ending here.
        if let Some(p1) = prev1 {
            f(bucket(fold(
                fold(FNV_OFFSET ^ ORDER_SALT.wrapping_mul(2), p1),
                c,
            )));
            count += 1;
            // 3-gram ending here.
            if let Some(p2) = prev2 {
                f(bucket(fold(
                    fold(fold(FNV_OFFSET ^ ORDER_SALT.wrapping_mul(3), p2), p1),
                    c,
                )));
                count += 1;
            }
        }
        prev2 = prev1;
        prev1 = Some(c);
    }
    count
}

/// Fixed-size inference scratch — construct once, reuse forever; `classify`
/// allocates nothing.
#[derive(Clone, Debug)]
pub struct ToneScratch {
    sum: [f32; EMBED],
    hid: [f32; HIDDEN],
    logits: [f32; TONE_COUNT],
}

impl Default for ToneScratch {
    fn default() -> Self {
        Self {
            sum: [0.0; EMBED],
            hid: [0.0; HIDDEN],
            logits: [0.0; TONE_COUNT],
        }
    }
}

/// Why a weight asset was refused. Every arm is a distinct byte-level fact so
/// the conformance test (and a future asset regeneration) can say exactly
/// what broke; the loader never panics.
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

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
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
    /// Decode + verify a weight asset. Byte-exact: any length drift, dim
    /// drift, or checksum mismatch is refused (no best-effort parsing — a
    /// half-read model would classify garbage with full confidence).
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

    /// Softmax scores in [`Tone::ALL`] order, or `None` when the text is too
    /// short to carry evidence ([`MIN_NGRAMS`]). Pure, deterministic,
    /// allocation-free (all state lives in `s`).
    pub fn scores(&self, text: &str, s: &mut ToneScratch) -> Option<[f32; TONE_COUNT]> {
        s.sum = [0.0; EMBED];
        let emb = &self.emb;
        let n = for_each_ngram_bucket(text, |bucket| {
            let row = bucket * EMBED;
            // `row + EMBED <= EMB_LEN` because bucket < BUCKETS by masking;
            // the slice indexing below can therefore never fail, but stay on
            // the checked path anyway — this feeds an audio-adjacent seam.
            if let Some(chunk) = emb.get(row..row + EMBED) {
                for (acc, &w) in s.sum.iter_mut().zip(chunk) {
                    *acc += w;
                }
            }
        });
        if n < MIN_NGRAMS {
            return None;
        }
        let inv = 1.0 / n as f32;
        // Hidden layer: tanh(b1 + e·W1).
        for (j, h) in s.hid.iter_mut().enumerate() {
            let mut acc = self.b1[j];
            for (i, &e) in s.sum.iter().enumerate() {
                acc += e * inv * self.w1[i * HIDDEN + j];
            }
            *h = acc.tanh();
        }
        // Output logits.
        for (c, l) in s.logits.iter_mut().enumerate() {
            let mut acc = self.b2[c];
            for (j, &h) in s.hid.iter().enumerate() {
                acc += h * self.w2[j * TONE_COUNT + c];
            }
            *l = acc;
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

    /// The verdict, ABSTENTION MADE EXPLICIT: the argmax tone, or `None` when
    /// the evidence is too thin to speak ([`MIN_NGRAMS`]) rather than folding
    /// that abstention into a real tone. Ties resolve to the LOWEST index —
    /// determinism over cleverness.
    ///
    /// Hosts that CACHE a mood across successive windows must use this: an
    /// evidence-thin window (a one-char line after Enter) has to LEAVE THE
    /// PRIOR VERDICT STANDING, not overwrite it — and it can only do that if
    /// abstention is distinguishable from a genuine neutral classification.
    pub fn classify_opt(&self, text: &str, s: &mut ToneScratch) -> Option<Tone> {
        let scores = self.scores(text, s)?;
        let mut best = 0usize;
        for (i, &p) in scores.iter().enumerate() {
            if p > scores[best] {
                best = i;
            }
        }
        Some(Tone::from_index(best))
    }

    /// The verdict: argmax tone, or the neutral [`Tone::Technical`] when the
    /// evidence is too thin. Ties resolve to the LOWEST index — determinism
    /// over cleverness. See [`classify_opt`](Self::classify_opt) for the
    /// abstention-preserving variant caching hosts need.
    pub fn classify(&self, text: &str, s: &mut ToneScratch) -> Tone {
        self.classify_opt(text, s).unwrap_or(Tone::Technical)
    }
}

/// Quantize + serialize full-precision weights into the asset format. Owned
/// by this module (beside the decoder) so the byte layout has exactly one
/// author; the offline trainer calls this, then round-trips the bytes back
/// through [`ToneModel::from_bytes`] and reports HOLDOUT accuracy of the
/// QUANTIZED model — what ships is what was measured.
///
/// Slices must have the compiled lengths; the function panics otherwise
/// (trainer-only code — a wrong-shaped tensor is a bug at the desk, not a
/// runtime input).
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

/// The shipped weights (`data/tone_model.atn`, produced by
/// `cargo run -p aterm-effects --example tone_train`). Loaded once;
/// `None` if the embedded asset fails verification — callers fall back to
/// the neutral tone rather than panicking an input-path frame (the
/// conformance suite pins that this is `Some` for the committed asset).
pub fn builtin() -> Option<&'static ToneModel> {
    static MODEL: OnceLock<Option<ToneModel>> = OnceLock::new();
    MODEL
        .get_or_init(|| ToneModel::from_bytes(include_bytes!("../data/tone_model.atn")).ok())
        .as_ref()
}

/// The committed held-out sanity set (never seen by SGD) — the conformance
/// tests' expectations file, exposed so hosts/tools can re-run the same
/// audit. Format: `label \t lang \t text`.
pub const HOLDOUT_TSV: &str = include_str!("../data/tone_holdout.tsv");

/// Parse one corpus/holdout TSV line (shared by the trainer and the
/// conformance tests). Blank lines and `#` comments yield `None`.
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

    /// The committed asset loads, byte-exactly, through the verifying
    /// decoder. This is the integrity pin: length, dims, and checksum all
    /// have to hold, so a truncated/corrupted/regenerated-but-not-committed
    /// asset fails HERE with a named reason instead of misclassifying
    /// quietly.
    #[test]
    fn shipped_weights_load_and_verify() {
        let bytes: &[u8] = include_bytes!("../data/tone_model.atn");
        assert_eq!(bytes.len(), ASSET_LEN, "asset length drifted");
        ToneModel::from_bytes(bytes).expect("shipped tone model must verify");
        assert!(builtin().is_some());
    }

    /// Every single-byte corruption class is refused with the right error —
    /// the loader is fail-closed, never best-effort.
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

    /// Same text ⇒ bit-identical scores, run after run — the classifier is a
    /// pure function (no clocks, no rng, no allocation-order dependence).
    #[test]
    fn inference_is_deterministic() {
        let m = builtin().expect("shipped model");
        let mut s1 = ToneScratch::default();
        let mut s2 = ToneScratch::default();
        for text in [
            "why is this broken again",
            "今日はとてもいい天気ですね",
            "git commit -m 'fix the seam'",
            "jajaja qué gracioso",
        ] {
            let a = m.scores(text, &mut s1).expect("long enough");
            let b = m.scores(text, &mut s2).expect("long enough");
            for (x, y) in a.iter().zip(b.iter()) {
                assert_eq!(x.to_bits(), y.to_bits(), "{text:?} drifted");
            }
        }
    }

    /// A near-empty line is evidence of nothing: below [`MIN_NGRAMS`] the
    /// model abstains and classification is the neutral Technical.
    #[test]
    fn thin_evidence_abstains_to_neutral() {
        let m = builtin().expect("shipped model");
        let mut s = ToneScratch::default();
        assert!(m.scores("", &mut s).is_none());
        assert!(m.scores("ok", &mut s).is_none());
        assert_eq!(m.classify("", &mut s), Tone::Technical);
        assert_eq!(m.classify("a", &mut s), Tone::Technical);
    }

    /// The abstention-preserving variant reports thin evidence as `None`
    /// (distinct from a genuine neutral verdict), so a caching host can leave
    /// its prior mood untouched. On a real line it agrees with [`classify`].
    #[test]
    fn classify_opt_reports_abstention_as_none() {
        let m = builtin().expect("shipped model");
        let mut s = ToneScratch::default();
        assert_eq!(m.classify_opt("", &mut s), None);
        assert_eq!(m.classify_opt("a", &mut s), None);
        let line = "why is this broken again ugh";
        assert_eq!(m.classify_opt(line, &mut s), Some(m.classify(line, &mut s)));
    }

    /// The inference budget: <100 µs per line, AVERAGED over a batch (a
    /// single wall-clock sample on shared CI would be noise). The lines are
    /// long (≈120 chars) and multilingual — the worst realistic case the
    /// input path will hand us at its throttled cadence.
    #[test]
    fn classification_fits_the_line_budget() {
        let m = builtin().expect("shipped model");
        let mut s = ToneScratch::default();
        let line = "why does the build break every time i touch this file 为什么又坏了 なんでまた壊れたの systemctl restart nginx --now ok";
        // Warm up (page in the weights).
        for _ in 0..16 {
            let _ = m.classify(line, &mut s);
        }
        let start = std::time::Instant::now();
        let iters = 1000u32;
        let mut sink = 0usize;
        for _ in 0..iters {
            sink = sink.wrapping_add(m.classify(line, &mut s).index());
        }
        let elapsed = start.elapsed();
        assert!(sink < usize::MAX, "keep the loop observable");
        let per_line = elapsed / iters;
        // The <100 µs contract is a property of the OPTIMIZED build the app
        // ships (measured ~5–20 µs there); an unoptimized test profile runs
        // the same arithmetic ~10–20× slower, so debug asserts a loose
        // sanity ceiling instead of failing on compiler flags.
        let budget = if cfg!(debug_assertions) {
            std::time::Duration::from_micros(2000)
        } else {
            std::time::Duration::from_micros(100)
        };
        assert!(
            per_line < budget,
            "budget blown: {per_line:?} per line (ceiling {budget:?})"
        );
    }

    /// The committed expectations file: the shipped QUANTIZED weights must
    /// clear a deliberately modest floor on the held-out set — overall and
    /// per focus language (zh/ja/ko/en/es). This pins that the committed
    /// asset is the one the trainer validated (asset integrity), NOT that
    /// the model is scientifically accurate; the floors are far below what a
    /// real sentiment system would claim and exactly what the melody hint
    /// needs.
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

    /// Multilingual smoke on unmistakable lines — one per script family,
    /// chosen from the TRAINING distribution's loudest markers (these are
    /// conformance pins on the committed weights, not generalization
    /// claims).
    #[test]
    fn canonical_lines_land_in_plausible_classes() {
        let m = builtin().expect("shipped model");
        let mut s = ToneScratch::default();
        for (text, want) in [
            (
                "git rebase -i HEAD~3 && cargo test --workspace",
                Tone::Technical,
            ),
            ("why is this broken again ugh", Tone::Frustrated),
            ("lol that cat video is so silly hehe", Tone::Playful),
            ("ㅋㅋㅋㅋ 너무 웃겨", Tone::Playful),
            ("それはいいですね、ゆっくりで大丈夫です", Tone::Calm),
            ("太棒了！我们成功了！", Tone::Excited),
            ("qué emoción, lo logramos!!", Tone::Excited),
        ] {
            assert_eq!(m.classify(text, &mut s), want, "{text:?}");
        }
    }

    /// Featurization is script-agnostic and shared: CJK text produces a full
    /// n-gram stream (no tokenizer needed), whitespace runs collapse, and
    /// ASCII case folds — the exact properties the trainer relies on.
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
    }

    /// Encode → decode is the trainer's exact path: a synthetic model
    /// round-trips through the asset bytes with i8 quantization applied
    /// (values land on the quantized lattice, not the originals).
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
