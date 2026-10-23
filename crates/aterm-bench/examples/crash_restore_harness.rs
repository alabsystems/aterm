// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Crash-restore journal benchmark harness for the `crash-restore` performance lane.

use std::hint::black_box;
use std::time::Instant;

use aterm_core::terminal::Terminal;
use aterm_restore::layout::{Geometry, TAIL_CAP_V1};
use aterm_restore::{
    HistoryIdentity, HistoryWriter, JournalWriter, KeyframeInput, RecoverPolicy, Recovery, Touched,
    VecStore, WriterIdentity, recover,
};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const TAP_BYTES: usize = 16 << 20;
const SLICE: usize = 64 * 1024;
const N_ITERS: usize = 9;
const WARMUP: usize = 2;
const PER_SAMPLE: usize = 64;

const WRITER: WriterIdentity<'static> = WriterIdentity {
    pid: 1,
    birth_us: 1,
    local_id: 1,
    build: 1,
    sid: "bench",
};

fn median(samples: &mut [f64]) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = samples.len();
    if n % 2 == 1 {
        samples[n / 2]
    } else {
        (samples[n / 2 - 1] + samples[n / 2]) / 2.0
    }
}

fn writer() -> JournalWriter<VecStore> {
    let geometry = Geometry::v1();
    JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER)
        .expect("a v1 region")
}

fn tap_writer() -> JournalWriter<Touched<VecStore>> {
    let geometry = Geometry::v1();
    let meter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    JournalWriter::create(
        Touched::new(VecStore::new(geometry.region_len), Some(meter)),
        geometry,
        &WRITER,
    )
    .expect("a v1 region")
}

fn tap_pass(corpus: &[u8], tap: Option<&mut JournalWriter<Touched<VecStore>>>) -> f64 {
    let mut term = Terminal::new(ROWS, COLS);
    let started = Instant::now();
    match tap {
        Some(writer) => {
            for slice in corpus.chunks(SLICE) {
                term.process(black_box(slice));
                writer.append_slice(slice).expect("appended");
            }
        }
        None => {
            for slice in corpus.chunks(SLICE) {
                term.process(black_box(slice));
            }
        }
    }
    let secs = started.elapsed().as_secs_f64().max(1e-9);
    black_box(term.cursor().col);
    corpus.len() as f64 / 1e6 / secs
}

fn shell(lines: usize) -> Terminal {
    let mut term = Terminal::new(ROWS, COLS);
    for i in 0..lines {
        term.process(
            format!(
                "\x1b[1;3{}m{i:05}\x1b[0m build step \x1b[32mok\x1b[0m in 0.{:03}s\r\n",
                i % 8,
                i % 999
            )
            .as_bytes(),
        );
    }
    term
}

fn tui() -> Terminal {
    let mut term = shell(2000);
    term.process(b"\x1b[?1049h\x1b[2J");
    for row in 1..=ROWS {
        term.process(
            format!(
                "\x1b[{row};1H\x1b[48;5;{}m\u{2502} pane {row:02} \x1b[0m some program output here",
                17 + row
            )
            .as_bytes(),
        );
    }
    term
}

fn keyframe_secs(term: &Terminal, bridge: usize) -> (f64, f64) {
    let mut writer = writer();
    let mut whole = Vec::with_capacity(N_ITERS);
    let mut capture = Vec::with_capacity(N_ITERS);
    for i in 0..WARMUP + N_ITERS {
        let started = Instant::now();
        for _ in 0..PER_SAMPLE {
            let checkpoint = term.checkpoint_journal_marked(bridge, 512).expect("ground");
            let input = KeyframeInput {
                checkpoint: &checkpoint,
                fence: term.history_fence(),
                resize_lifetime: term.resize_lifetime(),
                resize_ordinal: term.resize_ordinal(),
                bridge_lines: u32::try_from(bridge).unwrap_or(u32::MAX),
                config_fp: 1,
                integration_on: term.shell_integration_posture()
                    == aterm_core::terminal::ShellIntegrationPosture::On,
            };
            writer.write_keyframe(&input).expect("a keyframe fits");
        }
        let per_keyframe = started.elapsed().as_secs_f64() / PER_SAMPLE as f64;
        let started = Instant::now();
        for _ in 0..PER_SAMPLE {
            black_box(term.checkpoint_journal_marked(bridge, 512));
        }
        let per_capture = started.elapsed().as_secs_f64() / PER_SAMPLE as f64;
        if i >= WARMUP {
            whole.push(per_keyframe);
            capture.push(per_capture);
        }
    }
    (median(&mut whole), median(&mut capture))
}

fn export_us_per_line(term: &Terminal) -> f64 {
    let fence = term.history_fence();
    let geometry = Geometry::v1();
    let store = VecStore::new(geometry.region_len);
    let mut samples = Vec::with_capacity(N_ITERS);
    for i in 0..WARMUP + N_ITERS {
        let mut ring = HistoryWriter::new(&geometry);
        ring.reset_ring(
            &store,
            HistoryIdentity::of(&fence),
            fence.oldest,
            geometry.hist_cap_phys,
            false,
        )
        .expect("reset");
        let started = Instant::now();
        let mut row = fence.oldest;
        while row < fence.end {
            let lines = term
                .history_lines_since_fence(&fence, row, aterm_restore::records::CHUNK_LINES)
                .expect("the fence holds");
            ring.append_chunk(&store, row, black_box(&lines))
                .expect("a chunk fits");
            row += lines.len() as u64;
        }
        if i >= WARMUP {
            samples.push(started.elapsed().as_secs_f64() * 1e6 / fence.lines().max(1) as f64);
        }
    }
    median(&mut samples)
}

fn recover_secs(corpus: &[u8]) -> f64 {
    let mut writer = writer();
    let mut term = shell(2000);
    let checkpoint = term.checkpoint_journal(512).expect("ground");
    writer
        .write_keyframe(&KeyframeInput {
            checkpoint: &checkpoint,
            fence: term.history_fence(),
            resize_lifetime: term.resize_lifetime(),
            resize_ordinal: term.resize_ordinal(),
            bridge_lines: 512,
            config_fp: 1,
            integration_on: term.shell_integration_posture()
                == aterm_core::terminal::ShellIntegrationPosture::On,
        })
        .expect("a keyframe fits");
    let tail = &corpus[..TAIL_CAP_V1 - TAIL_CAP_V1 / 16];
    for slice in tail.chunks(4096) {
        term.process(slice);
        writer.append_slice(slice).expect("appended");
    }
    writer
        .set_gathered(writer.stream_processed())
        .expect("gathered");
    let region = writer.store().snapshot();
    let policy = RecoverPolicy {
        tail: true,
        config_fp: 1,
        scratch_nonce: [0xA5; 32],
    };
    let mut samples = Vec::with_capacity(N_ITERS);
    for i in 0..WARMUP + N_ITERS {
        let started = Instant::now();
        for _ in 0..PER_SAMPLE / 8 {
            match recover(&region, policy, Terminal::new) {
                Recovery::Restored(restored) => {
                    assert!(
                        restored.exact,
                        "the bench's restore is exact: {:?}",
                        restored.losses
                    );
                    black_box(restored);
                }
                Recovery::Meta(why) => panic!("the bench's region did not restore: {why:?}"),
            }
        }
        if i >= WARMUP {
            samples.push(started.elapsed().as_secs_f64() / (PER_SAMPLE / 8) as f64);
        }
    }
    median(&mut samples)
}

fn main() {
    let corpus = aterm_bench::throughput_corpus(TAP_BYTES);

    let mut tap_writer = tap_writer();
    let mut ratios = Vec::with_capacity(N_ITERS);
    let mut tapped = Vec::with_capacity(N_ITERS);
    for i in 0..WARMUP + N_ITERS {
        let without = tap_pass(&corpus, None);
        let with = tap_pass(&corpus, Some(&mut tap_writer));
        if i >= WARMUP {
            ratios.push(with / without);
            tapped.push(with);
        }
    }
    let tail_tap_ratio = median(&mut ratios);
    let tail_tap_mbps = median(&mut tapped);

    let shell_term = shell(2000);
    let (kf_shell, kf_shell_capture) = keyframe_secs(&shell_term, 0);
    let (kf_tui, kf_tui_capture) = keyframe_secs(&tui(), 0);
    let (kf_256, kf_256_capture) = keyframe_secs(&shell_term, 256);
    let (kf_512, kf_512_capture) = keyframe_secs(&shell_term, 512);
    let recover = recover_secs(&corpus);
    let export = export_us_per_line(&shell(20_000));

    let us = |secs: f64| secs * 1e6;
    eprintln!(
        "crash_restore_harness: tail tap {:.3}x ({tail_tap_mbps:.1} MB/s); keyframe shell {:.1} us \
         (capture {:.1}), tui {:.1} us ({:.1}), +256 {:.1} us ({:.1}), +512 {:.1} us ({:.1}); \
         recover {:.1} us; export {export:.3} us/line",
        tail_tap_ratio,
        us(kf_shell),
        us(kf_shell_capture),
        us(kf_tui),
        us(kf_tui_capture),
        us(kf_256),
        us(kf_256_capture),
        us(kf_512),
        us(kf_512_capture),
        us(recover),
    );
    println!(
        "{{\"tail_tap_ratio\":{tail_tap_ratio:.4},\"tail_tap_mbps\":{tail_tap_mbps:.3},\
         \"kf_shell_hz\":{:.3},\"kf_tui_hz\":{:.3},\"kf_bridge256_hz\":{:.3},\
         \"kf_bridge512_hz\":{:.3},\"recover_hz\":{:.3},\
         \"kf_shell_us\":{:.3},\"kf_shell_capture_us\":{:.3},\"kf_tui_us\":{:.3},\
         \"kf_tui_capture_us\":{:.3},\"kf_bridge256_us\":{:.3},\"kf_bridge256_capture_us\":{:.3},\
         \"kf_bridge512_us\":{:.3},\"kf_bridge512_capture_us\":{:.3},\"recover_us\":{:.3},\
         \"export_us_per_line\":{export:.4},\"n\":{N_ITERS},\"warmup\":{WARMUP}}}",
        1.0 / kf_shell,
        1.0 / kf_tui,
        1.0 / kf_256,
        1.0 / kf_512,
        1.0 / recover,
        us(kf_shell),
        us(kf_shell_capture),
        us(kf_tui),
        us(kf_tui_capture),
        us(kf_256),
        us(kf_256_capture),
        us(kf_512),
        us(kf_512_capture),
        us(recover),
    );
}
