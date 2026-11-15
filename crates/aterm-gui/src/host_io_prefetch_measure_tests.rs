// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual total-ingestion CPU/allocation measurement; no native latency or RSS claim.
//! Run the same ignored test in eager/deferred builds, preserving both test executables under
//! their Cargo names. Outer receipts bind declared source labels to build records and hashes.

use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Mutex;

use aterm_bench::measurement_clock::thread_cpu_ns;
use aterm_capacity::{Admit, Need, admit};
use aterm_core::terminal::kitty_graphics::take_kitty_payload_decodes_for_test;
use aterm_heap_count::allocations_in;

use super::*;

struct Case {
    name: &'static str,
    wire: Vec<u8>,
    split: Option<usize>,
    enabled: bool,
    image_bytes: Option<usize>,
    encoded_bytes: usize,
    eager_extra_decode: bool,
}

impl Case {
    fn check_decodes(&self, commands: usize, decodes: usize, bytes: usize) -> Result<(), String> {
        let expected = if self.image_bytes.is_some() {
            commands
        } else {
            0
        };
        let counts_match =
            decodes == expected || (self.eager_extra_decode && decodes == 2 * expected);
        if !counts_match || bytes != decodes * self.encoded_bytes {
            return Err(format!(
                "{}: incomplete decoder work ({decodes} calls, {bytes} bytes)",
                self.name
            ));
        }
        Ok(())
    }
}

struct Fixture {
    terminal: Mutex<Terminal>,
    prefetch: Prefetch,
}

impl Fixture {
    fn new(case: &Case) -> Self {
        let mut terminal = Terminal::new(4, 8);
        terminal.set_cell_pixel_size(10, 20);
        if case.enabled {
            terminal.set_kitty_file_resolver(take_kitty_medium);
        }
        Self {
            terminal: Mutex::new(terminal),
            prefetch: Prefetch::default(),
        }
    }

    fn feed(&mut self, case: &Case) {
        if let Some(split) = case.split {
            feed_as_reader_with(
                &mut self.prefetch,
                black_box(&self.terminal),
                black_box(&case.wire[..split]),
            );
            feed_as_reader_with(
                &mut self.prefetch,
                black_box(&self.terminal),
                black_box(&case.wire[split..]),
            );
        } else {
            feed_as_reader_with(
                &mut self.prefetch,
                black_box(&self.terminal),
                black_box(&case.wire),
            );
        }
    }

    fn check(&self, case: &Case, fed: usize) -> Result<(), String> {
        let mut terminal = crate::term_lock(&self.terminal);
        if terminal.host_io_ordinal() != 0 {
            return Err("inline fixture performed host I/O".into());
        }
        let commands = if case.image_bytes.is_some() { fed } else { 0 };
        if terminal.kitty_graphics_commands()
            != u64::try_from(commands).map_err(|_| "command count overflow")?
            || terminal.take_response().is_some()
        {
            return Err("fixture did not ingest the expected quiet commands".into());
        }
        let frame = terminal.cell_frame(4, 8);
        match case.image_bytes {
            Some(expected) => {
                let image = &frame.images[0]
                    .first()
                    .ok_or("image was not displayed")?
                    .1
                    .image;
                if image.bytes.len() != expected
                    || image.bytes.get(..4) != Some(&[17, 34, 51, 255][..])
                {
                    return Err("image payload changed during ingestion".into());
                }
            }
            None if !frame.images[0].is_empty() => {
                return Err("text fixture displayed an image".into());
            }
            None => {}
        }
        Ok(())
    }
}

fn image(width: usize, height: usize) -> Result<(Vec<u8>, usize), String> {
    let payload = [17, 34, 51, 255].repeat(width * height);
    let encoded = aterm_codec::base64::encode(&payload).map_err(|e| e.to_string())?;
    let wire = [
        format!("\x1b_Ga=T,f=32,s={width},v={height},i=7,p=1,q=2,C=1;").as_bytes(),
        encoded.as_bytes(),
        b"\x1b\\",
    ]
    .concat();
    Ok((wire, encoded.len()))
}

fn cases() -> Result<Vec<Case>, String> {
    let (small, small_encoded) = image(32, 16)?;
    let (large, large_encoded) = image(64, 64)?;
    if small.len() > 4096 || large.len() <= CARRY_MAX + 1 {
        return Err("fixture no longer spans both carry cases".into());
    }
    Ok(vec![
        Case {
            name: "small-enabled",
            wire: small.clone(),
            split: None,
            enabled: true,
            image_bytes: Some(32 * 16 * 4),
            encoded_bytes: small_encoded,
            eager_extra_decode: true,
        },
        Case {
            name: "small-split-enabled",
            wire: small.clone(),
            split: Some(small.len() / 2),
            enabled: true,
            image_bytes: Some(32 * 16 * 4),
            encoded_bytes: small_encoded,
            eager_extra_decode: true,
        },
        Case {
            name: "large-enabled",
            wire: large.clone(),
            split: None,
            enabled: true,
            image_bytes: Some(64 * 64 * 4),
            encoded_bytes: large_encoded,
            eager_extra_decode: true,
        },
        Case {
            name: "large-carry-control",
            wire: large,
            split: Some(CARRY_MAX + 1),
            enabled: true,
            image_bytes: Some(64 * 64 * 4),
            encoded_bytes: large_encoded,
            eager_extra_decode: false,
        },
        Case {
            name: "small-default-control",
            wire: small,
            split: None,
            enabled: false,
            image_bytes: Some(32 * 16 * 4),
            encoded_bytes: small_encoded,
            eager_extra_decode: false,
        },
        Case {
            name: "text-enabled-control",
            wire: b"\x1b[Hagent output without graphics".to_vec(),
            split: None,
            enabled: true,
            image_bytes: None,
            encoded_bytes: 0,
            eager_extra_decode: false,
        },
        Case {
            name: "text-default-control",
            wire: b"\x1b[Hagent output without graphics".to_vec(),
            split: None,
            enabled: false,
            image_bytes: None,
            encoded_bytes: 0,
            eager_extra_decode: false,
        },
    ])
}

fn number(name: &str, default: usize, maximum: usize) -> Result<usize, String> {
    let value = match std::env::var(name) {
        Ok(value) => value.parse::<usize>().map_err(|e| e.to_string())?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error.to_string()),
    };
    if value == 0 || value > maximum {
        return Err(format!("{name} must be 1..={maximum}"));
    }
    Ok(value)
}

/// One invocation is one arm-run, with correlated inner batches. An ABBA block consists of
/// four fresh invocations. Run a pilot block, then at least three paired ABBA blocks before
/// interpreting a difference. Admission samples readiness; it does not promise continuous
/// quiet. CSV is published only after every fixture and the final hold check pass.
#[test]
#[ignore = "manual paired total-ingestion CPU/allocation measurement"]
fn optional_prefetch_ingest_cost() -> Result<(), String> {
    let label = std::env::var("ATERM_KITTY_BENCH_LABEL").map_err(|e| e.to_string())?;
    if label.is_empty()
        || label.len() > 64
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err("ATERM_KITTY_BENCH_LABEL must be a short alphanumeric label".into());
    }
    let source = std::env::var("ATERM_KITTY_BENCH_SOURCE").map_err(|e| e.to_string())?;
    if source.len() != 40 || !source.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("ATERM_KITTY_BENCH_SOURCE must be the declared 40-digit source SHA".into());
    }
    let iterations = number("ATERM_KITTY_BENCH_ITERATIONS", 256, 4096)?;
    let batches = number("ATERM_KITTY_BENCH_BATCHES", 5, 16)?;
    let warmup = number("ATERM_KITTY_BENCH_WARMUP", 32, 1024)?;
    let cases = cases()?;
    let mut fixtures = Vec::new();
    for _ in 0..batches {
        let mut batch = Vec::new();
        for case in &cases {
            let mut fixture = Fixture::new(case);
            for _ in 0..warmup {
                fixture.feed(case);
            }
            fixture.check(case, warmup)?;
            batch.push(fixture);
        }
        fixtures.push(batch);
    }
    let mut csv = String::from(
        "record,label,declared_source,case,batch,iterations,warmup,input_bytes,split_at,prefetch_enabled,thread_cpu_ns,allocation_calls,decode_calls,decode_encoded_bytes\n",
    );
    let mut claim = admit(
        Need::timing("kitty-prefetch-ingest")
            .cores(1)
            .mem(128 * 1024 * 1024),
        Admit::Wait,
    )
    .map_err(|e| e.to_string())?;
    let entry = claim
        .measurement_entry()
        .map_err(|e| format!("measurement entry refused: {e:?}"))?;
    eprintln!("kitty-prefetch-ingest: sampled entry {entry:?}; not continuously quiet");
    for (batch, fixtures) in fixtures.iter_mut().enumerate() {
        for offset in 0..cases.len() {
            let index = (batch + offset) % cases.len();
            let case = &cases[index];
            let fixture = &mut fixtures[index];
            take_kitty_payload_decodes_for_test();
            let before = thread_cpu_ns().ok_or("thread CPU clock unavailable")?;
            let ((), allocations) = allocations_in(|| {
                for _ in 0..iterations {
                    fixture.feed(case);
                }
            });
            let after = thread_cpu_ns().ok_or("thread CPU clock unavailable")?;
            let cpu = after
                .checked_sub(before)
                .ok_or("thread CPU clock regressed")?;
            let (decodes, encoded_bytes) = take_kitty_payload_decodes_for_test();
            case.check_decodes(iterations, decodes, encoded_bytes)?;
            fixture.check(case, warmup + iterations)?;
            writeln!(csv, "kitty-prefetch-ingest,{label},{source},{},{batch},{iterations},{warmup},{},{},{},{cpu},{allocations},{decodes},{encoded_bytes}", case.name, case.wire.len(), case.split.unwrap_or(0), case.enabled).map_err(|e| e.to_string())?;
        }
    }
    let admitted = claim
        .timing_admission()
        .map_err(|e| format!("final timing hold refused: {e:?}"))?;
    eprintln!("kitty-prefetch-ingest: final admission {admitted:?}; allocation calls are not RSS");
    print!("{csv}");
    Ok(())
}
