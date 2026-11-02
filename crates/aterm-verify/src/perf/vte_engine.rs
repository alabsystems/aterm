// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `vte-engine` floor lane: the pinned vtebench's default suite and the arena's two d1 `cat`
//! corpora replayed into the engine (`Terminal::process`) at 120x40, with no PTY, GUI or window,
//! by `aterm-bench`'s `vte_engine_harness`. It is the engine-level half of ARENA-VTE and
//! ARENA-CAT (`docs/FASTER_THAN_GHOSTTY_PLAN.md` §2): the number a throughput change is judged
//! by before the windowed head-to-head.
//!
//! The payloads are what vtebench writes, not a reconstruction of it. The lane runs each pinned
//! benchmark's `setup` and `benchmark` script as vtebench's loader does (stdin `/dev/null`,
//! stdout and stderr piped, the caller's controlling terminal inherited) from a child whose
//! controlling terminal is a fresh PTY at the grid, as vtebench's is when it runs inside a
//! terminal; the scripts are the arena's copy, stock but for the tty lookup the `vte` lane trims
//! ([`super::arena::vte::trimmed_benchmarks`]). Where the `vte` lane has built the pinned
//! vtebench, the same child runs it once and the lane holds every captured payload to the bytes
//! vtebench itself wrote ([`cross_check`]).
//!
//! What that capture shows on macOS: the scripts' `tput cols` and `tput lines` read the size of
//! a terminal on their stdout or stderr, and vtebench pipes both, so they read xterm-256color's
//! terminfo default, 80x24, whatever the grid. Every vtebench payload, in the windowed `vte`
//! lane too, is the 80x24 one (`\e[1;23r` for the bottom region) played into a 120x40 grid.
//!
//! The lane takes no quiet claim: it runs at the load it finds and reports that load beside its
//! numbers. On a shared desk, load moves both clocks a sample can be timed on: the wall clock by
//! descheduling, and the thread's CPU clock by the performance cores' falling clock and by spells
//! on an efficiency core. So what it judges is cycles: per scenario, the best sample's MiB per
//! 10^9 performance-core cycles, counted by the kernel per core type (the harness's
//! `Cost::pcore_cycles`), against the floors' catastrophic-only [`super::floors::PASS_RATIO`]
//! and the same-box trend. Its ms per MiB, on both clocks, are reported and never judged. Where
//! the kernel keeps no such counters (another OS), or a scenario had no sample counted on
//! performance cores, the lane reports what it measured and judges nothing.

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_capacity::Machine as _;
use aterm_uds::spawnfd::{Command, Stdio, command};

use super::LaneRun;
use super::arena::cat::{CORPUS_MB, Corpus, corpus_file, inner_argv};
use super::arena::vte::{benchmark_dirs, pinned_checkout, trimmed_benchmarks};
use super::arena::{Halt, Scratch, arena_dir, pins};
use super::floors::{FloorLane, judge_keyed, measure_example_with, number};

/// The grid the payloads are captured at and replayed into: the `vte` lane's default.
const ROWS: u16 = 40;
const COLS: u16 = 120;

/// The terminal type aterm gives its shells (aterm-pty), which the scripts' `tput` reads.
const TERM: &str = "xterm-256color";

/// vtebench's `--min-bytes` default: a benchmark's output repeats until it reaches this.
const MIN_BYTES: usize = 1 << 20;

/// What vtebench writes before each sample's setup and after a benchmark's last sample: RIS.
const RESET: &[u8] = b"\x1bc";

/// The in-terminal verb that captures the payloads ([`capture_here`]).
pub(crate) const CAPTURE_VERB: &str = "vte-capture";

/// How long the capture child may run: a hang detector, never a budget (the scripts take
/// about ten seconds on an idle M4 Pro, and the shell loops of two of them scale with load).
const CAPTURE_HANG: Duration = Duration::from_secs(600);

/// The file the capture child writes vtebench's own stream to, for [`cross_check`].
const STREAM: &str = "vtebench.stream";

/// The lane: its gated keys are each scenario's best MiB per 10^9 performance-core cycles
/// (`1000 / min P-core Mcycles per MiB`).
pub(crate) const LANE: FloorLane = FloorLane {
    lane: "vte-engine",
    example: "vte_engine_harness",
    keys: &[
        "cursor_motion_best_mib_per_gcycle",
        "dense_cells_best_mib_per_gcycle",
        "light_cells_best_mib_per_gcycle",
        "medium_cells_best_mib_per_gcycle",
        "scrolling_best_mib_per_gcycle",
        "scrolling_bottom_region_best_mib_per_gcycle",
        "scrolling_bottom_small_region_best_mib_per_gcycle",
        "scrolling_fullscreen_best_mib_per_gcycle",
        "scrolling_top_region_best_mib_per_gcycle",
        "scrolling_top_small_region_best_mib_per_gcycle",
        "sync_medium_cells_best_mib_per_gcycle",
        "unicode_best_mib_per_gcycle",
        "d1_ascii_best_mib_per_gcycle",
        "d1_unicode_best_mib_per_gcycle",
    ],
    baseline_file: "perf-baseline-vte-engine.json",
    comment: "aterm VTE-ENGINE baseline: per scenario, the best sample's MiB per 10^9 \
              performance-core cycles (cycles per instruction on P-cores times all \
              instructions, from the kernel's per-core-type counters) of the pinned vtebench's \
              default suite (payloads captured from its own scripts) and the arena's 150 MB d1 \
              ascii and unicode corpora, replayed into Terminal::process at 120x40 in 16 KiB \
              slices on a live session's engine. BIGGER-IS-BETTER; the lane runs unadmitted at \
              the load it finds, which moves its ms per MiB on both clocks, so cycles are what \
              is judged. Re-record with targo --unverified verify perf --record vte-engine.",
    cli: "vte-engine",
};

/// The lane: capture, measure, judge (or record).
pub(crate) fn run(record: bool) -> LaneRun {
    match measure() {
        Ok(json) => match unjudgeable(&json) {
            Some(why) => {
                eprintln!("  vte-engine: NOT JUDGED — {why}");
                LaneRun::not_judged(why)
            }
            None => judge_keyed(&LANE, &json, record),
        },
        Err(Halt::Skip { why, fix }) => LaneRun::skip(why, fix),
        Err(Halt::Fail(e)) => LaneRun::fail(e),
    }
}

/// Why the harness's report holds no figure to judge, if it does not: a kernel that counts no
/// cycles per core type, or a scenario with no sample counted on performance cores (its line in
/// the harness's report says why).
fn unjudgeable(json: &aterm_json::Value) -> Option<String> {
    if json.get("counters").and_then(aterm_json::Value::as_bool) != Some(true) {
        return Some(
            "this kernel counts no cycles per core type, so only its ms per MiB, which move \
             with load, were measured (reported above)"
                .to_string(),
        );
    }
    let missing: Vec<&str> = LANE
        .keys
        .iter()
        .filter(|k| number(json, k).is_none())
        .filter_map(|k| k.strip_suffix("_best_mib_per_gcycle"))
        .collect();
    (!missing.is_empty()).then(|| {
        format!(
            "no sample of {} counted performance-core cycles (the harness's line says why)",
            missing.join(", ")
        )
    })
}

fn measure() -> Result<aterm_json::Value, Halt> {
    let pins = pins()?;
    let pin = &pins.harness.vtebench;
    let vte_dir = pinned_checkout(pin)?;
    let scratch = Scratch::new("vte-engine")?;
    let benchmarks = scratch.path().join("benchmarks");
    trimmed_benchmarks(&vte_dir.join("benchmarks"), &benchmarks)?;
    let built = vte_dir.join("target/release/vtebench");
    let vtebench = built.is_file().then_some(built.as_path());
    let part = scratch.path().join("payloads");
    capture(&benchmarks, &part, vtebench)?;
    let names = payload_names(&part)?;
    match vtebench {
        Some(bin) => {
            let stream_path = part.join(STREAM);
            let stream = std::fs::read(&stream_path)
                .map_err(|e| format!("{}: {e}", stream_path.display()))?;
            cross_check(&stream, &read_payloads(&part, &names)?)?;
            std::fs::remove_file(&stream_path)
                .map_err(|e| format!("{}: {e}", stream_path.display()))?;
            eprintln!(
                "  payloads: {} benchmarks, each the bytes {} wrote at {COLS}x{ROWS} \
                 (cross-checked)",
                names.len(),
                bin.display()
            );
        }
        None => eprintln!(
            "  payloads: {} benchmarks from the pinned scripts at {COLS}x{ROWS}; NOT cross-checked \
             against vtebench's own stream (no build at {}: `verify perf vte` builds it)",
            names.len(),
            built.display()
        ),
    }
    let captured = read_payloads(&part, &names)?;
    for line in describe(&captured) {
        eprintln!("  {line}");
    }
    let ascii = corpus_file(Corpus::Ascii, CORPUS_MB)?;
    let unicode = corpus_file(Corpus::Unicode, CORPUS_MB)?;
    let corpus = |name: &str, path: &Path| format!("{name}={}", path.display());
    let args = [
        part.display().to_string(),
        "--corpus".into(),
        corpus("d1_ascii", &ascii),
        "--corpus".into(),
        corpus("d1_unicode", &unicode),
        "--rows".into(),
        ROWS.to_string(),
        "--cols".into(),
        COLS.to_string(),
    ];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    eprintln!(
        "  unadmitted: no quiet claim, so the harness names the 1-minute load it replayed at; its \
         ms per MiB move with that load and are never judged, its performance-core cycles per \
         MiB are"
    );
    let from = aterm_capacity::Live.read(None);
    let json = measure_example_with(LANE.example, &[], &args)?;
    let to = aterm_capacity::Live.read(None);
    eprintln!(
        "  machine across the build and the replay: {}",
        aterm_capacity::between(&from, &to)
    );
    let shared = arena_dir()?.join(format!(
        "vte-engine-{}-{COLS}x{ROWS}-{}",
        pin.commit,
        digest(&captured)
    ));
    match share(&part, &shared) {
        Ok(()) => eprintln!("  payloads for a hand A/B: {}", shared.display()),
        Err(e) => eprintln!("  payloads not left for a hand A/B: {e}"),
    }
    Ok(json)
}

/// Leave this run's payloads at `shared` for a hand A/B, unless a run already left them there.
/// `shared` names their digest, so a folder found there holds these bytes; no run removes one,
/// and a run that loses the race to place it finds the winner's identical copy.
fn share(part: &Path, shared: &Path) -> Result<(), String> {
    if shared.is_dir() {
        return Ok(());
    }
    match std::fs::rename(part, shared) {
        Ok(()) => Ok(()),
        Err(_) if shared.is_dir() => Ok(()),
        Err(e) => Err(format!("{}: {e}", shared.display())),
    }
}

/// The SHA-256 prefix of every payload, name and bytes, in order: the shared folder's name.
fn digest(payloads: &[Payload]) -> String {
    let mut all = Vec::new();
    for p in payloads {
        for part in [p.name.as_bytes(), &p.setup, &p.once] {
            all.extend_from_slice(&(part.len() as u64).to_le_bytes());
            all.extend_from_slice(part);
        }
    }
    aterm_digest::Sha256::digest(all)
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A PTY at `rows` x `cols`: `(master, slave)`, both close-on-exec.
fn open_pty(rows: u16, cols: u16) -> Result<(OwnedFd, OwnedFd), String> {
    let (master, slave) =
        aterm_pty::open_pty_pair().map_err(|err| format!("open_pty_pair: {err}"))?;
    // SAFETY: `open_pty_pair` returned two fresh close-on-exec descriptors owned only here.
    let pair = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `pair.1` is an open PTY slave descriptor and `size` is an initialized `winsize`
    // `TIOCSWINSZ` only reads.
    if unsafe { libc::ioctl(pair.1.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) } != 0 {
        return Err(format!(
            "TIOCSWINSZ on the PTY: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(pair)
}

/// Capture the payloads of `benchmarks` into `out` from a child in a session of its own whose
/// controlling terminal is a fresh PTY at the grid ([`capture_here`]), and, given `vtebench`,
/// vtebench's own stream beside them.
fn capture(benchmarks: &Path, out: &Path, vtebench: Option<&Path>) -> Result<(), String> {
    let (master, slave) = open_pty(ROWS, COLS)?;
    let (rows, cols) = (ROWS.to_string(), COLS.to_string());
    let mut args = vec![benchmarks, out, Path::new(&rows), Path::new(&cols)];
    args.extend(vtebench);
    let argv = inner_argv(CAPTURE_VERB, &args)?;
    let mut cmd = command(&argv[0]);
    cmd.args(&argv[1..])
        .setsid(true)
        .env("TERM", TERM)
        .env_remove("LINES")
        .env_remove("COLUMNS");
    let got = aterm_uds::deadline::output_until_from(
        &mut cmd,
        Stdio::from(slave),
        Instant::now() + CAPTURE_HANG,
    );
    drop(master);
    let got = got.map_err(|e| format!("the payload capture: {e}"))?;
    if got.status.success() {
        Ok(())
    } else {
        Err(format!(
            "the payload capture exited {}: {}",
            got.status,
            String::from_utf8_lossy(&got.stderr).trim_end()
        ))
    }
}

/// The captured scenarios' names, by name (vtebench's order).
fn payload_names(dir: &Path) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".bench"))
                .map(str::to_string)
        })
        .collect();
    names.sort();
    Ok(names)
}

/// One captured benchmark: its name, its setup's output and one run of its benchmark's.
pub(crate) struct Payload {
    pub name: String,
    pub setup: Vec<u8>,
    pub once: Vec<u8>,
}

fn read_payloads(dir: &Path, names: &[String]) -> Result<Vec<Payload>, String> {
    names
        .iter()
        .map(|name| {
            let read = |ext: &str| {
                let p = dir.join(format!("{name}.{ext}"));
                std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
            };
            Ok(Payload {
                name: name.clone(),
                setup: if dir.join(format!("{name}.setup")).exists() {
                    read("setup")?
                } else {
                    Vec::new()
                },
                once: read("bench")?,
            })
        })
        .collect()
}

/// vtebench's loader: `count = (min_bytes - 1) / len + 1` copies of one run's output.
fn repeat_to_min(once: &[u8]) -> Vec<u8> {
    once.repeat((MIN_BYTES - 1) / once.len().max(1) + 1)
}

/// Hold `payloads` to vtebench's own stream (`--warmup 0 --max-samples 1`): per benchmark, in
/// order, its `Loading <name>...` line, then RIS, the setup, the benchmark repeated to 1 MiB and
/// RIS, and nothing after the last.
///
/// # Errors
/// The first benchmark whose bytes differ, or a stream that runs past or short of the payloads.
pub(crate) fn cross_check(stream: &[u8], payloads: &[Payload]) -> Result<(), String> {
    let mut rest = stream;
    for p in payloads {
        let marker = format!("Loading {}...\n", p.name);
        rest = rest.strip_prefix(marker.as_bytes()).ok_or_else(|| {
            format!(
                "vtebench's stream does not load `{}` next: it ran other benchmarks than were \
                 captured",
                p.name
            )
        })?;
        let want = [RESET, &p.setup, &repeat_to_min(&p.once), RESET].concat();
        rest = rest.strip_prefix(want.as_slice()).ok_or_else(|| {
            format!(
                "`{}`: vtebench wrote other bytes than its captured scripts printed, so the \
                 capture is not vtebench's payload",
                p.name
            )
        })?;
    }
    if rest.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "vtebench wrote {} bytes past the last captured benchmark",
            rest.len()
        ))
    }
}

/// One line per payload: its name, its setup (escaped, when short), one run's size and the
/// SHA-256 prefix of setup and run, so two runs show they replayed the same bytes.
fn describe(payloads: &[Payload]) -> Vec<String> {
    payloads
        .iter()
        .map(|p| {
            let digest = aterm_digest::Sha256::digest([p.setup.as_slice(), &p.once].concat());
            let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
            let setup = if p.setup.len() <= 32 {
                format!("setup {:?}", String::from_utf8_lossy(&p.setup))
            } else {
                format!("setup {} bytes", p.setup.len())
            };
            format!(
                "{:<30} {setup}, run {} bytes, sha256 {hex}",
                p.name,
                p.once.len()
            )
        })
        .collect()
}

/// `aterm-verify --arena-inner vte-capture <benchmarks> <out> <rows> <cols> [<vtebench>]`, the
/// capture child: a session leader whose stdin is the lane's PTY.
pub(crate) fn capture_main(args: &[&str]) -> ExitCode {
    let [benchmarks, out, rows, cols, rest @ ..] = args else {
        return capture_usage();
    };
    if !rows.bytes().all(|b| b.is_ascii_digit()) || !cols.bytes().all(|b| b.is_ascii_digit()) {
        return capture_usage();
    }
    let (Ok(rows), Ok(cols)) = (rows.parse::<u16>(), cols.parse::<u16>()) else {
        return capture_usage();
    };
    let vtebench = match rest {
        [] => None,
        [bin] => Some(Path::new(bin)),
        _ => return capture_usage(),
    };
    match capture_here(
        Path::new(benchmarks),
        Path::new(out),
        (rows, cols),
        vtebench,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vte-capture: {e}");
            ExitCode::FAILURE
        }
    }
}

fn capture_usage() -> ExitCode {
    eprintln!(
        "usage: aterm-verify --arena-inner {CAPTURE_VERB} <benchmarks> <out> <rows> <cols> \
         [<vtebench>]\n(the `vte-engine` lane's capture child; not for direct use)"
    );
    ExitCode::from(2)
}

/// Take stdin, the lane's PTY, as this session's controlling terminal, hold it to the grid, then
/// write each benchmark's payload into `out` ([`capture_scripts`]) and, given `vtebench`, its own
/// stream as [`STREAM`].
fn capture_here(
    benchmarks: &Path,
    out: &Path,
    (rows, cols): (u16, u16),
    vtebench: Option<&Path>,
) -> Result<(), String> {
    // SAFETY: fd 0 is this process's stdin; TIOCSCTTY takes an int (0: never steal a terminal
    // another session holds) and reads no memory.
    if unsafe { libc::ioctl(0, libc::TIOCSCTTY as _, 0) } != 0 {
        return Err(format!(
            "stdin did not become the controlling terminal: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: fd 0 is open; TIOCGWINSZ writes one `winsize` into `size`, which is writable.
    if unsafe { libc::ioctl(0, libc::TIOCGWINSZ as _, &mut size) } != 0 {
        return Err(format!(
            "the terminal's size: {}",
            std::io::Error::last_os_error()
        ));
    }
    if (size.ws_row, size.ws_col) != (rows, cols) {
        return Err(format!(
            "the terminal is {}x{} (cols x rows), not the asked {cols}x{rows}",
            size.ws_col, size.ws_row
        ));
    }
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    capture_scripts(benchmarks, out)?;
    if let Some(bin) = vtebench {
        let stream_path = out.join(STREAM);
        let stream = std::fs::File::create(&stream_path)
            .map_err(|e| format!("{}: {e}", stream_path.display()))?;
        let ran = command(bin)
            .arg("--benchmarks")
            .arg(benchmarks)
            .args(["--warmup", "0", "--max-samples", "1", "--silent"])
            .stdin(Stdio::null())
            .stdout(stream)
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("could not run {}: {e}", bin.display()))?;
        if !ran.status.success() {
            return Err(format!(
                "{} exited {}: {}",
                bin.display(),
                ran.status,
                String::from_utf8_lossy(&ran.stderr).trim_end()
            ));
        }
    }
    Ok(())
}

/// Each benchmark under `benchmarks` run as vtebench's loader runs it (`Command::output`: stdin
/// `/dev/null`, stdout and stderr piped, this process's environment and controlling terminal),
/// its stdout written to `out/<name>.setup` and `out/<name>.bench`. vtebench drops a benchmark
/// whose output is empty without a word; here that is an error, as is a script that exits
/// unsuccessfully, even when it printed a nonempty prefix.
pub(crate) fn capture_scripts(benchmarks: &Path, out: &Path) -> Result<(), String> {
    capture_scripts_on(benchmarks, out, |script| command(script))
}

/// The same capture with its executable construction handed in: controls run script data
/// through a kept, signed interpreter instead of executing a fresh script path on macOS.
fn capture_scripts_on(
    benchmarks: &Path,
    out: &Path,
    mut script_command: impl FnMut(&Path) -> Command,
) -> Result<(), String> {
    let mut run = |script: &Path| -> Result<Vec<u8>, String> {
        let got = script_command(script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("could not run {}: {e}", script.display()))?;
        if !got.status.success() {
            return Err(format!(
                "{} exited {}: {}",
                script.display(),
                got.status,
                String::from_utf8_lossy(&got.stderr).trim_end()
            ));
        }
        Ok(got.stdout)
    };
    for dir in benchmark_dirs(benchmarks)? {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{}: not a benchmark name", dir.display()))?;
        let write = |ext: &str, bytes: &[u8]| {
            let p = out.join(format!("{name}.{ext}"));
            std::fs::write(&p, bytes).map_err(|e| format!("{}: {e}", p.display()))
        };
        let setup = dir.join("setup");
        if setup.is_file() {
            write("setup", &run(&setup)?)?;
        }
        let bench = run(&dir.join("benchmark"))?;
        if bench.is_empty() {
            return Err(format!(
                "{name}'s benchmark printed nothing (vtebench would drop it silently)"
            ));
        }
        write("bench", &bench)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(name: &str, setup: &[u8], once: &[u8]) -> Payload {
        Payload {
            name: name.into(),
            setup: setup.to_vec(),
            once: once.to_vec(),
        }
    }

    fn stream_of(payloads: &[Payload]) -> Vec<u8> {
        let mut s = Vec::new();
        for p in payloads {
            s.extend_from_slice(format!("Loading {}...\n", p.name).as_bytes());
            s.extend_from_slice(RESET);
            s.extend_from_slice(&p.setup);
            for _ in 0..(MIN_BYTES - 1) / p.once.len() + 1 {
                s.extend_from_slice(&p.once);
            }
            s.extend_from_slice(RESET);
        }
        s
    }

    #[test]
    fn the_capture_matches_the_stream_vtebench_writes_for_it() {
        let payloads = [
            payload("scrolling", b"y\ny\n", b"y\n"),
            payload("scrolling_bottom_region", b"\x1b[?1049h\x1b[1;23r", b"y\n"),
            payload("unicode", b"\x1b[?1049h", "\u{2603}\n".as_bytes()),
        ];
        let stream = stream_of(&payloads);
        assert_eq!(cross_check(&stream, &payloads), Ok(()));
        // A run whose length does not divide 1 MiB repeats past it, as vtebench's loader does.
        assert_eq!(repeat_to_min(b"abc").len(), MIN_BYTES + 2);
    }

    #[test]
    fn a_planted_divergence_is_caught() {
        let payloads = [
            payload("scrolling", b"", b"y\n"),
            payload("scrolling_bottom_region", b"\x1b[?1049h\x1b[1;23r", b"y\n"),
        ];
        let stream = stream_of(&payloads);
        let planted = [
            payload("scrolling", b"", b"y\n"),
            payload("scrolling_bottom_region", b"\x1b[?1049h\x1b[1;39r", b"y\n"),
        ];
        let e = cross_check(&stream, &planted).expect_err("the region differs");
        assert!(e.contains("scrolling_bottom_region"), "{e}");
        let e = cross_check(&stream, &payloads[..1]).expect_err("a benchmark left uncaptured");
        assert!(e.contains("past the last"), "{e}");
        let e = cross_check(&stream_of(&payloads[..1]), &payloads).expect_err("one more");
        assert!(e.contains("scrolling_bottom_region"), "{e}");
    }

    #[test]
    fn a_report_without_performance_core_cycles_is_not_judged() {
        let report = |counters: bool, skip: Option<&str>| {
            let mut text = format!("{{\"counters\":{counters}");
            for key in LANE
                .keys
                .iter()
                .filter(|k| skip.is_none_or(|s| !k.starts_with(s)))
            {
                text.push_str(&format!(",\"{key}\":5.0"));
            }
            text.push('}');
            aterm_json::from_str::<aterm_json::Value>(&text).expect("report")
        };
        assert_eq!(unjudgeable(&report(true, None)), None);
        let why = unjudgeable(&report(false, None)).expect("no counters");
        assert!(why.contains("no cycles per core type"), "{why}");
        let why = unjudgeable(&report(true, Some("sync_medium_cells"))).expect("one missing");
        assert!(
            why.contains("no sample of sync_medium_cells counted"),
            "{why}"
        );
    }

    /// The loader's view of a script: its stdout alone (stderr is piped apart), no stdin, and
    /// an empty benchmark refused rather than dropped.
    #[test]
    fn scripts_run_as_vtebench_runs_them() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = Scratch::new("atv-vte-engine").expect("scratch");
        let benchmarks = dir.path().join("benchmarks");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).expect("out");
        let script = |bench: &str, name: &str, body: &str| {
            let d = benchmarks.join(bench);
            std::fs::create_dir_all(&d).expect("bench dir");
            let p = d.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("script");
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        };
        script("a", "setup", "printf 'S'; echo noise >&2");
        script("a", "benchmark", "read x; printf 'B%s' \"$x\"");
        script("b", "benchmark", "printf 'only'");
        capture_scripts_on(&benchmarks, &out, script_interpreter).expect("captures");
        assert_eq!(std::fs::read(out.join("a.setup")).expect("a.setup"), b"S");
        assert_eq!(std::fs::read(out.join("a.bench")).expect("a.bench"), b"B");
        assert!(!out.join("b.setup").exists());
        assert_eq!(
            std::fs::read(out.join("b.bench")).expect("b.bench"),
            b"only"
        );
        assert_eq!(payload_names(&out).expect("names"), ["a", "b"]);
        script("c", "benchmark", "true");
        let e = capture_scripts_on(&benchmarks, &out, script_interpreter).expect_err("empty");
        assert!(e.contains("c's benchmark printed nothing"), "{e}");
    }

    fn script_interpreter(script: &Path) -> Command {
        let mut shell = command(aterm_uds::deadline::stand_in(Path::new("/bin/sh")));
        shell.arg0("sh").arg(script);
        shell
    }

    /// A setup or benchmark can emit a nonempty prefix and then fail. Neither prefix is a
    /// complete payload; successful ordinary captures must still preserve stdout exactly.
    #[test]
    fn failed_scripts_cannot_become_nonempty_payloads() {
        let dir = Scratch::new("atv-vte-script-status").expect("scratch");
        let mut accepted = Vec::new();
        for failed in ["setup", "benchmark"] {
            let root = dir.path().join(failed);
            let benchmarks = root.join("benchmarks");
            let scenario = benchmarks.join("a");
            let ordinary = root.join("ordinary");
            let refused = root.join("refused");
            for path in [&scenario, &ordinary, &refused] {
                std::fs::create_dir_all(path).expect("owned directory");
            }
            for (name, body) in [("setup", "printf S"), ("benchmark", "printf B")] {
                std::fs::write(scenario.join(name), body).expect("script data");
            }
            capture_scripts_on(&benchmarks, &ordinary, script_interpreter)
                .expect("ordinary success");
            assert_eq!(std::fs::read(ordinary.join("a.setup")).unwrap(), b"S");
            assert_eq!(std::fs::read(ordinary.join("a.bench")).unwrap(), b"B");
            std::fs::write(
                scenario.join(failed),
                "printf partial; printf 'fixture refusal' >&2; exit 7",
            )
            .expect("failing script data");
            match capture_scripts_on(&benchmarks, &refused, script_interpreter) {
                Ok(()) => accepted.push(failed),
                Err(why) => {
                    assert!(
                        why.contains(failed) && why.contains("fixture refusal"),
                        "{why}"
                    );
                    assert!(!refused.join("a.bench").exists(), "no completed payload");
                }
            }
        }
        assert!(
            accepted.is_empty(),
            "failed scripts were accepted as complete payloads: {accepted:?}"
        );
    }
}
