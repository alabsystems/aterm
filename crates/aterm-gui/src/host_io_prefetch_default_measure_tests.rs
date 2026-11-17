// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Default PTY reader costs on a fresh thread per case. Enabled prefetch in another case
//! must not initialize this reader's state before its cold allocation is counted.

use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Mutex;

use aterm_bench::measurement_clock::thread_cpu_ns;
use aterm_capacity::{Admit, Need, admit};
use aterm_heap_count::allocations_in;

use super::*;

struct Case {
    name: &'static str,
    bytes: Vec<u8>,
    prefix: &'static str,
}

fn cases() -> Vec<Case> {
    let mut bulk = b"\x1b[H".to_vec();
    for _ in 0..16 {
        bulk.extend_from_slice(
            b"agent output: checking the next file, preserving terminal state\r\n",
        );
    }
    vec![
        Case {
            name: "token",
            bytes: b"\x1b[Htok".to_vec(),
            prefix: "tok",
        },
        Case {
            name: "status",
            bytes: b"\x1b[H\x1b[2K\x1b[32mRead\x1b[0m crates/aterm-gui/src/spawn.rs".to_vec(),
            prefix: "Read",
        },
        Case {
            name: "bulk",
            bytes: bulk,
            prefix: "agent output:",
        },
    ]
}

fn check(terminal: &Mutex<Terminal>, prefix: &str) -> Result<(), String> {
    let mut terminal = crate::term_lock(terminal);
    if Flags::of(&terminal).any()
        || terminal.rows() != 32
        || terminal.cols() != 110
        || terminal.host_io_ordinal() != 0
        || terminal.kitty_graphics_commands() != 0
        || terminal.take_response().is_some()
        || !terminal
            .row_text(0)
            .is_some_and(|text| text.starts_with(prefix))
    {
        return Err("default-reader fixture changed its text, admission or host I/O".into());
    }
    Ok(())
}

fn feed(prefetch: &mut Prefetch, terminal: &Mutex<Terminal>, bytes: &[u8], reader: bool) {
    if reader {
        feed_as_reader_with(prefetch, black_box(terminal), black_box(bytes));
    } else {
        crate::term_lock(black_box(terminal)).process(black_box(bytes));
    }
}

fn measure_case(
    case: Case,
    reader: bool,
    iterations: usize,
    batches: usize,
    warmup: usize,
) -> Result<String, String> {
    let terminal = Mutex::new(Terminal::new(32, 110));
    let mut prefetch = Prefetch::default();
    if Flags::of(&crate::term_lock(&terminal)).any() {
        return Err("default terminal unexpectedly admits prefetch".into());
    }
    let cold_allocations = if reader {
        allocations_in(|| {
            let fetched = prefetch.reads(&terminal, &case.bytes);
            assert_eq!(fetched.covered(), case.bytes.len());
            drop(fetched);
        })
        .1
    } else {
        0
    };
    for _ in 0..warmup {
        feed(&mut prefetch, &terminal, &case.bytes, reader);
    }
    check(&terminal, case.prefix)?;
    let mut rows = String::new();
    for batch in 0..batches {
        let before = thread_cpu_ns().ok_or("thread CPU clock unavailable")?;
        let ((), allocations) = allocations_in(|| {
            for _ in 0..iterations {
                feed(&mut prefetch, &terminal, &case.bytes, reader);
            }
        });
        let after = thread_cpu_ns().ok_or("thread CPU clock unavailable")?;
        let cpu = after
            .checked_sub(before)
            .ok_or("thread CPU clock regressed")?;
        check(&terminal, case.prefix)?;
        writeln!(
            rows,
            "{},{reader},{batch},{iterations},{warmup},{},{cold_allocations},{allocations},{cpu},{},{}",
            case.name,
            case.bytes.len(),
            size_of::<RefCell<Fetched>>(),
            size_of::<std::cell::OnceCell<Box<RefCell<Fetched>>>>(),
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(rows)
}

fn number(name: &str, default: usize, maximum: usize) -> Result<usize, String> {
    let value = match std::env::var(name) {
        Ok(value) => value.parse::<usize>().map_err(|error| error.to_string())?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error.to_string()),
    };
    if value == 0 || value > maximum {
        return Err(format!("{name} must be 1..={maximum}"));
    }
    Ok(value)
}

fn fresh_thread<T: Send + 'static>(
    run: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = run();
        let _ = send.send(result);
    });
    let result = receive
        .recv_timeout(std::time::Duration::from_secs(60))
        .map_err(|error| format!("default-reader thread hang detector: {error}"))?;
    worker
        .join()
        .map_err(|_| "default-reader measurement thread panicked")?;
    result
}

/// Heap work needs no quiet-machine claim and supplies no CPU, latency or RSS evidence.
#[test]
#[ignore = "manual fresh-thread default-reader allocation measurement; no timing claim"]
fn default_reader_cold_allocations() -> Result<(), String> {
    println!(
        "case,first_prefetch_allocations,empty_callback_allocations,repeated_prefetch_allocations,fetched_heap_bytes,lazy_tls_slot_bytes"
    );
    for case in cases() {
        let row = fresh_thread(move || {
            let terminal = Mutex::new(Terminal::new(32, 110));
            let mut prefetch = Prefetch::default();
            if Flags::of(&crate::term_lock(&terminal)).any() {
                return Err("default terminal unexpectedly admits prefetch".into());
            }
            let ((), first) = allocations_in(|| {
                let fetched = prefetch.reads(&terminal, &case.bytes);
                assert_eq!(fetched.covered(), case.bytes.len());
                drop(fetched);
            });
            let ((), callbacks) = allocations_in(|| {
                assert!(take_clipboard_query().is_none());
                assert!(take_kitty_medium(KittyMedium::TempFile, "/not-requested").is_none());
            });
            let ((), repeated) = allocations_in(|| {
                for _ in 0..1024 {
                    let fetched = prefetch.reads(&terminal, &case.bytes);
                    assert_eq!(fetched.covered(), case.bytes.len());
                    drop(fetched);
                }
            });
            Ok(format!(
                "{},{first},{callbacks},{repeated},{},{}",
                case.name,
                size_of::<RefCell<Fetched>>(),
                size_of::<std::cell::OnceCell<Box<RefCell<Fetched>>>>(),
            ))
        })?;
        println!("{row}");
    }
    Ok(())
}

/// Each process supplies one arm of a pilot or one of three ABBA blocks. Each case runs on
/// its own fresh thread: neither enabled prefetch nor a prior case can initialize its TLS.
/// Batches in a process are correlated. Compare fresh-process aggregates and retain all
/// engine-only controls. The normal Timing claim must pass at entry and at completion.
#[test]
#[ignore = "manual paired default-reader allocation and total-ingestion CPU measurement"]
fn default_reader_cost() -> Result<(), String> {
    let iterations = number("ATERM_PREFETCH_BENCH_ITERATIONS", 4096, 1 << 20)?;
    let batches = number("ATERM_PREFETCH_BENCH_BATCHES", 5, 16)?;
    let warmup = number("ATERM_PREFETCH_BENCH_WARMUP", 32, 1024)?;
    let mut claim = admit(
        Need::timing("default-pty-prefetch")
            .cores(1)
            .mem(128 * 1024 * 1024),
        Admit::Wait,
    )
    .map_err(|error| error.to_string())?;
    let entry = claim
        .measurement_entry()
        .map_err(|error| format!("measurement entry refused: {error:?}"))?;
    eprintln!("default-pty-prefetch: entry {entry:?}; not continuously quiet");
    let mut output = String::from(
        "case,reader,batch,iterations,warmup,input_bytes,cold_prefetch_allocations,ingest_allocations,thread_cpu_ns,fetched_heap_bytes,lazy_tls_slot_bytes\n",
    );
    for reader in [true, false] {
        for case in cases() {
            let rows =
                fresh_thread(move || measure_case(case, reader, iterations, batches, warmup))?;
            output.push_str(&rows);
        }
    }
    let final_hold = claim
        .timing_admission()
        .map_err(|error| format!("final timing hold refused: {error:?}"))?;
    eprintln!("default-pty-prefetch: final {final_hold:?}; allocations are not RSS");
    print!("{output}");
    Ok(())
}
