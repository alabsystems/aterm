// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-png` — first-party PNG decode and encode.

mod checksum;
mod decode;
mod deflate;
mod encode;
mod rgba;

pub use decode::{Decoder, Reader};
pub use encode::{Encoder, Writer, encoder_scratch_len, max_encoded_len};
pub use rgba::decode_rgba8;

/// How samples are laid out per pixel.
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
    #[inline]
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
    #[inline]
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
    #[inline]
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

/// Bits per sample.
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
    /// 16 bits per sample; every consumer folds these to 8 with [`Transformations::STRIP_16`].
    Sixteen = 16,
}

impl BitDepth {
    /// The depth as a bit count.
    #[must_use]
    #[inline]
    pub const fn bits(self) -> u32 {
        self as u32
    }

    /// Parse an IHDR bit-depth byte; `None` for any value that is not a legal
    /// PNG depth.
    #[inline]
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
    #[inline]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transformations(u32);

impl Transformations {
    /// No transform: the output buffer holds the file's own samples, at the file's own bit depth, packed exactly as PNG packs them.
    pub const IDENTITY: Self = Self(0);
    /// Expand palette images to RGB(A), sub-8-bit grayscale to 8-bit grayscale, and a `tRNS` chunk to a real alpha channel.
    pub const EXPAND: Self = Self(1);
    /// Fold 16-bit samples to 8-bit by keeping the high byte.
    pub const STRIP_16: Self = Self(2);

    #[inline]
    const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Transformations {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// A decoded-output budget, with a separate derived encoded-input ceiling.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Maximum bytes in the decoded output buffer.
    pub bytes: usize,
}

impl Limits {
    #[inline]
    pub(crate) const fn encoded_bytes(self) -> usize {
        self.bytes.saturating_mul(4).saturating_add(64 * 1024)
    }
}

impl Default for Limits {
    #[inline]
    fn default() -> Self {
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
    /// The `tRNS` chunk verbatim, when present.
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
    #[inline]
    pub const fn buffer_size(&self) -> usize {
        self.line_size * self.height as usize
    }
}

/// Why a PNG could not be decoded.
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
    /// A header field is not a legal PNG value, or the chunk sequence is not a legal PNG structure (no IHDR first, an indexed image with no palette, a `tRNS` of the wrong length, …).
    Malformed(&'static str),
    /// The compressed image data is not a valid zlib stream.
    BadZlib,
    /// The image data decompresses to substantially MORE than the header's dimensions call for.
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

/// Lets a decode sit inside an `io::Result` chain with `?`.
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
    /// The caller's parameters cannot describe a PNG (zero dimensions, a colour-type/bit-depth pair the format forbids, an indexed image with no palette, …).
    Parameter(&'static str),
    /// The pixel buffer's length does not match the declared geometry: the whole image's ([`Writer::write_image_data`]) or one row's ([`Writer::write_rows`]).
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

/// Lets an encode sit inside an `io::Result` chain with `?`.
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

/// Bytes needed for `width` pixels of `samples` samples at `depth` bits, rounded up to a whole byte (PNG pads each scanline to a byte boundary).
#[inline]
pub(crate) const fn row_bytes(width: u32, samples: usize, depth: u32) -> Option<usize> {
    let Some(w_samples) = (width as usize).checked_mul(samples) else {
        return None;
    };
    let Some(bits) = w_samples.checked_mul(depth as usize) else {
        return None;
    };
    Some(bits.div_ceil(8))
}
