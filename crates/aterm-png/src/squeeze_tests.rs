// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn inflate(stream: &[u8], len: usize) -> Vec<u8> {
    aterm_codec::inflate::inflate(stream, len + 64)
        .unwrap_or_else(|e| panic!("inflate failed on {len} bytes: {e:?}"))
}

/// Seeded inputs over every path: empty, one byte, text, every byte value, noise (stored
/// blocks), long runs, repeats at the window's far edge, and enough symbols for several
/// blocks.
fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    let mut state = 0x2545_F491u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let words = [
        "the ",
        "terminal ",
        "aterm ",
        "renders ",
        "a ",
        "glyph ",
        "atlas ",
        "and ",
        "<div ",
        "class=\"row\">",
        "</div>\n",
        "font-size: 14px; ",
        "function ",
        "return ",
        "{ ",
        "} ",
        "0.",
        "1",
        "2",
        "3",
    ];
    let text: String = (0..60_000)
        .map(|_| words[(next() % words.len() as u32) as usize])
        .collect();
    let noise: Vec<u8> = (0..200_000).map(|_| (next() >> 24) as u8).collect();
    let block: Vec<u8> = (0..300).map(|_| (next() >> 24) as u8).collect();
    let mut far = block.clone();
    far.extend((0..WINDOW - block.len()).map(|_| (next() >> 24) as u8));
    far.extend_from_slice(&block);
    far.extend((0..WINDOW - block.len() + 1).map(|_| (next() >> 24) as u8));
    far.extend_from_slice(&block);
    let mut skewed = Vec::new();
    for (i, b) in (0..=255u8).enumerate() {
        skewed.extend(std::iter::repeat_n(b, 1 + (1usize << (i % 18)) / 64));
        skewed.extend((0..(next() % 5)).map(|_| (next() >> 24) as u8));
    }
    vec![
        ("empty", Vec::new()),
        ("one", b"a".to_vec()),
        ("two", b"ab".to_vec()),
        ("short", b"abcabcabcabd".to_vec()),
        ("text", text.into_bytes()),
        ("bytes", (0..=255u8).cycle().take(100_000).collect()),
        ("noise", noise),
        ("run", vec![0xA5; 5 * WINDOW + 13]),
        ("far", far),
        ("skewed", skewed),
    ]
}

/// Every input inflates back through aterm's own inflater, which refuses an incomplete or
/// over-subscribed code, a bad stored length and a distance past the start. NEGATIVE
/// CONTROL: one flipped bit in a dynamic block's stream is caught or changes the output.
#[test]
fn every_input_round_trips() {
    for (label, data) in corpus() {
        let stream = deflate(&data);
        assert_eq!(inflate(&stream, data.len()), data, "{label}");
        assert_eq!(stream, deflate(&data), "{label}: the same bytes every time");
    }
    let noise: Vec<u8> = corpus()[6].1[..2 * STORED_MAX + 7].to_vec();
    let literals: Vec<Token> = noise.iter().map(|&b| Token::Literal(b)).collect();
    let mut w = BitWriter::with_capacity(0);
    write_block(&mut w, &literals, &noise, true);
    let stream = w.finish();
    assert_eq!(
        stream.len(),
        noise.len() + 3 * 5,
        "one block, stored as three pieces"
    );
    assert_eq!(inflate(&stream, noise.len()), noise);
    let text = &corpus()[4].1;
    let mut stream = deflate(text);
    assert_eq!(
        (stream[0] >> 1) & 0b11,
        2,
        "the text's first block is dynamic"
    );
    stream[40] ^= 0x10;
    assert_ne!(
        aterm_codec::inflate::inflate(&stream, text.len() + 64)
            .ok()
            .as_ref(),
        Some(text)
    );
}

/// Text compresses well past the fixed-Huffman encoder, and noise costs little more than
/// its own bytes (it goes stored). NEGATIVE CONTROL: the fixed-Huffman encoder's noise is
/// larger than the noise.
#[test]
fn each_block_is_the_smallest_kind() {
    let corpus = corpus();
    let text = &corpus[4].1;
    let fixed = crate::deflate::zlib_compress(text).len() - 6;
    let squeezed = deflate(text).len();
    assert!(
        squeezed * 100 < fixed * 85,
        "text: {squeezed} against fixed {fixed}"
    );
    let noise = &corpus[6].1;
    let stored = deflate(noise).len();
    assert!(
        stored <= noise.len() + 5 * noise.len().div_ceil(BLOCK_SYMBOLS) + 1,
        "noise: {stored} for {}",
        noise.len()
    );
    assert!(crate::deflate::zlib_compress(noise).len() > noise.len() + 1000);
}

/// The codes are length-limited and complete, whatever the counts: a Fibonacci-skewed
/// alphabet that a plain Huffman tree would make 25 bits deep fits in 15 (and 7), and
/// one or no symbol used is still a two-word code. NEGATIVE CONTROL: the unlimited tree
/// over the same counts is deeper than the limit.
#[test]
fn codes_are_limited_and_complete() {
    let mut fib = vec![1u32, 1];
    while fib.len() < 26 {
        let n = fib[fib.len() - 1] + fib[fib.len() - 2];
        fib.push(n);
    }
    let used: Vec<usize> = (0..fib.len()).collect();
    let weights: Vec<u64> = fib.iter().map(|&f| u64::from(f)).collect();
    assert!(depths(&weights, &used).iter().any(|&(_, d)| d > 15));
    for limit in [MAX_BITS, MAX_CL_BITS] {
        let lens = lengths(&fib, limit);
        assert!(lens.iter().all(|&l| (1..=limit).contains(&l)), "{lens:?}");
        let kraft: f64 = lens.iter().map(|&l| 0.5f64.powi(i32::from(l))).sum();
        assert!((kraft - 1.0).abs() < 1e-12, "complete: {kraft}");
    }
    assert_eq!(lengths(&[0, 0, 0], 15), [1, 1, 0]);
    assert_eq!(lengths(&[0, 0, 9], 15), [1, 0, 1]);
    assert_eq!(lengths(&[9, 0, 0], 15), [1, 1, 0]);
    assert_eq!(codes(&[2, 1, 3, 3]), [0b10, 0b0, 0b110, 0b111]);
}

/// The code-length runs say back the lengths they encode. NEGATIVE CONTROL: a run of two
/// is never a repeat code.
#[test]
fn code_length_runs_decode_to_the_lengths() {
    let lens: Vec<u8> = [vec![0; 140], vec![8; 9], vec![0; 4], vec![5; 2], vec![7; 4]].concat();
    let mut runs = Vec::new();
    rle(&lens, &mut runs);
    let mut back = Vec::new();
    for (s, extra, _) in &runs {
        match s {
            16 => {
                let last = *back.last().unwrap();
                back.extend(std::iter::repeat_n(last, 3 + *extra as usize));
            }
            17 => back.extend(std::iter::repeat_n(0, 3 + *extra as usize)),
            18 => back.extend(std::iter::repeat_n(0, 11 + *extra as usize)),
            l => back.push(*l),
        }
    }
    assert_eq!(back, lens);
    assert!(runs.windows(2).any(|w| w == [(5, 0, 0), (5, 0, 0)]));
}
