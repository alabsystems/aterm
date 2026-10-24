// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Decode `.br` files with aterm-brotli and print, for each, its decoded length, SHA-256
//! and decode rate: the lab step that holds the decoder to real streams too large to
//! commit (`docs/measured/grok-npm-2026-10-24.md`).
//!
//! `targo --unverified run --release -p aterm-brotli --example decode -- <file.br>...`

use std::io::Read;
use std::time::Instant;

fn main() {
    let mut failed = false;
    for path in std::env::args().skip(1) {
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{path}: {e}");
                failed = true;
                continue;
            }
        };
        let started = Instant::now();
        let mut decoder = aterm_brotli::Decoder::new(file);
        let mut sha = aterm_digest::Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut len = 0u64;
        let verdict = loop {
            match decoder.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    sha.update(&buf[..n]);
                    len += n as u64;
                }
                Err(e) => break Err(e),
            }
        };
        let secs = started.elapsed().as_secs_f64();
        match verdict {
            Ok(()) => {
                let hex: String = sha.finalize().iter().map(|b| format!("{b:02x}")).collect();
                println!(
                    "{path}\t{len}\t{hex}\t{secs:.3}s\t{:.0} MB/s",
                    len as f64 / secs / 1e6
                );
            }
            Err(e) => {
                println!("{path}\terror after {len} bytes: {e}");
                failed = true;
            }
        }
    }
    std::process::exit(i32::from(failed));
}
