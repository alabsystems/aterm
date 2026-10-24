// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Encoder ratio and throughput on files, each frame checked by decoding it back.
//!
//! `targo --unverified run --release -p aterm-zstd --example encode_measure -- 3,19 FILE...`
//! prints one TSV row per file and level: input bytes, frame bytes, the ratio, encode MB/s
//! (10^6 input bytes a second, one thread, in memory), and decode MB/s. `--window-log N`
//! sets the window.

use std::io::{Read, Write};
use std::time::Instant;

use aterm_zstd::encode::{Encoder, Options};

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut window_log = None;
    if args.peek().map(String::as_str) == Some("--window-log") {
        args.next();
        window_log = args.next().and_then(|w| w.parse::<u8>().ok());
    }
    let Some(levels) = args.next() else {
        eprintln!("usage: encode_measure [--window-log N] LEVEL[,LEVEL...] FILE...");
        std::process::exit(2);
    };
    let levels: Vec<u8> = levels.split(',').filter_map(|l| l.parse().ok()).collect();
    println!("file\tlevel\tbytes\tframe\tratio\tencode_mbps\tdecode_mbps");
    for path in args {
        let input = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        for &level in &levels {
            let mut options = Options::new().level(level).content_size(input.len() as u64);
            if let Some(w) = window_log {
                options = options.window_log(w);
            }
            let t0 = Instant::now();
            let mut enc = Encoder::new(Vec::with_capacity(input.len() / 3), options);
            for piece in input.chunks(1 << 20) {
                enc.write_all(piece).expect("in-memory write");
            }
            let zst = enc.finish().expect("in-memory finish");
            let encode = t0.elapsed().as_secs_f64();

            let t0 = Instant::now();
            let mut back = Vec::with_capacity(input.len());
            aterm_zstd::Decoder::with_buffer(&zst[..])
                .read_to_end(&mut back)
                .unwrap_or_else(|e| panic!("{path} level {level}: our frame did not decode: {e}"));
            let decode = t0.elapsed().as_secs_f64();
            assert!(back == input, "{path} level {level}: decoded other bytes");

            let mb = input.len() as f64 / 1e6;
            println!(
                "{path}\t{level}\t{}\t{}\t{:.4}\t{:.2}\t{:.1}",
                input.len(),
                zst.len(),
                input.len() as f64 / zst.len().max(1) as f64,
                mb / encode,
                mb / decode,
            );
            std::io::stdout().flush().ok();
        }
    }
}
