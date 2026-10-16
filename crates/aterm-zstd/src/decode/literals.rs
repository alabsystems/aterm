// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/blocks/literals_section.rs and
// src/decoding/literals_section_decoder.rs, modified by the aterm project in
// 2026. See ../../LICENSE-MIT for the upstream MIT license.

//! The literals section, the first half of a compressed block (RFC 8878
//! §3.1.1.3.1): the bytes the sequences copy from, stored raw, as one
//! repeated byte, or Huffman-coded in one or four streams.

use super::error::DecodeError;
use super::huffman::HuffmanTable;
use super::window::SLACK;

/// Fewest literals a four-stream section may hold (libzstd's
/// `MIN_LITERALS_FOR_4_STREAMS`): below it the fourth stream's share of the
/// split would be negative.
const MIN_LITERALS_FOR_4_STREAMS: usize = 6;

/// `Literals_Block_Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Raw,
    Rle,
    Compressed,
    /// Huffman-coded with the previous compressed section's table.
    Treeless,
}

/// A parsed `Literals_Section_Header`.
#[derive(Clone, Copy, Debug)]
struct Header {
    kind: Kind,
    /// Header bytes (1 to 5).
    len: usize,
    regenerated_size: usize,
    /// Bytes after the header: the Huffman streams (and tree), or the raw
    /// literals, or the one RLE byte.
    compressed_size: usize,
    four_streams: bool,
}

/// Parses the header at the front of `raw` (RFC 8878 §3.1.1.3.1.1).
///
/// Each size format reads its fixed count of header bytes as one
/// little-endian word and takes the size fields out with constant shifts and
/// masks, so every field is bounded by its width.
fn parse_header(raw: &[u8]) -> Result<Header, DecodeError> {
    let Some(&b0) = raw.first() else {
        return Err(short_header());
    };
    let kind = match b0 & 3 {
        0 => Kind::Raw,
        1 => Kind::Rle,
        2 => Kind::Compressed,
        _ => Kind::Treeless,
    };
    let size_format = (b0 >> 2) & 3;
    let header = match kind {
        Kind::Raw | Kind::Rle => {
            let (len, regenerated) = match size_format {
                // One bit of size format, five bits of size.
                0 | 2 => (1, u32::from(b0 >> 3)),
                // 12 bits.
                1 => {
                    let [h0, h1] = head::<2>(raw)?;
                    (2, u32::from(u16::from_le_bytes([h0, h1]) >> 4))
                }
                // 20 bits.
                _ => {
                    let [h0, h1, h2] = head::<3>(raw)?;
                    (3, u32::from_le_bytes([h0, h1, h2, 0]) >> 4)
                }
            };
            let regenerated = regenerated as usize;
            Header {
                kind,
                len,
                regenerated_size: regenerated,
                compressed_size: if kind == Kind::Rle { 1 } else { regenerated },
                four_streams: false,
            }
        }
        Kind::Compressed | Kind::Treeless => {
            let (len, regenerated, compressed) = match size_format {
                // Both sizes 10 bits; format 0 is one stream, 1 is four.
                0 | 1 => {
                    let [h0, h1, h2] = head::<3>(raw)?;
                    let word = u32::from_le_bytes([h0, h1, h2, 0]);
                    (3, (word >> 4) & 0x3FF, (word >> 14) & 0x3FF)
                }
                // Both 14 bits.
                2 => {
                    let word = u32::from_le_bytes(head::<4>(raw)?);
                    (4, (word >> 4) & 0x3FFF, (word >> 18) & 0x3FFF)
                }
                // Both 18 bits.
                _ => {
                    let [h0, h1, h2, h3, h4] = head::<5>(raw)?;
                    let word = u64::from_le_bytes([h0, h1, h2, h3, h4, 0, 0, 0]);
                    (
                        5,
                        ((word >> 4) & 0x3_FFFF) as u32,
                        ((word >> 22) & 0x3_FFFF) as u32,
                    )
                }
            };
            Header {
                kind,
                len,
                regenerated_size: regenerated as usize,
                compressed_size: compressed as usize,
                four_streams: size_format != 0,
            }
        }
    };
    Ok(header)
}

/// The first `N` bytes of `raw`: a literals section header of that length.
fn head<const N: usize>(raw: &[u8]) -> Result<[u8; N], DecodeError> {
    match raw.first_chunk::<N>() {
        Some(bytes) => Ok(*bytes),
        None => Err(short_header()),
    }
}

fn short_header() -> DecodeError {
    DecodeError::Corrupt("literals section header runs past the block")
}

/// Decodes the literals section at the front of `block[..size]`, where
/// `block` runs [`SLACK`] bytes past `size`. Returns the literals, the count
/// of them, and the bytes of the block the section took; the literals slice
/// holds [`SLACK`] bytes past the last literal for chunked copies (borrowed
/// from `block` when stored raw, else decoded into `buf`).
pub(crate) fn decode<'a>(
    block: &'a [u8],
    size: usize,
    huf: &mut HuffmanTable,
    buf: &'a mut Vec<u8>,
    block_max: usize,
) -> Result<(&'a [u8], usize, usize), DecodeError> {
    let block_body = block.get(..size).unwrap_or_default();
    let header = parse_header(block_body)?;
    if header.regenerated_size > block_max {
        return Err(DecodeError::Corrupt("literals larger than a block"));
    }
    let end = header.len + header.compressed_size;
    let Some(body) = block_body.get(header.len..end) else {
        return Err(DecodeError::Corrupt("literals run past the block"));
    };
    let count = header.regenerated_size;
    match header.kind {
        Kind::Raw => {
            // The block's own slack follows its last byte.
            let lits = block.get(header.len..).unwrap_or_default();
            Ok((lits, count, end))
        }
        Kind::Rle => {
            let byte = body.first().copied().unwrap_or(0);
            buf.clear();
            buf.resize(count, byte);
            buf.resize(count + SLACK, 0);
            Ok((buf.as_slice(), count, end))
        }
        Kind::Compressed | Kind::Treeless => {
            buf.clear();
            decode_huffman(&header, body, huf, buf)?;
            buf.resize(count + SLACK, 0);
            Ok((buf.as_slice(), count, end))
        }
    }
}

/// Huffman-coded literals: the tree (Compressed only), then one stream, or a
/// 6-byte jump table and four.
fn decode_huffman(
    header: &Header,
    body: &[u8],
    huf: &mut HuffmanTable,
    out: &mut Vec<u8>,
) -> Result<(), DecodeError> {
    let regenerated = header.regenerated_size;
    // Treeless reuses the last table; with none, decoding refuses.
    let streams = if header.kind == Kind::Compressed {
        let tree = huf.build_decoder(body)?;
        body.get(tree..)
            .ok_or(DecodeError::Corrupt("Huffman tree runs past the literals"))?
    } else {
        body
    };
    let start = out.len();
    out.resize(start + regenerated, 0);
    let dst = out.get_mut(start..).unwrap_or_default();

    if !header.four_streams {
        return huf.decode_streams([streams], [dst]);
    }
    if regenerated < MIN_LITERALS_FOR_4_STREAMS {
        return Err(DecodeError::Corrupt("too few literals for four streams"));
    }
    let Some((jumps, data)) = streams.split_first_chunk::<6>() else {
        return Err(DecodeError::Corrupt(
            "four-stream literals lack their jump table",
        ));
    };
    let [a0, a1, b0, b1, c0, c1] = *jumps;
    let end1 = usize::from(u16::from_le_bytes([a0, a1]));
    let end2 = end1 + usize::from(u16::from_le_bytes([b0, b1]));
    let end3 = end2 + usize::from(u16::from_le_bytes([c0, c1]));
    if end3 > data.len() {
        return Err(DecodeError::Corrupt(
            "four-stream jump table runs past the literals",
        ));
    }
    // The first three streams each decode a quarter (rounded up); the
    // fourth, the rest (at least 6 literals keep it non-negative).
    let quarter = regenerated.div_ceil(4);
    let Some((d0, rest)) = dst.split_at_mut_checked(quarter) else {
        return Err(DecodeError::Corrupt("too few literals for four streams"));
    };
    let Some((d1, rest)) = rest.split_at_mut_checked(quarter) else {
        return Err(DecodeError::Corrupt("too few literals for four streams"));
    };
    let Some((d2, d3)) = rest.split_at_mut_checked(quarter) else {
        return Err(DecodeError::Corrupt("too few literals for four streams"));
    };
    let streams = [
        data.get(..end1).unwrap_or_default(),
        data.get(end1..end2).unwrap_or_default(),
        data.get(end2..end3).unwrap_or_default(),
        data.get(end3..).unwrap_or_default(),
    ];
    huf.decode_streams(streams, [d0, d1, d2, d3])
}

#[cfg(test)]
mod tests {
    use super::{Kind, parse_header};

    #[test]
    fn header_sizes_at_each_format() {
        // Raw, 1-byte header, 5-bit size 31.
        let h = parse_header(&[31 << 3]).unwrap();
        assert_eq!((h.kind, h.len, h.regenerated_size), (Kind::Raw, 1, 31));
        // RLE, 2-byte header: 12-bit size 0xABC.
        let h = parse_header(&[0b1100_0101, 0xAB]).unwrap();
        assert_eq!(
            (h.kind, h.len, h.regenerated_size, h.compressed_size),
            (Kind::Rle, 2, 0xABC, 1)
        );
        // Raw, 3-byte header: 20-bit size.
        let h = parse_header(&[0x0C | 0x10, 0x23, 0x01]).unwrap();
        assert_eq!((h.len, h.regenerated_size), (3, 0x1231));
        // Compressed, one stream, 10-bit sizes 1023 and 1023.
        let h = parse_header(&[0xF2, 0xFF, 0xFF]).unwrap();
        assert_eq!(
            (
                h.kind,
                h.len,
                h.regenerated_size,
                h.compressed_size,
                h.four_streams
            ),
            (Kind::Compressed, 3, 1023, 1023, false)
        );
        // Treeless, 18-bit sizes.
        let h = parse_header(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF]).unwrap();
        assert_eq!(
            (
                h.kind,
                h.len,
                h.regenerated_size,
                h.compressed_size,
                h.four_streams
            ),
            (Kind::Treeless, 5, (1 << 18) - 1, (1 << 18) - 1, true)
        );
        // Compressed, four streams, 14-bit sizes 0x2ABC and 0x1234.
        let h = parse_header(&[0xCA, 0xAB, 0xD2, 0x48]).unwrap();
        assert_eq!(
            (
                h.kind,
                h.len,
                h.regenerated_size,
                h.compressed_size,
                h.four_streams
            ),
            (Kind::Compressed, 4, 0x2ABC, 0x1234, true)
        );
        // RLE, 3-byte header: 20-bit size 0xFEDCB.
        let h = parse_header(&[0xBD, 0xDC, 0xFE]).unwrap();
        assert_eq!(
            (h.kind, h.len, h.regenerated_size, h.compressed_size),
            (Kind::Rle, 3, 0xF_EDCB, 1)
        );
        // Each format one byte short of its header.
        assert!(parse_header(&[]).is_err());
        assert!(parse_header(&[0b0100]).is_err());
        assert!(parse_header(&[0x0E, 0, 0]).is_err());
        assert!(parse_header(&[0xCA, 0xAB, 0xD2]).is_err());
        assert!(parse_header(&[0xBD, 0xDC]).is_err());
        assert!(parse_header(&[0x02, 0]).is_err());
    }
}
