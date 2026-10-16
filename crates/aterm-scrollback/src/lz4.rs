// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Thin wrapper over [`aterm_lz4`] for scrollback compression.
//!
//! Keeps the scrollback-facing [`Lz4Error`] surface and adds the
//! scrollback-layer caps (the 64 MiB page cap on output, 512 : 1 claimed
//! ratio). The wire format is the standard LZ4 block with a 4-byte
//! little-endian size prefix.

use std::fmt;

/// Error type for LZ4 decompression failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lz4Error {
    /// Input is too short to contain valid data (shorter than the 4-byte
    /// size prefix).
    InputTooShort,
    /// The size prefix indicates a length that exceeds the scrollback
    /// safety cap ([`MAX_DECOMPRESSED_SIZE`]).
    OutputTooLarge(usize),
    /// A back-reference offset is zero or exceeds the output written so far.
    InvalidOffset {
        /// The offset value from the compressed stream.
        offset: usize,
        /// How many bytes have been written so far.
        output_pos: usize,
    },
    /// The decompressed output does not match the expected size from the prefix.
    SizeMismatch {
        /// Expected size from the 4-byte prefix.
        expected: usize,
        /// Actual decompressed size.
        actual: usize,
    },
    /// Compressed data ended unexpectedly mid-sequence, or was otherwise
    /// rejected by the decoder for a reason not covered by the other variants.
    UnexpectedEof,
    /// The claimed decompressed size implies a compression ratio over
    /// [`MAX_COMPRESSION_RATIO`]: a corrupt or adversarial size prefix.
    CompressionBombSuspected {
        /// Claimed decompressed size (from the 4-byte LE prefix).
        claimed_size: usize,
        /// Size of the compressed body (input minus 4-byte prefix).
        compressed_body_len: usize,
    },
}

impl Lz4Error {
    /// Render the `Display` message without `format_args!`, whose expansion
    /// embeds an unsafe `fmt::Arguments` construction the strict Trust gate
    /// fails closed on (`dec_string` renders decimals like `usize`'s `Display`).
    pub(crate) fn to_message(&self) -> String {
        use crate::error::dec_string;
        match self {
            Self::InputTooShort => String::from("LZ4: input too short"),
            Self::OutputTooLarge(size) => {
                let mut s = String::from("LZ4: decompressed size ");
                s.push_str(&dec_string(*size));
                s.push_str(" exceeds safety limit");
                s
            }
            Self::InvalidOffset { offset, output_pos } => {
                let mut s = String::from("LZ4: invalid back-reference offset ");
                s.push_str(&dec_string(*offset));
                s.push_str(" at output position ");
                s.push_str(&dec_string(*output_pos));
                s
            }
            Self::SizeMismatch { expected, actual } => {
                let mut s = String::from("LZ4: size mismatch: expected ");
                s.push_str(&dec_string(*expected));
                s.push_str(" bytes, got ");
                s.push_str(&dec_string(*actual));
                s
            }
            Self::UnexpectedEof => String::from("LZ4: unexpected end of compressed data"),
            Self::CompressionBombSuspected {
                claimed_size,
                compressed_body_len,
            } => {
                let mut s = String::from("LZ4: claimed size ");
                s.push_str(&dec_string(*claimed_size));
                s.push_str(" from ");
                s.push_str(&dec_string(*compressed_body_len));
                s.push_str("-byte body exceeds safe compression ratio (possible compression bomb)");
                s
            }
        }
    }
}

impl fmt::Display for Lz4Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_message())
    }
}

impl std::error::Error for Lz4Error {}

/// Maximum safe decompressed size: the scrollback page cap
/// ([`MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES`](crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES)),
/// the limit the warm tier pages its blocks by, so no block it stores is
/// refused. A scrollback policy against allocation bombs, not an LZ4 format
/// constraint, so it lives here rather than in [`aterm_lz4`].
const MAX_DECOMPRESSED_SIZE: usize = crate::codec::MAX_DECOMPRESSED_SCROLLBACK_PAGE_BYTES;

/// Maximum acceptable claimed compression ratio (prefix size ÷ compressed body).
///
/// The absolute cap alone lets a 4-byte prefix turn a ~5-byte payload into a
/// 64 MiB allocation. LZ4's block format tops out near 255 : 1 and real
/// scrollback pages compress at 1 : 1 to ~40 : 1, so 512 : 1 only catches a
/// corrupt or adversarial prefix. Checked before decompression runs.
const MAX_COMPRESSION_RATIO: usize = 512;

/// Compress `input` using LZ4 block format, prepending the original size as
/// a 4-byte little-endian prefix.
///
/// The output layout is: `[original_size: u32 LE][lz4_block_data...]`.
///
/// Returns `Err` if `input.len()` exceeds `u32::MAX` (the 4-byte size prefix
/// cannot represent the original size).
pub fn compress_prepend_size(input: &[u8]) -> Result<Vec<u8>, Lz4Error> {
    // `aterm_lz4` does not check that `input.len()` fits the u32 prefix.
    u32::try_from(input.len()).map_err(|_| Lz4Error::OutputTooLarge(input.len()))?;
    Ok(aterm_lz4::compress_prepend_size(input))
}

/// Decompress data that was produced by [`compress_prepend_size`] (or the
/// equivalent upstream `lz4_flex::compress_prepend_size`).
///
/// Reads a 4-byte LE size prefix, enforces the scrollback-layer page cap
/// against allocation bombs, then decompresses the remainder.
pub fn decompress_size_prepended(input: &[u8]) -> Result<Vec<u8>, Lz4Error> {
    if input.len() < 4 {
        return Err(Lz4Error::InputTooShort);
    }
    let original_size = u32::from_le_bytes([input[0], input[1], input[2], input[3]]) as usize;
    if original_size > MAX_DECOMPRESSED_SIZE {
        return Err(Lz4Error::OutputTooLarge(original_size));
    }
    // Fast path: zero-length payload. `aterm_lz4` (and upstream `lz4_flex`)
    // require at least one token byte in the block; an empty body after a
    // zero-size prefix is rejected as `ExpectedAnotherByte`. Scrollback
    // callers treat an empty block as a valid zero-byte decode.
    if original_size == 0 && input.len() == 4 {
        return Ok(Vec::new());
    }
    // Compression-bomb ceiling (see MAX_COMPRESSION_RATIO), checked before the
    // decoder allocates anything. `input.len() >= 4` (guard above), so the
    // subtraction cannot underflow.
    let compressed_body_len = input.len() - 4;
    if original_size > compressed_body_len.saturating_mul(MAX_COMPRESSION_RATIO) {
        return Err(Lz4Error::CompressionBombSuspected {
            claimed_size: original_size,
            compressed_body_len,
        });
    }
    // Callers wrap the error's message into `ScrollbackError::Decompression`.
    match aterm_lz4::decompress_size_prepended(input) {
        Ok(output) => {
            if output.len() != original_size {
                return Err(Lz4Error::SizeMismatch {
                    expected: original_size,
                    actual: output.len(),
                });
            }
            Ok(output)
        }
        Err(err) => Err(map_decompress_error(err)),
    }
}

/// Map [`aterm_lz4::DecompressError`] variants onto the scrollback
/// [`Lz4Error`] surface. `OffsetOutOfBounds` maps onto `InvalidOffset`
/// (without the precise offset/output_pos fields, which the backend does
/// not expose); everything else collapses to `UnexpectedEof`.
fn map_decompress_error(err: aterm_lz4::DecompressError) -> Lz4Error {
    match err {
        aterm_lz4::DecompressError::OffsetOutOfBounds => Lz4Error::InvalidOffset {
            offset: 0,
            output_pos: 0,
        },
        // `DecompressError` is `#[non_exhaustive]`; every other variant means the
        // decoder could not finish.
        _ => Lz4Error::UnexpectedEof,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_size_prefix_empty_body_is_ok() {
        // 4-byte prefix of 0 + 0-byte body = valid "empty" decode.
        let r = decompress_size_prepended(&[0, 0, 0, 0]);
        assert!(r.is_ok(), "empty claim + empty body must succeed: {r:?}");
        assert_eq!(r.unwrap().len(), 0);
    }

    #[test]
    fn garbage_after_valid_prefix_errors_not_panics() {
        // Prefix says "4 bytes", body is garbage. Must not panic.
        for byte in 0u8..=255 {
            let input = [4, 0, 0, 0, byte];
            let _ = decompress_size_prepended(&input);
        }
    }

    /// A decode that succeeds returns exactly the length its prefix claims, so a
    /// peer cannot make the decoder emit more or fewer bytes than declared.
    /// Covers every claim 0..=16 × every body of 0–2 bytes (1.1M inputs); every
    /// input shorter than the prefix is `InputTooShort`.
    #[test]
    fn small_claims_decode_to_their_claim_or_fail() {
        let mut bodies: Vec<Vec<u8>> = vec![Vec::new()];
        bodies.extend((0..=u8::MAX).map(|a| vec![a]));
        for a in 0..=u8::MAX {
            bodies.extend((0..=u8::MAX).map(|b| vec![a, b]));
        }
        let mut decoded = 0usize;
        for claim in 0..=16u32 {
            for body in &bodies {
                let input = [&claim.to_le_bytes()[..], body].concat();
                if let Ok(out) = decompress_size_prepended(&input) {
                    assert_eq!(out.len(), claim as usize, "{input:02x?}");
                    decoded += 1;
                }
            }
        }
        assert!(decoded > 0, "no input decoded: the law held vacuously");
        for short in [&b""[..], b"\x00", b"\x00\x00", b"\xff\xff\xff"] {
            assert_eq!(
                decompress_size_prepended(short),
                Err(Lz4Error::InputTooShort),
                "{short:02x?}"
            );
        }
    }

    #[test]
    fn test_decompress_too_short() {
        assert!(matches!(
            decompress_size_prepended(b"abc"),
            Err(Lz4Error::InputTooShort)
        ));
    }

    #[test]
    fn test_max_decompressed_size_rejection() {
        // Craft a size prefix claiming one byte over the limit.
        let size = u32::try_from(MAX_DECOMPRESSED_SIZE + 1).expect("cap fits the prefix");
        let mut data = size.to_le_bytes().to_vec();
        data.push(0x00);
        assert!(matches!(
            decompress_size_prepended(&data),
            Err(Lz4Error::OutputTooLarge(_))
        ));
    }

    #[test]
    fn test_max_decompressed_size_at_limit() {
        // Exactly the limit should be accepted (the check is >).
        let input = vec![0u8; MAX_DECOMPRESSED_SIZE];
        let compressed = compress_prepend_size(&input).unwrap();
        let decompressed = decompress_size_prepended(&compressed).expect("at limit");
        assert_eq!(decompressed.len(), MAX_DECOMPRESSED_SIZE);
    }

    #[test]
    fn test_compression_bomb_rejected_under_limit() {
        // Claim 1 MiB, under MAX_DECOMPRESSED_SIZE, from a 100-byte body:
        // ~10,485 : 1, far above the 512 : 1 ceiling.
        let claimed: u32 = 1024 * 1024;
        let mut data = claimed.to_le_bytes().to_vec();
        data.extend(std::iter::repeat_n(0u8, 100));
        let err = decompress_size_prepended(&data)
            .expect_err("impossible ratio must be rejected as a compression bomb");
        match err {
            Lz4Error::CompressionBombSuspected {
                claimed_size,
                compressed_body_len,
            } => {
                assert_eq!(claimed_size, claimed as usize);
                assert_eq!(compressed_body_len, 100);
            }
            other => panic!("expected CompressionBombSuspected, got {other:?}"),
        }
    }

    #[test]
    fn test_compression_bomb_rejected_tiny_body_huge_claim() {
        // The classic bomb shape: minimal body, large claimed size.
        // Claim 16 MiB from a 16-byte body (~1 048 576 : 1 ratio).
        let claimed: u32 = 16 * 1024 * 1024;
        let mut data = claimed.to_le_bytes().to_vec();
        data.extend(std::iter::repeat_n(0u8, 16));
        let err = decompress_size_prepended(&data).expect_err("must reject bomb");
        assert!(matches!(err, Lz4Error::CompressionBombSuspected { .. }));
    }

    #[test]
    fn test_legitimate_high_ratio_still_accepted() {
        // All-zeros compresses to ~tens of bytes but the ratio stays under
        // the 512 : 1 ceiling for realistic sizes. Must still round-trip.
        let input = vec![0u8; 8 * 1024]; // 8 KiB zeros
        let compressed = compress_prepend_size(&input).unwrap();
        let ratio = input.len() / (compressed.len() - 4);
        assert!(
            ratio < MAX_COMPRESSION_RATIO,
            "test assumes 8 KiB zeros compresses with ratio < {MAX_COMPRESSION_RATIO}; got {ratio}"
        );
        let decompressed = decompress_size_prepended(&compressed)
            .expect("legitimate high-ratio input must still round-trip");
        assert_eq!(decompressed, input);
    }

    #[test]
    fn test_ratio_boundary_exactly_at_limit_accepted() {
        // claimed = MAX_COMPRESSION_RATIO * body_len passes the ratio check; the
        // garbage body then fails the decode, but not as a bomb.
        let body_len = 64usize;
        let claimed = MAX_COMPRESSION_RATIO * body_len; // exactly at limit
        let claimed_u32 = u32::try_from(claimed).expect("test claim fits in u32");
        let mut data = claimed_u32.to_le_bytes().to_vec();
        data.extend(std::iter::repeat_n(0u8, body_len));
        let err = decompress_size_prepended(&data).expect_err("garbage payload fails decode");
        // Not the bomb error: the claim is at the boundary, not over it.
        assert!(
            !matches!(err, Lz4Error::CompressionBombSuspected { .. }),
            "ratio exactly at MAX_COMPRESSION_RATIO must be accepted by the bomb gate; got {err:?}"
        );
    }

    #[test]
    fn test_cross_impl_compatibility_with_aterm_lz4() {
        // Data compressed directly by `aterm_lz4` (bypassing the scrollback
        // u32 validation) must round-trip through this module's
        // `decompress_size_prepended`. Guards against accidental wire-format
        // divergence between the wrapper and the backend.
        let input: Vec<u8> = b"scrollback-page-XYZ"
            .iter()
            .copied()
            .cycle()
            .take(4096)
            .collect();
        let compressed = aterm_lz4::compress_prepend_size(&input);
        let decompressed = decompress_size_prepended(&compressed)
            .expect("aterm_lz4 output must decompress via scrollback wrapper");
        assert_eq!(decompressed, input);
    }
}
