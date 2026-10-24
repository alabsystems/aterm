// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The decoder held to the brotli 1.2.0 CLI's own answers: what each recorded stream
//! decodes to, and its verdict on every recorded mutation of them (`tests/recorded`).

use std::io::{self, Read};
use std::path::PathBuf;

use crate::support::{Trickle, decode, sha256_hex};

const CORPUS: &str = include_str!("recorded/corpus-brotli-1.2.0.tsv");
const NEGATIVES: &str = include_str!("recorded/negatives-brotli-1.2.0.tsv");

fn rows(tsv: &str) -> impl Iterator<Item = Vec<&str>> {
    tsv.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| l.split('\t').collect())
}

fn stream(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/recorded/corpus")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `(stream, decoded length, SHA-256)` of every recorded stream.
fn corpus() -> Vec<(String, usize, String)> {
    let out: Vec<_> = rows(CORPUS)
        .map(|r| (r[0].to_string(), r[1].parse().unwrap(), r[2].to_string()))
        .collect();
    assert_eq!(out.len(), 35, "the recorded corpus");
    out
}

/// EVERY QUALITY, WINDOW AND KIND OF INPUT the CLI was asked for decodes to the CLI's own
/// answer: text and machine code at qualities 0 to 11 and windows of 10 to 24 bits,
/// incompressible data (which the CLI stores in uncompressed meta-blocks), long runs, one
/// byte and nothing.
#[test]
fn every_recorded_stream_decodes_to_the_clis_answer() {
    for (name, len, sha) in corpus() {
        let out = decode(&stream(&name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((out.len(), sha256_hex(&out)), (len, sha), "{name}");
    }
}

/// The decoder is a reader: input arriving a byte at a time and output read a few bytes at
/// a time give the same bytes as one whole read.
#[test]
fn every_recorded_stream_reads_the_same_in_steps() {
    for (i, (name, len, sha)) in corpus().into_iter().enumerate() {
        let br = stream(&name);
        let mut decoder = aterm_brotli::Decoder::new(Trickle {
            rest: &br,
            step: 1 + (i * 5) % 17,
        });
        let mut buf = vec![0u8; 1 + (i * 3) % 11];
        let mut out = Vec::new();
        loop {
            let n = decoder
                .read(&mut buf)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!((out.len(), sha256_hex(&out)), (len, sha), "{name}");
        assert_eq!(decoder.decoded(), len as u64, "{name}");
    }
}

/// The CLI's large-window extension (`--large_window=25`) is not RFC 7932: refused at the
/// header, before any output.
#[test]
fn the_large_window_extension_is_refused() {
    let br = stream("large-window-text-q5-w25.br");
    let mut decoder = aterm_brotli::Decoder::new(&br[..]);
    let mut out = Vec::new();
    let err = decoder.read_to_end(&mut out).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        err.get_ref()
            .and_then(|e| e.downcast_ref::<aterm_brotli::DecodeError>()),
        Some(&aterm_brotli::DecodeError::LargeWindow)
    );
    assert!(out.is_empty());
}

fn mutated(br: &[u8], how: &str) -> Vec<u8> {
    let (kind, arg) = how.split_once(':').unwrap();
    match kind {
        "cut" => br[..arg.parse::<usize>().unwrap()].to_vec(),
        "tail" => {
            let mut out = br.to_vec();
            out.push(u8::from_str_radix(arg, 16).unwrap());
            out
        }
        "flip" => {
            let bit: usize = arg.parse().unwrap();
            let mut out = br.to_vec();
            out[bit / 8] ^= 1 << (bit % 8);
            out
        }
        other => panic!("mutation {other}"),
    }
}

/// THE C DECODER'S VERDICTS: every recorded cut, trailing byte and bit flip is refused
/// exactly when the CLI refused it, and decodes to the CLI's bytes when it did not. Brotli
/// has no checksum, so a flip the CLI decodes is a different stream, not a corruption it
/// missed; a decoder looser than the CLI (one accepting over-long copies or trailing
/// bytes) fails here.
#[test]
fn every_recorded_mutation_gets_the_clis_verdict() {
    let mut streams = std::collections::BTreeMap::new();
    let (mut refused, mut decoded) = (0, 0);
    for r in rows(NEGATIVES) {
        let (name, how, want) = (r[0], r[1], r[2]);
        let br = streams.entry(name).or_insert_with(|| stream(name));
        let got = match decode(&mutated(br, how)) {
            Err(_) => String::from("err"),
            Ok(out) => format!("ok:{}:{}", out.len(), &sha256_hex(&out)[..16]),
        };
        assert_eq!(got, want, "{name} {how}");
        if want == "err" {
            refused += 1;
        } else {
            decoded += 1;
        }
    }
    assert_eq!((refused, decoded), (2575, 1152), "the recorded verdicts");
}
