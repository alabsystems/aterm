// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The integer bag-of-ids MLP job kind under the numerical profile `int32-exact/v1` (design §12.2–12.3): its layout, params builder and checker, batch packer and CPU reference.

use core::fmt;
use core::ops::Range;

/// The digest type of every identity this job kind pins.
pub use aterm_aos::Digest;

#[cfg(test)]
mod tests;

/// The numerical profile every operation of this job kind belongs to.
pub const PROFILE: &str = "int32-exact/v1";
/// The layout version the `job` and `params` headers carry.
pub const LAYOUT_VERSION: u32 = 1;
/// The `job` header's magic word, `IBMJ` little-endian.
pub const JOB_MAGIC_VALUE: u32 = u32::from_le_bytes(*b"IBMJ");
/// The `params` header's magic word, `IBMP` little-endian.
pub const PARAMS_MAGIC_VALUE: u32 = u32::from_le_bytes(*b"IBMP");

/// The largest bucket count B.
pub const B_MAX: u32 = 65_536;
/// The largest embedding width E.
pub const E_MAX: u32 = 32;
/// The largest hidden width H.
pub const H_MAX: u32 = 128;
/// The largest class count C.
pub const C_MAX: u32 = 16;
/// The most ids one row may hold, so that `127²·E·n < 2³¹` at E = [`E_MAX`].
pub const MAX_ROW_IDS: u32 = 4_096;
/// The most rows one batch may hold.
pub const ROWS_MAX: u32 = 1 << 20;
/// The declared budget of one job's four buffers, summed.
pub const JOB_BYTES_MAX: u64 = 64 << 20;
/// The declared budget of one params blob.
pub const PARAMS_BYTES_MAX: u64 = 16 << 20;
/// The declared budget of every live buffer of one compute domain, which aterm-gpu's compute module enforces.
pub const DOMAIN_BYTES_MAX: u64 = 256 << 20;
/// The exclusive bound on every `|b2[c]|`.
pub const B2_LIMIT: i32 = 1 << 29;
/// The alignment of every tensor in a params blob, in bytes.
pub const TENSOR_ALIGN: u32 = 16;
/// The domain under which [`out_digest`] digests output words.
pub const OUT_DOMAIN: &[u8] = b"aterm-effects-int-bag-mlp-out-v1\0";

const _: () = {
    assert!(127 * 127 * E_MAX as u64 * MAX_ROW_IDS as u64 <= i32::MAX as u64);
    assert!(127 * 127 * H_MAX as u64 + B2_LIMIT as u64 <= i32::MAX as u64);
};

const E_CAP: usize = E_MAX as usize;
const H_CAP: usize = H_MAX as usize;
const C_CAP: usize = C_MAX as usize;

/// Declares a header's word indices once, in a module, and the table the kernel prelude is generated from.
macro_rules! layout {
    ($(#[$m:meta])* $module:ident, $prefix:literal, $fields:ident, $words:ident;
     $($(#[$fm:meta])* $name:ident = $index:literal,)+) => {
        $(#[$m])*
        pub mod $module {
            $($(#[$fm])* pub const $name: usize = $index;)+
        }
        #[doc = concat!("The `", stringify!($module), "` header's fields in word order, each by the name the kernel prelude gives its index.")]
        pub const $fields: &[(&str, usize)] = &[$((concat!($prefix, stringify!($name)), $module::$name),)+];
        #[doc = concat!("The `", stringify!($module), "` header's length in words.")]
        pub const $words: usize = $fields.len();
    };
}

layout! {
    /// Word indices of the `job` header (binding 0).
    job, "JOB_", JOB_FIELDS, JOB_WORDS;
    /// [`JOB_MAGIC_VALUE`](super::JOB_MAGIC_VALUE).
    MAGIC = 0,
    /// [`LAYOUT_VERSION`](super::LAYOUT_VERSION).
    LAYOUT_VERSION = 1,
    /// The row count.
    ROWS = 2,
    /// The id count, `offsets[ROWS]`.
    TOTAL_IDS = 3,
    /// The out words per row, C + 1.
    OUT_STRIDE = 4,
}

layout! {
    /// Word indices of the `params` header (binding 1); each `*_OFF` is a byte offset into the blob and each `*_LEN` an element count.
    params, "PARAMS_", PARAMS_FIELDS, PARAMS_WORDS;
    /// [`PARAMS_MAGIC_VALUE`](super::PARAMS_MAGIC_VALUE).
    MAGIC = 0,
    /// [`LAYOUT_VERSION`](super::LAYOUT_VERSION).
    LAYOUT_VERSION = 1,
    /// The bucket count B.
    B = 2,
    /// The embedding width E.
    E = 3,
    /// The hidden width H.
    H = 4,
    /// The class count C.
    C = 5,
    /// The fewest ids a row needs not to abstain.
    MIN_IDS = 6,
    /// The step-5 multiplier.
    MULT = 7,
    /// The step-5 clamp, `⌊(2³¹ − 1) / MULT⌋`.
    QMAX = 8,
    /// The step-5 shift.
    G = 9,
    /// The step-6 clamp, an i32 in two's complement.
    T = 10,
    /// The embedding table's byte offset.
    EMB_OFF = 11,
    /// The embedding table's length, B·E.
    EMB_LEN = 12,
    /// The first layer's byte offset.
    W1_OFF = 13,
    /// The first layer's length, E·H.
    W1_LEN = 14,
    /// The output layer's byte offset.
    W2_OFF = 15,
    /// The output layer's length, H·C.
    W2_LEN = 16,
    /// The activation table's byte offset.
    LUT_OFF = 17,
    /// The activation table's length, 2T + 1.
    LUT_LEN = 18,
    /// The first layer's biases' byte offset.
    B1_OFF = 19,
    /// The first layer's bias count, H.
    B1_LEN = 20,
    /// The output layer's biases' byte offset.
    B2_OFF = 21,
    /// The output layer's bias count, C.
    B2_LEN = 22,
}

/// Both headers' tables, the one source of every header index.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// The `job` header's fields.
    pub job: &'static [(&'static str, usize)],
    /// The `params` header's fields.
    pub params: &'static [(&'static str, usize)],
}

impl Layout {
    /// Every field, the `job` header's then the `params` header's, in word order.
    pub fn fields(&self) -> impl Iterator<Item = (&'static str, usize)> {
        self.job.iter().chain(self.params).copied()
    }
}

/// The layout of `int32-exact/v1`.
pub const LAYOUT: Layout = Layout {
    job: JOB_FIELDS,
    params: PARAMS_FIELDS,
};

/// The dimensions B, E, H and C.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Dims {
    /// B: ids lie below it.
    pub buckets: u32,
    /// E: the embedding width.
    pub embed: u32,
    /// H: the hidden width.
    pub hidden: u32,
    /// C: the class count, and the abstaining class.
    pub classes: u32,
}

impl Dims {
    /// The out words per row: C logits then the class.
    #[must_use]
    #[inline]
    pub const fn out_stride(self) -> u32 {
        self.classes + 1
    }
}

/// One of the four dimensions, as a refusal names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dim {
    /// B.
    Buckets,
    /// E.
    Embed,
    /// H.
    Hidden,
    /// C.
    Classes,
}

/// One of a params blob's six tensors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tensor {
    /// The i8 embedding table, B × E.
    Emb,
    /// The i8 first layer, E × H.
    W1,
    /// The i8 output layer, H × C.
    W2,
    /// The i8 activation table, 2T + 1.
    Lut,
    /// The i32 first-layer biases, H.
    B1,
    /// The i32 output biases, C.
    B2,
}

impl Tensor {
    /// The tensors in blob order.
    pub const ALL: [Tensor; 6] = [
        Tensor::Emb,
        Tensor::W1,
        Tensor::W2,
        Tensor::Lut,
        Tensor::B1,
        Tensor::B2,
    ];

    /// The header indices of its byte offset and its length.
    #[must_use]
    #[inline]
    pub const fn fields(self) -> (usize, usize) {
        match self {
            Tensor::Emb => (params::EMB_OFF, params::EMB_LEN),
            Tensor::W1 => (params::W1_OFF, params::W1_LEN),
            Tensor::W2 => (params::W2_OFF, params::W2_LEN),
            Tensor::Lut => (params::LUT_OFF, params::LUT_LEN),
            Tensor::B1 => (params::B1_OFF, params::B1_LEN),
            Tensor::B2 => (params::B2_OFF, params::B2_LEN),
        }
    }

    /// The bytes one element takes.
    #[must_use]
    #[inline]
    pub const fn elem_bytes(self) -> u32 {
        match self {
            Tensor::B1 | Tensor::B2 => 4,
            Tensor::Emb | Tensor::W1 | Tensor::W2 | Tensor::Lut => 1,
        }
    }
}

/// `⌊(2³¹ − 1) / mult⌋`, the step-5 clamp, or 0 for a zero `mult`.
#[must_use]
#[inline]
pub const fn qmax(mult: u32) -> u32 {
    match (i32::MAX as u32).checked_div(mult) {
        Some(q) => q,
        None => 0,
    }
}

/// What [`build`] writes into a params blob, each tensor row-major.
#[derive(Clone, Copy, Debug)]
pub struct Spec<'a> {
    /// B, E, H and C.
    pub dims: Dims,
    /// The fewest ids a row needs not to abstain.
    pub min_ids: u32,
    /// The step-5 multiplier.
    pub mult: u32,
    /// The step-5 shift.
    pub g: u32,
    /// The step-6 clamp.
    pub t: i32,
    /// The embedding table, B × E.
    pub emb: &'a [i8],
    /// The first layer, E × H.
    pub w1: &'a [i8],
    /// The output layer, H × C.
    pub w2: &'a [i8],
    /// The activation table, 2T + 1.
    pub lut: &'a [i8],
    /// The first-layer biases, H.
    pub b1: &'a [i32],
    /// The output biases, C.
    pub b2: &'a [i32],
}

/// Write `spec` as a params blob, the header (QMAX computed from MULT) then each tensor at the next 16-byte-aligned offset, checking nothing: [`check`] refuses what it must.
pub fn build(spec: &Spec<'_>) -> Vec<u8> {
    let word = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    let mut h = [0u32; PARAMS_WORDS];
    h[params::MAGIC] = PARAMS_MAGIC_VALUE;
    h[params::LAYOUT_VERSION] = LAYOUT_VERSION;
    h[params::B] = spec.dims.buckets;
    h[params::E] = spec.dims.embed;
    h[params::H] = spec.dims.hidden;
    h[params::C] = spec.dims.classes;
    h[params::MIN_IDS] = spec.min_ids;
    h[params::MULT] = spec.mult;
    h[params::QMAX] = qmax(spec.mult);
    h[params::G] = spec.g;
    h[params::T] = spec.t as u32;
    let bytes = |v: &[i8]| v.iter().map(|&x| x as u8).collect::<Vec<u8>>();
    let words = |v: &[i32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let mut out = vec![0u8; PARAMS_WORDS * 4];
    for t in Tensor::ALL {
        out.resize(out.len().next_multiple_of(TENSOR_ALIGN as usize), 0);
        let (off, len) = t.fields();
        h[off] = word(out.len());
        let (count, body) = match t {
            Tensor::Emb => (spec.emb.len(), bytes(spec.emb)),
            Tensor::W1 => (spec.w1.len(), bytes(spec.w1)),
            Tensor::W2 => (spec.w2.len(), bytes(spec.w2)),
            Tensor::Lut => (spec.lut.len(), bytes(spec.lut)),
            Tensor::B1 => (spec.b1.len(), words(spec.b1)),
            Tensor::B2 => (spec.b2.len(), words(spec.b2)),
        };
        h[len] = word(count);
        out.extend_from_slice(&body);
    }
    for (slot, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *slot = w.to_le_bytes();
    }
    out
}

/// Why [`check`] refused a params blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamsError {
    /// The blob is larger than [`PARAMS_BYTES_MAX`].
    TooLarge {
        /// Its length.
        bytes: u64,
        /// The budget.
        limit: u64,
    },
    /// The blob is shorter than its header.
    Truncated {
        /// Its length.
        bytes: u64,
    },
    /// The magic word is not [`PARAMS_MAGIC_VALUE`].
    BadMagic {
        /// The word found.
        found: u32,
    },
    /// The layout version is not [`LAYOUT_VERSION`].
    BadVersion {
        /// The word found.
        found: u32,
    },
    /// A dimension lies outside `1..=` its maximum.
    DimOutOfBounds {
        /// Which.
        dim: Dim,
        /// Its value.
        value: u32,
    },
    /// MIN_IDS is 0, so step 4 could divide by zero.
    MinIdsZero,
    /// G lies outside 1..=31.
    ShiftOutOfRange {
        /// Its value.
        g: u32,
    },
    /// T is negative.
    NegativeT {
        /// Its value.
        t: i32,
    },
    /// MULT is 0 or at least 2³¹.
    MultOutOfRange {
        /// Its value.
        mult: u32,
    },
    /// The stored QMAX is not `⌊(2³¹ − 1) / MULT⌋`.
    QmaxMismatch {
        /// The word found.
        stored: u32,
        /// The value recomputed from MULT.
        want: u32,
    },
    /// A tensor's length is not its dimensions' product.
    BadLength {
        /// Which.
        tensor: Tensor,
        /// The length stored.
        len: u32,
        /// The product.
        want: u64,
    },
    /// A tensor ends past the blob.
    OutsideBlob {
        /// Which.
        tensor: Tensor,
        /// Its byte offset.
        offset: u32,
        /// Its end.
        end: u64,
    },
    /// A tensor's offset is not 16-byte aligned.
    Unaligned {
        /// Which.
        tensor: Tensor,
        /// Its byte offset.
        offset: u32,
    },
    /// A tensor overlaps the header or an earlier tensor.
    Overlap {
        /// Which.
        tensor: Tensor,
    },
    /// Bytes follow the last tensor.
    Trailing {
        /// How many.
        bytes: u64,
    },
    /// An i8 tensor holds −128, outside [−127, 127].
    Int8Min {
        /// Which.
        tensor: Tensor,
        /// The first element that does.
        index: u32,
    },
    /// `M − max|b1| < T`, so step 5's clamp would not be lossless.
    Lossy {
        /// `⌊QMAX·MULT / 2^G⌋`.
        m: i64,
        /// The largest `|b1[k]|`.
        max_b1: i64,
        /// T.
        t: i32,
    },
    /// A `|b2[c]|` is at least 2²⁹.
    B2OutOfRange {
        /// Its class.
        class: u32,
        /// Its value.
        value: i32,
    },
}

/// A params blob [`check`] accepted, with its header read.
#[derive(Clone, Copy)]
pub struct ParamsView<'a> {
    bytes: &'a [u8],
    dims: Dims,
    min_ids: u32,
    mult: u32,
    qmax: u32,
    g: u32,
    t: i32,
    emb: &'a [u8],
    w1: &'a [u8],
    w2: &'a [u8],
    lut: &'a [u8],
    b1: [i32; H_CAP],
    b2: [i32; C_CAP],
}

impl fmt::Debug for ParamsView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParamsView")
            .field("bytes", &self.bytes.len())
            .field("dims", &self.dims)
            .field("min_ids", &self.min_ids)
            .field("mult", &self.mult)
            .field("g", &self.g)
            .field("t", &self.t)
            .finish_non_exhaustive()
    }
}

/// The little-endian u32 at word `i` of `bytes`, which holds it.
#[inline]
const fn word_at(bytes: &[u8], i: usize) -> u32 {
    let off = i * 4;
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

/// Refuse, by name, every params blob outside the profile's bounds (design §12.2), before any buffer exists.
pub fn check(bytes: &[u8]) -> Result<ParamsView<'_>, ParamsError> {
    let blob = bytes.len() as u64;
    if blob > PARAMS_BYTES_MAX {
        return Err(ParamsError::TooLarge {
            bytes: blob,
            limit: PARAMS_BYTES_MAX,
        });
    }
    if bytes.len() < PARAMS_WORDS * 4 {
        return Err(ParamsError::Truncated { bytes: blob });
    }
    let w = |i: usize| word_at(bytes, i);
    if w(params::MAGIC) != PARAMS_MAGIC_VALUE {
        return Err(ParamsError::BadMagic {
            found: w(params::MAGIC),
        });
    }
    if w(params::LAYOUT_VERSION) != LAYOUT_VERSION {
        return Err(ParamsError::BadVersion {
            found: w(params::LAYOUT_VERSION),
        });
    }
    let dims = Dims {
        buckets: w(params::B),
        embed: w(params::E),
        hidden: w(params::H),
        classes: w(params::C),
    };
    for (dim, value, max) in [
        (Dim::Buckets, dims.buckets, B_MAX),
        (Dim::Embed, dims.embed, E_MAX),
        (Dim::Hidden, dims.hidden, H_MAX),
        (Dim::Classes, dims.classes, C_MAX),
    ] {
        if !(1..=max).contains(&value) {
            return Err(ParamsError::DimOutOfBounds { dim, value });
        }
    }
    let min_ids = w(params::MIN_IDS);
    if min_ids < 1 {
        return Err(ParamsError::MinIdsZero);
    }
    let g = w(params::G);
    if !(1..=31).contains(&g) {
        return Err(ParamsError::ShiftOutOfRange { g });
    }
    let t = w(params::T) as i32;
    if t < 0 {
        return Err(ParamsError::NegativeT { t });
    }
    let mult = w(params::MULT);
    if mult < 1 || mult > i32::MAX as u32 {
        return Err(ParamsError::MultOutOfRange { mult });
    }
    let stored = w(params::QMAX);
    if stored != qmax(mult) {
        return Err(ParamsError::QmaxMismatch {
            stored,
            want: qmax(mult),
        });
    }
    let (b, e, h, c) = (
        u64::from(dims.buckets),
        u64::from(dims.embed),
        u64::from(dims.hidden),
        u64::from(dims.classes),
    );
    let header_end = (PARAMS_WORDS * 4) as u64;
    let mut spans = [(0u64, 0u64, Tensor::Emb); 6];
    for (slot, tensor) in spans.iter_mut().zip(Tensor::ALL) {
        let (off_at, len_at) = tensor.fields();
        let (offset, len) = (w(off_at), w(len_at));
        let want = match tensor {
            Tensor::Emb => b * e,
            Tensor::W1 => e * h,
            Tensor::W2 => h * c,
            Tensor::Lut => 2 * t as u64 + 1,
            Tensor::B1 => h,
            Tensor::B2 => c,
        };
        if u64::from(len) != want {
            return Err(ParamsError::BadLength { tensor, len, want });
        }
        let end = u64::from(offset) + u64::from(len) * u64::from(tensor.elem_bytes());
        if end > blob {
            return Err(ParamsError::OutsideBlob {
                tensor,
                offset,
                end,
            });
        }
        if !offset.is_multiple_of(TENSOR_ALIGN) {
            return Err(ParamsError::Unaligned { tensor, offset });
        }
        *slot = (u64::from(offset), end, tensor);
    }
    let mut ordered = spans;
    ordered.sort_by_key(|s| s.0);
    let mut covered = header_end;
    for &(start, end, tensor) in &ordered {
        if start < covered {
            return Err(ParamsError::Overlap { tensor });
        }
        covered = end;
    }
    if blob > covered {
        return Err(ParamsError::Trailing {
            bytes: blob - covered,
        });
    }
    let slice = |tensor: Tensor| -> &[u8] {
        let (start, end, _) = spans[tensor as usize];
        &bytes[start as usize..end as usize]
    };
    let (emb, w1, w2, lut) = (
        slice(Tensor::Emb),
        slice(Tensor::W1),
        slice(Tensor::W2),
        slice(Tensor::Lut),
    );
    for (tensor, data) in [
        (Tensor::Emb, emb),
        (Tensor::W1, w1),
        (Tensor::W2, w2),
        (Tensor::Lut, lut),
    ] {
        if let Some(index) = data.iter().position(|&x| x == 0x80) {
            return Err(ParamsError::Int8Min {
                tensor,
                index: index as u32,
            });
        }
    }
    let mut b1 = [0i32; H_CAP];
    for (slot, chunk) in b1.iter_mut().zip(slice(Tensor::B1).as_chunks::<4>().0) {
        *slot = i32::from_le_bytes(*chunk);
    }
    let mut b2 = [0i32; C_CAP];
    for (slot, chunk) in b2.iter_mut().zip(slice(Tensor::B2).as_chunks::<4>().0) {
        *slot = i32::from_le_bytes(*chunk);
    }
    let m = (i64::from(stored) * i64::from(mult)) >> g;
    let max_b1 = b1.iter().map(|&x| i64::from(x).abs()).max().unwrap_or(0);
    if m - max_b1 < i64::from(t) {
        return Err(ParamsError::Lossy { m, max_b1, t });
    }
    if let Some((class, &value)) = b2
        .iter()
        .enumerate()
        .find(|&(_, &x)| i64::from(x).abs() >= i64::from(B2_LIMIT))
    {
        return Err(ParamsError::B2OutOfRange {
            class: class as u32,
            value,
        });
    }
    Ok(ParamsView {
        bytes,
        dims,
        min_ids,
        mult,
        qmax: stored,
        g,
        t,
        emb,
        w1,
        w2,
        lut,
        b1,
        b2,
    })
}

impl<'a> ParamsView<'a> {
    /// The whole blob, header included: the bytes the GPU's `params` buffer holds.
    #[must_use]
    #[inline]
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// B, E, H and C.
    #[must_use]
    #[inline]
    pub const fn dims(&self) -> Dims {
        self.dims
    }

    /// The fewest ids a row needs not to abstain, at least 1.
    #[must_use]
    #[inline]
    pub const fn min_ids(&self) -> u32 {
        self.min_ids
    }

    /// The step-5 multiplier.
    #[must_use]
    #[inline]
    pub const fn mult(&self) -> u32 {
        self.mult
    }

    /// The step-5 clamp, as recomputed from MULT.
    #[must_use]
    #[inline]
    pub const fn qmax(&self) -> u32 {
        self.qmax
    }

    /// The step-5 shift.
    #[must_use]
    #[inline]
    pub const fn g(&self) -> u32 {
        self.g
    }

    /// The step-6 clamp.
    #[must_use]
    #[inline]
    pub const fn t(&self) -> i32 {
        self.t
    }

    /// `⌊QMAX·MULT / 2^G⌋`, the largest step-5 magnitude before the bias.
    #[must_use]
    #[inline]
    pub const fn m(&self) -> u32 {
        ((self.qmax as u64 * self.mult as u64) >> self.g) as u32
    }

    /// The largest `|b1[k]|`.
    #[must_use]
    #[inline]
    pub const fn max_abs_b1(&self) -> u32 {
        let b1 = self.b1();
        let mut max = 0u32;
        let mut i = 0;
        while i < b1.len() {
            let v = b1[i].unsigned_abs();
            if v > max {
                max = v;
            }
            i += 1;
        }
        max
    }

    /// The header's words.
    #[must_use]
    pub const fn header(&self) -> [u32; PARAMS_WORDS] {
        let mut out = [0u32; PARAMS_WORDS];
        let mut i = 0;
        while i < PARAMS_WORDS {
            out[i] = word_at(self.bytes, i);
            i += 1;
        }
        out
    }

    /// The embedding table, B × E row-major, one two's-complement i8 per byte.
    #[must_use]
    #[inline]
    pub const fn emb(&self) -> &'a [u8] {
        self.emb
    }

    /// The first layer, E × H row-major, one two's-complement i8 per byte.
    #[must_use]
    #[inline]
    pub const fn w1(&self) -> &'a [u8] {
        self.w1
    }

    /// The output layer, H × C row-major, one two's-complement i8 per byte.
    #[must_use]
    #[inline]
    pub const fn w2(&self) -> &'a [u8] {
        self.w2
    }

    /// The activation table, 2T + 1 entries, one two's-complement i8 per byte.
    #[must_use]
    #[inline]
    pub const fn lut(&self) -> &'a [u8] {
        self.lut
    }

    /// The first-layer biases, H of them.
    #[must_use]
    #[inline]
    pub const fn b1(&self) -> &[i32] {
        self.b1.split_at(self.dims.hidden as usize).0
    }

    /// The output biases, C of them.
    #[must_use]
    #[inline]
    pub const fn b2(&self) -> &[i32] {
        self.b2.split_at(self.dims.classes as usize).0
    }
}

/// The byte size of each of a job's four buffers, each at least 4 because a zero-length buffer cannot be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobBytes {
    /// The `job` header.
    pub job: u64,
    /// `offsets`, ROWS + 1 words.
    pub offsets: u64,
    /// `ids`, TOTAL_IDS halfwords padded to 4 bytes.
    pub ids: u64,
    /// `out`, ROWS × OUT_STRIDE words.
    pub out: u64,
}

impl JobBytes {
    /// The buffers of a batch of `rows` rows, `total_ids` ids and `classes` classes.
    #[must_use]
    pub const fn of(rows: u64, total_ids: u64, classes: u32) -> JobBytes {
        const fn at_least_4(n: u64) -> u64 {
            if n < 4 { 4 } else { n }
        }
        JobBytes {
            job: at_least_4(JOB_WORDS as u64 * 4),
            offsets: at_least_4(rows.saturating_add(1).saturating_mul(4)),
            ids: at_least_4(total_ids.saturating_mul(2).saturating_add(3) / 4 * 4),
            out: at_least_4(rows.saturating_mul(classes as u64 + 1).saturating_mul(4)),
        }
    }

    /// The four summed, which [`JOB_BYTES_MAX`] bounds.
    #[must_use]
    #[inline]
    pub const fn total(&self) -> u64 {
        self.job
            .saturating_add(self.offsets)
            .saturating_add(self.ids)
            .saturating_add(self.out)
    }
}

/// Why [`Batch::pack`] refused a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchError {
    /// It has no rows.
    Empty,
    /// It has more than [`ROWS_MAX`] rows.
    TooManyRows {
        /// Its rows.
        rows: u64,
        /// The limit.
        limit: u32,
    },
    /// Its four buffers would exceed [`JOB_BYTES_MAX`].
    TooLarge {
        /// Their bytes, summed.
        bytes: u64,
        /// The budget.
        limit: u64,
    },
    /// `offsets` does not span `ids` exactly: `offsets[0] ≠ 0` or `offsets[ROWS] ≠ ids.len()`.
    BadTotal {
        /// `offsets[0]`.
        first: u32,
        /// `offsets[ROWS]`.
        last: u32,
        /// `ids.len()`.
        ids: u64,
    },
    /// `offsets[row + 1] < offsets[row]`.
    NotMonotone {
        /// The row.
        row: u32,
    },
    /// A row holds more than [`MAX_ROW_IDS`] ids.
    RowTooLong {
        /// The row.
        row: u32,
        /// Its ids.
        len: u32,
    },
    /// An id is not below B.
    IdOutOfRange {
        /// Its index in `ids`.
        at: u32,
        /// The id.
        id: u16,
        /// B.
        buckets: u32,
    },
}

/// One job's host-side inputs, validated against a params blob's dimensions: CSR `offsets` and `ids`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    dims: Dims,
    offsets: Vec<u32>,
    ids: Vec<u16>,
}

impl Batch {
    /// Validate `offsets` and `ids` against `p` (design §12.3) and keep them; a row with zero ids is valid and abstains.
    pub fn pack(p: &ParamsView<'_>, offsets: Vec<u32>, ids: Vec<u16>) -> Result<Batch, BatchError> {
        let rows = offsets.len().saturating_sub(1);
        if rows == 0 {
            return Err(BatchError::Empty);
        }
        if rows > ROWS_MAX as usize {
            return Err(BatchError::TooManyRows {
                rows: rows as u64,
                limit: ROWS_MAX,
            });
        }
        let bytes = JobBytes::of(rows as u64, ids.len() as u64, p.dims.classes).total();
        if bytes > JOB_BYTES_MAX {
            return Err(BatchError::TooLarge {
                bytes,
                limit: JOB_BYTES_MAX,
            });
        }
        let (first, last) = (offsets[0], offsets[rows]);
        if first != 0 || last as usize != ids.len() {
            return Err(BatchError::BadTotal {
                first,
                last,
                ids: ids.len() as u64,
            });
        }
        for (row, w) in offsets.windows(2).enumerate() {
            let row = row as u32;
            let Some(len) = w[1].checked_sub(w[0]) else {
                return Err(BatchError::NotMonotone { row });
            };
            if len > MAX_ROW_IDS {
                return Err(BatchError::RowTooLong { row, len });
            }
        }
        if let Some((at, &id)) = ids
            .iter()
            .enumerate()
            .find(|&(_, &id)| u32::from(id) >= p.dims.buckets)
        {
            return Err(BatchError::IdOutOfRange {
                at: at as u32,
                id,
                buckets: p.dims.buckets,
            });
        }
        Ok(Batch {
            dims: p.dims,
            offsets,
            ids,
        })
    }

    /// The dimensions it was validated against.
    #[must_use]
    #[inline]
    pub const fn dims(&self) -> Dims {
        self.dims
    }

    /// ROWS, at least 1.
    #[must_use]
    #[inline]
    pub const fn rows(&self) -> u32 {
        (self.offsets.len() - 1) as u32
    }

    /// TOTAL_IDS.
    #[must_use]
    #[inline]
    pub const fn total_ids(&self) -> u32 {
        self.ids.len() as u32
    }

    /// The CSR row starts, ROWS + 1 of them.
    #[must_use]
    #[inline]
    pub const fn offsets(&self) -> &[u32] {
        self.offsets.as_slice()
    }

    /// Every row's ids, end to end.
    #[must_use]
    #[inline]
    pub const fn ids(&self) -> &[u16] {
        self.ids.as_slice()
    }

    /// Row `row`'s ids, or `None` past the last row.
    #[must_use]
    #[inline]
    pub const fn row(&self, row: u32) -> Option<&[u16]> {
        let r = row as usize;
        let offsets = self.offsets.as_slice();
        if r + 1 >= offsets.len() {
            return None;
        }
        let start = offsets[r] as usize;
        let end = offsets[r + 1] as usize;
        let ids = self.ids.as_slice();
        if start > end || end > ids.len() {
            return None;
        }
        Some(ids.split_at(end).0.split_at(start).1)
    }

    /// The out words per row, C + 1.
    #[must_use]
    #[inline]
    pub const fn out_stride(&self) -> u32 {
        self.dims.out_stride()
    }

    /// The out words of the whole batch, ROWS × OUT_STRIDE.
    #[must_use]
    #[inline]
    pub const fn out_words(&self) -> usize {
        self.rows() as usize * self.out_stride() as usize
    }

    /// The `job` header's words.
    #[must_use]
    pub const fn header(&self) -> [u32; JOB_WORDS] {
        let mut h = [0u32; JOB_WORDS];
        h[job::MAGIC] = JOB_MAGIC_VALUE;
        h[job::LAYOUT_VERSION] = LAYOUT_VERSION;
        h[job::ROWS] = self.rows();
        h[job::TOTAL_IDS] = self.total_ids();
        h[job::OUT_STRIDE] = self.out_stride();
        h
    }

    /// The byte size of each job buffer it fills.
    #[must_use]
    #[inline]
    pub const fn bytes(&self) -> JobBytes {
        JobBytes::of(
            self.rows() as u64,
            self.total_ids() as u64,
            self.dims.classes,
        )
    }

    /// Give back `offsets` and `ids`, so a later batch can reuse their capacity.
    #[must_use]
    #[inline]
    pub fn into_parts(self) -> (Vec<u32>, Vec<u16>) {
        (self.offsets, self.ids)
    }
}

/// Why [`forward`] refused its arguments before computing anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardError {
    /// The batch was packed against params of other dimensions.
    DimsMismatch {
        /// The batch's.
        batch: Dims,
        /// The params'.
        params: Dims,
    },
    /// The rows asked for lie outside the batch.
    RowsOutOfRange {
        /// The first row asked for.
        start: u32,
        /// One past the last.
        end: u32,
        /// The batch's rows.
        rows: u32,
    },
    /// `out` does not hold OUT_STRIDE words per row asked for.
    OutLength {
        /// The words needed.
        want: usize,
        /// The words given.
        got: usize,
    },
}

/// The profile's arithmetic, checked or wrapping.
trait Arith {
    fn add(a: i32, b: i32) -> i32;
    fn mul(a: i32, b: i32) -> i32;
    fn neg(a: i32) -> i32;
    fn abs(a: i32) -> u32;
    fn div(a: u32, b: u32) -> u32;
    fn mul_u(a: u32, b: u32) -> u32;
    fn shr(a: u32, s: u32) -> u32;
    fn signed(a: u32) -> i32;
}

/// Every operation checked; a broken bound panics.
struct Checked;

/// Every operation wrapping, valid only under the checker's and the packer's bounds.
struct Wrapping;

const BROKEN: &str = "an int32-exact/v1 bound is broken";

impl Arith for Checked {
    #[inline]
    fn add(a: i32, b: i32) -> i32 {
        a.checked_add(b).expect(BROKEN)
    }
    #[inline]
    fn mul(a: i32, b: i32) -> i32 {
        a.checked_mul(b).expect(BROKEN)
    }
    #[inline]
    fn neg(a: i32) -> i32 {
        a.checked_neg().expect(BROKEN)
    }
    #[inline]
    fn abs(a: i32) -> u32 {
        a.checked_abs().expect(BROKEN).unsigned_abs()
    }
    #[inline]
    fn div(a: u32, b: u32) -> u32 {
        a.checked_div(b).expect(BROKEN)
    }
    #[inline]
    fn mul_u(a: u32, b: u32) -> u32 {
        a.checked_mul(b)
            .filter(|&v| v <= i32::MAX as u32)
            .expect(BROKEN)
    }
    #[inline]
    fn shr(a: u32, s: u32) -> u32 {
        a.checked_shr(s).expect(BROKEN)
    }
    #[inline]
    fn signed(a: u32) -> i32 {
        i32::try_from(a).expect(BROKEN)
    }
}

impl Arith for Wrapping {
    #[inline]
    fn add(a: i32, b: i32) -> i32 {
        a.wrapping_add(b)
    }
    #[inline]
    fn mul(a: i32, b: i32) -> i32 {
        a.wrapping_mul(b)
    }
    #[inline]
    fn neg(a: i32) -> i32 {
        a.wrapping_neg()
    }
    #[inline]
    fn abs(a: i32) -> u32 {
        a.unsigned_abs()
    }
    #[inline]
    fn div(a: u32, b: u32) -> u32 {
        a.wrapping_div(b)
    }
    #[inline]
    fn mul_u(a: u32, b: u32) -> u32 {
        a.wrapping_mul(b)
    }
    #[inline]
    fn shr(a: u32, s: u32) -> u32 {
        a.wrapping_shr(s)
    }
    #[inline]
    fn signed(a: u32) -> i32 {
        a as i32
    }
}

/// Steps 2–5 for a row of at least MIN_IDS `ids`: each hidden unit's pre-activation p before step 6's clamp, returning how many units step 5 clamped; `interior`, when given, counts the units with 0 < q ≤ QMAX and |p| ≤ T.
#[inline]
fn pre_activations<A: Arith>(
    p: &ParamsView<'_>,
    ids: &[u16],
    pre: &mut [i32; H_CAP],
    mut interior: Option<&mut u32>,
) -> u32 {
    let (e, h) = (p.dims.embed as usize, p.dims.hidden as usize);
    let n = ids.len() as u32;
    let mut s = [0i32; E_CAP];
    for &id in ids {
        let row = &p.emb[usize::from(id) * e..][..e];
        for (acc, &w) in s[..e].iter_mut().zip(row) {
            *acc = A::add(*acc, i32::from(w as i8));
        }
    }
    let mut a = [0i32; H_CAP];
    for (j, &sj) in s[..e].iter().enumerate() {
        let row = &p.w1[j * h..][..h];
        for (acc, &w) in a[..h].iter_mut().zip(row) {
            *acc = A::add(*acc, A::mul(sj, i32::from(w as i8)));
        }
    }
    let mut clamped = 0;
    for ((slot, &ak), &bk) in pre[..h].iter_mut().zip(&a[..h]).zip(&p.b1[..h]) {
        let q = A::div(A::abs(ak), n);
        clamped += u32::from(q > p.qmax);
        let mag = A::signed(A::shr(A::mul_u(q.min(p.qmax), p.mult), p.g));
        let signed = if ak < 0 { A::neg(mag) } else { mag };
        *slot = A::add(signed, bk);
        if let Some(n) = interior.as_deref_mut() {
            *n += u32::from(q > 0 && q <= p.qmax && slot.unsigned_abs() <= p.t.unsigned_abs());
        }
    }
    clamped
}

/// Steps 6–8 from the pre-activations: one row's out words, `L[0..C]` as i32 bits then the class.
#[inline]
fn finish<A: Arith>(p: &ParamsView<'_>, pre: &[i32; H_CAP], out: &mut [u32]) {
    let (h, c) = (p.dims.hidden as usize, p.dims.classes as usize);
    let mut hid = [0i32; H_CAP];
    for (hk, &pk) in hid[..h].iter_mut().zip(&pre[..h]) {
        let at = A::add(pk.clamp(-p.t, p.t), p.t);
        *hk = i32::from(p.lut[at as usize] as i8);
    }
    let mut logits = [0i32; C_CAP];
    logits[..c].copy_from_slice(&p.b2[..c]);
    for (k, &hk) in hid[..h].iter().enumerate() {
        let row = &p.w2[k * c..][..c];
        for (acc, &w) in logits[..c].iter_mut().zip(row) {
            *acc = A::add(*acc, A::mul(hk, i32::from(w as i8)));
        }
    }
    let mut best = 0;
    for (i, &l) in logits[..c].iter().enumerate().skip(1) {
        if l > logits[best] {
            best = i;
        }
    }
    for (o, &l) in out[..c].iter_mut().zip(&logits[..c]) {
        *o = l as u32;
    }
    out[c] = best as u32;
}

/// Validate the arguments, then run every row of `rows` through steps 1–8.
fn run<A: Arith>(
    p: &ParamsView<'_>,
    b: &Batch,
    rows: Range<u32>,
    out: &mut [u32],
) -> Result<(), ForwardError> {
    if b.dims != p.dims {
        return Err(ForwardError::DimsMismatch {
            batch: b.dims,
            params: p.dims,
        });
    }
    if rows.start > rows.end || rows.end > b.rows() {
        return Err(ForwardError::RowsOutOfRange {
            start: rows.start,
            end: rows.end,
            rows: b.rows(),
        });
    }
    let stride = p.dims.out_stride() as usize;
    let want = (rows.end - rows.start) as usize * stride;
    if out.len() != want {
        return Err(ForwardError::OutLength {
            want,
            got: out.len(),
        });
    }
    let c = p.dims.classes as usize;
    let mut pre = [0i32; H_CAP];
    for (r, o) in rows.zip(out.chunks_exact_mut(stride)) {
        let start = b.offsets[r as usize] as usize;
        let ids = &b.ids[start..b.offsets[r as usize + 1] as usize];
        if ids.len() < p.min_ids as usize {
            o[..c].fill(0);
            o[c] = p.dims.classes;
        } else {
            pre_activations::<A>(p, ids, &mut pre, None);
            finish::<A>(p, &pre, o);
        }
    }
    Ok(())
}

/// The CPU reference: every row of `b` into `out` (ROWS × OUT_STRIDE words), every operation checked, panicking on any broken bound.
pub fn forward(p: &ParamsView<'_>, b: &Batch, out: &mut [u32]) -> Result<(), ForwardError> {
    run::<Checked>(p, b, 0..b.rows(), out)
}

/// [`forward`] with every operation wrapping, which equals it under the checker's and the packer's bounds.
pub fn forward_fast(p: &ParamsView<'_>, b: &Batch, out: &mut [u32]) -> Result<(), ForwardError> {
    run::<Wrapping>(p, b, 0..b.rows(), out)
}

/// [`forward_fast`] over `rows` only, into `out` (their OUT_STRIDE words each), so threads can split a batch.
pub fn forward_fast_rows(
    p: &ParamsView<'_>,
    b: &Batch,
    rows: Range<u32>,
    out: &mut [u32],
) -> Result<(), ForwardError> {
    run::<Wrapping>(p, b, rows, out)
}

/// What the checked reference computes for one row before step 6.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowTrace {
    /// Each hidden unit's pre-activation p, before step 6's clamp.
    pub pre: Vec<i32>,
    /// How many hidden units step 5 clamped.
    pub clamped: u32,
    /// How many hidden units step 6 saturated (`|p| > T`).
    pub saturated: u32,
    /// How many hidden units are interior: 0 < q ≤ QMAX and |p| ≤ T, so steps 4–6 each reach the output.
    pub interior: u32,
}

/// Trace row `row` of `b` through steps 1–5 of the checked reference, with how many units are clamped, saturated and interior, or `None` when it abstains.
pub fn trace(p: &ParamsView<'_>, b: &Batch, row: u32) -> Result<Option<RowTrace>, ForwardError> {
    if b.dims != p.dims {
        return Err(ForwardError::DimsMismatch {
            batch: b.dims,
            params: p.dims,
        });
    }
    let Some(ids) = b.row(row) else {
        return Err(ForwardError::RowsOutOfRange {
            start: row,
            end: row.saturating_add(1),
            rows: b.rows(),
        });
    };
    if ids.len() < p.min_ids as usize {
        return Ok(None);
    }
    let (mut pre, mut interior) = ([0i32; H_CAP], 0);
    let clamped = pre_activations::<Checked>(p, ids, &mut pre, Some(&mut interior));
    let pre = pre[..p.dims.hidden as usize].to_vec();
    let saturated = pre.iter().filter(|&&x| x < -p.t || x > p.t).count() as u32;
    Ok(Some(RowTrace {
        pre,
        clamped,
        saturated,
        interior,
    }))
}

/// `Digest::of(OUT_DOMAIN, [the words, little-endian])`.
pub fn out_digest(words: &[u32]) -> Digest {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    Digest::of(OUT_DOMAIN, &[&bytes])
}

/// A mixer for the extreme blob's sign patterns.
#[inline]
const fn mix(a: u32, b: u32) -> u32 {
    let mut x = a.wrapping_mul(0x9e37_79b9) ^ b.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x
}

/// A params blob at the profile's extremes: B, E, H and C at their maxima, every weight ±127, `max|b1|` at the lossless bound, every `|b2|` at 2²⁹ − 1, and w2's classes in identical pairs so that every row's argmax is a tie.
pub fn extreme_params() -> Vec<u8> {
    extreme_params_for(
        Dims {
            buckets: B_MAX,
            embed: E_MAX,
            hidden: H_MAX,
            classes: C_MAX,
        },
        2,
    )
}

/// [`extreme_params`] at `dims` and `min_ids`, which [`check`] refuses when they lie outside their bounds.
pub fn extreme_params_for(dims: Dims, min_ids: u32) -> Vec<u8> {
    let (e, h, c) = (
        dims.embed as usize,
        dims.hidden as usize,
        dims.classes as usize,
    );
    let last = dims.buckets.saturating_sub(1);
    let sign = |bit: bool| if bit { 127i8 } else { -127 };
    let emb: Vec<i8> = (0..dims.buckets as usize * e)
        .map(|i| {
            let (id, j) = ((i / e) as u32, (i % e) as u32);
            match id {
                0 => 127,
                id if id == last => -127,
                id => sign(mix(id, j) & 1 == 0),
            }
        })
        .collect();
    let w1: Vec<i8> = (0..e * h)
        .map(|i| match (i / h, i % h) {
            (_, 0) => 127,
            (j, k) => sign(mix(j as u32 + 1, k as u32 + 7) & 2 == 0),
        })
        .collect();
    let w2: Vec<i8> = (0..h * c)
        .map(|i| sign(mix((i / c) as u32 + 3, (i % c / 2) as u32 + 11) & 4 == 0))
        .collect();
    let (mult, g, t) = (79_474u32, 20u32, 1_024i32);
    let m = ((u64::from(qmax(mult)) * u64::from(mult)) >> g) as i32;
    let lut: Vec<i8> = (-t..=t).map(|i| (i.signum() * 127) as i8).collect();
    let b1: Vec<i32> = (0..h as u32)
        .map(|k| if mix(k, 5) & 1 == 0 { m - t } else { t - m })
        .collect();
    let b2: Vec<i32> = (0..c)
        .map(|class| {
            if (class / 2) % 2 == 0 {
                B2_LIMIT - 1
            } else {
                1 - B2_LIMIT
            }
        })
        .collect();
    build(&Spec {
        dims,
        min_ids,
        mult,
        g,
        t,
        emb: &emb,
        w1: &w1,
        w2: &w2,
        lut: &lut,
        b1: &b1,
        b2: &b2,
    })
}

/// CSR rows for [`extreme_params`]: two at [`MAX_ROW_IDS`] reaching step 3's bound with either sign, one of mixed ids at it, rows below MIN_IDS and empty, a row whose embeddings cancel, and short mixed rows.
pub fn extreme_rows() -> (Vec<u32>, Vec<u16>) {
    let full = MAX_ROW_IDS as usize;
    let top = (B_MAX - 1) as u16;
    let rows: Vec<Vec<u16>> = vec![
        vec![0; full],
        vec![top; full],
        (0..MAX_ROW_IDS)
            .map(|i| (mix(i, 13) % B_MAX) as u16)
            .collect(),
        vec![],
        vec![7],
        vec![0, top],
        vec![1, 2, 3],
        vec![top - 1, top - 2],
        vec![0, 0, top],
        vec![],
        (0..64).map(|i| (mix(i, 17) % B_MAX) as u16).collect(),
    ];
    let mut offsets = vec![0u32];
    let mut ids = Vec::new();
    for row in rows {
        ids.extend_from_slice(&row);
        offsets.push(ids.len() as u32);
    }
    (offsets, ids)
}

/// A params blob at `dims` and `min_ids` whose every unit stays in the unclamped interior: emb and w1 in ±1 and ±2, so q ≤ 4E ≤ 128 ≪ QMAX, with emb's sign set by the bucket's half so a row of one half sums linearly; MULT = 181 and G = 8, so q·MULT is a multiple of 2^G only at q = 0; |b1| ≤ 30, so |p| ≤ 90 + 30 < T = 127; the identity table `lut[i] = i − T`; and no w2 weight 0, so a step 4, 5 or 6 that differs from the profile's moves a logit.
pub fn interior_params_for(dims: Dims, min_ids: u32) -> Vec<u8> {
    let (e, h, c) = (
        dims.embed as usize,
        dims.hidden as usize,
        dims.classes as usize,
    );
    let half = (dims.buckets / 2).max(1);
    let small = |a: u32, b: u32| [-2i8, -1, 1, 2][(mix(a, b) % 4) as usize];
    let emb: Vec<i8> = (0..dims.buckets as usize * e)
        .map(|i| {
            let (id, j) = ((i / e) as u32, (i % e) as u32);
            let mag = small(id + 1, j + 29).abs();
            let lean = small(j + 1, 67).signum();
            if id < half { lean * mag } else { -lean * mag }
        })
        .collect();
    let w1: Vec<i8> = (0..e * h)
        .map(|i| small((i / h) as u32 + 31, (i % h) as u32 + 37))
        .collect();
    let w2: Vec<i8> = (0..h * c)
        .map(|i| {
            let x = mix((i / c) as u32 + 41, (i % c) as u32 + 43);
            let mag = (x % 127) as i8 + 1;
            if x & 0x100 == 0 { mag } else { -mag }
        })
        .collect();
    let t = 127i32;
    let lut: Vec<i8> = (-t..=t).map(|i| i as i8).collect();
    let b1: Vec<i32> = (0..h as u32)
        .map(|k| (mix(k, 47) % 61) as i32 - 30)
        .collect();
    let b2: Vec<i32> = (0..c as u32)
        .map(|class| (mix(class, 53) % 2_001) as i32 - 1_000)
        .collect();
    build(&Spec {
        dims,
        min_ids,
        mult: 181,
        g: 8,
        t,
        emb: &emb,
        w1: &w1,
        w2: &w2,
        lut: &lut,
        b1: &b1,
        b2: &b2,
    })
}

/// CSR rows for [`interior_params_for`] at `dims` and `min_ids`: 64 rows of MIN_IDS to MIN_IDS + 10 ids (at most [`MAX_ROW_IDS`]), each row's ids from one half of the buckets, the halves alternating.
pub fn interior_rows(dims: Dims, min_ids: u32) -> (Vec<u32>, Vec<u16>) {
    let half = (dims.buckets / 2).max(1);
    let mut offsets = vec![0u32];
    let mut ids = Vec::new();
    for row in 0..64u32 {
        let len = min_ids.saturating_add(row % 11).min(MAX_ROW_IDS);
        let base = if dims.buckets > 1 { row % 2 * half } else { 0 };
        ids.extend((0..len).map(|i| (base + mix(row + 59, i + 61) % half) as u16));
        offsets.push(ids.len() as u32);
    }
    (offsets, ids)
}

/// The seeded blobs' generator, xorshift64*.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// Uniform in `-bound..=bound`.
    fn signed(&mut self, bound: i64) -> i64 {
        self.below(2 * bound as u64 + 1) as i64 - bound
    }

    fn weight(&mut self) -> i8 {
        self.signed(127) as i8
    }
}

/// The seeded blob `seed` within every bound, with its CSR rows: 1 to 48 rows of 0 to 40 ids and, every eighth blob, a first row at [`MAX_ROW_IDS`]; MULT is `⌊(2³¹ − 1)/2^r⌋` for one draw r in 0..=20, so QMAX is log-uniform from 1 to about 2²⁰ and the blobs reach both the clamped and the interior regime.
pub fn seeded(seed: u64) -> (Vec<u8>, Vec<u32>, Vec<u16>) {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15 ^ (seed + 1).wrapping_mul(0xbf58_476d_1ce4_e5b9));
    let dims = Dims {
        buckets: 1 + r.below(4_096) as u32,
        embed: 1 + r.below(u64::from(E_MAX)) as u32,
        hidden: 1 + r.below(u64::from(H_MAX)) as u32,
        classes: 1 + r.below(u64::from(C_MAX)) as u32,
    };
    let g = 1 + r.below(31) as u32;
    let mult = i32::MAX as u32 >> r.below(21);
    let m = ((u64::from(qmax(mult)) * u64::from(mult)) >> g) as i64;
    let t = r.below(m.min(2_048) as u64 + 1) as i32;
    let b1_bound = m - i64::from(t);
    let b2_bound = (1i64 << r.below(30)) - 1;
    let (b, e, h, c) = (
        dims.buckets as usize,
        dims.embed as usize,
        dims.hidden as usize,
        dims.classes as usize,
    );
    let min_ids = 1 + r.below(8) as u32;
    let emb: Vec<i8> = (0..b * e).map(|_| r.weight()).collect();
    let w1: Vec<i8> = (0..e * h).map(|_| r.weight()).collect();
    let w2: Vec<i8> = (0..h * c).map(|_| r.weight()).collect();
    let lut: Vec<i8> = (0..2 * t + 1).map(|_| r.weight()).collect();
    let b1: Vec<i32> = (0..h).map(|_| r.signed(b1_bound) as i32).collect();
    let b2: Vec<i32> = (0..c).map(|_| r.signed(b2_bound) as i32).collect();
    let bytes = build(&Spec {
        dims,
        min_ids,
        mult,
        g,
        t,
        emb: &emb,
        w1: &w1,
        w2: &w2,
        lut: &lut,
        b1: &b1,
        b2: &b2,
    });
    let rows = 1 + r.below(48) as usize;
    let mut offsets = vec![0u32];
    let mut ids = Vec::new();
    for row in 0..rows {
        let len = if row == 0 && seed.is_multiple_of(8) {
            u64::from(MAX_ROW_IDS)
        } else {
            r.below(41)
        };
        for _ in 0..len {
            ids.push(r.below(b as u64) as u16);
        }
        offsets.push(ids.len() as u32);
    }
    (bytes, offsets, ids)
}
