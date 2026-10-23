// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Recorded-answer differential tests holding [`aterm_zstd::Decoder`] to `zstd` 1.5.7.

use crate::support;

use std::io::{self, Read};

use crate::support::{
    Census, FLIP_ROWS, FLIP_SEED, SWEEP, SweepRow, XorShift, flip_bits, input, sweep,
    without_checksum,
};
use aterm_digest::Sha256;
use aterm_zstd::Decoder;

fn ours(zst: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    Decoder::with_buffer(zst).read_to_end(&mut out)?;
    Ok(out)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn answer(bytes: &[u8]) -> (usize, String) {
    (bytes.len(), sha256_hex(bytes))
}

fn recorded(r: &SweepRow) -> (usize, String) {
    (r.decoded_len, r.sha256.clone())
}

#[test]
fn every_sweep_input_regenerates_to_its_recorded_answer() {
    for r in sweep() {
        assert_eq!(answer(&input(&r.input)), recorded(&r), "row {}", r.row);
    }
}

#[test]
fn the_decoder_decodes_every_sweep_frame_to_its_recorded_answer() {
    for r in sweep() {
        let decoded =
            ours(r.frame()).unwrap_or_else(|e| panic!("row {} ({}): {e}", r.row, r.command));
        assert_eq!(
            answer(&decoded),
            recorded(&r),
            "row {} ({})",
            r.row,
            r.command
        );
    }
}

#[test]
fn the_decoder_reads_every_small_sweep_frame_in_steps() {
    for r in sweep().iter().filter(|r| r.decoded_len < 20_000) {
        let mut decoder = Decoder::new(r.frame());
        let mut out = Vec::new();
        let mut buf = [0u8; 7];
        loop {
            match decoder.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) => panic!("row {}: {e}", r.row),
            }
        }
        assert_eq!(answer(&out), recorded(r), "row {}", r.row);
    }
}

#[test]
fn the_whole_sweep_is_one_stream() {
    let want: Vec<u8> = sweep().iter().flat_map(|r| input(&r.input)).collect();
    assert!(ours(SWEEP).expect("one stream") == want);
}

fn skippable(n: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = (0x184D_2A50u32 + u32::from(n)).to_le_bytes().to_vec();
    f.extend((payload.len() as u32).to_le_bytes());
    f.extend(payload);
    f
}

#[test]
fn multi_frame_streams_with_skippable_and_empty_frames() {
    let rows = sweep();
    let pick = |spec: &str, command: &str| {
        rows.iter()
            .find(|r| r.input == spec && r.command == command)
            .unwrap_or_else(|| panic!("no row {spec} by {command}"))
    };
    let a = pick("words:1536:5", "zstd -q --fast=5 -c <file>");
    let b = pick("words:1536:17", "zstd -q -17 -c < <file>");
    let empty = pick("words:0:1000", "zstd -q --fast=3 -c <file>").frame();
    let mut stream = skippable(0, b"");
    stream.extend(a.frame());
    stream.extend(skippable(15, &support::noise(1000, 1)));
    stream.extend(empty);
    stream.extend(without_checksum(empty));
    let mut raw = Vec::new();
    aterm_zstd::frame::write_raw(
        &mut raw,
        &input(&b.input),
        aterm_zstd::frame::Checksum::Omitted,
    )
    .expect("raw");
    stream.extend(raw);
    stream.extend(b.frame());
    stream.extend(skippable(7, b"tail"));
    let mut want = input(&a.input);
    want.extend(input(&b.input));
    want.extend(input(&b.input));
    assert!(ours(&stream).expect("multi-frame") == want);

    let only_skippable = [skippable(1, b"x"), skippable(2, b"")].concat();
    assert_eq!(ours(&only_skippable).expect("only skippable frames"), b"");
    assert_eq!(ours(b"").unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    let short = &skippable(3, b"0123")[..10];
    assert_eq!(
        ours(short).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn the_sweep_reaches_every_block_literals_and_table_mode() {
    let mut c = Census::default();
    c.add(SWEEP);
    assert_eq!(c.frames, sweep().len(), "{c:?}");
    assert!(c.blocks.iter().all(|&n| n > 0), "block types {c:?}");
    assert!(c.literals.iter().all(|&n| n > 0), "literals types {c:?}");
    assert!(c.modes.iter().all(|&n| n > 0), "table modes {c:?}");
    assert!(
        c.rle_extra_bits.iter().all(|&n| n > 0),
        "RLE codes with extra bits {c:?}"
    );
    assert!(c.single_segment > 0 && c.single_segment < c.frames, "{c:?}");
    assert!(c.with_checksum > 0 && c.with_checksum < c.frames, "{c:?}");
    assert!(
        c.with_content_size > 0 && c.with_content_size < c.frames,
        "{c:?}"
    );
}

#[test]
fn every_cut_of_a_sweep_frame_is_an_early_end() {
    support::on_every_core(&sweep(), |r| {
        let zst = r.frame();
        let len = zst.len();
        let mut cuts: Vec<usize> = (0..len.min(64))
            .chain((0..len).step_by(len.div_ceil(32).max(1)))
            .chain(len.saturating_sub(16)..len)
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        for cut in cuts {
            let e = ours(&zst[..cut]).expect_err("cut short");
            assert_eq!(
                e.kind(),
                io::ErrorKind::UnexpectedEof,
                "row {} cut at {cut} of {len}: {e}",
                r.row
            );
        }
    });
}

struct Flips {
    row: usize,
    checksum: bool,
    zst: Vec<u8>,
    input: Vec<u8>,
    bits: Vec<usize>,
    /// `E`, `=` or `W` per flip.
    verdicts: Vec<u8>,
    /// The first 3 bytes of the SHA-256 of libzstd's bytes, per `W`, in hex.
    wrong: Vec<String>,
}

fn flips() -> Vec<Flips> {
    let rows = sweep();
    let recorded: Vec<Vec<&str>> = include_str!("recorded/flips-libzstd-1.5.7.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| l.split('\t').collect())
        .collect();
    let mut x = XorShift::new(FLIP_SEED);
    let mut out = Vec::new();
    let mut lines = recorded.iter();
    for row in FLIP_ROWS {
        let r = &rows[row];
        for checksum in [true, false] {
            let zst = if checksum {
                r.frame().to_vec()
            } else {
                without_checksum(r.frame())
            };
            let bits = flip_bits(zst.len(), &mut x);
            let line = lines.next().expect("a verdict line per flip frame");
            let word = if checksum { "on" } else { "off" };
            assert_eq!(
                (line[0], line[1]),
                (row.to_string().as_str(), word),
                "verdict line order"
            );
            let verdicts = line[2].as_bytes().to_vec();
            assert_eq!(
                verdicts.len(),
                bits.len(),
                "row {row} {word}: one verdict per flip"
            );
            let wrong: Vec<String> = line
                .get(3)
                .filter(|w| !w.is_empty())
                .map(|w| w.split(',').map(str::to_string).collect())
                .unwrap_or_default();
            assert_eq!(
                wrong.len(),
                verdicts.iter().filter(|&&v| v == b'W').count(),
                "row {row} {word}: one digest per W"
            );
            out.push(Flips {
                row,
                checksum,
                zst,
                input: input(&r.input),
                bits,
                verdicts,
                wrong,
            });
        }
    }
    assert!(lines.next().is_none(), "a verdict line for no flip frame");
    out
}

fn flipped(zst: &[u8], bit: usize) -> Vec<u8> {
    let mut bad = zst.to_vec();
    bad[bit / 8] ^= 1 << (bit % 8);
    bad
}

fn verdict(decoded: &io::Result<Vec<u8>>, input: &[u8]) -> (u8, Option<String>) {
    match decoded {
        Err(_) => (b'E', None),
        Ok(v) if v == input => (b'=', None),
        Ok(v) => (b'W', Some(sha256_hex(v)[..6].to_string())),
    }
}

#[test]
fn bit_flips_get_libzstd_s_recorded_verdicts() {
    let mut mismatches = Vec::new();
    let (mut agreed, mut accepted, mut refused_damage) = (0, 0, 0);
    for (a, b, c, m) in support::on_every_core(&flips(), |f| {
        let mut mismatches = Vec::new();
        let (mut agreed, mut accepted, mut refused_damage) = (0, 0, 0);
        let mut wrong = f.wrong.iter();
        for (&bit, &want) in f.bits.iter().zip(&f.verdicts) {
            let decoded = ours(&flipped(&f.zst, bit));
            let want_digest = (want == b'W').then(|| wrong.next().expect("a digest per W"));
            let (got, digest) = verdict(&decoded, &f.input);
            let at = format!("row {} checksum {} bit {bit}", f.row, f.checksum);
            if f.checksum {
                assert!(got != b'W', "{at}: the checksum passed wrong bytes");
            }
            match (want, got) {
                (b'E', b'E') => agreed += 1,
                (b'=', b'=') => {
                    agreed += 1;
                    accepted += 1;
                }
                (b'W', b'W') if digest.as_ref() == want_digest => {
                    agreed += 1;
                    accepted += 1;
                }
                (b'W', b'E')
                    if decoded
                        .as_ref()
                        .is_err_and(|e| e.kind() == io::ErrorKind::InvalidData) =>
                {
                    refused_damage += 1;
                }
                _ => mismatches.push(format!(
                    "{at}: libzstd {}, aterm {}",
                    want as char,
                    match &decoded {
                        Ok(v) if v == &f.input => "the input".to_string(),
                        Ok(v) => format!("{} other bytes", v.len()),
                        Err(e) => format!("{:?}: {e}", e.kind()),
                    }
                )),
            }
        }
        (agreed, accepted, refused_damage, mismatches)
    }) {
        agreed += a;
        accepted += b;
        refused_damage += c;
        mismatches.extend(m);
    }
    assert!(
        accepted > 0,
        "no flip was accepted by both: the agreement check never ran"
    );
    assert!(
        mismatches.is_empty(),
        "{} disagreements:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    eprintln!(
        "{agreed} flips agree with libzstd ({accepted} accepted by both); {refused_damage} \
         refused here that libzstd decoded wrongly"
    );
}
