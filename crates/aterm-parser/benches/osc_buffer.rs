// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// OSC ACCUMULATION BUFFER: the grow/shrink cycle on back-to-back large OSC
// payloads (ROADMAP WS-K).
//
// WHY THIS EXISTS: `Parser::dispatch_osc` releases any `osc_data` capacity above
// 4 KiB after EVERY dispatch (`shrink_to(128)`, added by #7272 so one OSC 1337
// image cannot hold up to MAX_OSC_DATA = 8 MiB per parser instance for the
// session lifetime). The `Parser` is a long-lived field of `Terminal`, so a
// client that emits large OSC payloads back to back re-grows 128 -> payload
// through `Vec`'s doubling on EVERY sequence. No bench in the tree re-entered
// that cycle: aterm-gpu/benches/gpu_frame.rs issues its OSC 1337 escape ONCE
// during setup, outside `b.iter`.
//
// THE FEED IS CHUNKED, AND THE CHUNKING IS THE POINT. A single
// `extend_from_slice` of the whole payload reserves the exact size in one step
// and copies nothing, which would measure the defect away. The shipping GUI
// re-slices each PTY batch into 8 KiB / 16 KiB holds before `t.process(...)`
// (aterm-gui/src/spawn.rs, `ingest_chunk_width`), so a 2.8 MiB payload arrives
// as ~340 separate `extend_from_slice` calls into a `Vec` that starts at capacity
// 128 — which is what makes the doubling ladder real. This bench feeds 8 KiB
// chunks for the same reason.
//
// THE LANES BRACKET THE SHRINK GUARD (`capacity() > 4096`):
//   * `osc1337_fullhd` payloads are far above 4 KiB, so the guard fires on every
//     dispatch and the next sequence re-grows from scratch.
//   * `osc_title_small` payloads are ~40 bytes and the capacity never leaves its
//     initial 128, so the guard NEVER fires. That lane must be insensitive to
//     any change in the retention policy — it is the negative half of the reach
//     guard and a within-run control arm.
// `verify_reaches_target` asserts both, through the sink's observed payload
// sizes (capacity is `pub(crate)`, payload length is not, and capacity >= len).
//
//   cargo bench -p aterm-parser --bench osc_buffer

use aterm_parser::{ActionSink, Parser};
use aterm_provenance::{Provenance, Pty};
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

/// Payload bytes in one large OSC 1337 sequence. 2.8 MB is the size the finding
/// itself names: a full-HD PNG in base64, the payload MAX_OSC_DATA (8 MiB) was
/// sized to admit. From the 128-byte floor that is ~15 doublings per sequence.
const BIG_PAYLOAD: usize = 2_800_000;

/// Large sequences per iteration. More than one is essential: the defect is that
/// sequence N+1 cannot reuse sequence N's buffer, so a single-sequence workload
/// would measure nothing.
const BIG_COUNT: usize = 3;

/// Small sequences per iteration, sized to about the same total byte count so
/// the two lanes are comparable in scale.
const SMALL_COUNT: usize = 20_000;

/// The chunk width the GUI's ingest loop hands to `Terminal::process`.
const CHUNK: usize = 8 * 1024;

/// The shrink guard's threshold, mirrored here so the reach assertions name the
/// same number the code under test branches on.
const SHRINK_GUARD_BYTES: usize = 4096;

/// Sink that observes every OSC payload it is handed. Observing the LENGTH is
/// what makes the accumulation non-elidable and what the reach guard reads.
#[derive(Default)]
struct LenSink {
    dispatches: usize,
    max_param_len: usize,
    total_bytes: usize,
}

impl ActionSink for LenSink {
    fn print(&mut self, _: char) {}
    fn print_ascii_bulk(&mut self, _: &Provenance<[u8], Pty>) {}
    fn execute(&mut self, _: u8) {}
    fn csi_dispatch(&mut self, _: &Provenance<[u16], Pty>, _: &Provenance<[u8], Pty>, _: u8) {}
    fn esc_dispatch(&mut self, _: &Provenance<[u8], Pty>, _: u8) {}
    fn osc_dispatch(&mut self, params: &Provenance<[&[u8]], Pty>) {
        self.dispatches += 1;
        for p in params.as_ref() {
            self.max_param_len = self.max_param_len.max(p.len());
            self.total_bytes += p.len();
        }
    }
    fn dcs_hook(&mut self, _: &Provenance<[u16], Pty>, _: &Provenance<[u8], Pty>, _: u8) {}
    fn dcs_put(&mut self, _: u8) {}
    fn dcs_unhook(&mut self, _: bool) {}
    fn apc_start(&mut self) {}
    fn apc_put(&mut self, _: u8) {}
    fn apc_end(&mut self) {}
}

/// `BIG_COUNT` OSC 1337 `File=` sequences, each carrying `BIG_PAYLOAD` bytes of
/// base64 alphabet — the iTerm2 inline-image shape (`imgcat`, `chafa -f iterm`,
/// `timg -p iterm`), which is the real producer of back-to-back large OSC.
fn big_corpus() -> Vec<u8> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(BIG_COUNT * (BIG_PAYLOAD + 64));
    for seq in 0..BIG_COUNT {
        out.extend_from_slice(b"\x1b]1337;File=inline=1;size=2800000:");
        for i in 0..BIG_PAYLOAD {
            out.push(alphabet[(i + seq * 7) % alphabet.len()]);
        }
        out.push(0x07);
    }
    out
}

/// Many small OSC 0 title sets — the highest-frequency real OSC, and a payload
/// that never takes the capacity past its initial 128 bytes.
///
/// NOTE for anyone reading a result off this lane: it is NOT immune to a change
/// in `dispatch_osc` itself. It runs the function 20,000 times per iteration, so
/// adding or removing even the capacity COMPARE moves it. It is a control for
/// the RETENTION POLICY (its capacity never leaves 128, so no shrink/regrow ever
/// happens on it), not for the function as a whole.
fn small_corpus() -> Vec<u8> {
    let mut out = Vec::with_capacity(SMALL_COUNT * 48);
    for i in 0..SMALL_COUNT {
        out.extend_from_slice(b"\x1b]0;bench title ");
        out.extend_from_slice(format!("{i:08}").as_bytes());
        out.push(0x07);
    }
    out
}

struct Workload {
    name: &'static str,
    /// Whether this lane's payloads cross the `capacity() > 4096` shrink guard.
    trips_shrink_guard: bool,
    corpus: Vec<u8>,
}

fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "osc1337_fullhd",
            trips_shrink_guard: true,
            corpus: big_corpus(),
        },
        Workload {
            name: "osc_title_small",
            trips_shrink_guard: false,
            corpus: small_corpus(),
        },
    ]
}

/// Feed the corpus through ONE long-lived parser in GUI-sized chunks — the same
/// arrangement the shipping reader uses, and the arrangement the defect needs
/// (a fresh parser per sequence would have nothing to retain).
fn run(corpus: &[u8]) -> LenSink {
    let mut parser = Parser::new();
    let mut sink = LenSink::default();
    for chunk in corpus.chunks(CHUNK) {
        parser.advance_fast(chunk, &mut sink);
    }
    sink
}

/// Two-sided: the big lane must really carry payloads past the shrink guard, and
/// the small lane must really stay under it. If either flips, the pair stops
/// isolating the retention policy and starts measuring something else.
fn verify_reaches_target(w: &Workload) {
    let sink = run(&w.corpus);
    assert!(
        sink.dispatches > 0,
        "{}: no OSC dispatch happened at all — the corpus never reaches \
         `Parser::dispatch_osc`",
        w.name
    );
    if w.trips_shrink_guard {
        assert_eq!(
            sink.dispatches, BIG_COUNT,
            "{}: expected exactly {BIG_COUNT} large sequences",
            w.name
        );
        assert!(
            sink.max_param_len > SHRINK_GUARD_BYTES,
            "{}: largest payload is {} bytes, which does NOT push `osc_data` \
             capacity past the {SHRINK_GUARD_BYTES}-byte shrink guard — this \
             lane would measure a policy that never runs",
            w.name,
            sink.max_param_len
        );
    } else {
        assert_eq!(
            sink.dispatches, SMALL_COUNT,
            "{}: expected exactly {SMALL_COUNT} small sequences",
            w.name
        );
        assert!(
            sink.max_param_len < SHRINK_GUARD_BYTES,
            "{}: largest payload is {} bytes — the control lane must stay \
             under the {SHRINK_GUARD_BYTES}-byte guard so the retention policy \
             is provably never exercised on it",
            w.name,
            sink.max_param_len
        );
    }
}

fn osc_buffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("osc_buffer");
    for w in workloads() {
        verify_reaches_target(&w);
        group.throughput(Throughput::Bytes(w.corpus.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(w.name), &w, |b, w| {
            b.iter(|| {
                let sink = run(black_box(&w.corpus));
                black_box(sink.total_bytes);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, osc_buffer);
criterion_main!(benches);
