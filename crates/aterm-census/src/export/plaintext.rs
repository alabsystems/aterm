// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! What the publication engine's content guard reads of an exported file
//! beyond its bytes, by the file's first bytes (never its name): the plaintext
//! of a gzip or zstd stream, and the text of a UTF-16/32 file with a BOM.
//!
//! This is the engine's `bin/pub` (`_compressed_kind`, `_gzip_decode`,
//! `_zstd_decode`, `_decompressed_gzip_scan`, and the BOM branch of
//! `_forbidden_records`), decoded with aterm's own inflate and zstd decoder.
//! Where the engine's reading depends on a decoder's partial output that these
//! do not reproduce (zlib's inside a broken deflate stream, the zstd CLI's
//! inside a frame it fails), the file is [`Plaintext::Unjudged`], never passed.

use std::io::Read as _;

use aterm_codec::crc32::Crc32;
use aterm_codec::inflate::{InflateError, inflate_prefix};

/// What the engine reads of one file besides its bytes and their lines.
#[derive(Debug, PartialEq, Eq)]
pub enum Plaintext {
    /// Nothing more.
    None,
    /// A UTF-16/32 file that opens with a BOM, decoded as the engine decodes it
    /// (`bytes.decode(encoding, errors="ignore")`, the BOM naming the byte
    /// order and dropped). The engine searches the text whole with Python's
    /// `re`, besides its grep of the bytes.
    Bom {
        /// `utf-16` or `utf-32`.
        encoding: &'static str,
        /// The decoded text.
        text: String,
    },
    /// A gzip or zstd stream, and the copy the engine greps (`-a`, a binary
    /// file's verdict): the plaintext, then each gzip member's FEXTRA, FNAME
    /// and FCOMMENT, or a zstd file's raw bytes, each on a line of its own.
    Stream {
        /// `gzip` or `zstd`.
        kind: &'static str,
        /// The copy.
        copy: Vec<u8>,
        /// `None` when the stream decoded whole: the copy's verdict replaces
        /// the bytes' binary one. Otherwise why it stopped where the engine's
        /// decoder stops: the copy holds what decoded before that, and the
        /// bytes' own verdict stands beside it.
        stopped: Option<String>,
    },
    /// A stream this guard cannot read as the engine does, and why.
    Unjudged {
        /// `gzip` or `zstd`.
        kind: &'static str,
        /// Why, as a clause.
        why: String,
    },
}

/// The BOMs, in the engine's order: UTF-32's four bytes before UTF-16's two,
/// so `FF FE 00 00` reads as UTF-32.
const BOMS: [(&[u8], &str); 4] = [
    (&[0xff, 0xfe, 0x00, 0x00], "utf-32"),
    (&[0x00, 0x00, 0xfe, 0xff], "utf-32"),
    (&[0xff, 0xfe], "utf-16"),
    (&[0xfe, 0xff], "utf-16"),
];

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// The engine's bound on one stream's plaintext: 4 GiB, and never 1000 times
/// its compressed size. It reads the first this many bytes of a longer one.
fn engine_bound(compressed: usize) -> usize {
    let bound =
        (4u64 << 30).min(u64::try_from(compressed).map_or(u64::MAX, |c| c.saturating_mul(1000)));
    usize::try_from(bound).unwrap_or(usize::MAX)
}

/// The plaintext this guard holds of one file: a stream that decodes past it
/// is unjudged.
const HELD: usize = 256 << 20;

/// Reads `bytes`, a file as the export carries it, as the engine does.
#[must_use]
pub fn read(bytes: &[u8]) -> Plaintext {
    if bytes.starts_with(&GZIP_MAGIC) {
        return gzip(bytes, engine_bound(bytes.len()));
    }
    let skippable =
        bytes.get(1..4) == Some(&[0x2a, 0x4d, 0x18][..]) && (0x50..=0x5f).contains(&bytes[0]);
    if bytes.starts_with(&ZSTD_MAGIC) || skippable {
        return zstd(bytes, engine_bound(bytes.len()));
    }
    match BOMS.iter().find(|(bom, _)| bytes.starts_with(bom)) {
        Some(&(bom, encoding)) => Plaintext::Bom {
            encoding,
            text: decode_bom(&bytes[bom.len()..], bom[0] == 0xff, bom.len() == 4),
        },
        None => Plaintext::None,
    }
}

/// `body` (the bytes after the BOM) as UTF-16 or UTF-32 (`wide`), dropping
/// what does not decode: a lone surrogate, a code point past U+10FFFF, a
/// trailing partial unit.
fn decode_bom(body: &[u8], little: bool, wide: bool) -> String {
    if wide {
        let (units, _) = body.as_chunks::<4>();
        let unit = |q| {
            if little {
                u32::from_le_bytes(q)
            } else {
                u32::from_be_bytes(q)
            }
        };
        return units
            .iter()
            .filter_map(|&q| char::from_u32(unit(q)))
            .collect();
    }
    let (units, _) = body.as_chunks::<2>();
    let units = units.iter().map(|&p| {
        if little {
            u16::from_le_bytes(p)
        } else {
            u16::from_be_bytes(p)
        }
    });
    char::decode_utf16(units).filter_map(Result::ok).collect()
}

/// The engine's copy: `plain`, then each field on a line of its own.
fn copy(mut plain: Vec<u8>, fields: &[&[u8]]) -> Vec<u8> {
    for field in fields {
        plain.push(b'\n');
        plain.extend_from_slice(field);
    }
    plain.push(b'\n');
    plain
}

fn too_large(bound: usize) -> String {
    format!("it decodes to more than {bound} bytes, the engine's bound of 4 GiB or 1000x its size")
}

fn past_held() -> String {
    format!("decodes past the {HELD} bytes this guard holds")
}

/// The engine's `_gzip_decode`, member by member, reading at most `bound`
/// bytes: every member is decoded and its CRC-32 and ISIZE checked, and a
/// stream that stops on the way stops where the engine's does, with what it
/// had decoded by then.
fn gzip(data: &[u8], bound: usize) -> Plaintext {
    let (mut plain, mut fields) = (Vec::new(), Vec::new());
    match members(data, bound, &mut plain, &mut fields) {
        Ok(stopped) => {
            let fields: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
            Plaintext::Stream {
                kind: "gzip",
                copy: copy(plain, &fields),
                stopped,
            }
        }
        Err(why) => Plaintext::Unjudged { kind: "gzip", why },
    }
}

/// Walks `data`'s members into `plain` and `fields`: `Ok(None)` when every
/// member verified and nothing follows the last, `Ok(Some(why))` when the walk
/// stopped at a header, a trailer or the engine's bound, `Err(why)` when it
/// stopped inside the deflate data, where zlib's partial output (the engine's
/// prefix) is not reproduced here.
fn members(
    data: &[u8],
    bound: usize,
    plain: &mut Vec<u8>,
    fields: &mut Vec<Vec<u8>>,
) -> Result<Option<String>, String> {
    const FHCRC: u8 = 0x02;
    const FEXTRA: u8 = 0x04;
    const FNAME: u8 = 0x08;
    const FCOMMENT: u8 = 0x10;
    let stop = |why: &str| Ok(Some(why.to_string()));
    let truncated = || stop("the stream ends inside a member");
    let mut pos = 0;
    loop {
        let Some(&[_, _, method, flags, ..]) = data.get(pos..pos + 10) else {
            return truncated();
        };
        if method != 8 {
            return stop(&format!("compression method {method}, not deflate"));
        }
        if flags & 0xe0 != 0 {
            return stop("a reserved header flag is set");
        }
        let mut p = pos + 10;
        if flags & FEXTRA != 0 {
            let Some(&[lo, hi]) = data.get(p..p + 2) else {
                return truncated();
            };
            let end = p + 2 + usize::from(u16::from_le_bytes([lo, hi]));
            let Some(field) = data.get(p + 2..end) else {
                return truncated();
            };
            fields.push(field.to_vec());
            p = end;
        }
        for flag in [FNAME, FCOMMENT] {
            if flags & flag != 0 {
                let Some(len) = data[p..].iter().position(|&b| b == 0) else {
                    return truncated();
                };
                fields.push(data[p..p + len].to_vec());
                p += len + 1;
            }
        }
        if flags & FHCRC != 0 {
            let Some(&[lo, hi]) = data.get(p..p + 2) else {
                return truncated();
            };
            if Crc32::of(&data[pos..p]).to_le_bytes()[..2] != [lo, hi] {
                return stop("the header CRC does not match");
            }
            p += 2;
        }
        let (out, used) = match inflate_prefix(&data[p..], HELD.saturating_sub(plain.len())) {
            Ok(done) => done,
            Err(InflateError::OutputTooLarge) => return Err(past_held()),
            Err(e) => {
                return Err(format!(
                    "stops inside its deflate data ({e}), where the engine holds the prefix \
                     zlib decoded, which aterm's inflate does not reproduce"
                ));
            }
        };
        plain.extend_from_slice(&out);
        if plain.len() > bound {
            plain.truncate(bound);
            return stop(&too_large(bound));
        }
        let Some(&[c0, c1, c2, c3, s0, s1, s2, s3]) = data.get(p + used..p + used + 8) else {
            return truncated();
        };
        if Crc32::of(&out) != u32::from_le_bytes([c0, c1, c2, c3]) {
            return stop("the CRC-32 of the decoded bytes does not match the trailer");
        }
        if u32::try_from(out.len()).ok() != Some(u32::from_le_bytes([s0, s1, s2, s3])) {
            return stop("the decoded length does not match the trailer");
        }
        pos = p + used + 8;
        if pos == data.len() {
            return Ok(None);
        }
        if !data[pos..].starts_with(&GZIP_MAGIC) {
            return stop("bytes after the last member are not another gzip member");
        }
    }
}

/// The engine's `_zstd_decode`: the zstd CLI's plaintext, read to `bound`,
/// then the raw bytes (a skippable frame's payload, which a decoder drops, is
/// stored there as it is). A stream refused at a frame's magic number or
/// header stops where the CLI's does, after the whole frames before it; one
/// refused inside a frame is unjudged, as what the CLI prints of that frame
/// is its own.
fn zstd(data: &[u8], bound: usize) -> Plaintext {
    let mut plain = Vec::new();
    let room = bound.min(HELD) + 1;
    let mut decoder = aterm_zstd::Decoder::with_buffer(data);
    let stopped = match (&mut decoder).take(room as u64).read_to_end(&mut plain) {
        Err(e) if decoder.between_frames() && !cli_reads_on(&e) => Some(e.to_string()),
        Err(e) => {
            return Plaintext::Unjudged {
                kind: "zstd",
                why: format!(
                    "does not decode ({e}), where the engine holds what the zstd CLI printed \
                     of it, which aterm's decoder does not reproduce"
                ),
            };
        }
        Ok(_) if plain.len() > bound => {
            plain.truncate(bound);
            Some(too_large(bound))
        }
        Ok(_) if plain.len() > HELD => {
            return Plaintext::Unjudged {
                kind: "zstd",
                why: past_held(),
            };
        }
        Ok(_) => None,
    };
    Plaintext::Stream {
        kind: "zstd",
        copy: copy(plain, &[data]),
        stopped,
    }
}

/// Whether the zstd CLI decodes on where aterm's decoder refused (`e`) the
/// magic number of a frame: one of a legacy zstd frame, or of a gzip, xz,
/// lzma or lz4 stream, each of which `zstd -d` reads after the frames before
/// it (zstd 1.5.7's `FIO_decompressFrames`). It refuses any other as aterm's
/// decoder does.
fn cli_reads_on(e: &std::io::Error) -> bool {
    let Some(&aterm_zstd::DecodeError::BadMagic(magic)) =
        e.get_ref().and_then(|e| e.downcast_ref())
    else {
        return false;
    };
    let head = magic.to_le_bytes();
    matches!(magic, 0x1eb5_2ffd | 0xfd2f_b522..=0xfd2f_b527 | 0x184d_2204)
        || [&GZIP_MAGIC[..], &[0xfd, 0x37], &[0x5d, 0x00]]
            .iter()
            .any(|m| head.starts_with(m))
}

#[cfg(test)]
#[path = "plaintext_tests.rs"]
pub(crate) mod tests;
