// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Thin wrapper over [`aterm_lz4`] for scrollback compression (#7943).
//!
//! Historically this module contained an inline LZ4 block-format
//! compressor/decompressor (introduced by #7698 during the zero-external-
//! dependency campaign). Wave 4 of that campaign (#7730) separately vendored
//! a more thorough, upstream-tracking block-mode subset of `lz4_flex` into
//! the in-tree [`aterm_lz4`] crate. For a while both implementations coexisted.
//! Issue #7943 consolidates onto the single [`aterm_lz4`] backend and leaves
//! this module as a thin wrapper that preserves the scrollback-specific
//! [`Lz4Error`] surface and the 16 MiB allocation-bomb cap.
//!
//! On-wire compatibility: both the previous inline implementation and
//! [`aterm_lz4`] emit the **standard LZ4 block format** with a 4-byte
//! little-endian size prefix. Pages compressed by either implementation
//! decompress correctly here, so no scrollback snapshot migration is needed.
//!
//! Public surface:
//!   - [`compress_prepend_size`]: LZ4 block compress with 4-byte LE size prefix.
//!   - [`decompress_size_prepended`]: decompress with allocation-bomb cap.
//!   - [`Lz4Error`]: scrollback-facing error enum. Variants that cannot be
//!     produced by the current backend are retained for backwards-compatible
//!     pattern matching by callers (notably the Kani proofs in
//!     `lz4_kani.rs`).

use std::fmt;

/// Error type for LZ4 decompression failures.
///
/// Matches the variants the original inline implementation could produce,
/// kept stable so the scrollback Kani proofs (#7934) and regression tests
/// can continue to `matches!` on the same enum.
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
    /// The claimed decompressed size implies a compression ratio that exceeds
    /// [`MAX_COMPRESSION_RATIO`] — interpreted as a potential compression bomb
    /// (corrupted or adversarial size prefix inflating a tiny payload).
    /// #7940 §09 compression-bomb ceiling.
    CompressionBombSuspected {
        /// Claimed decompressed size (from the 4-byte LE prefix).
        claimed_size: usize,
        /// Size of the compressed body (input minus 4-byte prefix).
        compressed_body_len: usize,
    },
}

impl Lz4Error {
    /// Render the exact `Display` message without going through
    /// `format_args!`: the `write!` expansion embeds an unsafe
    /// `fmt::Arguments` construction the strict Trust gate cannot lower and
    /// fails closed on. Byte-identical to the previous `write!`-based arms
    /// (`dec_string` renders decimals exactly like `usize`'s `Display`).
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

/// Maximum safe decompressed size (16 MiB). Prevents allocation bombs from
/// corrupt size prefixes while being generous enough for any real scrollback
/// page. This cap is enforced here (not in the shared [`aterm_lz4`] crate)
/// because it is a scrollback-layer policy, not an LZ4 format constraint.
const MAX_DECOMPRESSED_SIZE: usize = 16 * 1024 * 1024;

/// Maximum acceptable compression ratio (output bytes ÷ compressed bytes).
///
/// #7940 §09 compression-bomb ceiling: the absolute `MAX_DECOMPRESSED_SIZE`
/// cap alone is not enough — a 4-byte size prefix claiming 16 MiB expands a
/// ~5-byte payload into 16 MiB of allocation, which on systems that don't
/// bound memory tightly amplifies attacker bytes 3 000 000 : 1 before the
/// allocation-bomb cap even engages.
///
/// LZ4 block format has a theoretical maximum ratio of 255 : 1 (the longest
/// single match token); real scrollback pages compress at 1 : 1 to ~40 : 1.
/// We set the guardrail at 512 : 1 — well above any realistic scrollback
/// workload, but catches "1 MB of decompressed zeroes from 16 bytes of
/// input" style amplifications where a corrupted or adversarial size prefix
/// claims orders of magnitude more than the payload can legitimately hold.
///
/// This is a ceiling on *claimed* ratio (prefix_size / compressed_body_len),
/// checked before decompression actually runs, so an attacker cannot force
/// a 16 MiB allocation from a 4-byte LZ4 block.
const MAX_COMPRESSION_RATIO: usize = 512;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compress `input` using LZ4 block format, prepending the original size as
/// a 4-byte little-endian prefix.
///
/// The output layout is: `[original_size: u32 LE][lz4_block_data...]`.
///
/// Returns `Err` if `input.len()` exceeds `u32::MAX` (the 4-byte size prefix
/// cannot represent the original size).
pub fn compress_prepend_size(input: &[u8]) -> Result<Vec<u8>, Lz4Error> {
    // The underlying `aterm_lz4::compress_prepend_size` takes `&[u8]` and
    // returns `Vec<u8>`; it does not validate that `input.len()` fits in a
    // `u32`. Do that check here so the 4-byte LE prefix is always honest.
    u32::try_from(input.len()).map_err(|_| Lz4Error::OutputTooLarge(input.len()))?;
    Ok(aterm_lz4::compress_prepend_size(input))
}

/// Decompress data that was produced by [`compress_prepend_size`] (or the
/// equivalent upstream `lz4_flex::compress_prepend_size`).
///
/// Reads a 4-byte LE size prefix, enforces the scrollback-layer 16 MiB
/// allocation-bomb cap, then decompresses the remainder.
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
    // zero-size prefix is rejected as `ExpectedAnotherByte`. The previous
    // inline decoder, and the scrollback callers that rely on it, treat an
    // empty block as a valid zero-byte decode, so preserve that behavior.
    if original_size == 0 && input.len() == 4 {
        return Ok(Vec::new());
    }
    // #7940 §09 compression-bomb ceiling: reject claimed sizes that imply a
    // ratio far outside the LZ4 format's theoretical maximum. A corrupted or
    // adversarial prefix is the only way claimed_size / body_len can exceed
    // 512 : 1 in normal use. Checked *before* the decoder runs so we do not
    // allocate even a scratch buffer for a suspected bomb.
    //
    // The body length is input.len() - 4 (strip the size prefix). We know
    // input.len() >= 5 at this point: the `< 4` guard rejected shorter
    // inputs, and the `original_size == 0 && input.len() == 4` fast path
    // returned above.
    let compressed_body_len = input.len() - 4;
    if original_size > compressed_body_len.saturating_mul(MAX_COMPRESSION_RATIO) {
        return Err(Lz4Error::CompressionBombSuspected {
            claimed_size: original_size,
            compressed_body_len,
        });
    }
    // Delegate the actual decode to `aterm_lz4`. Its `DecompressError`
    // distinguishes offset-out-of-bounds, literal-out-of-bounds, expected-
    // another-byte, and output-too-small — all of which we collapse to
    // `UnexpectedEof` here (except the one case that maps cleanly to
    // `InvalidOffset`). The scrollback call sites only care about
    // `InputTooShort` and `OutputTooLarge`; everything else flows through
    // `Display` and gets formatted into the surrounding
    // `ScrollbackError::Decompression` context.
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
        // `DecompressError` is `#[non_exhaustive]` upstream, so any variant
        // we don't call out explicitly is collapsed to `UnexpectedEof`. That
        // matches the "decoder ran out of input or encountered something it
        // couldn't finish" semantics the scrollback layer cares about.
        _ => Lz4Error::UnexpectedEof,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decompress_too_short() {
        assert!(matches!(
            decompress_size_prepended(b"abc"),
            Err(Lz4Error::InputTooShort)
        ));
    }

    #[test]
    fn test_max_decompressed_size_rejection() {
        // Craft a size prefix claiming 17 MiB (just over the 16 MiB limit).
        let size: u32 = 17 * 1024 * 1024;
        let mut data = size.to_le_bytes().to_vec();
        data.push(0x00);
        assert!(matches!(
            decompress_size_prepended(&data),
            Err(Lz4Error::OutputTooLarge(_))
        ));
    }

    #[test]
    fn test_max_decompressed_size_at_limit() {
        // Exactly 16 MiB should be accepted (the check is >).
        let input = vec![0u8; 16 * 1024 * 1024];
        let compressed = compress_prepend_size(&input).unwrap();
        let decompressed = decompress_size_prepended(&compressed).expect("16 MiB at limit");
        assert_eq!(decompressed.len(), 16 * 1024 * 1024);
    }

    #[test]
    fn test_compression_bomb_rejected_under_limit() {
        // Craft a prefix claiming a size that fits in MAX_DECOMPRESSED_SIZE
        // but implies an impossible compression ratio from the body length.
        // Claim 1 MiB from a 100-byte body (~10,485 : 1 ratio, far above the
        // 512 : 1 ceiling).
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
        // The classic bomb shape: minimal body, near-max claimed size.
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
        // Sanity check: this workload should be under the 512 ceiling.
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
        // claimed = MAX_COMPRESSION_RATIO * body_len is accepted;
        // claimed = MAX_COMPRESSION_RATIO * body_len + 1 is rejected.
        // We construct an artificial input that will fail decode (since we're
        // not building a real LZ4 block), but the ratio check runs first and
        // should not fire at the boundary.
        let body_len = 64usize;
        let claimed = MAX_COMPRESSION_RATIO * body_len; // exactly at limit
        let claimed_u32 = u32::try_from(claimed).expect("test claim fits in u32");
        let mut data = claimed_u32.to_le_bytes().to_vec();
        data.extend(std::iter::repeat_n(0u8, body_len));
        let err = decompress_size_prepended(&data).expect_err("garbage payload fails decode");
        // Must NOT be the bomb error — we're at the boundary, not over it.
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
