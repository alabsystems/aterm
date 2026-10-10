// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fresh-terminal batches.
//!
//! The long-lived fuzz terminals of `fuzz_process_never_panics` never see a
//! fresh 24x80 terminal take one multi-KiB `process()` call, never feed the C1
//! introducers 0x9b/0x90/0x9c inside an escape-weighted soup, never build a
//! 1..=3 x 1..=3 grid, and never read back through `visible_content()`. These
//! LCG loops cover those angles (400 cases each). Each run draws a fresh seed,
//! printed with the command that replays it; a failing case prints its index
//! and input length. `PROPTEST_CASES` raises the case count.
//!
//! Two binaries mount this file, one per engine build, because every soup run
//! carries dozens of `ESC P [0-9;]* q` sixel hooks and the two builds route
//! them differently:
//! - aterm-core's `fuzz_process_never_panics` (as `mod fresh`) runs against
//!   whatever aterm-core its invocation builds: WITHOUT `sixel` under a plain
//!   `-p aterm-core` (the library default and the Linux lane's
//!   `--no-default-features` build: the hooks take the consume-only
//!   `DcsType::Unknown` path), with it under `--features sixel` or the
//!   `--workspace` gate, where feature unification turns it on.
//! - aterm-conformance's `vt` suite (as `mod fresh_terminal`, by `#[path]`)
//!   always runs against aterm-core[sixel], that crate's dependency: there
//!   the hooks reach the decoder and image placement on every run,
//!   `-p aterm-conformance` alone included.
//!
//! `known_adversarial_corpus_is_survived` pins each binary's build. A `sixel`
//! self dev-dependency cannot be used instead: cargo cannot turn a
//! dev-dependency's feature off, so no test could run the no-sixel engine.

use aterm_core::terminal::Terminal;

/// One step of a deterministic LCG (the constants of the rest of the fuzz).
#[inline]
fn next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u32
}

/// The package this file is compiled into: aterm-core (its own fuzz binary)
/// or aterm-conformance (the `vt` suite, which mounts it).
const HOST: &str = env!("CARGO_PKG_NAME");

/// A well-formed 4x6 two-colour sixel image, one cell at the default 8x16
/// metric (aterm-conformance's `SIXEL_4X6`).
const SIXEL_4X6: &[u8] = b"\x1bP0;0;8q\"1;1;4;6#0;2;0;0;0#1;2;100;0;0#1~~~~$-\x1b\\";

/// Whether the engine this binary links decodes sixel: DA1 advertises code 4
/// exactly when aterm-core is built with `sixel`. Read from the engine rather
/// than from a `cfg`, since the `vt` suite's own crate has no `sixel` feature.
fn engine_has_sixel() -> bool {
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[c");
    let reply = t.take_response().unwrap_or_default();
    let params = reply
        .strip_prefix(b"\x1b[?")
        .and_then(|r| r.strip_suffix(b"c"))
        .unwrap_or_default();
    params.split(|&b| b == b';').skip(1).any(|p| p == b"4")
}

/// Cases per fresh-terminal property by default (the proptest config's
/// `cases: 400`).
const FRESH_CASES: u32 = 400;

/// This run's case count per fresh-terminal property: [`FRESH_CASES`], raised
/// by `PROPTEST_CASES` for a deep run. Never lowered: a `PROPTEST_CASES`
/// exported to shorten some other suite cannot cut these below the depth they
/// had as proptest properties. A value that is not a count is reported and
/// ignored, as proptest treated it.
fn fresh_cases() -> u32 {
    let Ok(v) = std::env::var("PROPTEST_CASES") else {
        return FRESH_CASES;
    };
    match v.trim().parse::<u32>() {
        Ok(n) => n.max(FRESH_CASES),
        Err(e) => {
            eprintln!(
                "PROPTEST_CASES={v:?} is not a case count ({e}); running the default \
                 {FRESH_CASES}"
            );
            FRESH_CASES
        }
    }
}

/// This run's seed and case count for the fresh-terminal property `test` (its
/// function name). The seed is `ATERM_FRESH_FUZZ_SEED` (hex with `0x`, or
/// decimal) when set, else drawn from the clock, the pid and `lane` (so the
/// four properties differ within a run). Both are printed with the exact
/// replay command for this binary, which selects the test by libtest's name
/// with `--exact` — so that name must be the running test's, or the replay
/// runs 0 tests and exits 0, and a failing seed looks fixed.
fn fresh_run(test: &str, lane: u64) -> (u64, u32) {
    // libtest names a test by its path below the crate root, and names the
    // test's thread after it (the main thread runs it only when the harness
    // cannot spawn one).
    let name = match module_path!().split_once("::") {
        Some((_, module)) => format!("{module}::{test}"),
        None => test.to_string(),
    };
    if let Some(running) = std::thread::current().name()
        && running != "main"
    {
        assert_eq!(
            running, name,
            "the replay command would select no test: pass this test's function name"
        );
    }
    let cases = fresh_cases();
    let seed = match std::env::var("ATERM_FRESH_FUZZ_SEED") {
        Ok(v) => {
            let v = v.trim();
            v.strip_prefix("0x")
                .map_or_else(|| v.parse(), |h| u64::from_str_radix(h, 16))
                .unwrap_or_else(|e| panic!("ATERM_FRESH_FUZZ_SEED={v:?}: {e}"))
        }
        Err(_) => {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            let mut s = nanos ^ (u64::from(std::process::id()) << 32) ^ lane;
            (u64::from(next(&mut s)) << 32) | u64::from(next(&mut s))
        }
    };
    let deep = if cases == FRESH_CASES {
        String::new()
    } else {
        format!(" PROPTEST_CASES={cases}")
    };
    let sixel = engine_has_sixel();
    // aterm-core's build has `sixel` only when asked for; aterm-conformance's
    // always does, and has no such feature to name.
    let features = if sixel && HOST == "aterm-core" {
        " --features sixel"
    } else {
        ""
    };
    eprintln!(
        "{name}: seed {seed:#018x}, {cases} cases, sixel={sixel}; replay with \
         ATERM_FRESH_FUZZ_SEED={seed:#x}{deep} targo --unverified test -p {HOST}{features} \
         --test {target} -- {name} --exact",
        target = env!("CARGO_CRATE_NAME"),
    );
    (seed, cases)
}

/// Names the failing case while a fresh-terminal property unwinds: its index
/// in this seed's stream and its input length. Silent for a passing case.
struct FailingCase<'a> {
    test: &'a str,
    case: u32,
    len: usize,
}

impl Drop for FailingCase<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "{}: FAILED at case {} of this seed, on a {}-byte input",
                self.test, self.case, self.len
            );
        }
    }
}

/// A byte biased toward escape-sequence machinery, with robustness.rs's
/// weights: 2 ESC, 2 introducer/param punctuation, 2 digits, 2 final bytes,
/// 1 C0/C1 control (incl. 0x9b CSI, 0x90 DCS, 0x9c ST), 1 arbitrary byte.
fn escapey_byte(s: &mut u64) -> u8 {
    fn pick(s: &mut u64, from: &[u8]) -> u8 {
        from[(next(s) as usize) % from.len()]
    }
    match next(s) % 10 {
        0 | 1 => 0x1b,
        2 | 3 => pick(s, b"[]P;:?"),
        4 | 5 => pick(s, b"0123456789"),
        6 | 7 => pick(s, b"mHJKABCDfdghlnpqrstu"),
        8 => pick(s, &[0x07, 0x08, 0x09, 0x0a, 0x0d, 0x9b, 0x90, 0x9c]),
        _ => (next(s) & 0xFF) as u8,
    }
}

/// `len` in `0..max_len`, filled by `byte`.
fn fresh_input(s: &mut u64, max_len: u32, byte: fn(&mut u64) -> u8) -> Vec<u8> {
    let len = next(s) % max_len;
    (0..len).map(|_| byte(s)).collect()
}

fn uniform_byte(s: &mut u64) -> u8 {
    (next(s) & 0xFF) as u8
}

/// No panic on up to 4 KiB of uniform bytes in one `process()` call on a fresh
/// 24x80 terminal, and reading the model back must not panic either.
#[test]
fn fresh_terminal_single_batch_arbitrary_bytes_never_panics() {
    const TEST: &str = "fresh_terminal_single_batch_arbitrary_bytes_never_panics";
    let (mut s, cases) = fresh_run(TEST, 1);
    for case in 0..cases {
        let input = fresh_input(&mut s, 4096, uniform_byte);
        let _failing = FailingCase {
            test: TEST,
            case,
            len: input.len(),
        };
        let mut t = Terminal::new(24, 80);
        t.process(&input);
        let _ = t.visible_content();
    }
}

/// No panic on up to 4 KiB of escape-dense soup in one `process()` call.
#[test]
fn fresh_terminal_single_batch_escape_soup_never_panics() {
    const TEST: &str = "fresh_terminal_single_batch_escape_soup_never_panics";
    let (mut s, cases) = fresh_run(TEST, 2);
    for case in 0..cases {
        let input = fresh_input(&mut s, 4096, escapey_byte);
        let _failing = FailingCase {
            test: TEST,
            case,
            len: input.len(),
        };
        let mut t = Terminal::new(24, 80);
        t.process(&input);
        let _ = t.visible_content();
    }
}

/// Up to 2 KiB of soup split across `process()` calls at up to 64 random
/// 1..64-byte boundaries (then 7-byte chunks): a sequence straddling a chunk
/// boundary is a classic parser bug class.
#[test]
fn fresh_terminal_chunked_escape_soup_never_panics() {
    const TEST: &str = "fresh_terminal_chunked_escape_soup_never_panics";
    let (mut s, cases) = fresh_run(TEST, 3);
    for case in 0..cases {
        let input = fresh_input(&mut s, 2048, escapey_byte);
        let _failing = FailingCase {
            test: TEST,
            case,
            len: input.len(),
        };
        let nsplits = next(&mut s) % 64;
        let splits: Vec<usize> = (0..nsplits)
            .map(|_| 1 + (next(&mut s) % 63) as usize)
            .collect();
        let mut t = Terminal::new(24, 80);
        let mut rest = &input[..];
        let mut si = 0;
        while !rest.is_empty() {
            let n = splits.get(si).copied().unwrap_or(7).min(rest.len());
            let (head, tail) = rest.split_at(n);
            t.process(head);
            rest = tail;
            si += 1;
        }
        let _ = t.visible_content();
    }
}

/// Fresh 1..=3 x 1..=3 grids fed up to 1 KiB of soup: boundary arithmetic on
/// cursor and scroll with almost no room.
#[test]
fn fresh_tiny_grid_escape_soup_never_panics() {
    const TEST: &str = "fresh_tiny_grid_escape_soup_never_panics";
    let (mut s, cases) = fresh_run(TEST, 4);
    for case in 0..cases {
        let input = fresh_input(&mut s, 1024, escapey_byte);
        let _failing = FailingCase {
            test: TEST,
            case,
            len: input.len(),
        };
        let rows = 1 + (next(&mut s) % 3) as u16;
        let cols = 1 + (next(&mut s) % 3) as u16;
        let mut t = Terminal::new(rows, cols);
        t.process(&input);
        let _ = t.visible_content();
    }
}

/// A corpus of hand-picked nasties that must each be survived, and the sixel
/// build this binary's soups ran against.
#[test]
fn known_adversarial_corpus_is_survived() {
    let corpus: &[&[u8]] = &[
        b"\x1b[",                      // incomplete CSI
        b"\x1b[999999999999999999m",   // overflowing param
        b"\x1b[;;;;;;;;;;;;;;;;;;;;m", // many empty params
        b"\x1b]0;",                    // unterminated OSC
        b"\x1b]8;;\x07",               // empty hyperlink
        b"\x1bP+q\x1b\\",              // DCS
        b"\xff\xfe\xfd",               // invalid UTF-8
        b"\xf0\x28\x8c\x28",           // invalid UTF-8 sequence
        b"\x1b[1;1H\x1b[999;999H",     // out-of-range cursor moves
        b"\x1b[r\x1b[?1049h\x1b[2J",   // reset region + alt + clear
        b"\x08\x08\x08\x08",           // backspaces at col 0
        b"\x1b[100S\x1b[100T",         // huge scroll up/down
    ];
    for bytes in corpus {
        let mut t = Terminal::new(24, 80);
        t.process(bytes);
        let _ = t.visible_content();
    }

    // The engine's two sixel builds, each held to its own contract: with
    // `sixel` (DA1 advertises code 4) a well-formed image is decoded and
    // placed; without it the DCS is only consumed and nothing is placed.
    // Neither build may leak the payload onto the glyph grid.
    let sixel = engine_has_sixel();
    let mut t = Terminal::new(24, 80);
    t.process(SIXEL_4X6);
    assert_eq!(
        t.images_row(0).len(),
        usize::from(sixel),
        "a well-formed sixel image must be placed exactly when the engine advertises sixel \
         (DA1 code 4: {sixel})"
    );
    assert_eq!(
        t.visible_content().trim(),
        "",
        "sixel payload bytes must not leak as glyphs (sixel={sixel})"
    );
    // The vt suite mounts this file to run the soups against aterm-core[sixel]
    // (its dependency) on every run; a copy that lost it would pass silently.
    if HOST != "aterm-core" {
        assert!(
            sixel,
            "{HOST} builds aterm-core with `sixel`, so the mounted soups must reach the \
             decoder, not the consume-only DCS path"
        );
    }
}
