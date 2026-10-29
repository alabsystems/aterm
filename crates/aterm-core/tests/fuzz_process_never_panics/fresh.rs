// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fresh-terminal LCG fuzz tests: single-batch escape soup, chunked escape soup,
//! tiny `1..=3` grids, and a known adversarial corpus.

use aterm_core::terminal::Terminal;

/// One step of a deterministic LCG (Knuth's MMIX constants, as in the rest of the fuzz).
#[inline]
fn next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u32
}

const FRESH_DEFAULT_SEED: u64 = 0x5eed_a7e2_f2e5_0001;
const FRESH_CASES: u32 = 400;

fn fresh_cases() -> u32 {
    let Ok(v) = std::env::var("ATERM_FRESH_FUZZ_CASES") else {
        return FRESH_CASES;
    };
    match v.trim().parse::<u32>() {
        Ok(n) => n.max(FRESH_CASES),
        Err(e) => {
            eprintln!(
                "ATERM_FRESH_FUZZ_CASES={v:?} is not a case count ({e}); running the default \
                 {FRESH_CASES}"
            );
            FRESH_CASES
        }
    }
}

fn fresh_run(test: &str, lane: u64) -> (u64, u32) {
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
        format!(" ATERM_FRESH_FUZZ_CASES={cases}")
    };
    eprintln!(
        "{name}: seed {seed:#018x}, {cases} cases; replay with \
         ATERM_FRESH_FUZZ_SEED={seed:#x}{deep} targo --unverified test -p aterm-core \
         --test {target} -- {name} --exact",
        target = env!("CARGO_CRATE_NAME"),
    );
    (seed, cases)
}

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

fn fresh_input(s: &mut u64, max_len: u32, byte: fn(&mut u64) -> u8) -> Vec<u8> {
    let len = next(s) % max_len;
    (0..len).map(|_| byte(s)).collect()
}

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
