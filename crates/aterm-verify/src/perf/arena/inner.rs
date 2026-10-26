// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-terminal `drain` and `vte` halves for the arena performance lanes.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Stdio, command};

use super::report;

pub(crate) fn main(args: &[String]) -> ExitCode {
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
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
mod tests {
    use super::*;

    #[test]
    fn the_benchmarks_wait_for_the_asked_grid() {
        let mut seen = ["17 49", "17 49", "40 120"].into_iter();
        let mut reads = 0;
        await_grid("40 120", GRID_HANG, || {
            reads += 1;
            Ok(seen.next().unwrap_or("24 80").to_string())
        })
        .expect("the third read is the asked grid");
        assert_eq!(reads, 3);
    }

    #[test]
    fn a_grid_never_taken_ends_the_trial_naming_the_grid_it_had() {
        let e = await_grid("40 120", Duration::ZERO, || Ok("17 49".to_string()))
            .expect_err("past the deadline");
        assert!(e.contains("'17 49'") && e.contains("'40 120'"), "{e}");
        assert!(e.ends_with("after 0s"), "the wait it was given: {e}");
        let e = await_grid("40 120", Duration::ZERO, || Err("no tty".to_string()))
            .expect_err("a failed read");
        assert_eq!(e, "no tty");
    }
}
