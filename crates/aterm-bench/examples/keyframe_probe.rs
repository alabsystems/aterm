// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyframe work/clock diagnostic, with no performance verdict or baseline writes.

use aterm_bench::measurement_clock as clock;
use aterm_core::terminal::Terminal;
use aterm_restore::layout::Geometry;
use aterm_restore::{JournalWriter, KeyframeInput, VecStore, WriterIdentity};
use std::hint::black_box;
use std::time::Instant;

const ROWS: u16 = 24;
const COLS: u16 = 80;
const WRITER: WriterIdentity<'static> = WriterIdentity {
    pid: 1,
    birth_us: 1,
    local_id: 1,
    build: 1,
    sid: "bench",
};

fn writer() -> JournalWriter<VecStore> {
    let geometry = Geometry::v1();
    JournalWriter::create(VecStore::new(geometry.region_len), geometry, &WRITER)
        .expect("a v1 region")
}

fn shell(lines: usize) -> Terminal {
    let mut term = Terminal::new(ROWS, COLS);
    for i in 0..lines {
        term.process(
            format!(
                "\x1b[1;3{}m{i:05}\x1b[0m build step \x1b[32mok\x1b[0m in 0.{:03}s\r\n",
                i % 8,
                i % 999,
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
                17 + row,
            )
            .as_bytes(),
        );
    }
    term
}

fn write(term: &Terminal, bridge: usize, writer: &mut JournalWriter<VecStore>) {
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
    black_box(writer.write_keyframe(&input).expect("a keyframe fits"));
}

fn main() {
    let interactive = std::env::args().any(|arg| arg == "--interactive");
    if interactive {
        assert!(clock::declare_interactive(), "QoS request unavailable");
    }
    let shell_term = shell(2000);
    let tui_term = tui();
    let cases = [
        ("shell", &shell_term, 0),
        ("tui", &tui_term, 0),
        ("bridge256", &shell_term, 256),
        ("bridge512", &shell_term, 512),
    ];
    let mut writers: Vec<_> = cases.iter().map(|_| writer()).collect();
    for (index, (_, term, bridge)) in cases.iter().enumerate() {
        for _ in 0..128 {
            write(term, *bridge, &mut writers[index]);
        }
    }
    for iterations in [64, 4096] {
        for sample in 0..7 {
            for offset in 0..cases.len() {
                let index = (sample + offset) % cases.len();
                let (name, term, bridge) = cases[index];
                let before = clock::counters();
                let started = Instant::now();
                for _ in 0..iterations {
                    write(black_box(term), bridge, &mut writers[index]);
                }
                let wall_ns = started.elapsed().as_nanos() as f64;
                let counts = before.zip(clock::counters()).map(|(a, b)| b.since(a));
                let per = |value: f64| value / iterations as f64;
                let instructions = counts.map(|c| per(c.instructions as f64));
                let cycles = counts.map(|c| per(c.cycles as f64));
                let cpu_ns = counts.map(|c| per(c.cpu_ns));
                let pshare = counts.map(|c| c.pinstructions as f64 / c.instructions as f64);
                let pcycles = counts
                    .filter(|c| {
                        c.instructions > 0 && c.pinstructions as f64 >= c.instructions as f64 * 0.5
                    })
                    .map(
                        |c| per(c.pcycles as f64 * c.instructions as f64 / c.pinstructions as f64),
                    );
                let pghz = counts
                    .filter(|c| c.pcpu_ns > 0.0)
                    .map(|c| c.pcycles as f64 / c.pcpu_ns);
                println!(
                    "keyframe-probe case={name} interactive={interactive} sample={sample} iterations={iterations} wall_ns={} cpu_ns={cpu_ns:?} instructions={instructions:?} cycles={cycles:?} pshare={pshare:?} pcycles={pcycles:?} pghz={pghz:?}",
                    per(wall_ns)
                );
            }
        }
    }
}
