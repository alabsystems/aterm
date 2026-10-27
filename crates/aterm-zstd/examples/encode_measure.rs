// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Encoder ratio and throughput on files, each frame checked by decoding it back.
//!
//! `targo --unverified run --release -p aterm-zstd --example encode_measure -- 3,19 FILE...`
//! prints one TSV row per file and level: input bytes, frame bytes, the ratio, encode MB/s
//! (10^6 input bytes a second, one thread), and decode MB/s. Each file is streamed in
//! 1 MiB writes with a checksum and no declared size, the frame atpkg-publish's
//! `tarwrite::Zstd` writes at `--window-log 27` for a tar over 2^27 bytes; `--declare-size`
//! declares the file's size in the header too (four more bytes there, and a single segment
//! for a file within the window), the frame it writes for a tar within 2^27 bytes at
//! `--window-log` the smallest power of two that holds it. `--window-log N` sets the
//! window; `--write DIR` keeps each frame as `DIR/<file name>.<level>.zst`.
//!
//! `encode_measure --decode FRAME...` instead decodes each frame from its file as atpkg's
//! extractor does (`Decoder::new(file).require_checksum()`), printing the bytes and decode
//! MB/s, so `/usr/bin/time -l` reads the decoder's peak memory.

use std::io::{BufReader, Read, Write};
use std::time::Instant;

use aterm_zstd::encode::{Encoder, Options};
use aterm_zstd::frame::Checksum;

/// Reads `path` to its end in 1 MiB pieces, handing each to `piece`.
fn each_piece(path: &str, piece: &mut dyn FnMut(&[u8])) {
    let mut file =
        BufReader::new(std::fs::File::open(path).unwrap_or_else(|e| panic!("{path}: {e}")));
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        if n == 0 {
            return;
        }
        piece(&buf[..n]);
    }
}

fn decode_only(paths: impl Iterator<Item = String>) {
    println!("frame\tbytes\tdecode_mbps");
    for path in paths {
        let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let t0 = Instant::now();
        let n = std::io::copy(
            &mut aterm_zstd::Decoder::new(file).require_checksum(),
            &mut std::io::sink(),
        )
        .unwrap_or_else(|e| panic!("{path}: {e}"));
        let decode = t0.elapsed().as_secs_f64();
        println!("{path}\t{n}\t{:.1}", n as f64 / 1e6 / decode);
    }
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut window_log = None;
    let mut keep: Option<std::path::PathBuf> = None;
    let mut declare = false;
    loop {
        match args.peek().map(String::as_str) {
            Some("--window-log") => {
                args.next();
                window_log = args.next().and_then(|w| w.parse::<u8>().ok());
            }
            Some("--write") => {
                args.next();
                keep = args.next().map(Into::into);
            }
            Some("--declare-size") => {
                args.next();
                declare = true;
            }
            Some("--decode") => {
                args.next();
                decode_only(args);
                return;
            }
            _ => break,
        }
    }
    let Some(levels) = args.next() else {
        eprintln!(
            "usage: encode_measure [--window-log N] [--declare-size] [--write DIR] LEVEL[,LEVEL...] FILE...\n       encode_measure --decode FRAME..."
        );
        std::process::exit(2);
    };
    let levels: Vec<u8> = levels.split(',').filter_map(|l| l.parse().ok()).collect();
    println!("file\tlevel\tbytes\tframe\tratio\tencode_mbps\tdecode_mbps");
    for path in args {
        let len = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .len();
        for &level in &levels {
            let mut options = Options::new().level(level).checksum(Checksum::Xxh64);
            if declare {
                options = options.content_size(len);
            }
            if let Some(w) = window_log {
                options = options.window_log(w);
            }
            let t0 = Instant::now();
            let mut enc = Encoder::new(Vec::new(), options);
            each_piece(&path, &mut |piece| {
                enc.write_all(piece).expect("in-memory write");
            });
            let zst = enc.finish().expect("in-memory finish");
            let encode = t0.elapsed().as_secs_f64();
            if let Some(dir) = &keep {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .map_or_else(|| "frame".into(), |n| n.to_string_lossy().into_owned());
                let to = dir.join(format!("{name}.{level}.zst"));
                std::fs::write(&to, &zst).unwrap_or_else(|e| panic!("{}: {e}", to.display()));
            }

            // Back through the decoder, against the file read again.
            let t0 = Instant::now();
            let mut back = aterm_zstd::Decoder::with_buffer(&zst[..]).require_checksum();
            let mut want = vec![0u8; 1 << 20];
            each_piece(&path, &mut |piece| {
                let got = &mut want[..piece.len()];
                back.read_exact(got).unwrap_or_else(|e| {
                    panic!("{path} level {level}: our frame did not decode: {e}")
                });
                assert!(got == piece, "{path} level {level}: decoded other bytes");
            });
            let mut rest = Vec::new();
            back.read_to_end(&mut rest)
                .unwrap_or_else(|e| panic!("{path} level {level}: the frame's end: {e}"));
            assert!(rest.is_empty(), "{path} level {level}: decoded more bytes");
            let decode = t0.elapsed().as_secs_f64();

            let mb = len as f64 / 1e6;
            println!(
                "{path}\t{level}\t{len}\t{}\t{:.4}\t{:.2}\t{:.1}",
                zst.len(),
                len as f64 / zst.len().max(1) as f64,
                mb / encode,
                mb / decode,
            );
            std::io::stdout().flush().ok();
        }
    }
}
