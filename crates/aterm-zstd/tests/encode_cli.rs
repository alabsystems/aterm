// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`aterm_zstd::encode`] held to the reference decoder: the zstd CLI (its
//! `aterm_uds::host::TOOLS` row) tests and decompresses the encoder's frames of the
//! generated inputs, and refuses them damaged. Without the CLI the test ends COULD NOT RUN.

use std::path::{Path, PathBuf};

use crate::encode_frames::{Feed, corpus, write_frame};
use crate::support::{Census, on_every_core};
use aterm_zstd::encode::Options;
use aterm_zstd::frame::Checksum;

fn cli() -> PathBuf {
    aterm_uds::host::need_tool("zstd")
}

/// Whether `zstd -q -t <file>` passes, and what `zstd -q -d -c <file>` writes (`None` when
/// it fails).
fn judged(zstd: &Path, file: &Path) -> (bool, Option<Vec<u8>>) {
    let tested = aterm_uds::spawnfd::command(zstd)
        .args(["-q", "-t"])
        .arg(file)
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", zstd.display()));
    let decoded = aterm_uds::spawnfd::command(zstd)
        .args(["-q", "-d", "-c"])
        .arg(file)
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", zstd.display()));
    (
        tested.status.success(),
        decoded.status.success().then_some(decoded.stdout),
    )
}

/// Every corpus frame at eight configurations passes `zstd -t` and `zstd -d` gives its
/// input back; the frames the CLI judged carry every width of the sequence count.
#[test]
fn the_zstd_cli_decodes_every_frame_the_encoder_writes() {
    let zstd = cli();
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-zstd-encode-cli")
        .tempdir()
        .expect("scratch");
    let inputs = corpus();
    let mut jobs = Vec::new();
    for (i, (name, input)) in inputs.iter().enumerate() {
        let big = input.len() > 150_000;
        for (level, window_log, checksum, feed) in [
            (1, None, Checksum::Xxh64, Feed::OneShot),
            (3, None, Checksum::Omitted, Feed::OneShot),
            (19, None, Checksum::Xxh64, Feed::OneShot),
            // A lazy level matches every word of `many_sequences`' last block: more sequences
            // than a two-byte count holds, which the parse by price would split into parts.
            (12, None, Checksum::Omitted, Feed::OneShot),
            (
                7,
                Some(11),
                Checksum::Xxh64,
                Feed::Chunks(if big { 70_001 } else { 7 }),
            ),
            (
                5,
                None,
                Checksum::Xxh64,
                Feed::Flushed(if big { 200_000 } else { 1000 }),
            ),
            // The parse by price without fractional prices (level 19's is above).
            (
                17,
                None,
                Checksum::Omitted,
                Feed::Chunks(if big { 131_072 } else { 4096 }),
            ),
            // The producer's packs: level 19 streamed at the widest window, whose long
            // matches past the tree come from anchors.
            (
                19,
                Some(27),
                Checksum::Xxh64,
                Feed::Chunks(if big { 100_000 } else { 1000 }),
            ),
        ] {
            jobs.push((i, name.clone(), level, window_log, checksum, feed));
        }
    }
    let frames = on_every_core(&jobs, |(i, name, level, window_log, checksum, feed)| {
        let input = &inputs[*i].1;
        let mut options = Options::new().level(*level).checksum(*checksum);
        if let Some(w) = window_log {
            options = options.window_log(*w);
        }
        let zst = write_frame(input, options, *feed);
        let file = dir.path().join(
            format!("{name}-{level}-{window_log:?}-{feed:?}.zst")
                .replace([':', ' ', '(', ')'], "_"),
        );
        std::fs::write(&file, &zst).expect("write the frame");
        let what = format!("{name} level {level} window {window_log:?} {checksum:?} {feed:?}");
        let (tested, decoded) = judged(&zstd, &file);
        assert!(tested, "{what}: zstd -t refused it");
        let decoded = decoded.unwrap_or_else(|| panic!("{what}: zstd -d refused it"));
        assert!(
            decoded == *input,
            "{what}: zstd -d wrote {} bytes, not the input",
            decoded.len()
        );
        std::fs::remove_file(&file).expect("remove the frame");
        zst
    });
    let mut census = Census::default();
    for zst in &frames {
        census.add(zst);
    }
    assert!(
        census.sequence_counts.iter().all(|&n| n > 0),
        "1-, 2- and 3-byte sequence counts: {census:?}"
    );
}

/// The far end of the format held to the CLI: frames at the largest window this encoder
/// writes (2^27, `Window_Descriptor` exponent 17, RFC 8878 §3.1.1.1.2) with matches of
/// offset codes 24 to 27, written in one call and streamed, with and without flushes.
/// Every far copy is matched: the frame is about the 16 KiB of noise and a 4-byte RLE block
/// per 128 KiB of zeros, and a missed copy adds its 4 KiB of noise as literals; the
/// control, under a 2^26 window, misses the two copies past it.
#[test]
fn the_zstd_cli_decodes_frames_at_the_largest_window() {
    let zstd = cli();
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-zstd-encode-cli-far")
        .tempdir()
        .expect("scratch");
    let input = crate::support::far_offsets(71);
    let jobs = [
        (1, Feed::OneShot),
        (3, Feed::Chunks(1 << 20)),
        (19, Feed::Flushed(3 << 20)),
    ];
    let rle_blocks = 4 * input.len().div_ceil(128 << 10);
    on_every_core(&jobs, |&(level, feed)| {
        let zst = write_frame(&input, Options::new().level(level).window_log(27), feed);
        let what = format!("level {level} {feed:?}");
        assert_eq!(zst[4] & 0x20, 0, "{what}: a windowed frame");
        assert_eq!(zst[5], (27 - 10) << 3, "{what}: a 2^27 window");
        assert!(
            zst.len() < 16 * 1024 + rle_blocks + 2048,
            "{what}: {} bytes, a far copy unmatched",
            zst.len()
        );
        let file = dir.path().join(format!("far-{level}.zst"));
        std::fs::write(&file, &zst).expect("write the frame");
        let (tested, decoded) = judged(&zstd, &file);
        assert!(tested, "{what}: zstd -t refused it");
        let decoded = decoded.unwrap_or_else(|| panic!("{what}: zstd -d refused it"));
        assert!(decoded == input, "{what}: zstd -d wrote other bytes");
        assert!(
            crate::encode_frames::decode(&zst).expect("our decoder") == input,
            "{what}: our decoder"
        );
        std::fs::remove_file(&file).expect("remove the frame");
    });
    // The control on the bound: the two farthest copies' noise is back in the frame, which
    // the CLI decodes too (exponent 16, offset codes 24 and 25).
    let near = write_frame(
        &input,
        Options::new().level(1).window_log(26),
        Feed::OneShot,
    );
    assert_eq!(near[5], (26 - 10) << 3, "a 2^26 window");
    assert!(
        near.len() > 16 * 1024 + rle_blocks + 2 * 4096,
        "{} bytes under 2^26",
        near.len()
    );
    let file = dir.path().join("far-26.zst");
    std::fs::write(&file, &near).expect("write the frame");
    let (tested, decoded) = judged(&zstd, &file);
    assert!(tested, "2^26: zstd -t refused it");
    assert!(decoded == Some(input), "2^26: zstd -d");
}

/// The negative control: frames of ours with a flipped checksum bit, a block cut short, or
/// a damaged block header are refused by `zstd -t` and `zstd -d` alike.
#[test]
fn the_zstd_cli_refuses_our_frames_damaged() {
    let zstd = cli();
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-zstd-encode-cli-bad")
        .tempdir()
        .expect("scratch");
    let input = crate::support::mixed(300_000, 41);
    let zst = aterm_zstd::compress(&input, &Options::new().level(9));
    let good = dir.path().join("good.zst");
    std::fs::write(&good, &zst).unwrap();
    assert_eq!(
        judged(&zstd, &good),
        (true, Some(input)),
        "the undamaged frame passes"
    );

    let mut damaged = Vec::new();
    let mut flipped = zst.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 0x01;
    damaged.push(("checksum", flipped));
    damaged.push(("cut", zst[..zst.len() - 100].to_vec()));
    let mut header = zst.clone();
    // The first block's header follows the 4-byte magic, the descriptor and the 4-byte
    // content size: make its type the reserved value 3.
    header[9] |= 0b110;
    damaged.push(("reserved block type", header));
    for (what, bytes) in damaged {
        let file = dir.path().join(format!("{what}.zst").replace(' ', "_"));
        std::fs::write(&file, &bytes).unwrap();
        let (tested, decoded) = judged(&zstd, &file);
        assert!(!tested, "{what}: zstd -t passed it");
        assert!(decoded.is_none(), "{what}: zstd -d decoded it");
        assert!(
            crate::encode_frames::decode(&bytes).is_err(),
            "{what}: our decoder decoded it"
        );
    }
}

/// The acceptance bar of the producer's encoder (docs/DESIGN-init-system-2026-10-20.md,
/// row 18): level 19 at its own window (2^23, `zstd -19`'s) makes frames within 3% of what
/// `zstd -19 --single-thread` makes of the same input, here on archive-shaped inputs (a
/// toolchain-like mix with far copies, source-like text, and a pattern with one-byte
/// inserts). Both frames declare the content size and carry the checksum. The control:
/// level 9, a lazy parse, misses that bar on every one of them, so it is a bar a weaker
/// parse fails.
#[test]
fn level_19_stays_within_3_percent_of_zstd_19() {
    let zstd = cli();
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-zstd-encode-cli-ratio")
        .tempdir()
        .expect("scratch");
    let inputs = [
        ("mixed:2m", crate::support::mixed(2 << 20, 11)),
        ("words:1m", crate::support::words(1 << 20, 12)),
        ("inserts:300k", crate::support::inserts(300_000, 9)),
    ];
    let within = |ours: usize, theirs: usize| ours * 100 <= theirs * 103;
    on_every_core(&inputs, |(name, input)| {
        let file = dir.path().join(name.replace(':', "_"));
        std::fs::write(&file, input).expect("write the input");
        let cli = aterm_uds::spawnfd::command(&zstd)
            .args(["-q", "-19", "--single-thread", "-c"])
            .arg(&file)
            .output()
            .unwrap_or_else(|e| panic!("{}: {e}", zstd.display()));
        assert!(
            cli.status.success(),
            "{name}: zstd -19: {}",
            String::from_utf8_lossy(&cli.stderr)
        );
        let theirs = cli.stdout.len();
        let at = |level| {
            aterm_zstd::compress(
                input,
                &Options::new().level(level).checksum(Checksum::Xxh64),
            )
            .len()
        };
        let (ours, lazy) = (at(19), at(9));
        assert!(
            within(ours, theirs),
            "{name}: level 19 made {ours} B, zstd -19 {theirs} B (+{:.2}%)",
            (ours as f64 / theirs as f64 - 1.0) * 100.0
        );
        assert!(
            !within(lazy, theirs),
            "{name}: the control, level 9, made {lazy} B, within 3% of zstd -19's {theirs} B"
        );
    });
}
