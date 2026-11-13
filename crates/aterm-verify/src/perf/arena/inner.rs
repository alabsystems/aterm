// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-terminal `drain` and `vte` halves for the arena performance lanes, and the `vte-engine`
//! lane's payload capture child ([`crate::perf::vte_engine::capture_main`]).

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crate::exec::check_command as command;
use aterm_uds::spawnfd::Stdio;

use super::report;

pub(crate) fn main(args: &[String]) -> ExitCode {
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Some((verb, rest)) = argv.split_first()
        && *verb == crate::perf::vte_engine::CAPTURE_VERB
    {
        return crate::perf::vte_engine::capture_main(rest);
    }
    let (sock, outcome) = match argv.as_slice() {
        ["drain", corpus, sock] => (*sock, drain(Path::new(corpus))),
        [
            "vte",
            vtebench,
            benchmarks,
            prefix,
            max_secs,
            rows,
            cols,
            sock,
        ] => (
            *sock,
            vte(
                Path::new(vtebench),
                Path::new(benchmarks),
                Path::new(prefix),
                max_secs,
                &format!("{rows} {cols}"),
            ),
        ),
        _ => {
            eprintln!(
                "usage: aterm-verify --arena-inner drain <corpus> <report-sock>\n\
                 \x20      aterm-verify --arena-inner vte <vtebench> <benchmarks> <prefix> <max-secs> <rows> <cols> <report-sock>\n\
                 (the in-terminal half the arena lanes launch; not for direct use)"
            );
            return ExitCode::from(2);
        }
    };
    let line = match &outcome {
        Ok(line) => format!("{line}\n"),
        Err(e) => format!("fail {e}\n"),
    };
    match report(Path::new(sock), &line) {
        Ok(()) if outcome.is_ok() => ExitCode::SUCCESS,
        Ok(()) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("arena-inner: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `cat` the corpus to this terminal and time it from here, after startup.
fn drain(corpus: &Path) -> Result<String, String> {
    let started = Instant::now();
    let status = command("/bin/cat")
        .arg(corpus)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("could not run cat: {e}"))?;
    let secs = started.elapsed().as_secs_f64();
    if !status.success() {
        return Err(format!("cat {} exited {status}", corpus.display()));
    }
    Ok(format!("drain {secs:.6}"))
}

/// This terminal's grid, `rows cols`, as `stty size` reads it.
fn grid() -> Result<String, String> {
    let size = command(aterm_uds::os_path!("stty"))
        .arg("size")
        .stdin(Stdio::inherit())
        .output()
        .map_err(|e| format!("could not run stty: {e}"))?;
    Ok(String::from_utf8_lossy(&size.stdout).trim().to_string())
}

/// How long a terminal has to take the grid it was asked for: a hang detector, not a budget.
const GRID_HANG: Duration = Duration::from_secs(60);

/// Wait until `read` says the grid is `want` (`rows cols`), reading it every 20 ms for up to
/// `hang`. A terminal can start its command before its window takes the size it was asked
/// for (ghostty's first vte trial on 2026-10-25 read `17 49` for `40 120`), and a benchmark
/// started then runs at the wrong grid.
fn await_grid(
    want: &str,
    hang: Duration,
    mut read: impl FnMut() -> Result<String, String>,
) -> Result<(), String> {
    let deadline = Instant::now() + hang;
    loop {
        let grid = read()?;
        if grid == want {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the terminal never took the asked '{want}' grid (rows cols): it was still \
                 '{grid}' after {}s",
                hang.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Run vtebench in this terminal once it has the asked grid (`want`, `rows cols`), reporting the
/// grid it had when the benchmarks ended.
fn vte(
    vtebench: &Path,
    benchmarks: &Path,
    prefix: &Path,
    max_secs: &str,
    want: &str,
) -> Result<String, String> {
    await_grid(want, GRID_HANG, grid)?;
    let part = prefix.with_extension("part");
    let status = command(vtebench)
        .args(["--silent", "--benchmarks"])
        .arg(benchmarks)
        .args(["--max-secs", max_secs, "--dat"])
        .arg(&part)
        .status()
        .map_err(|e| format!("could not run vtebench: {e}"))?;
    if !status.success() {
        return Err(format!("vtebench exited {status}"));
    }
    std::fs::rename(&part, prefix.with_extension("dat"))
        .map_err(|e| format!("could not place the .dat: {e}"))?;
    Ok(format!("vte {}", grid()?))
}

#[cfg(test)]
#[path = "inner_tests.rs"]
mod tests;
