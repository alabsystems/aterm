// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-png` — first-party PNG decode and encode, replacing the `png` crate.
//!
//! ## Why
//!
//! `png` cost 33,439 lines across SEVEN packages (`png`, `flate2`,
//! `miniz_oxide`, `fdeflate`, `simd-adler32`, `crc32fast`, `adler2`) for a
//! surface aterm uses narrowly and knows exactly: decode an 8-bit image to RGBA,
//! and encode an RGB/RGBA/indexed framebuffer. Most of that tonnage is the
//! DEFLATE stack, and aterm already carried half of it — `aterm_codec::inflate`
//! — for its own reasons. This crate supplies the PNG container, the filters,
//! and the compressor half that was missing, and reuses the inflate that was
//! already there rather than importing a second one.
//!
//! ## Scope, stated as limits
//!
//! Decoding covers the WHOLE base PNG format, because the inputs are not
//! aterm's to choose: colour-font strikes (Noto Color Emoji's CBDT images are
//! INDEXED with a `tRNS` chunk; Apple sbix are RGBA), user wallpapers, inline
//! images pasted into the terminal, and AppKit's own window-capture output.
//! So: all five colour types, bit depths 1/2/4/8/16, `PLTE`, `tRNS`, and Adam7
//! interlacing. Adam7 is IMPLEMENTED rather than refused precisely because a
//! user's wallpaper is allowed to be interlaced and refusing it would be a
//! silent regression.
//!
//! What is NOT here, and is not needed: `gAMA`/`iCCP` colour management,
//! animation (APNG), 16-bit OUTPUT (every consumer takes `STRIP_16`), and
//! progressive/streaming decode. Ancillary chunks are validated for structure
//! and skipped; `sRGB` is the one whose value is surfaced, because the
//! screenshot verbs assert on it.
//!
//! ## Malformed input: render it, do not refuse it
//!
//! The inputs are not aterm's to choose, so the default on a file that breaks a
//! rule is to produce the same picture the retired crate produced, not to
//! refuse. A colour-emoji strike with one stray palette index used to make the
//! whole glyph disappear; that is a worse failure than one wrong pixel. So an
//! index past `PLTE` renders opaque black, a `tRNS` longer than the palette is
//! ignored, a `tRNS` colour key is masked to the sample width, a zero-length
//! chunk is absent rather than present-and-empty, and bytes after the end of the
//! zlib stream inside `IDAT` are simply not part of it. Each of those is pinned
//! by a differential test naming the case.
//!
//! Structure is where it stays strict, because the retired crate was: a
//! duplicate `PLTE`, a `tRNS` before `PLTE` and `IDAT` chunks interrupted by
//! another chunk are all REFUSED, as they were before.
//!
//! There is ONE deliberate divergence, and it is a memory bound rather than a
//! format opinion. Image data that decompresses to more than the header's
//! geometry plus one maximal DEFLATE stored block (65,535 bytes — the largest
//! single block of padding an encoder can emit) is refused with
//! [`DecodingError::ExcessImageData`]. `png` decodes such a file, because it
//! stops reading once it has enough rows. The excess is unbounded and nothing
//! will ever read it, so it is treated as a decompression bomb; the separate
//! error exists so it is never reported as corruption.
//!
//! Encoding covers what aterm writes: 8-bit Grayscale/RGB/RGBA/Indexed with
//! optional `PLTE`/`tRNS`/`sRGB`. See [`deflate`] for the compressor's
//! deliberate ratio trade.
//!
//! ## The oracle
//!
//! `png` is kept as a `[dev-dependencies]` ORACLE. `tests/oracle.rs` asserts
//! byte-identical decoded buffers over the repository's own 969-file PNG corpus
//! and over a synthesized matrix covering every (colour type x bit depth x
//! interlace x tRNS x palette length) combination the format allows — under all
//! FOUR points of the public transform surface, not merely the pair the shipped
//! call sites use, because a chunk that promotes Rgb to Rgba changes the output
//! SHAPE and only `EXPAND` alone can see it. It cross-decodes every encoder
//! output with `png` and every `png` output with this decoder, and it carries a
//! named case for each malformed-input rule above. See that file's header for
//! what each check pins.

mod checksum;
mod decode;
mod deflate;
mod encode;

pub use decode::{Decoder, Reader};
pub use encode::{Encoder, Writer};

/// How samples are laid out per pixel. The discriminants are PNG's own IHDR
/// colour-type codes, so the wire value and the enum are the same number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ColorType {
    /// One sample: luminance.
    Grayscale = 0,
    /// Three samples: red, green, blue.
    Rgb = 2,
    /// One sample: an index into `PLTE`.
    Indexed = 3,
    /// Two samples: luminance, alpha.
    GrayscaleAlpha = 4,
    /// Four samples: red, green, blue, alpha.
    Rgba = 6,
}

impl ColorType {
    /// Samples per pixel.
    #[must_use]
    pub const fn samples(self) -> usize {
        match self {
            Self::Grayscale | Self::Indexed => 1,
            Self::Rgb => 3,
            Self::GrayscaleAlpha => 2,
            Self::Rgba => 4,
        }
    }

    /// Parse an IHDR colour-type byte. `None` for the reserved values 1, 5 and
    /// anything above 6 — a file using one is malformed, not merely exotic.
    const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Grayscale),
            2 => Some(Self::Rgb),
            3 => Some(Self::Indexed),
            4 => Some(Self::GrayscaleAlpha),
            6 => Some(Self::Rgba),
            _ => None,
        }
    }

    /// Whether `depth` is a bit depth PNG allows for this colour type
    /// (specification table 11.1) — the pairing is NOT free, and accepting an
    /// illegal pair would mean computing a scanline stride the file does not
    /// have.
    const fn allows_depth(self, depth: BitDepth) -> bool {
        match self {
            Self::Grayscale => true,
            Self::Indexed => !matches!(depth, BitDepth::Sixteen),
            Self::Rgb | Self::GrayscaleAlpha | Self::Rgba => {
                matches!(depth, BitDepth::Eight | BitDepth::Sixteen)
            }
        }
    }
}

/// Bits per sample. The discriminants are PNG's own IHDR values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum BitDepth {
    /// 1 bit per sample (grayscale or indexed).
    One = 1,
    /// 2 bits per sample (grayscale or indexed).
    Two = 2,
    /// 4 bits per sample (grayscale or indexed).
    Four = 4,
    /// 8 bits per sample — everything aterm writes.
    Eight = 8,
    /// 16 bits per sample; every consumer folds these to 8 with
    /// [`Transformations::STRIP_16`].
    Sixteen = 16,
}

impl BitDepth {
    /// The depth as a bit count.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self as u32
    }

    /// Parse an IHDR bit-depth byte; `None` for any value that is not a legal
    /// PNG depth.
    const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            4 => Some(Self::Four),
            8 => Some(Self::Eight),
            16 => Some(Self::Sixteen),
            _ => None,
        }
    }
}

/// The `sRGB` chunk's rendering intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SrgbRenderingIntent {
    /// Perceptual — the intent aterm's screenshot verbs declare.
    Perceptual = 0,
    /// Relative colorimetric.
    RelativeColorimetric = 1,
    /// Saturation.
    Saturation = 2,
    /// Absolute colorimetric.
    AbsoluteColorimetric = 3,
}

impl SrgbRenderingIntent {
    const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Perceptual),
            1 => Some(Self::RelativeColorimetric),
            2 => Some(Self::Saturation),
            3 => Some(Self::AbsoluteColorimetric),
            _ => None,
        }
    }
}

/// Output transforms applied on the way out of the decoder.
///
/// The two aterm uses are the two that matter for reading somebody else's
/// image: `EXPAND` turns the compact encodings (palette indices, sub-byte
/// grayscale, a `tRNS` chunk standing in for an alpha channel) into plain
/// samples, and `STRIP_16` folds 16-bit channels to 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transformations(u32);

impl Transformations {
    /// No transform: the output buffer holds the file's own samples, at the
    /// file's own bit depth, packed exactly as PNG packs them.
    pub const IDENTITY: Self = Self(0);
    /// Expand palette images to RGB(A), sub-8-bit grayscale to 8-bit
    /// grayscale, and a `tRNS` chunk to a real alpha channel.
    pub const EXPAND: Self = Self(1);
    /// Fold 16-bit samples to 8-bit by keeping the high byte.
    pub const STRIP_16: Self = Self(2);

    const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Transformations {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// A budget on what a decode may allocate.
///
/// A few hundred bytes of PNG can declare a 4-gigapixel image; the point of this
/// is that such a file is REFUSED before anything is allocated for it, rather
/// than aborting the process on a failed allocation. Checked against the
/// decompressed scanline buffer and the output buffer, both computed from the
/// header before either exists.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Maximum bytes the decoder may allocate for one image.
    pub bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        // The `png` crate's own default, so a consumer that sets no limit gets
        // the same admission decisions it did before.
        Self {
            bytes: 1024 * 1024 * 64,
        }
    }
}

/// What a PNG's header says about it, before any transform is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// Image width in pixels; never zero in a valid file.
    pub width: u32,
    /// Image height in pixels; never zero in a valid file.
    pub height: u32,
    /// Bits per sample as stored.
    pub bit_depth: BitDepth,
    /// Sample layout as stored.
    pub color_type: ColorType,
    /// Whether the image is Adam7-interlaced.
    pub interlaced: bool,
    /// The `sRGB` chunk's rendering intent, when the file carries one.
    pub srgb: Option<SrgbRenderingIntent>,
    /// The `PLTE` palette as RGB triples, when present.
    pub palette: Option<Vec<u8>>,
    /// The `tRNS` chunk verbatim, when present. Its meaning depends on the
    /// colour type: per-index alpha (indexed), a transparent grey (grayscale),
    /// or a transparent RGB triple (truecolour).
    pub trns: Option<Vec<u8>>,
}

/// What the decoder actually produced, AFTER transforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputInfo {
    /// Width of the produced buffer in pixels.
    pub width: u32,
    /// Height of the produced buffer in pixels.
    pub height: u32,
    /// Sample layout of the produced buffer.
    pub color_type: ColorType,
    /// Bits per sample in the produced buffer.
    pub bit_depth: BitDepth,
    /// Bytes per row of the produced buffer.
    pub line_size: usize,
}

impl OutputInfo {
    /// Total bytes of the produced image.
    #[must_use]
    pub const fn buffer_size(&self) -> usize {
        self.line_size * self.height as usize
    }
}

/// Why a PNG could not be decoded. Never a panic, and never a partial image
/// handed back as if it were whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodingError {
    /// The 8-byte PNG signature is absent or wrong.
    NotPng,
    /// The file ends inside a chunk, or inside the image data.
    Truncated,
    /// A chunk's CRC-32 does not match its contents.
    BadCrc {
        /// The four-character chunk type whose CRC failed.
        chunk: [u8; 4],
    },
    /// A header field is not a legal PNG value, or the chunk sequence is not a
    /// legal PNG structure (no IHDR first, an indexed image with no palette,
    /// a `tRNS` of the wrong length, …). Carries a human-readable reason.
    Malformed(&'static str),
    /// The compressed image data is not a valid zlib stream.
    BadZlib,
    /// The image data decompresses to substantially MORE than the header's
    /// dimensions call for. Distinct from [`DecodingError::BadZlib`] because
    /// the stream is perfectly well formed — there is simply more of it than
    /// the image can be, which is the shape of a decompression bomb.
    ExcessImageData,
    /// The image would need more than [`Limits::bytes`] to decode.
    LimitsExceeded,
    /// An I/O error reading the source.
    Io(std::io::ErrorKind),
}

impl std::fmt::Display for DecodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPng => write!(f, "not a PNG file (bad signature)"),
            Self::Truncated => write!(f, "truncated PNG"),
            Self::BadCrc { chunk } => {
                write!(
                    f,
                    "CRC mismatch in chunk {}",
                    String::from_utf8_lossy(chunk)
                )
            }
            Self::Malformed(why) => write!(f, "malformed PNG: {why}"),
            Self::BadZlib => write!(f, "corrupt compressed image data"),
            Self::ExcessImageData => {
                write!(
                    f,
                    "image data decompresses past the size the header declares"
                )
            }
            Self::LimitsExceeded => write!(f, "PNG exceeds the configured decode limits"),
            Self::Io(kind) => write!(f, "I/O error reading PNG: {kind:?}"),
        }
    }
}

impl std::error::Error for DecodingError {}

/// Lets a decode sit inside an `io::Result` chain with `?`, as the retired
/// crate's error did — several dump/capture helpers are written that way.
impl From<DecodingError> for std::io::Error {
    fn from(e: DecodingError) -> Self {
        let text = e.to_string();
        match e {
            DecodingError::Io(kind) => Self::new(kind, text),
            _ => Self::new(std::io::ErrorKind::InvalidData, text),
        }
    }
}

/// Why a PNG could not be encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodingError {
    /// The caller's parameters cannot describe a PNG (zero dimensions, a
    /// colour-type/bit-depth pair the format forbids, an indexed image with no
    /// palette, …).
    Parameter(&'static str),
    /// The pixel buffer's length does not match the declared geometry.
    WrongDataSize {
        /// Bytes the geometry requires.
        expected: usize,
        /// Bytes the caller supplied.
        got: usize,
    },
    /// The sink refused a write.
    Io(std::io::ErrorKind),
}

impl std::fmt::Display for EncodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parameter(why) => write!(f, "invalid PNG encoding parameters: {why}"),
            Self::WrongDataSize { expected, got } => {
                write!(f, "PNG image data is {got} bytes, expected {expected}")
            }
            Self::Io(kind) => write!(f, "I/O error writing PNG: {kind:?}"),
        }
    }
}

impl std::error::Error for EncodingError {}

/// The encoding twin of the conversion above: `write_header()?` inside a
/// function returning `io::Result<()>` is the shape every gallery/dump helper
/// in the tree uses.
impl From<EncodingError> for std::io::Error {
    fn from(e: EncodingError) -> Self {
        let text = e.to_string();
        match e {
            EncodingError::Io(kind) => Self::new(kind, text),
            _ => Self::new(std::io::ErrorKind::InvalidInput, text),
        }
    }
}

/// The 8-byte file signature every PNG starts with.
pub(crate) const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Bytes needed for `width` pixels of `samples` samples at `depth` bits,
/// rounded up to a whole byte (PNG pads each scanline to a byte boundary).
/// `None` on overflow, so a hostile header cannot wrap the stride to a small
/// number and get a short buffer treated as a big image.
pub(crate) fn row_bytes(width: u32, samples: usize, depth: u32) -> Option<usize> {
    let bits = (width as usize)
        .checked_mul(samples)?
        .checked_mul(depth as usize)?;
    Some(bits.div_ceil(8))
}
