// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The literals section, the first half of a compressed block (RFC 8878 §3.1.1.3.1): raw,
//! RLE, Huffman-coded with a new code, or Huffman-coded with the previous block's code.

use super::huffman::Code;

/// `Literals_Block_Type` values (RFC 8878 §3.1.1.3.1.1).
const RAW: u8 = 0;
const RLE: u8 = 1;
const COMPRESSED: u8 = 2;
const TREELESS: u8 = 3;

/// Below this many literals a new Huffman code rarely pays for its description.
const MIN_FOR_NEW_CODE: usize = 32;

/// Below this many literals one stream is used, otherwise four (a coding choice; four
/// decode faster). One stream also needs both sizes to fit 10 bits (RFC 8878
/// §3.1.1.3.1.1, `Size_Format` 00).
const ONE_STREAM_BELOW: usize = 256;

/// Writes `lits` as a literals section to `out`. `prev` is the code the decoder holds
/// from the frame's last `Compressed_Literals_Block`, which a `Treeless_Literals_Block`
/// reuses (RFC 8878 §3.1.1.3.1.4). Returns the code a new `Compressed_Literals_Block`
/// described, which then becomes the one the decoder holds.
pub(crate) fn write(lits: &[u8], prev: Option<&Code>, out: &mut Vec<u8>) -> Option<Code> {
    let n = lits.len();
    let mut counts = [0u32; 256];
    for &b in lits {
        if let Some(c) = counts.get_mut(usize::from(b)) {
            *c += 1;
        }
    }
    let distinct = counts.iter().filter(|&&c| c > 0).count();
    if distinct == 1 && n > 1 {
        write_raw_header(RLE, n, out);
        out.extend_from_slice(lits.get(..1).unwrap_or_default());
        return None;
    }
    let raw_size = raw_header_len(n) + n;
    if distinct < 2 || n < 8 {
        write_raw(lits, out);
        return None;
    }
    let streams = if n < ONE_STREAM_BELOW { 1 } else { 4 };
    let overhead = |bits: u64| bits.div_ceil(8) as usize + if streams == 4 { 6 + 3 } else { 0 };

    let reuse = prev.filter(|code| code.covers(&counts)).map(|code| {
        (
            code,
            compressed_header_len(n) + overhead(code.cost_bits(&counts)),
        )
    });
    let fresh = if n >= MIN_FOR_NEW_CODE {
        Code::new(&counts).and_then(|code| {
            let mut desc = Vec::new();
            code.write_description(&mut desc).then(|| {
                let size =
                    compressed_header_len(n) + desc.len() + overhead(code.cost_bits(&counts));
                (code, desc, size)
            })
        })
    } else {
        None
    };

    let start = out.len();
    let reuse_size = reuse.as_ref().map_or(usize::MAX, |r| r.1);
    match fresh {
        Some((code, desc, size)) if size < reuse_size && size < raw_size => {
            if write_huffman(COMPRESSED, lits, &code, &desc, streams, out)
                && out.len() - start < raw_size
            {
                return Some(code);
            }
        }
        _ => {
            if let Some((code, size)) = reuse
                && size < raw_size
                && write_huffman(TREELESS, lits, code, &[], streams, out)
                && out.len() - start < raw_size
            {
                return None;
            }
        }
    }
    out.truncate(start);
    write_raw(lits, out);
    None
}

/// A `Raw_Literals_Block`: the header, then the bytes (RFC 8878 §3.1.1.3.1.2).
fn write_raw(lits: &[u8], out: &mut Vec<u8>) {
    write_raw_header(RAW, lits.len(), out);
    out.extend_from_slice(lits);
}

/// Header bytes of a raw or RLE section of `n` literals: `Size_Format` x0 holds 5 bits,
/// 01 holds 12, 11 holds 20 (RFC 8878 §3.1.1.3.1.1).
const fn raw_header_len(n: usize) -> usize {
    if n < 32 {
        1
    } else if n < 4096 {
        2
    } else {
        3
    }
}

fn write_raw_header(kind: u8, n: usize, out: &mut Vec<u8>) {
    let n = n as u32;
    match raw_header_len(n as usize) {
        1 => out.push(kind | (n << 3) as u8),
        2 => out.extend_from_slice(&(u16::from(kind) | 1 << 2 | (n << 4) as u16).to_le_bytes()),
        _ => {
            let word = u32::from(kind) | 3 << 2 | n << 4;
            out.extend_from_slice(word.to_le_bytes().get(..3).unwrap_or_default());
        }
    }
}

/// Header bytes of a Huffman-coded section whose sizes both fit `n`'s field: 10 bits in
/// 3 bytes, 14 in 4, 18 in 5 (RFC 8878 §3.1.1.3.1.1).
const fn compressed_header_len(n: usize) -> usize {
    if n < 1024 {
        3
    } else if n < 16384 {
        4
    } else {
        5
    }
}

/// A Huffman-coded section of `kind` ([`COMPRESSED`] carrying `desc`, or [`TREELESS`]):
/// the header, the description, then one stream or a 6-byte `Jump_Table` and four
/// (RFC 8878 §3.1.1.3.1.4, §3.1.1.3.1.6). `false`, writing nothing, when four streams would
/// hold fewer than 256 literals or the sizes do not fit the header or the jump table (the
/// caller then writes the literals raw).
fn write_huffman(
    kind: u8,
    lits: &[u8],
    code: &Code,
    desc: &[u8],
    streams: usize,
    out: &mut Vec<u8>,
) -> bool {
    let mut body = Vec::with_capacity(lits.len());
    body.extend_from_slice(desc);
    if streams == 1 {
        code.write_stream(lits, &mut body);
    } else {
        // Each of the first three streams holds a quarter, rounded up; the fourth the rest
        // (at least 256 literals, so all four hold some).
        let quarter = lits.len().div_ceil(4);
        if lits.len() < ONE_STREAM_BELOW {
            return false;
        }
        let jump = body.len();
        body.extend_from_slice(&[0; 6]);
        let mut sizes = [0usize; 3];
        for (i, part) in lits.chunks(quarter).enumerate() {
            let before = body.len();
            code.write_stream(part, &mut body);
            if let Some(s) = sizes.get_mut(i) {
                *s = body.len() - before;
            }
        }
        for (i, &s) in sizes.iter().enumerate() {
            let Ok(s) = u16::try_from(s) else {
                return false;
            };
            if let Some(slot) = body.get_mut(jump + 2 * i..jump + 2 * i + 2) {
                slot.copy_from_slice(&s.to_le_bytes());
            }
        }
    }
    let (n, c) = (lits.len() as u64, body.len() as u64);
    let size_format = match (streams, n.max(c)) {
        (1, 0..=1023) => 0u64,
        (_, 0..=1023) => 1,
        (_, 1024..=16383) => 2,
        (_, 16384..=262_143) => 3,
        _ => return false,
    };
    let kind = u64::from(kind) | size_format << 2;
    match size_format {
        0 | 1 => {
            let word = kind | n << 4 | c << 14;
            out.extend_from_slice(word.to_le_bytes().get(..3).unwrap_or_default());
        }
        2 => {
            let word = kind | n << 4 | c << 18;
            out.extend_from_slice(word.to_le_bytes().get(..4).unwrap_or_default());
        }
        _ => {
            let word = kind | n << 4 | c << 22;
            out.extend_from_slice(word.to_le_bytes().get(..5).unwrap_or_default());
        }
    }
    out.extend_from_slice(&body);
    true
}

#[cfg(test)]
mod tests {
    use super::write;
    use crate::decode::huffman::HuffmanTable;
    use crate::decode::literals;
    use crate::encode::huffman::Code;

    /// The section decoded by the crate's literals decoder, the table carried across.
    fn decode(section: &[u8], table: &mut HuffmanTable) -> (Vec<u8>, usize) {
        let mut padded = section.to_vec();
        padded.resize(section.len() + 64, 0);
        let mut buf = Vec::new();
        let (lits, count, used) =
            literals::decode(&padded, section.len(), table, &mut buf, 128 << 10).unwrap();
        (lits[..count].to_vec(), used)
    }

    fn text(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                b"etaoin shrdlu,.\n"[(x % 16) as usize]
                    ^ if x.is_multiple_of(97) { 0x20 } else { 0 }
            })
            .collect()
    }

    /// Every section kind and header width: each decodes to its literals, and a code a
    /// compressed section described is what a later treeless one reuses.
    #[test]
    fn every_kind_and_width_decodes() {
        let mut table = HuffmanTable::new();
        let mut prev: Option<Code> = None;
        let mut kinds = [0usize; 4];
        let mut inputs: Vec<Vec<u8>> = vec![Vec::new(), b"x".to_vec(), vec![7; 31], vec![7; 32]];
        inputs.extend([vec![9; 4095], vec![9; 4096], vec![9; 100_000]]);
        for len in [
            8,
            31,
            32,
            200,
            255,
            256,
            1000,
            1023,
            1024,
            5000,
            16_383,
            16_384,
            128 << 10,
        ] {
            inputs.push(text(len, len as u64));
            inputs.push(text(len, len as u64 + 1));
        }
        let noise: Vec<u8> = (0..3000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        inputs.push(noise);
        for lits in &inputs {
            let mut section = Vec::new();
            let fresh = write(lits, prev.as_ref(), &mut section);
            kinds[usize::from(section[0] & 3)] += 1;
            let (back, used) = decode(&section, &mut table);
            assert_eq!(
                used,
                section.len(),
                "{} literals: section length",
                lits.len()
            );
            assert!(back == *lits, "{} literals", lits.len());
            if fresh.is_some() {
                prev = fresh;
            }
        }
        assert!(
            kinds.iter().all(|&k| k > 0),
            "raw, RLE, compressed and treeless: {kinds:?}"
        );
    }

    /// The negative control: a treeless section read with no table is refused.
    #[test]
    fn a_treeless_section_without_a_table_is_refused() {
        let lits = text(5000, 3);
        let mut first = Vec::new();
        let code = write(&lits, None, &mut first).expect("a new code");
        let mut second = Vec::new();
        assert!(write(&lits, Some(&code), &mut second).is_none());
        assert_eq!(second[0] & 3, 3, "treeless");
        let mut padded = second.clone();
        padded.resize(second.len() + 64, 0);
        let mut buf = Vec::new();
        let mut fresh = HuffmanTable::new();
        assert!(literals::decode(&padded, second.len(), &mut fresh, &mut buf, 128 << 10).is_err());
    }
}
