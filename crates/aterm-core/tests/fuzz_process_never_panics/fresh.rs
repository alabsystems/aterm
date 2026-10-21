// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fresh-terminal batches, which the long-lived terminals of `fuzz_process_never_panics`
//! never reach: a fresh 24x80 terminal taking one multi-KiB `process()` call, the C1
//! introducers 0x9b/0x90/0x9c in an escape-weighted soup, 1..=3 x 1..=3 grids, and reads
//! back through `visible_content()`. Each run draws a fresh seed, printed with the command
//! that replays it; a failing case prints its index and input length.
//!
//! The long-lived fuzz terminals of `fuzz_process_never_panics` never see a
//! fresh 24x80 terminal take one multi-KiB `process()` call, never feed the C1
//! introducers 0x9b/0x90/0x9c inside an escape-weighted soup, never build a
//! 1..=3 x 1..=3 grid, and never read back through `visible_content()`. These
//! LCG loops cover those angles (400 cases each). Each run prints its seed
//! (fixed by default, `ATERM_FRESH_FUZZ_SEED` to pick another) with the
//! command that replays it; a failing case prints its index
//! and input length. `PROPTEST_CASES` raises the case count. Every soup run
//! carries dozens of `ESC P [0-9;]* q` sixel hooks, which reach the decoder
//! and image placement.

use aterm_core::terminal::Terminal;

/// One step of a deterministic LCG (Knuth's MMIX constants, as in the rest of the fuzz).
#[inline]
fn next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u32
}

/// The default seed: fixed, so the suite is deterministic. A deep run picks
/// its own with `ATERM_FRESH_FUZZ_SEED`.
const FRESH_DEFAULT_SEED: u64 = 0x5eed_a7e2_f2e5_0001;

/// Cases per fresh-terminal property by default.
const FRESH_CASES: u32 = 400;

/// This run's case count per fresh-terminal property: [`FRESH_CASES`], raised by
/// `PROPTEST_CASES` for a deep run but never lowered, so a value exported to shorten some
/// other suite cannot cut these. A value that is not a count is reported and ignored.
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
/// decimal) when set, else [`FRESH_DEFAULT_SEED`] mixed with `lane` (so the
/// four properties differ), the same on every run so a commit turns red only
/// on its own change. Both are printed with the exact
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
            let mut s = FRESH_DEFAULT_SEED ^ lane;
            (u64::from(next(&mut s)) << 32) | u64::from(next(&mut s))
        }
    };
    let deep = if cases == FRESH_CASES {
        String::new()
    } else {
        format!(" PROPTEST_CASES={cases}")
    };
    eprintln!(
        "{name}: seed {seed:#018x}, {cases} cases; replay with \
         ATERM_FRESH_FUZZ_SEED={seed:#x}{deep} targo --unverified test -p aterm-core \
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

/// A byte biased toward escape-sequence machinery, weighted 2 ESC, 2 introducer/param
/// punctuation, 2 digits, 2 final bytes, 1 C0/C1 control (incl. 0x9b CSI, 0x90 DCS,
/// 0x9c ST), 1 arbitrary byte.
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

/// A corpus of hand-picked nasties that must each be survived.
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
}
