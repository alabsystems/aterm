// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Measures main-thread Metal shader and pipeline compilation time for the first frame.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    match run() {
        Ok(line) => {
            println!("{line}");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("first_frame_compile: could not run: {e}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(target_os = "macos")]
fn run() -> Result<String, String> {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    let warmed = std::env::var("FIRST_FRAME_BENCH").is_ok_and(|m| m == "warm");
    let prologue = Duration::from_millis(
        std::env::var("FIRST_FRAME_BENCH_PROLOGUE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
    );
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("clock: {e}"))?
        .as_nanos();
    let t0 = Instant::now();
    let warm = if warmed {
        let aurora = std::env::var("FIRST_FRAME_BENCH_AURORA").map_or(true, |v| v != "0");
        Some(std::thread::spawn(aterm_gpu::first_frame_bench_warm(
            nonce, aurora,
        )?))
    } else {
        None
    };
    if let Some(left) = prologue.checked_sub(t0.elapsed()) {
        std::thread::sleep(left);
    }
    let [waited, lib, rows, present, total] = aterm_gpu::first_frame_bench_main(nonce)?;
    let warm_ms = match warm {
        None => "-".to_owned(),
        Some(h) => {
            let took = h
                .join()
                .map_err(|_| "the warm thread panicked".to_owned())??;
            format!("{:.2}", took.as_secs_f64() * 1e3)
        }
    };
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    Ok(format!(
        "BENCH mode={} prologue_ms={} waited_ms={:.2} cell_lib_ms={:.2} frame_psos_ms={:.2} \
         present_ms={:.2} main_total_ms={:.2} aurora_ms={:.2} warm_ms={warm_ms}",
        if warmed { "warm" } else { "inline" },
        prologue.as_millis(),
        ms(waited),
        ms(lib),
        ms(rows - lib),
        ms(present - rows),
        ms(present),
        ms(total - present),
    ))
}

/// The Metal arm exists only on macOS.
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("first_frame_compile: the Metal arm is macOS's; nothing to measure here");
}
