// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Benchmark measuring [`aterm_restore::recover`] over damaged history rings.

use std::time::Instant;

use astream_wire::frame::{HEADER_SIZE, MAGIC, MAX_PAYLOAD_LEN, VERSION};
use aterm_core::terminal::Terminal;
use aterm_restore::layout::{OFF_HIST_HI, OFF_HIST_LO};
use aterm_restore::{
    Geometry, HistoryIdentity, JournalWriter, KeyframeInput, RecoverPolicy, Recovery, RegionStore,
    RegionView, VecStore, WriterIdentity, recover,
};

const WRITER: WriterIdentity<'static> = WriterIdentity {
    pid: 7,
    birth_us: 1,
    local_id: 2,
    build: 105,
    sid: "s-resync-walk",
};

const CONFIG: u64 = 0x5EED;

/// The planted sizes, in KiB, up to the full v1 ring.
const SIZES_KIB: &[usize] = &[64, 128, 256, 512, 1024, 2048, 4096, 6912];

/// The look-alike run of `lookalike-chunk`: a chunk's worth.
const CHUNK_LOOKALIKES: usize = 32 * 1024;

/// A v1 region the real writer built, and where its ring's chunks lie.
struct Base {
    store: VecStore,
    geometry: Geometry,
    /// Half-open ring byte ranges for each chunk, oldest first.
    chunks: Vec<(u64, u64)>,
    lo: u64,
    hi: u64,
}

fn word(view: &VecStore, at: usize) -> u64 {
    view.load_u64_acquire(at).expect("a header word")
}

fn base() -> Base {
    let geometry = Geometry::v1();
    let mut writer = JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER)
        .expect("created");
    let mut term = Terminal::new(24, 80);
    for i in 0..2_000u64 {
        term.process(
            format!(
                "history line {i}: the quick brown fox jumps over the lazy dog {}\r\n",
                i.wrapping_mul(7_919) % 10_007
            )
            .as_bytes(),
        );
    }
    let fence = term.history_fence();
    writer
        .reset_history(
            HistoryIdentity::of(&fence),
            fence.oldest,
            geometry.hist_cap_phys,
            false,
        )
        .expect("reset");
    let mut chunks = Vec::new();
    let mut row = fence.oldest;
    while row < fence.end {
        let start = word(writer.store(), OFF_HIST_HI);
        let lines = term
            .history_lines_since_fence(&fence, row, 64)
            .expect("holds");
        writer.append_history(row, &lines).expect("fits");
        chunks.push((start, word(writer.store(), OFF_HIST_HI)));
        row += lines.len() as u64;
    }
    let checkpoint = term.checkpoint_journal(512).expect("ground");
    writer
        .write_keyframe(&KeyframeInput {
            checkpoint: &checkpoint,
            fence: term.history_fence(),
            resize_lifetime: term.resize_lifetime(),
            resize_ordinal: term.resize_ordinal(),
            bridge_lines: 512,
            config_fp: CONFIG,
            integration_on: false,
        })
        .expect("keyframe");
    let store = writer.store().snapshot();
    let lo = word(&store, OFF_HIST_LO);
    let hi = word(&store, OFF_HIST_HI);
    Base {
        store,
        geometry,
        chunks,
        lo,
        hi,
    }
}

impl Base {
    /// The region offset of ring position `pos` (the planted cases never wrap).
    fn at(&self, pos: u64) -> usize {
        let cap = self.geometry.hist_cap_phys as u64;
        assert!(pos >= self.lo && pos - self.lo <= cap, "inside one lap");
        self.geometry.hist_off + usize::try_from(pos % cap).expect("an offset")
    }

    /// The ring position `kib` KiB past the last chunk, if the ring holds it unwrapped.
    fn past_end(&self, kib: usize) -> Option<u64> {
        let end = self.hi + (kib as u64) * 1024;
        let cap = self.geometry.hist_cap_phys as u64;
        (self.lo % cap + (end - self.lo) <= cap).then_some(end)
    }

    fn flip(&self, store: &VecStore, pos: u64) {
        let at = self.at(pos);
        let mut b = [0u8; 1];
        store.read(at, &mut b).expect("read");
        store.write(at, &[b[0] ^ 0xFF]).expect("write");
    }

    /// One payload byte of chunk `i` flipped: its CRC fails, its header stays whole.
    fn damage_chunk(&self, store: &VecStore, i: usize) {
        let (start, end) = self.chunks[i];
        let pos = start + HEADER_SIZE as u64 + 3;
        assert!(pos < end, "a payload byte");
        self.flip(store, pos);
    }

    /// Writes synthetic frame look-alikes across ring positions `from..until`.
    fn lookalikes(&self, store: &VecStore, from: u64, until: u64, seg_end: u64) {
        let mut p = from;
        while p + (HEADER_SIZE as u64) < until.min(seg_end) {
            let len = (seg_end - p - HEADER_SIZE as u64).min(MAX_PAYLOAD_LEN as u64);
            let len = u32::try_from(len).expect("under the frame cap");
            let mut h = [0u8; HEADER_SIZE];
            h[0] = MAGIC[0];
            h[1] = MAGIC[1];
            h[2] = VERSION;
            h[4..8].copy_from_slice(&len.to_le_bytes());
            store.write(self.at(p), &h).expect("write");
            p += HEADER_SIZE as u64;
        }
    }

    fn random(&self, store: &VecStore, from: u64, until: u64) {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut bytes = Vec::with_capacity(usize::try_from(until - from).expect("a size"));
        while bytes.len() < bytes.capacity() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        bytes.truncate(usize::try_from(until - from).expect("a size"));
        store.write(self.at(from), &bytes).expect("write");
    }

    fn set_hi(&self, store: &VecStore, hi: u64) {
        store.store_u64_release(OFF_HIST_HI, hi).expect("hi");
    }
}

/// The ring's verdict and the chunks it kept, as `recover` returns them.
fn verdict(view: &VecStore) -> (String, usize) {
    match restore(view) {
        Recovery::Restored(r) => (
            r.ring
                .fault
                .as_ref()
                .map_or_else(|| "clean".to_owned(), |f| format!("{f:?}")),
            r.ring.chunks.len(),
        ),
        Recovery::Meta(why) => (format!("META {why:?}"), 0),
    }
}

fn restore(view: &VecStore) -> Recovery {
    recover(
        view,
        RecoverPolicy {
            tail: true,
            config_fp: CONFIG,
            scratch_nonce: [0xA5; 32],
        },
        Terminal::new,
    )
}

/// Median, min and max milliseconds of `runs` restores of `view`.
fn time(view: &VecStore, runs: usize) -> (f64, f64, f64) {
    let mut ms: Vec<f64> = (0..runs)
        .map(|_| {
            #[expect(
                clippy::disallowed_methods,
                reason = "a measurement times recover; the crate's library still reads no clock"
            )]
            let t = Instant::now();
            let r = restore(view);
            #[expect(
                clippy::disallowed_methods,
                reason = "a measurement times recover; the crate's library still reads no clock"
            )]
            let elapsed = t.elapsed().as_secs_f64() * 1000.0;
            drop(std::hint::black_box(r));
            elapsed
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    (ms[ms.len() / 2], ms[0], ms[ms.len() - 1])
}

fn row(case: &str, kib: Option<usize>, view: &VecStore, runs: usize) {
    if arg_str("--only").is_some_and(|only| only != case) {
        return;
    }
    let (fault, kept) = verdict(view);
    let (median, min, max) = time(view, runs);
    let size = kib.map_or_else(String::new, |k| format!("{k} KiB"));
    println!(
        "{case:<16} {size:>9}  median {median:>10.3} ms  (min {min:.3}, max {max:.3}, n={runs})  kept {kept:>2}  {fault}"
    );
}

fn arg_str(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn arg(name: &str) -> Option<usize> {
    arg_str(name).and_then(|v| v.parse().ok())
}

fn main() {
    let runs = arg("--runs").unwrap_or(7).max(1);
    let max_kib = arg("--max-kib").unwrap_or(usize::MAX);
    let base = base();
    println!(
        "v1 region: ring {} bytes, {} real chunks over {} bytes; debug_assertions={}",
        base.geometry.hist_cap_phys,
        base.chunks.len(),
        base.hi - base.lo,
        cfg!(debug_assertions)
    );

    row("clean", None, &base.store, runs);

    let flipped = base.store.snapshot();
    base.damage_chunk(&flipped, 0);
    row("bitflip", None, &flipped, runs);

    let last = base.chunks.len() - 1;
    for &kib in SIZES_KIB.iter().filter(|k| **k <= max_kib) {
        let Some(end) = base.past_end(kib) else {
            println!("{kib} KiB does not fit the ring past its chunks: skipped");
            continue;
        };
        let random = base.store.snapshot();
        base.damage_chunk(&random, last);
        base.random(&random, base.hi, end);
        base.set_hi(&random, end);
        row("random", Some(kib), &random, runs);

        let tail = base.store.snapshot();
        base.damage_chunk(&tail, last);
        base.lookalikes(&tail, base.hi, end, end);
        base.set_hi(&tail, end);
        row("lookalike-tail", Some(kib), &tail, runs);

        let chunk = base.store.snapshot();
        let (c0, c1) = base.chunks[0];
        let mut real = vec![0u8; usize::try_from(c1 - c0).expect("a chunk")];
        base.store.read(base.at(c0), &mut real).expect("read");
        let zeros = vec![0u8; usize::try_from(end - base.lo).expect("a size")];
        chunk.write(base.at(base.lo), &zeros).expect("write");
        let real_at = end - real.len() as u64;
        chunk.write(base.at(real_at), &real).expect("write");
        base.lookalikes(&chunk, base.lo, base.lo + CHUNK_LOOKALIKES as u64, end);
        base.set_hi(&chunk, end);
        row("lookalike-chunk", Some(kib), &chunk, runs);
    }
}
