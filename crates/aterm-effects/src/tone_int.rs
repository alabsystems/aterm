// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! tone-int v1: the shipped tone classifier derived by a pure, libm-free function into an `int32-exact/v1` params blob, with its featurization, workload and pins (design §12.2, §12.8).

use std::sync::OnceLock;

use aterm_aos::Digest;

use crate::int_bag_mlp::{
    self, Batch, BatchError, Dims, LAYOUT_VERSION, PROFILE, ParamsError, ParamsView, Spec, Tensor,
};
use crate::tone::{self, Tone, ToneModelError};

#[cfg(test)]
mod tests;

/// The domain under which [`id`] digests a tone-int descriptor.
pub const TONE_INT_DOMAIN: &[u8] = b"aterm-effects-tone-int-v1\0";
/// The domain under which [`workload_digest`] digests the workload's texts.
pub const WORKLOAD_DOMAIN: &[u8] = b"aterm-effects-tone-int-workload-v1\0";

/// The activation grid's fractional bits: p counts steps of 2^−F.
pub const F: u32 = 8;
/// The step-5 shift.
pub const G: u32 = 20;
/// The step-6 clamp, 4·2^F, where tanh is within 0.0007 of ±1.
pub const T: i32 = 4 << F;
/// tone-int's dimensions, the shipped classifier's.
pub const DIMS: Dims = Dims {
    buckets: tone::BUCKETS as u32,
    embed: tone::EMBED as u32,
    hidden: tone::HIDDEN as u32,
    classes: tone::TONE_COUNT as u32,
};
/// The fewest ids a row needs not to abstain, the shipped [`tone::MIN_NGRAMS`].
pub const MIN_IDS: u32 = tone::MIN_NGRAMS as u32;

/// Why [`derive`] refused an asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeriveError {
    /// The asset failed the shipped checks.
    Asset(ToneModelError),
    /// An asset tensor holds −128, which the profile refuses.
    Int8Min {
        /// Which.
        tensor: Tensor,
        /// The first element that does.
        index: u32,
    },
    /// A derived constant does not fit its word.
    OutOfRange {
        /// `MULT`, `b1`, `b2` or `lut`.
        what: &'static str,
        /// Its element.
        index: u32,
    },
    /// The derived blob failed the params checker.
    Params(ParamsError),
}

/// `x` rounded to the nearest integer with halves away from zero, by a truncating cast and an exact binary64 subtraction; `None` unless `|x| < 2⁵²`.
#[must_use]
#[inline]
pub const fn round_half_away(x: f64) -> Option<i64> {
    const LIMIT: f64 = 4_503_599_627_370_496.0;
    if x.is_nan() || x <= -LIMIT || x >= LIMIT {
        return None;
    }
    let t = x as i64;
    let frac = x - t as f64;
    Some(if frac >= 0.5 {
        t + 1
    } else if frac <= -0.5 {
        t - 1
    } else {
        t
    })
}

/// tanh by Lambert's continued fraction, evaluated bottom-up from t = 47 with binary64 `+`, `×` and `÷` alone (design §12.2).
#[must_use]
#[inline]
pub const fn tanh(x: f64) -> f64 {
    let x2 = x * x;
    let mut t = 47.0;
    let mut k = 23u32;
    while k >= 1 {
        t = (2 * k - 1) as f64 + x2 / t;
        k -= 1;
    }
    x / t
}

/// Derive tone-int's params blob from a tone asset: its i8 tensors verbatim and MULT, b1, b2 and the tanh table by the fixed-point rules of design §12.2, checked by [`int_bag_mlp::check`].
pub fn derive(asset: &[u8]) -> Result<Vec<u8>, DeriveError> {
    const fn pow2(n: u32) -> f64 {
        (1u32 << n) as f64
    }
    const fn fits(x: f64, what: &'static str, index: usize) -> Result<i64, DeriveError> {
        match round_half_away(x) {
            Some(v) => Ok(v),
            None => Err(DeriveError::OutOfRange {
                what,
                index: index as u32,
            }),
        }
    }
    const fn narrow(v: i64, what: &'static str, index: usize) -> Result<i32, DeriveError> {
        if v < i32::MIN as i64 || v > i32::MAX as i64 {
            Err(DeriveError::OutOfRange {
                what,
                index: index as u32,
            })
        } else {
            Ok(v as i32)
        }
    }
    let raw = tone::raw(asset).map_err(DeriveError::Asset)?;
    let int8 = |tensor: Tensor, bytes: &[u8]| -> Result<Vec<i8>, DeriveError> {
        let mut out = Vec::with_capacity(bytes.len());
        for (index, &b) in bytes.iter().enumerate() {
            if b == 0x80 {
                return Err(DeriveError::Int8Min {
                    tensor,
                    index: index as u32,
                });
            }
            out.push(b as i8);
        }
        Ok(out)
    };
    let emb = int8(Tensor::Emb, raw.emb())?;
    let w1 = int8(Tensor::W1, raw.w1())?;
    let w2 = int8(Tensor::W2, raw.w2())?;
    let mult = fits(
        f64::from(raw.emb_scale()) * f64::from(raw.w1_scale()) * pow2(F + G),
        "MULT",
        0,
    )?;
    let mult = u32::try_from(mult).map_err(|_| DeriveError::OutOfRange {
        what: "MULT",
        index: 0,
    })?;
    let mut b1 = [0i32; tone::HIDDEN];
    for (k, &x) in raw.b1().iter().enumerate() {
        b1[k] = narrow(fits(f64::from(x) * pow2(F), "b1", k)?, "b1", k)?;
    }
    let s2 = f64::from(raw.w2_scale());
    let mut b2 = [0i32; tone::TONE_COUNT];
    for (c, &x) in raw.b2().iter().enumerate() {
        b2[c] = narrow(fits(f64::from(x) * 127.0 / s2, "b2", c)?, "b2", c)?;
    }
    let lut_len = (2 * T + 1) as usize;
    let mut lut = Vec::with_capacity(lut_len);
    let inv_pow2_f = 1.0 / pow2(F);
    for (i, at) in (0..=2 * T).enumerate() {
        let v = fits(127.0 * tanh(f64::from(at - T) * inv_pow2_f), "lut", i)?;
        let q = i8::try_from(v).map_err(|_| DeriveError::OutOfRange {
            what: "lut",
            index: i as u32,
        })?;
        lut.push(q);
    }
    let bytes = int_bag_mlp::build(&Spec {
        dims: DIMS,
        min_ids: MIN_IDS,
        mult,
        g: G,
        t: T,
        emb: &emb,
        w1: &w1,
        w2: &w2,
        lut: &lut,
        b1: &b1,
        b2: &b2,
    });
    int_bag_mlp::check(&bytes).map_err(DeriveError::Params)?;
    Ok(bytes)
}

/// tone-int v1's params blob, [`derive`] of the shipped [`tone::ASSET`], derived once.
pub fn v1() -> Result<&'static [u8], DeriveError> {
    static V1: OnceLock<Result<Vec<u8>, DeriveError>> = OnceLock::new();
    match V1.get_or_init(|| derive(tone::ASSET)) {
        Ok(bytes) => Ok(bytes),
        Err(e) => Err(*e),
    }
}

/// tone-int's id: `Digest::of(TONE_INT_DOMAIN, [LAYOUT_VERSION, PROFILE, B, E, H, C, the params bytes])`, each number a little-endian u32.
pub fn id(p: &ParamsView<'_>) -> Digest {
    let d = p.dims();
    Digest::of(
        TONE_INT_DOMAIN,
        &[
            &LAYOUT_VERSION.to_le_bytes(),
            PROFILE.as_bytes(),
            &d.buckets.to_le_bytes(),
            &d.embed.to_le_bytes(),
            &d.hidden.to_le_bytes(),
            &d.classes.to_le_bytes(),
            p.bytes(),
        ],
    )
}

/// Append `text`'s n-gram buckets to `ids` through the shipped [`tone::for_each_ngram_bucket`], returning how many it emitted.
pub fn featurize(text: &str, ids: &mut Vec<u16>) -> usize {
    tone::for_each_ngram_bucket(text, |b| {
        ids.push(u16::try_from(b).unwrap_or(u16::MAX));
    })
}

/// Featurize `texts` into a [`Batch`] for `p`, one row per text in order.
pub fn pack(p: &ParamsView<'_>, texts: &[&str]) -> Result<Batch, BatchError> {
    pack_into(p, texts, Vec::new(), Vec::new())
}

/// [`pack`] into the capacity of `offsets` and `ids`, as [`Batch::into_parts`] gives them back.
pub fn pack_into(
    p: &ParamsView<'_>,
    texts: &[&str],
    mut offsets: Vec<u32>,
    mut ids: Vec<u16>,
) -> Result<Batch, BatchError> {
    offsets.clear();
    ids.clear();
    offsets.push(0);
    for text in texts {
        featurize(text, &mut ids);
        offsets.push(u32::try_from(ids.len()).unwrap_or(u32::MAX));
    }
    Batch::pack(p, offsets, ids)
}

/// The training corpus, `data/tone_corpus.tsv`.
pub const CORPUS_TSV: &str = include_str!("../data/tone_corpus.tsv");
/// The held-out set, `data/tone_holdout.tsv`.
pub const HOLDOUT_TSV: &str = include_str!("../data/tone_holdout.tsv");
/// The corpus's lines.
pub const CORPUS_LINES: usize = 300;
/// The held-out set's lines.
pub const HOLDOUT_LINES: usize = 71;
/// The workload's lines, the corpus's then the held-out set's.
pub const WORKLOAD_LINES: usize = CORPUS_LINES + HOLDOUT_LINES;
/// B1's text, the corpus's first line.
pub const B1_TEXT: &str = "ok sounds good to me";

/// One workload line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    /// Its label.
    pub tone: Tone,
    /// Its language column.
    pub lang: &'static str,
    /// Its text.
    pub text: &'static str,
    /// Whether it is a held-out line.
    pub held_out: bool,
}

/// The workload: every corpus line then every held-out line, in file order, as [`tone::parse_corpus_line`] reads them.
pub fn workload() -> Vec<Line> {
    let mut out = Vec::with_capacity(WORKLOAD_LINES);
    for (tsv, held_out) in [(CORPUS_TSV, false), (HOLDOUT_TSV, true)] {
        for l in tsv.lines() {
            if let Some((tone, lang, text)) = tone::parse_corpus_line(l) {
                out.push(Line {
                    tone,
                    lang,
                    text,
                    held_out,
                });
            }
        }
    }
    out
}

/// The workload's texts, in order.
pub fn texts() -> Vec<&'static str> {
    let lines = workload();
    let mut out = Vec::with_capacity(lines.len());
    for l in lines {
        out.push(l.text);
    }
    out
}

/// `Digest::of(WORKLOAD_DOMAIN, [each workload text])`.
pub fn workload_digest() -> Digest {
    let texts = texts();
    let mut parts = Vec::with_capacity(texts.len());
    for t in &texts {
        parts.push(t.as_bytes());
    }
    Digest::of(WORKLOAD_DOMAIN, &parts)
}

/// A class as [`F_ARGMAX_371`] codes it: the tone's index, or C (abstain) for `None`.
#[must_use]
#[inline]
pub const fn class_code(tone: Option<Tone>) -> u8 {
    match tone {
        Some(t) => t.index() as u8,
        None => DIMS.classes as u8,
    }
}

/// ASCII digits to their values, at compile time.
const fn digits<const N: usize>(s: &[u8; N]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        assert!(s[i].is_ascii_digit());
        out[i] = s[i] - b'0';
        i += 1;
    }
    out
}

/// tone-int v1's [`id`].
pub const V1_ID: &str = "f2299a7c94d530521d5a04bb7dd137c6c36a72d4c86e72d80e3b5faa77919823";
/// [`workload_digest`].
pub const WORKLOAD_DIGEST: &str =
    "a1e0780d3c01fde2afcfcdc70909f0c76f265115f1569ab2b53f4cda30410ac3";
/// [`int_bag_mlp::out_digest`] of [`int_bag_mlp::forward`] by v1 over the workload as one batch (B371).
pub const B371_OUT_DIGEST: &str =
    "2b7e3012199b49ac429ec6d4137a4ab189810a6dcf862373a5bd8326e47849c5";
/// The shipped `ToneModel::classify_opt` per workload line as [`class_code`]s, computed with macOS 27.0.1 (26A434)'s libm `tanhf` on aarch64, so an OS update may move it.
pub const F_ARGMAX_371: [u8; WORKLOAD_LINES] = digits(
    b"0000000000000000000000000000000000000000000000000000000000000111111111\
    1111111111111111111111111111111111111111111111111112222222222222222222\
    2222222222222222222222222222222222222222233333333333333333333333333333\
    3333333333333333333333333333334444444444444444444444444444444444444444\
    4444444444444444444402000000000000111121111111112222222122222233433433\
    333343444244444442324",
);
