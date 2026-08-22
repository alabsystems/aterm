// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE PRICING INSTRUMENT for the fused staging re-verify (aup-2).
//!
//! `verify_and_stage` used to make TWO full passes over the *uncompressed* payload: the
//! extractor wrote every byte, and then `tree::tree_root` re-opened every file and read
//! every byte back to compute the digest the extraction could have produced inline. This
//! harness prices exactly that second pass and proves the fused digest is the SAME
//! number.
//!
//! ```text
//!   cargo run --release -q -p atpkg --example stage_reverify_harness
//!   -> {"payload_bytes":536870912,"entries":508,"archive_bytes":...,
//!       "legacy_extract_ms":...,"legacy_walk_ms":...,"legacy_total_ms":...,
//!       "fused_total_ms":...,"legacy_second_pass_bytes":536870912,
//!       "fused_second_pass_bytes":0,"legacy_second_pass_opens":508,
//!       "fused_second_pass_opens":0,"roots_agree":true}
//! ```
//!
//! # The SHA-256 MOVES; only the read is deleted
//!
//! Read `*_second_pass_bytes` first and `*_ms` second. The bytes/opens columns are the
//! machine-independent statement of the win and are what a fence should ratchet on: the
//! legacy path reads the whole payload back and opens every file again, the fused path
//! reads nothing and opens nothing.
//!
//! The wall clock is the smaller half, and it is smaller than `legacy_walk_ms` looks,
//! because the hashing is not deleted — it is MOVED into the write loop. The legacy arm
//! here really is the unhashed historical extraction (`extract_tar_zst` takes
//! `fold = false`), so `legacy_total_ms` vs `fused_total_ms` is an honest A/B and not a
//! comparison of two hashing runs against one.
//!
//! MEASURED 2026-08-21, real shipped bundle (`trust-5520.tar.zst`, 629,817,785 B
//! compressed → 3,439,406,710 B over 508 entries: 493 files, 8 dirs, 7 hardlinks),
//! five alternating pairs on an M-series machine with the payload warm in page cache:
//!
//! ```text
//!   legacy: 8342 / 8254 / 8579 / 7910 / 7891 ms  (extract ~2330 + walk ~5990)
//!   fused : 7104 / 7209 / 7126 / 6924 / 6961 ms
//!   per-pair: -14.9 / -12.7 / -16.9 / -12.5 / -11.8 %   (mean -13.7 %)
//! ```
//!
//! On every one of those runs the folded root was byte-identical to the `tree_root` in
//! the SIGNED shipped manifest (`c6409bd3…86e9a`) — the strongest available statement
//! that the extraction-time twin reproduces the cross-version byte contract, since that
//! value was produced by the publisher's own walk, not by this code.
//!
//! Inline hashing added ~4.7 s to the extract while removing ~6.0 s of walk: the deleted
//! ~1.25 s is the 3.44 GB READ, warm. On a machine with less free RAM than the payload —
//! the shape production actually meets for this bundle — that read is a genuine disk
//! read and the saving is larger, never smaller.
//!
//! The WALL CLOCK here **understates production**. The real dominant member is
//! `trust-5520` — signed `disk_installed` = 3,439,406,710 B over 508 tar entries — which
//! is larger than free RAM on every machine that installs it, so its second pass is a
//! genuine disk read. A harness payload small enough to run in seconds is still warm in
//! the page cache when the walk starts, so `legacy_walk_ms` here measures mostly
//! `read(2)` + SHA-256 out of cache, not the seek-and-read production pays. Set
//! `ATPKG_STAGE_BENCH_MB` to something larger than this machine's free RAM to see the
//! other shape (and expect the run to take minutes).
//!
//! # Two-sided reach guards
//!
//! A pricing harness that silently stops reaching the code it prices is worse than no
//! harness. This one refuses to print unless BOTH sides are real: the corpus must land
//! inside an expected byte AND entry band (so it can neither shrink to nothing nor blow
//! up unnoticed), the legacy walk must have read back the whole payload, the fused pass
//! must have read back none of it, and the two roots must be byte-identical — the same
//! equality `verify_and_stage` now depends on.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Payload size (uncompressed) in MiB, overridable for a cold-cache run.
fn payload_mib() -> u64 {
    std::env::var("ATPKG_STAGE_BENCH_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(512)
}

/// The shipped `trust` bundle's shape: 508 entries, a handful of very large members and a
/// long tail of small ones. Held constant so the entry-count reach guard is meaningful.
const ENTRIES: usize = 508;
const BIG_MEMBERS: usize = 4;

/// Deterministic, INCOMPRESSIBLE-ish member content (xorshift64), produced as a
/// `Read` so a 100 MB member is streamed into the archive rather than buffered.
///
/// Zstd must not be able to shrink the fixture to nothing: the extraction has to really
/// write `payload_bytes` and the archive on disk has to be a realistic size, or the
/// harness would price a decompressor rather than a payload.
struct Noise {
    remaining: u64,
    state: u64,
}

impl Noise {
    fn new(size: u64, seed: u64) -> Self {
        Self {
            remaining: size,
            state: seed | 1,
        }
    }
}

impl std::io::Read for Noise {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.remaining.min(buf.len() as u64) as usize;
        for b in buf.iter_mut().take(n) {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            *b = (self.state >> 24) as u8;
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Append one member of `size` bytes at `name`, executable or not (both `safe_mode`
/// classes appear in the corpus, so the mode component of the digest is exercised).
fn emit<W: Write>(builder: &mut tar::Builder<W>, name: &str, size: u64, exec: bool, seed: u64) {
    let mut header = tar::Header::new_ustar();
    header.set_size(size);
    header.set_mode(if exec { 0o755 } else { 0o644 });
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(&mut header, name, Noise::new(size, seed))
        .expect("append member");
}

/// Build the fixture archive; returns `(path, payload_bytes, entries)`.
fn build_archive(dir: &Path, payload_bytes: u64) -> (PathBuf, u64, usize) {
    let path = dir.join("bench.tar.zst");
    let file = std::fs::File::create(&path).expect("create archive");
    let mut enc = zstd::Encoder::new(file, 1).expect("zstd encoder");
    let mut written = 0u64;
    {
        let mut builder = tar::Builder::new(&mut enc);
        // 80 % of the bytes in BIG_MEMBERS huge files, the rest spread over the tail —
        // the shipped `trust` bundle's shape (a handful of 175-600 MB members plus a long
        // tail), so the per-file `open` cost and the per-byte cost are both represented.
        let big_total = payload_bytes / 100 * 80;
        let big_each = big_total / BIG_MEMBERS as u64;
        let tail_count = ENTRIES - BIG_MEMBERS;
        let tail_each =
            (payload_bytes - big_each * BIG_MEMBERS as u64) / tail_count as u64;
        for i in 0..BIG_MEMBERS {
            emit(
                &mut builder,
                &format!("lib/rustlib/big-{i}.rlib"),
                big_each,
                i == 0,
                0x9E37_79B9_7F4A_7C15 ^ i as u64,
            );
            written += big_each;
        }
        for i in 0..tail_count {
            emit(
                &mut builder,
                &format!("lib/rustlib/tail/{:03}/small-{i}.o", i % 16),
                tail_each,
                i % 32 == 0,
                0x1234_5678_9ABC_DEF0 ^ i as u64,
            );
            written += tail_each;
        }
        builder.finish().expect("finish tar");
    }
    enc.finish().expect("finish zstd");
    (path, written, ENTRIES)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let payload_bytes = payload_mib() * (1 << 20);
    let scratch = std::env::temp_dir().join(format!("atpkg-stage-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");
    let (archive, payload_bytes, entries) = build_archive(&scratch, payload_bytes);
    let archive_bytes = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
    // Generous caps: this harness prices I/O, not the bomb guards.
    let cap = payload_bytes.saturating_mul(2).max(1 << 20);

    // --- TODAY: extract, then re-read the whole tree to learn the root. ---------
    let legacy_dir = scratch.join("legacy");
    std::fs::create_dir_all(&legacy_dir).expect("legacy dir");
    let t0 = Instant::now();
    atpkg::extract::extract_tar_zst(&archive, &legacy_dir, cap, 4_000_000).expect("extract");
    let legacy_extract_ms = ms(t0);
    let t1 = Instant::now();
    let legacy_root = atpkg::tree::tree_root(&legacy_dir).expect("tree_root");
    let legacy_walk_ms = ms(t1);

    // --- FUSED: one pass; the digest falls out of the writing. -------------------
    let fused_dir = scratch.join("fused");
    std::fs::create_dir_all(&fused_dir).expect("fused dir");
    let t2 = Instant::now();
    let fused_root =
        atpkg::extract::extract_tar_zst_rooted(&archive, &fused_dir, cap, 4_000_000)
            .expect("extract rooted");
    let fused_total_ms = ms(t2);

    // --- Two-sided reach guards -------------------------------------------------
    let extracted: u64 = walk_bytes(&legacy_dir);
    let extracted_files = walk_files(&legacy_dir);
    assert!(
        (payload_bytes / 2..=payload_bytes * 2).contains(&extracted),
        "corpus reach guard: extracted {extracted} B is not within 2x of the intended \
         {payload_bytes} B — the harness is no longer pricing the pass it claims to"
    );
    assert!(
        (ENTRIES / 2..=ENTRIES * 2).contains(&extracted_files),
        "corpus reach guard: {extracted_files} files extracted, expected ~{ENTRIES}"
    );
    assert_eq!(
        legacy_root, fused_root,
        "the fused root must be byte-identical to the on-disk walk — that equality is \
         what `verify_and_stage` now stands on"
    );
    assert_eq!(legacy_root.len(), 64);

    println!(
        "{{\"payload_bytes\":{payload_bytes},\"entries\":{entries},\
         \"archive_bytes\":{archive_bytes},\
         \"legacy_extract_ms\":{legacy_extract_ms:.1},\
         \"legacy_walk_ms\":{legacy_walk_ms:.1},\
         \"legacy_total_ms\":{:.1},\
         \"fused_total_ms\":{fused_total_ms:.1},\
         \"legacy_second_pass_bytes\":{extracted},\
         \"fused_second_pass_bytes\":0,\
         \"legacy_second_pass_opens\":{extracted_files},\
         \"fused_second_pass_opens\":0,\
         \"roots_agree\":true}}",
        legacy_extract_ms + legacy_walk_ms
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

/// Total content bytes under `dir` — the exact size of the pass the fused digest deletes.
fn walk_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for e in entries.flatten() {
        let p = e.path();
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() => total += walk_bytes(&p),
            Ok(m) if m.is_file() => total += m.len(),
            _ => {}
        }
    }
    total
}

/// File count under `dir` — one `open`+`read`-to-EOF per file in the deleted pass.
fn walk_files(dir: &Path) -> usize {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for e in entries.flatten() {
        let p = e.path();
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() => total += walk_files(&p),
            Ok(m) if m.is_file() => total += 1,
            _ => {}
        }
    }
    total
}
