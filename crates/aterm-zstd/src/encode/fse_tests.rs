// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Table, cost_bits, normalize, optimal_log, write_description};
use crate::decode::bits::BitReaderReversed;
use crate::decode::fse::{FseDecoder, FseTable};
use crate::decode::sequences::{
    LL_DEFAULT_ACC_LOG, LL_DEFAULT_DISTRIBUTION, ML_DEFAULT_ACC_LOG, ML_DEFAULT_DISTRIBUTION,
    OF_DEFAULT_ACC_LOG, OF_DEFAULT_DISTRIBUTION,
};
use crate::encode::bits::BitWriter;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Symbols drawn from a skewed distribution over `0..alphabet`.
fn draw(rng: &mut Rng, alphabet: u32, n: usize) -> Vec<u8> {
    (0..n)
        .map(|_| {
            let r = rng.next();
            let a = (r % u64::from(alphabet)) as u32;
            let b = ((r >> 20) % u64::from(alphabet)) as u32;
            a.min(b).min((r >> 40) as u32 % alphabet) as u8
        })
        .collect()
}

/// One state encodes `symbols` back to front; the crate's decoder reads them front
/// to back from the table it builds out of the same description.
fn round_trip(norm: &[i16], log: u32, symbols: &[u8]) -> Vec<u8> {
    let table = Table::new(norm, log);
    let mut stream = Vec::new();
    let mut w = BitWriter::new(&mut stream);
    let (last, rest) = symbols.split_last().unwrap();
    let mut state = table.init(*last);
    for &s in rest.iter().rev() {
        table.encode(&mut w, &mut state, s);
    }
    table.flush(&mut w, state);
    w.close();

    let mut desc = Vec::new();
    write_description(norm, log, &mut desc);
    let mut dt = FseTable::new(255);
    let read = dt.build_decoder(&desc, 9).unwrap();
    assert_eq!(read, desc.len(), "the description is read whole");
    let mut br = BitReaderReversed::new(&stream);
    br.skip_padding().unwrap();
    let mut dec = FseDecoder::new(&dt);
    dec.init_state(&mut br).unwrap();
    let mut out = Vec::new();
    for i in 0..symbols.len() {
        out.push(dec.symbol());
        if i + 1 < symbols.len() {
            dec.update_state(&mut br);
        }
    }
    assert_eq!(br.bits_remaining(), 0, "the stream is read exactly");
    out
}

#[test]
fn histograms_round_trip_through_the_decoder() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for case in 0..300u32 {
        let alphabet = 2 + case % 53;
        let n = 2 + (rng.next() % 3000) as usize;
        let symbols = draw(&mut rng, alphabet, n);
        let mut counts = vec![0u32; alphabet as usize];
        for &s in &symbols {
            counts[usize::from(s)] += 1;
        }
        while counts.last() == Some(&0) {
            counts.pop();
        }
        if counts.iter().filter(|&&c| c > 0).count() < 2 {
            continue;
        }
        let log = optimal_log(9, n, counts.len() as u32 - 1);
        let mut norm = Vec::new();
        normalize(&counts, log, &mut norm);
        let sum: i32 = norm.iter().map(|&p| i32::from(p.max(0))).sum();
        assert_eq!(sum, 1 << log, "case {case}");
        assert!(
            counts.iter().zip(&norm).all(|(&c, &p)| (c > 0) == (p > 0)),
            "case {case}: every present symbol has a state, no absent one does"
        );
        assert!(cost_bits(&counts, &norm, log).is_some());
        assert_eq!(round_trip(&norm, log, &symbols), symbols, "case {case}");
    }
}

/// The predefined distributions (with their "less than 1" entries) encode what the
/// decoder's predefined tables decode.
#[test]
fn predefined_distributions_round_trip() {
    let mut rng = Rng(77);
    for (log, dist) in [
        (LL_DEFAULT_ACC_LOG, &LL_DEFAULT_DISTRIBUTION[..]),
        (ML_DEFAULT_ACC_LOG, &ML_DEFAULT_DISTRIBUTION[..]),
        (OF_DEFAULT_ACC_LOG, &OF_DEFAULT_DISTRIBUTION[..]),
    ] {
        let norm: Vec<i16> = dist.iter().map(|&p| p as i16).collect();
        let symbols = draw(&mut rng, dist.len() as u32, 5000);
        assert_eq!(round_trip(&norm, u32::from(log), &symbols), symbols);
    }
}

/// The negative control: a stream decoded with another table's description is not
/// the stream.
#[test]
fn a_stream_decoded_under_another_table_differs() {
    let norm_a: Vec<i16> = vec![20, 6, 4, 2];
    let norm_b: Vec<i16> = vec![2, 4, 6, 20];
    let symbols: Vec<u8> = (0..200).map(|i| [0, 0, 1, 2, 0, 3][i % 6]).collect();
    let table = Table::new(&norm_a, 5);
    let mut stream = Vec::new();
    let mut w = BitWriter::new(&mut stream);
    let (last, rest) = symbols.split_last().unwrap();
    let mut state = table.init(*last);
    for &s in rest.iter().rev() {
        table.encode(&mut w, &mut state, s);
    }
    table.flush(&mut w, state);
    w.close();
    let mut desc = Vec::new();
    write_description(&norm_b, 5, &mut desc);
    let mut dt = FseTable::new(255);
    dt.build_decoder(&desc, 9).unwrap();
    let mut br = BitReaderReversed::new(&stream);
    br.skip_padding().unwrap();
    let mut dec = FseDecoder::new(&dt);
    dec.init_state(&mut br).unwrap();
    let mut out = Vec::new();
    for _ in 0..symbols.len() {
        out.push(dec.symbol());
        dec.update_state(&mut br);
    }
    assert_ne!(out, symbols);
}

#[test]
fn byte_aligned_short_last_probability_round_trips() {
    let norm: [i16; 7] = [0, 16, 0, 12, 2, 1, -1];
    let mut desc = Vec::new();
    write_description(&norm, 5, &mut desc);
    assert_eq!(desc, 0x4F41_8810u32.to_le_bytes());
    let mut dt = FseTable::new(35);
    assert_eq!(dt.build_decoder(&desc, 9).unwrap(), 4);
}
