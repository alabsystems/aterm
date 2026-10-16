// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The in-terminal halves of the arena lanes: `aterm-verify --arena-inner <verb> …`,
//! the program each terminal under test runs with `-e`. Everything arrives as
//! argv (environment does not survive `open -na`, which is how ghostty is
//! launched), and each half answers on the runner's report socket — the
//! runner waits on that report, never on a file.
//!
//! * `drain <corpus> <report-sock>`: stamp, `cat` the corpus to the terminal,
//!   report `drain <seconds>`.
//! * `vte <vtebench> <benchmarks> <prefix> <max-secs> <report-sock>`: read the
//!   grid the terminal gave (`stty size`), run vtebench into `<prefix>.dat`,
//!   report `vte <rows> <cols>`.

use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use aterm_uds::spawnfd::{Stdio, command};

use super::report;

pub(crate) fn main(args: &[String]) -> ExitCode {
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let (sock, outcome) = match argv.as_slice() {
        ["drain", corpus, sock] => (*sock, drain(Path::new(corpus))),
        ["vte", vtebench, benchmarks, prefix, max_secs, sock] => (
            *sock,
            vte(
                Path::new(vtebench),
                Path::new(benchmarks),
                Path::new(prefix),
                max_secs,
            ),
        ),
        _ => {
            eprintln!(
                "usage: aterm-verify --arena-inner drain <corpus> <report-sock>\n\
                 \x20      aterm-verify --arena-inner vte <vtebench> <benchmarks> <prefix> <max-secs> <report-sock>\n\
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

/// Run vtebench in this terminal; report the grid it ran at.
fn vte(
    vtebench: &Path,
    benchmarks: &Path,
    prefix: &Path,
    max_secs: &str,
) -> Result<String, String> {
    let size = command("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .output()
        .map_err(|e| format!("could not run stty: {e}"))?;
    let grid = String::from_utf8_lossy(&size.stdout).trim().to_string();
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
    // vtebench writes its --dat in one pass at the very end, not atomically.
    std::fs::rename(&part, prefix.with_extension("dat"))
        .map_err(|e| format!("could not place the .dat: {e}"))?;
    Ok(format!("vte {grid}"))
}
