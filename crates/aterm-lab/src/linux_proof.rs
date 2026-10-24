// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux lane's proof (`linux proof`; `targo --unverified verify linux` runs it).
//!
//! STOCK EXCEPTION: the guest's Rust is rustup's stock toolchain
//! ([`crate::linux_guest::RUST`]); no Linux build of Trust exists.
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::linux::{self, Out, Up};
use crate::linux_guest::{self as guest, Arch, Display};
use crate::{Fail, LAB, Res};

/// A row's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    NotRun,
}

impl Verdict {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::NotRun => "NOT RUN",
        }
    }
}

/// One line of the report.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub verdict: Verdict,
    pub secs: f64,
    pub detail: String,
}

/// The run's exit status: 1 when a row failed, else 3 when a row did not run or there were none, else 0.
#[must_use]
pub fn exit_code(rows: &[Row]) -> i32 {
    if rows.iter().any(|r| r.verdict == Verdict::Fail) {
        1
    } else if rows.is_empty() || rows.iter().any(|r| r.verdict == Verdict::NotRun) {
        linux::NOT_RUN
    } else {
        0
    }
}

/// What `proof` (or `gone`) was asked.
#[derive(Debug, PartialEq, Eq)]
pub struct ProofArgs {
    pub arches: Vec<Arch>,
    pub displays: Vec<Display>,
    pub keep_build: bool,
    pub keep_up: bool,
    /// `gone`: only the rows that kill the display under aterm-gui ([`crate::linux_gone`]), no tests, sessions or baseline.
    pub gone_only: bool,
    /// `cpu`: only the CPU presenter rows (LW.6), no tests, GPU sessions, display-gone rows or baseline.
    pub cpu_only: bool,
    /// `a11y`: only the accessibility runtime's rows ([`crate::linux_a11y`]), no other tests, sessions, display-gone rows or baseline.
    pub a11y_only: bool,
    /// `deck`: only the stats Deck's layout rows ([`crate::linux_baseline::deck_row`]), one per display family, no tests, other sessions, display-gone rows or baseline.
    pub deck_only: bool,
    /// `edges`: only the untested edges' rows ([`crate::linux_edges`]), no other tests, sessions, display-gone rows or baseline.
    pub edges_only: bool,
    /// `edges --edge E`: the edge rows to run; every one when empty.
    pub edges: Vec<crate::linux_edges::Edge>,
    /// `gone --wgpu-oracle`.
    pub wgpu_oracle: bool,
    /// `--weston-renderer pixman`: every weston the run starts draws with its pixman renderer (the VM and kiosk case) instead of GL.
    pub weston_renderer: &'static str,
}

/// `proof [--arch A]... [--display D]... [--keep-build] [--keep-up]`.
///
/// # Errors
/// A usage error.
pub fn parse_proof(args: &[String]) -> Result<ProofArgs, Fail> {
    let mut p = parse_args(args, "proof")?;
    if p.displays.is_empty() {
        p.displays = Display::PROOF.to_vec();
    }
    Ok(p)
}

/// `gone [--arch A]... [--display D]... [--wgpu-oracle] [--keep-build] [--keep-up]`.
///
/// # Errors
/// A usage error.
pub fn parse_gone(args: &[String]) -> Result<ProofArgs, Fail> {
    let mut p = parse_args(args, "gone")?;
    p.gone_only = true;
    if p.displays.is_empty() {
        p.displays = crate::linux_gone::GONE.to_vec();
    }
    Ok(p)
}

/// `cpu [--arch A]... [--display D]... [--keep-build] [--keep-up]`.
///
/// # Errors
/// A usage error.
pub fn parse_cpu(args: &[String]) -> Result<ProofArgs, Fail> {
    let mut p = parse_args(args, "cpu")?;
    p.cpu_only = true;
    if p.displays.is_empty() {
        p.displays = Display::PROOF.to_vec();
    }
    Ok(p)
}

/// `deck [--arch A]... [--display D]... [--keep-build] [--keep-up]`.
///
/// # Errors
/// A usage error.
pub fn parse_deck(args: &[String]) -> Result<ProofArgs, Fail> {
    let mut p = parse_args(args, "deck")?;
    p.deck_only = true;
    if p.displays.is_empty() {
        p.displays = Display::PROOF.to_vec();
    }
    Ok(p)
}

/// `edges [--arch A]... [--edge E]... [--keep-build] [--keep-up]`: every arch and every edge row when none is named.
///
/// # Errors
/// A usage error.
pub fn parse_edges(args: &[String]) -> Result<ProofArgs, Fail> {
    let mut p = parse_args(args, "edges")?;
    p.edges_only = true;
    Ok(p)
}

/// `a11y [--arch A]... [--keep-build] [--keep-up]`: every arch when none is named; its aterm-gui rows run under Xvfb.
///
/// # Errors
/// A usage error.
pub fn parse_a11y(args: &[String]) -> Result<ProofArgs, Fail> {
    let mut p = parse_args(args, "a11y")?;
    p.a11y_only = true;
    Ok(p)
}

fn parse_args(args: &[String], verb: &str) -> Result<ProofArgs, Fail> {
    let mut p = ProofArgs {
        arches: Vec::new(),
        displays: Vec::new(),
        keep_build: false,
        keep_up: false,
        gone_only: false,
        cpu_only: false,
        a11y_only: false,
        deck_only: false,
        edges_only: false,
        edges: Vec::new(),
        wgpu_oracle: false,
        weston_renderer: "gl",
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--arch" => {
                let w = it.next().map_or("", String::as_str);
                let arch = Arch::parse(w).ok_or_else(|| {
                    Fail::usage(format!("linux {verb}: --arch {w}: aarch64 or x86_64"))
                })?;
                if !p.arches.contains(&arch) {
                    p.arches.push(arch);
                }
            }
            "--edge" if verb == "edges" => {
                let w = it.next().map_or("", String::as_str);
                let e = crate::linux_edges::Edge::parse(w).ok_or_else(|| {
                    let all: Vec<&str> = crate::linux_edges::Edge::ALL
                        .iter()
                        .map(|e| e.slug())
                        .collect();
                    Fail::usage(format!(
                        "linux {verb}: --edge {w}: one of {}",
                        all.join(", ")
                    ))
                })?;
                if !p.edges.contains(&e) {
                    p.edges.push(e);
                }
            }
            "--display" if verb != "a11y" && verb != "edges" => {
                let w = it.next().map_or("", String::as_str);
                let d = Display::parse(w)
                    .filter(|d| Display::PROOF.contains(d))
                    .ok_or_else(|| {
                        Fail::usage(format!(
                            "linux {verb}: --display {w}: xvfb, weston, mutter or xwayland"
                        ))
                    })?;
                if !p.displays.contains(&d) {
                    p.displays.push(d);
                }
            }
            "--keep-build" => p.keep_build = true,
            "--keep-up" => p.keep_up = true,
            "--wgpu-oracle" if verb == "gone" => p.wgpu_oracle = true,
            "--weston-renderer" if verb != "a11y" && verb != "edges" => {
                p.weston_renderer = match it.next().map(String::as_str) {
                    Some("gl") => "gl",
                    Some("pixman") => "pixman",
                    w => {
                        return Err(Fail::usage(format!(
                            "linux {verb}: --weston-renderer {}: gl or pixman",
                            w.unwrap_or("")
                        )));
                    }
                };
            }
            other => {
                return Err(Fail::usage(format!(
                    "linux {verb}: unknown argument '{other}' ({LAB} linux help)"
                )));
            }
        }
    }
    if p.arches.is_empty() {
        p.arches = Arch::ALL.to_vec();
    }
    Ok(p)
}

/// `row`, failed when `text` (its tests' uncaptured output) has a test saying it skipped (`SKIP: …`).
#[must_use]
pub fn no_skips(mut row: Row, text: &str) -> Row {
    let skips: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("SKIP:"))
        .collect();
    if row.verdict == Verdict::Pass && !skips.is_empty() {
        row.verdict = Verdict::Fail;
        row.detail = format!(
            "{}; {} skipped, which the lane counts as not run:\n{}",
            row.detail,
            skips.len(),
            skips
                .iter()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    row
}

/// The prefix of the line a test prints for what it cannot decide under a binary translator, the translator's name and the test's own words after it (aterm-uds's `spawnfd` tests: `NOT RUN under Rosetta: <test>: <why>`).
pub const TRANSLATOR_NOT_RUN: &str = "NOT RUN under ";

/// `row`, judged on what `text` (its tests' uncaptured output) says they could not decide under a binary translator ([`TRANSLATOR_NOT_RUN`]).
#[must_use]
pub fn translator_not_runs(mut row: Row, text: &str, arch: Arch) -> Row {
    let rosetta = format!("{TRANSLATOR_NOT_RUN}Rosetta:");
    let lines: Vec<&str> = text
        .lines()
        .filter_map(|l| l.get(l.find(TRANSLATOR_NOT_RUN)?..).map(str::trim_end))
        .collect();
    let list = |lines: &[&str]| {
        lines
            .iter()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let misread: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| arch != Arch::X86_64 || !l.starts_with(&rosetta))
        .collect();
    if !misread.is_empty() {
        row.verdict = Verdict::Fail;
        row.detail = format!(
            "{}; {} said not run under a translator this cell does not run under:\n{}",
            row.detail,
            misread.len(),
            list(&misread)
        );
    } else if !lines.is_empty() {
        row.detail = format!(
            "{}; {} not runnable under Rosetta, which a real x86_64 CPU decides:\n{}",
            row.detail,
            lines.len(),
            list(&lines)
        );
    }
    row
}

/// What a `cargo test` run reported.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    /// `<binary>: <test>`, each once.
    pub failures: Vec<String>,
    /// The first `error:` line cargo printed, when one did.
    pub error: Option<String>,
}

fn binary_of(running: &str) -> String {
    let path = running
        .rsplit_once('(')
        .map_or(running, |(_, p)| p.trim_end_matches(')'));
    let file = path.rsplit('/').next().unwrap_or(path);
    file.rsplit_once('-')
        .map_or(file, |(name, _)| name)
        .to_string()
}

/// Read `cargo test --no-fail-fast`'s output, stderr merged in order.
#[must_use]
pub fn tally(output: &str) -> Tally {
    const fn close(t: &mut Tally, last: &mut Option<(u64, u64, u64)>) {
        if let Some((p, f, i)) = last.take() {
            t.passed += p;
            t.failed += f;
            t.ignored += i;
        }
    }
    let mut t = Tally::default();
    let mut binary = String::new();
    let mut last_result: Option<(u64, u64, u64)> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Running ") {
            close(&mut t, &mut last_result);
            binary = binary_of(rest);
        } else if trimmed.starts_with("Doc-tests ") {
            close(&mut t, &mut last_result);
            binary = trimmed.to_string();
        } else if let Some(rest) = trimmed.strip_prefix("test result: ") {
            let count = |word: &str| {
                rest.split(';')
                    .find_map(|part| {
                        let part = part
                            .trim()
                            .trim_start_matches("ok. ")
                            .trim_start_matches("FAILED. ");
                        part.strip_suffix(word)?.trim().parse::<u64>().ok()
                    })
                    .unwrap_or(0)
            };
            last_result = Some((count("passed"), count("failed"), count("ignored")));
        } else if let Some(name) = trimmed
            .strip_prefix("test ")
            .and_then(|r| r.strip_suffix(" ... FAILED"))
        {
            let entry = format!("{binary}: {name}");
            if !t.failures.contains(&entry) {
                t.failures.push(entry);
            }
        } else if t.error.is_none()
            && trimmed.starts_with("error")
            && !trimmed.starts_with("error: test failed")
            && !trimmed.contains("target failed")
            && !trimmed.contains("targets failed")
        {
            t.error = Some(trimmed.to_string());
        }
    }
    close(&mut t, &mut last_result);
    t
}

/// A test run's row: PASS on cargo's exit 0 when a test passed, NOT RUN when none did (a selection that matched nothing decided nothing), else FAIL naming what failed.
#[must_use]
pub fn test_verdict(name: &str, out: &Out) -> Row {
    let t = tally(&out.text);
    let counts = format!(
        "{} passed, {} failed, {} ignored",
        t.passed, t.failed, t.ignored
    );
    let (verdict, detail) = if out.ok() && t.passed > 0 {
        (Verdict::Pass, counts)
    } else if out.ok() {
        (
            Verdict::NotRun,
            format!("cargo exited 0 but ran no test ({counts})"),
        )
    } else if !t.failures.is_empty() {
        (
            Verdict::Fail,
            format!(
                "{counts}\n{}",
                t.failures
                    .iter()
                    .map(|f| format!("    {f}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
    } else {
        (
            Verdict::Fail,
            format!(
                "exit {:?}: {}",
                out.code,
                t.error.unwrap_or_else(|| out.tail(5))
            ),
        )
    };
    Row {
        name: name.to_string(),
        verdict,
        secs: out.secs,
        detail,
    }
}

/// How the x86_64 cell's uds, pty and libc tests run: one at a time.
pub const ROSETTA_ONE_AT_A_TIME: &str = "--test-threads=1";

/// A test row's hang detector; it times the tests alone ([`test_run`] builds them first).
pub const TEST_ROW_HANG_SECS: u32 = 1800;

/// `timeout` for a guest command: TERM at `secs`, KILL ten seconds later.
#[must_use]
pub fn hang_detector(secs: u32) -> Vec<String> {
    vec!["timeout".into(), "--kill-after=10".into(), secs.to_string()]
}

/// A test row's `cargo test` (or `bench`) arguments with `--no-run` in place
/// of what follows `--` (the test binaries' own arguments).
#[must_use]
pub fn no_run<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut built: Vec<&str> = args.iter().copied().take_while(|a| *a != "--").collect();
    built.push("--no-run");
    built
}

/// A test row's run, `cargo(args)` being its cargo argv: built first ([`no_run`]) with no hang
/// detector, since a cold build on a loaded host is slow, not hung, then run under
/// [`TEST_ROW_HANG_SECS`]. A failed build is the row's output; the seconds are both steps'.
#[must_use]
pub fn test_run(up: &Up, cargo: impl Fn(&[&str]) -> Vec<String>, args: &[&str]) -> Out {
    let built = up
        .lima
        .shell_streamed(&up.home, &cargo(&no_run(args)), "  | ");
    if !built.ok() {
        return built;
    }
    let mut argv = hang_detector(TEST_ROW_HANG_SECS);
    argv.extend(cargo(args));
    let mut out = up.lima.shell_streamed(&up.home, &argv, "  | ");
    out.secs += built.secs;
    out
}

/// The packages whose Linux tests the proof runs.
pub const TESTED: [&str; 3] = ["aterm-uds", "aterm-pty", "libc"];

/// The package whose library tests the proof runs on their own row: the renderer's glyph and
/// icon tests measure the face the guest resolves as its system face.
pub const RENDER: &str = "aterm-render";

/// The static bootstrap verifier's package and executable (install.sh's Linux verifier).
pub const VERIFIER: &str = "aterm-bootstrap-verify";

/// Static builds of [`VERIFIER`] made by the lane as `(shape, target_dir, debug_assertions)`.
pub const VERIFIER_SHAPES: [(&str, &str, bool); 2] = [
    ("shipped", "verifier-target", false),
    ("test", "verifier-test-target", true),
];

/// Cargo arguments for a static build of [`VERIFIER`] for `triple`.
#[must_use]
pub fn verifier_build_args(manifest: &str, triple: &str, assertions: bool) -> Vec<String> {
    let mut argv: Vec<String> = [
        "cargo",
        "build",
        "--locked",
        "--release",
        "--manifest-path",
        manifest,
        "-p",
        VERIFIER,
        "--target",
        triple,
        "--config",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    argv.push(aterm_bootstrap_verify::pin::static_build_config(triple));
    if assertions {
        argv.extend([
            "--config".to_string(),
            "profile.release.debug-assertions=true".to_string(),
        ]);
    }
    argv
}

fn verifier_rows(up: &Up, arch: Arch, tag: &str, repo: &str, lane: &str) -> Vec<Row> {
    let manifest = format!("{repo}/Cargo.toml");
    let mut rows = Vec::new();
    let mut built = Vec::new();
    for (shape, dir, assertions) in VERIFIER_SHAPES {
        let target_dir = format!("{lane}/{dir}");
        let mut argv = hang_detector(TEST_ROW_HANG_SECS);
        argv.extend(guest::with_env(
            &guest::cargo_env(arch, &target_dir, false),
            &verifier_build_args(&manifest, arch.triple(), assertions),
        ));
        let out = up.lima.shell_streamed(&up.home, &argv, "  | ");
        let exe = format!("{target_dir}/{}/release/{VERIFIER}", arch.triple());
        let name = format!("{tag} bootstrap verifier: static {shape} build");
        if !out.ok() {
            rows.push(Row {
                name,
                verdict: Verdict::Fail,
                secs: out.secs,
                detail: format!("exit {:?}: {}", out.code, out.tail(5)),
            });
            return rows;
        }
        rows.push(Row {
            name,
            verdict: Verdict::Pass,
            secs: out.secs,
            detail: exe.clone(),
        });
        built.push(exe);
    }
    let mut env = guest::cargo_env(arch, &format!("{lane}/target"), false);
    env.push(("ATERM_TEST_BOOTSTRAP_VERIFIER".into(), built[1].clone()));
    env.push((
        "ATERM_TEST_BOOTSTRAP_VERIFIER_SHIPPED".into(),
        built[0].clone(),
    ));
    let suites: [(&str, &[&str]); 2] = [
        (
            "bootstrap verifier, static, through install.sh",
            &["-p", VERIFIER, "--tests", "--", "--include-ignored"],
        ),
        (
            "install.sh's embedded verifier (Python 3.11+, OpenSSL 3)",
            &[
                "-p",
                "aterm-update",
                "--test",
                "install_sh",
                "--",
                "linux_verify::",
            ],
        ),
    ];
    for (what, selection) in suites {
        let mut cargo = vec![
            "cargo".to_string(),
            "test".into(),
            "--locked".into(),
            "--no-fail-fast".into(),
            "--manifest-path".into(),
            manifest.clone(),
        ];
        cargo.extend(arch.target_args());
        cargo.extend(selection.iter().map(|s| (*s).to_string()));
        let mut argv = hang_detector(TEST_ROW_HANG_SECS);
        argv.extend(guest::with_env(&env, &cargo));
        let out = up.lima.shell_streamed(&up.home, &argv, "  | ");
        rows.push(translator_not_runs(
            test_verdict(&format!("{tag} tests: {what}"), &out),
            &out.text,
            arch,
        ));
    }
    rows
}

/// The shell integration's suite (aterm-shell-integration's unit and integration tests), on real shells on a real pty.
pub const SHELLS: &str = "aterm-shell-integration";

/// The first-party windowing crates (LW.6).
pub const WINDOWING: [&str; 5] = [
    "aterm-dl",
    "aterm-x11",
    "aterm-wayland",
    "aterm-xkb",
    "aterm-window",
];

/// What `aterm-wayland`'s `live_seat` test reads to reach Mutter with a seat.
#[must_use]
pub fn seat_env(runtime: &str, mutter_pid: &str, bin: &str) -> Vec<(String, String)> {
    let (_, socket) = Display::Mutter.client_env(runtime);
    vec![
        ("LANE_SEAT_WAYLAND_DISPLAY".into(), socket),
        ("LANE_SEAT_MUTTER_PID".into(), mutter_pid.into()),
        ("LANE_SEAT_BUS".into(), format!("{bin}/lane-bus")),
        (
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path={runtime}/bus"),
        ),
    ]
}

fn no_shm(up: &Up) -> Result<(), String> {
    let out = up.sh(&guest::with_env(
        &[Display::Xvfb.client_env(&up.runtime)],
        &["xdpyinfo".into(), "-queryExtensions".into()],
    ));
    if !out.ok() {
        return Err(format!("xdpyinfo: {}", out.tail(3)));
    }
    if lists_extension(&out.text, "MIT-SHM") {
        return Err("the server still offers MIT-SHM".into());
    }
    Ok(())
}

/// Whether `xdpyinfo -queryExtensions` output lists `name`.
#[must_use]
pub fn lists_extension(xdpyinfo: &str, name: &str) -> bool {
    xdpyinfo
        .lines()
        .any(|l| l.split_whitespace().next() == Some(name))
}

/// A lane unit's main pid.
pub(crate) fn unit_pid(up: &Up, unit: &str) -> Option<String> {
    let out = up.sh(&[
        "systemctl".into(),
        "--user".into(),
        "show".into(),
        "--property=MainPID".into(),
        "--value".into(),
        unit.into(),
    ]);
    let pid = out.text.trim().to_string();
    (out.ok() && !pid.is_empty() && pid != "0").then_some(pid)
}

/// The fields of a `status` reply the proof reads.
#[must_use]
pub fn field<'a>(reply: &'a str, key: &str) -> Option<&'a str> {
    reply
        .split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// Whether a `dims` reply says the grid is `rows` x `cols`.
#[must_use]
pub fn dims_are(reply: &str, rows: u32, cols: u32) -> bool {
    let mut w = reply.split_whitespace();
    w.next() == Some("OK")
        && w.next() == Some(rows.to_string().as_str())
        && w.next() == Some(cols.to_string().as_str())
}

/// `n_present` and the present percentiles of a `metrics percentiles` reply.
#[must_use]
pub fn present_summary(reply: &str) -> String {
    let get = |k: &str| field(reply, k).unwrap_or("?");
    format!(
        "n_present={} present_p50/p95/p99_ms={}/{}/{}",
        get("n_present"),
        get("present_p50_ms"),
        get("present_p95_ms"),
        get("present_p99_ms")
    )
}

/// What the guest's journal says aterm-gui renders on (`GPU rendering on …`).
#[must_use]
pub fn renderer(journal: &str) -> Option<&str> {
    journal
        .lines()
        .find_map(|l| l.split_once("GPU rendering on ").map(|(_, r)| r.trim()))
}

/// What aterm-gui prints when a window's GPU surface did not attach although its device did (`crates/aterm-gui/src/app_window.rs`, the `Err` arm of the window attach's GPU surface).
pub const SURFACE_FAILED: &str = "GPU surface creation failed";

/// A GPU row's check that it drew on the GPU, its frame taken.
///
/// # Errors
/// Why the row did not draw on the GPU.
pub fn drew_on_gpu<'a>(journal: &'a str, backend: Option<&str>) -> Result<Option<&'a str>, String> {
    if let Some(line) = journal.lines().find(|l| l.contains(SURFACE_FAILED)) {
        return Err(format!(
            "a GPU row, but the window's GPU surface did not attach: {:?}",
            line.trim()
        ));
    }
    if backend != Some("gpu") {
        return Err(format!(
            "a GPU row, but `metrics` says backend={} after the frame, not gpu",
            backend.unwrap_or("(none)")
        ));
    }
    Ok(renderer(journal))
}

/// The line the proof types, and the echo only running it prints.
pub const TYPED: &str = "echo LANE-$((6*7))";
pub const ECHO: &str = "LANE-42";
/// What the CPU rows type at the prompt after their first photograph, not entered: a change to one row.
pub const MORE: &str = "lane-damage";

/// The clipboard round trip on an X11 display crosses the X server both ways, through a second client (xclip).
pub const PASTE_BEFORE: &str = "echo X-";
pub const PASTE_AFTER: &str = " | tr A-Z a-z";
pub const FOREIGN: &str = "XCLIP-42";
pub const PASTED: &str = "x-xclip-42";

/// A control-socket reply as the proof reads it.
#[must_use]
pub fn reply(text: &str) -> &str {
    text.trim_end()
}

/// Why a control verb's run was not taken: the verb, what it answered and how it ended ([`Out::status`]).
#[must_use]
pub(crate) fn refused(verb: &[&str], out: &Out) -> String {
    format!(
        "`{}` answered {:?} ({})",
        verb.join(" "),
        crate::util::one_line(reply(&out.text), 200),
        out.status()
    )
}

/// The screen row (0-based, as `select line` takes it) that holds [`ECHO`] alone: the output of [`TYPED`], never the typed line.
#[must_use]
pub fn echo_row(screen: &str) -> Option<usize> {
    screen.lines().position(|l| l.trim_end() == ECHO)
}

/// The scratch HOME's `.bashrc` (a login shell reads it through `.bash_profile`).
pub const BASHRC: &str = "PS1='lane$ '\n";

/// The prompt [`BASHRC`] sets, as a row of `text` shows it (trailing blanks trimmed).
pub const PROMPT: &str = "lane$";

/// One aterm-gui under `display` (or `--headless` with none), driven over its control socket in the guest.
pub(crate) struct Gui<'a> {
    pub(crate) up: &'a Up,
    /// Where aterm-gui and aterm-ctl are, in the guest.
    pub(crate) bin: String,
    /// The scratch world: HOME, config, runtime dir.
    pub(crate) dir: String,
    /// `<arch>-<display>`, for the files kept on this Mac.
    pub(crate) name: String,
    /// Draw on the CPU (`--cpu`).
    pub(crate) cpu: bool,
    /// The stats Deck on (`[sys] stats_bars`, aterm's default) rather than held off.
    pub(crate) deck: bool,
}

/// The lane's session config holding the stats Deck off (`Gui::deck`): appended after [`crate::instance::CONFIG_OFF`]'s tables.
pub(crate) const DECK_OFF: &str = "[sys]\nstats_bars = \"off\"\n";

impl Gui<'_> {
    pub(crate) fn sock(&self) -> String {
        format!("{}/run/aterm/aterm.sock", self.dir)
    }

    pub(crate) fn ctl(&self, verb: &[&str]) -> Out {
        self.up.sh(&self.ctl_argv(verb))
    }

    /// The guest argv of one `aterm-ctl` verb against this aterm: the verb's words last.
    pub(crate) fn ctl_argv(&self, verb: &[&str]) -> Vec<String> {
        let mut argv = vec![
            "env".to_string(),
            format!("XDG_RUNTIME_DIR={}/run", self.dir),
            format!("XDG_CONFIG_HOME={}/cfg", self.dir),
            format!("{}/aterm-ctl", self.bin),
            "--sock".into(),
            self.sock(),
            "--timeout".into(),
            "90".into(),
        ];
        argv.extend(verb.iter().map(|s| (*s).to_string()));
        argv
    }

    /// What this one start of the unit logged: its invocation's lines only (the unit's name is reused run after run).
    pub(crate) fn journal(&self, invocation: &str) -> String {
        if invocation.is_empty() {
            return String::new();
        }
        self.up
            .sh(&[
                "journalctl".into(),
                "--user".into(),
                "--no-pager".into(),
                "-o".into(),
                "cat".into(),
                format!("_SYSTEMD_INVOCATION_ID={invocation}"),
            ])
            .text
    }

    /// Make the scratch world.
    pub(crate) fn prepare(&self) -> Result<(), String> {
        let d = &self.dir;
        let mut config = if self.cpu {
            format!(
                "cursor_blink = false\ncursor_trail = false\n{}",
                crate::instance::CONFIG_OFF
            )
        } else {
            crate::instance::CONFIG_OFF.to_string()
        };
        if !self.deck {
            config.push_str(DECK_OFF);
        }
        let steps: [(Vec<String>, Option<&str>); 5] = [
            (vec!["rm".into(), "-rf".into(), d.clone()], None),
            (
                ["install", "-d", "-m", "700"]
                    .iter()
                    .map(|s| (*s).to_string())
                    .chain(
                        ["", "/home", "/run", "/run/aterm", "/cfg", "/cfg/aterm"]
                            .iter()
                            .map(|s| format!("{d}{s}")),
                    )
                    .collect(),
                None,
            ),
            (
                vec!["tee".into(), format!("{d}/cfg/aterm/aterm.toml")],
                Some(config.as_str()),
            ),
            (
                vec!["tee".into(), format!("{d}/home/.bashrc")],
                Some(BASHRC),
            ),
            (
                vec!["tee".into(), format!("{d}/home/.bash_profile")],
                Some(". ~/.bashrc\n"),
            ),
        ];
        for (argv, stdin) in steps {
            let out = self.up.lima.shell(&self.up.home, &argv, stdin);
            if !out.ok() {
                return Err(format!("{}: {}", argv.join(" "), out.tail(3)));
            }
        }
        Ok(())
    }

    fn drive(&self, display: Option<Display>, results: &Path) -> (Verdict, String) {
        if let Err(e) = self.prepare() {
            return (Verdict::NotRun, format!("scratch world: {e}"));
        }
        let invocation = match self.start(display, &[]) {
            Ok(id) => id,
            Err(why) => return (Verdict::Fail, why),
        };
        let result = match display {
            Some(d) if self.cpu => self.judge_cpu(d, results).map(|detail| (detail, None)),
            _ => self.judge(display, results),
        };
        let journal = self.journal(&invocation);
        self.stop();
        let result = result.and_then(|(detail, backend)| {
            let gpu = if self.cpu {
                renderer(&journal)
            } else {
                drew_on_gpu(&journal, backend.as_deref())?
            };
            Ok(match gpu {
                Some(r) => format!("{detail}; GPU: {r}"),
                None => detail,
            })
        });
        match result {
            Ok(detail) => (Verdict::Pass, detail),
            Err(why) => {
                let tail: Vec<&str> = journal.lines().collect();
                let tail = tail[tail.len().saturating_sub(12)..].join("\n    ");
                let kept = self.keep_crash_reports(results);
                (
                    Verdict::Fail,
                    format!("{why}\n    aterm-gui's journal:\n    {tail}{kept}"),
                )
            }
        }
    }

    /// The unit aterm-gui runs as.
    pub(crate) fn unit_name() -> String {
        format!("{}aterm", guest::UNIT_PREFIX)
    }

    /// Send `signal` (`SIGSTOP`, `SIGCONT`) to the unit's main process, aterm-gui itself when it was started logged.
    pub(crate) fn signal(&self, signal: &str) -> bool {
        self.up
            .sh(&[
                "systemctl".into(),
                "--user".into(),
                "kill".into(),
                "--kill-whom=main".into(),
                format!("--signal={signal}"),
                Self::unit_name(),
            ])
            .ok()
    }

    /// Start aterm-gui as a lane unit in the prepared world, with `extra` added to its environment.
    ///
    /// # Errors
    /// `systemd-run` refused it.
    pub(crate) fn start(
        &self,
        display: Option<Display>,
        extra: &[(String, String)],
    ) -> Result<String, String> {
        self.start_with(display, extra, None)
    }

    /// [`Gui::start`], with aterm-gui's stderr written to the guest file `stderr` instead of the journal.
    ///
    /// # Errors
    /// `systemd-run` refused it.
    pub(crate) fn start_logged(
        &self,
        display: Option<Display>,
        extra: &[(String, String)],
        stderr: &str,
    ) -> Result<String, String> {
        self.start_with(display, extra, Some(stderr))
    }

    /// [`Gui::start_logged`] on a display that is not one of the lane's [`Display`]s (Mutter's own Xwayland, whose number and authority file Mutter picks).
    ///
    /// # Errors
    /// `systemd-run` refused it.
    pub(crate) fn start_logged_on(
        &self,
        client: &[(String, String)],
        stderr: &str,
    ) -> Result<String, String> {
        self.start_unit(Some(client), &[], Some(stderr), false)
    }

    /// [`Gui::start_logged`] under `display`, with aterm-gui's exit status (`$?`: 128 plus the signal when one killed it) written to the guest file `status` once it ends, whole.
    ///
    /// # Errors
    /// `systemd-run` refused it.
    pub(crate) fn start_watched(
        &self,
        display: Display,
        stderr: &str,
        status: &str,
    ) -> Result<String, String> {
        self.start_unit(
            Some(&[display.client_env(&self.up.runtime)]),
            &[("ATERM_LANE_STATUS".into(), status.into())],
            Some(stderr),
            true,
        )
    }

    /// [`Gui::start_logged_on`] with `extra` added to its environment.
    ///
    /// # Errors
    /// `systemd-run` refused it.
    pub(crate) fn start_logged_with(
        &self,
        client: &[(String, String)],
        extra: &[(String, String)],
        stderr: &str,
    ) -> Result<String, String> {
        self.start_unit(Some(client), extra, Some(stderr), false)
    }

    /// [`Gui::start_watched`] on a display that is not one of the lane's [`Display`]s: `client` is what aterm-gui is told to reach it by.
    ///
    /// # Errors
    /// `systemd-run` refused it.
    pub(crate) fn start_watched_on(
        &self,
        client: &[(String, String)],
        stderr: &str,
        status: &str,
    ) -> Result<String, String> {
        self.start_unit(
            Some(client),
            &[("ATERM_LANE_STATUS".into(), status.into())],
            Some(stderr),
            true,
        )
    }

    fn start_with(
        &self,
        display: Option<Display>,
        extra: &[(String, String)],
        stderr: Option<&str>,
    ) -> Result<String, String> {
        let client = display.map(|d| [d.client_env(&self.up.runtime)]);
        self.start_unit(client.as_ref().map(|c| &c[..]), extra, stderr, false)
    }

    fn start_unit(
        &self,
        client: Option<&[(String, String)]>,
        extra: &[(String, String)],
        stderr: Option<&str>,
        watched: bool,
    ) -> Result<String, String> {
        let mut env = vec![
            ("HOME".to_string(), format!("{}/home", self.dir)),
            ("XDG_RUNTIME_DIR".into(), format!("{}/run", self.dir)),
            ("XDG_CONFIG_HOME".into(), format!("{}/cfg", self.dir)),
            ("SHELL".into(), "/bin/bash".into()),
            (
                "DBUS_SESSION_BUS_ADDRESS".into(),
                format!("unix:path={}/bus", self.up.runtime),
            ),
        ];
        let mut argv = vec![
            format!("{}/aterm-gui", self.bin),
            "--control-sock".into(),
            self.sock(),
        ];
        if self.cpu {
            argv.push("--cpu".into());
        }
        match client {
            Some(client) => env.extend(client.iter().cloned()),
            None => argv.push("--headless".into()),
        }
        env.extend(extra.iter().cloned());
        if let Some(path) = stderr {
            env.push(("ATERM_LANE_STDERR".into(), path.into()));
            let script = if watched {
                "\"$0\" \"$@\" 2>\"$ATERM_LANE_STDERR\"; echo $? >\"$ATERM_LANE_STATUS.part\" \
                 && mv \"$ATERM_LANE_STATUS.part\" \"$ATERM_LANE_STATUS\""
            } else {
                "exec \"$0\" \"$@\" 2>\"$ATERM_LANE_STDERR\""
            };
            let mut wrapped = vec!["sh".to_string(), "-c".into(), script.into()];
            wrapped.extend(argv);
            argv = wrapped;
        }
        let unit = guest::Unit {
            name: Self::unit_name(),
            env,
            argv,
            socket: self.sock(),
        };
        let started = self
            .up
            .sh(&guest::run_unit(&unit, Some(&format!("{}/home", self.dir))));
        if !started.ok() {
            return Err(format!("systemd-run: {}", started.tail(3)));
        }
        Ok(self
            .up
            .sh(&[
                "systemctl".into(),
                "--user".into(),
                "show".into(),
                "--property=InvocationID".into(),
                "--value".into(),
                unit.name.clone(),
            ])
            .text
            .trim()
            .to_string())
    }

    /// Stop aterm-gui's unit.
    pub(crate) fn stop(&self) {
        let _ = self.up.sh(&[
            "systemctl".into(),
            "--user".into(),
            "stop".into(),
            Self::unit_name(),
        ]);
    }

    /// Copy the crash reports this instance wrote to this Mac, beside the frames (the guest's `/tmp` is emptied at its next boot).
    pub(crate) fn keep_crash_reports(&self, results: &Path) -> String {
        let found = self.up.sh(&[
            "find".into(),
            format!("{}/home", self.dir),
            "-name".into(),
            "crash-*.log".into(),
        ]);
        let mut kept = Vec::new();
        for guest_path in found.text.lines().filter(|l| l.starts_with('/')) {
            let file = guest_path.rsplit('/').next().unwrap_or("crash.log");
            let name = format!("{}-{file}", self.name);
            if self.up.lima.copy_out(guest_path, &results.join(&name)).ok() {
                kept.push(name);
            }
        }
        if kept.is_empty() {
            String::new()
        } else {
            format!("\n    crash report kept: {}", kept.join(", "))
        }
    }

    /// Send `verb`; its reply when it exited 0 and `ok` accepts it, else [`refused`].
    pub(crate) fn expect(
        &self,
        verb: &[&str],
        ok: &dyn Fn(&str) -> bool,
    ) -> Result<String, String> {
        let out = self.ctl(verb);
        let reply = reply(&out.text).to_string();
        if out.ok() && ok(&reply) {
            Ok(reply)
        } else {
            Err(refused(verb, &out))
        }
    }

    fn judge_cpu(&self, display: Display, results: &Path) -> Result<String, String> {
        if !self.up.sh(&guest::wait_socket(&self.sock(), 60)).ok() {
            return Err("the control socket never listened (60 s)".into());
        }
        self.expect(&["ready", "60000"], &|r| r.starts_with("OK ready"))?;
        self.expect(&["send", TYPED], &|r| r.starts_with("OK"))?;
        self.expect(&["key", "enter"], &|r| r.starts_with("OK"))?;
        self.expect(&["await", "match", ECHO, "timeout=60000"], &|r| {
            r.starts_with("OK match")
        })?;
        self.expect(&["resize", "30", "100"], &|r| r.starts_with("OK"))?;
        self.expect(&["dims"], &|r| dims_are(r, 30, 100))?;
        let status = self.expect(&["status"], &|r| field(r, "render") == Some("ok"))?;
        if field(&status, "backend").is_some_and(|b| b != "cpu") {
            return Err(format!("not on the CPU: {status}"));
        }
        let resized = self.held_to_server(display, results, "")?;
        self.expect(&["send", MORE], &|r| r.starts_with("OK"))?;
        self.expect(&["await", "match", MORE, "timeout=60000"], &|r| {
            r.starts_with("OK match")
        })?;
        let typed = self.held_to_server(display, results, "-typed")?;
        Ok(format!(
            "typed, {ECHO} echoed, resized to 30x100, render=ok on the CPU; {resized}; \
             after `{MORE}` typed, {typed}"
        ))
    }

    /// Photograph the presented frame (`image`, kept as `<name><suffix>.png`) and hold it to the display server's own pixels.
    pub(crate) fn held_to_server(
        &self,
        display: Display,
        results: &Path,
        suffix: &str,
    ) -> Result<String, String> {
        self.held_to_server_but(display, results, suffix, 0..0)
    }

    /// [`Gui::held_to_server`], but for the frame's rows in `live`.
    pub(crate) fn held_to_server_but(
        &self,
        display: Display,
        results: &Path,
        suffix: &str,
        live: std::ops::Range<usize>,
    ) -> Result<String, String> {
        let frame = format!("{}{suffix}.png", self.name);
        let image = self.expect(&["image", &frame], &|r| r.starts_with("OK "))?;
        let guest_png = image
            .split_whitespace()
            .nth(3)
            .ok_or_else(|| format!("`image` answered {image}"))?;
        let png = results.join(&frame);
        let copied = self.up.lima.copy_out(guest_png, &png);
        if !copied.ok() {
            return Err(format!("{guest_png} not copied out: {}", copied.tail(2)));
        }
        let presented = std::fs::read(&png)
            .ok()
            .and_then(|b| aterm_png::decode_rgba8(&b, 8192))
            .ok_or_else(|| format!("{}: not a PNG this lab decodes", png.display()))?;
        let (w, h) = (presented.1, presented.2);
        match display {
            Display::Xvfb | Display::Xwayland => {
                let ppm = self.x_grab(display, results, suffix)?;
                let server = std::fs::read(&ppm)
                    .ok()
                    .and_then(|b| parse_ppm(&b))
                    .ok_or_else(|| format!("{}: not a PPM this lab reads", ppm.display()))?;
                let (sw, sh) = (server.1, server.2);
                if (sw, sh) == (w, h) {
                    compare_rgb(&presented.0, w, h, &server, live).map_err(|why| {
                        format!("{why} (frame {frame}, server {})", ppm.display())
                    })?;
                    return Ok(format!(
                        "the server's {w}x{h} window pixels (lane-bus xgrab) equal the \
                         presented frame exactly"
                    ));
                }
                let d = self.expect(&["dims"], &|r| r.starts_with("OK "))?;
                let num = |k: &str| field(&d, k).and_then(|v| v.parse::<i64>().ok());
                if (num("surface_w"), num("surface_h"))
                    != (i64::try_from(sw).ok(), i64::try_from(sh).ok())
                {
                    return Err(format!(
                        "the server's window is {sw}x{sh}, the frame {w}x{h}, and `dims` \
                         names another surface: {d}"
                    ));
                }
                let (ox, oy) = (num("offset_x").unwrap_or(0), num("offset_y").unwrap_or(0));
                let window = (
                    server
                        .0
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .flat_map(|p| [p[0], p[1], p[2], 0xff])
                        .collect::<Vec<u8>>(),
                    sw,
                    sh,
                );
                let whole = WestonView {
                    x: 0,
                    y: 0,
                    width: sw,
                    height: sh,
                    format: "XRGB8888".into(),
                };
                held_at(&window, &presented, ox, oy, live)
                    .map_err(|why| format!("{why} (frame {frame}, server {})", ppm.display()))?;
                let n = bands_held(&window, &presented, &whole, (ox, oy))
                    .map_err(|why| format!("{why} (frame {frame}, server {})", ppm.display()))?;
                Ok(format!(
                    "the server's {sw}x{sh} window pixels (lane-bus xgrab) hold the presented \
                     {w}x{h} frame exactly at ({ox},{oy}), and its {n} band pixels each the \
                     frame's nearest pixel"
                ))
            }
            Display::Weston | Display::WestonX11 => {
                let view = self.weston_view(display)?;
                if view.format != "XRGB8888" {
                    return Err(format!(
                        "weston shows a {}x{} {} buffer for the {w}x{h} frame {frame}",
                        view.width, view.height, view.format
                    ));
                }
                let (ox, oy) = if (view.width, view.height) == (w, h) {
                    (0, 0)
                } else {
                    let d = self.expect(&["dims"], &|r| r.starts_with("OK "))?;
                    let num = |k: &str| field(&d, k).and_then(|v| v.parse::<i64>().ok());
                    let surface = (num("surface_w"), num("surface_h"));
                    let shown = (
                        i64::try_from(view.width).ok(),
                        i64::try_from(view.height).ok(),
                    );
                    if surface != shown {
                        return Err(format!(
                            "weston shows a {}x{} buffer for the {w}x{h} frame {frame}, and \
                             `dims` names a surface of {:?}",
                            view.width, view.height, surface
                        ));
                    }
                    (num("offset_x").unwrap_or(0), num("offset_y").unwrap_or(0))
                };
                let shot = self.weston_shot(display, results, suffix)?;
                let output = std::fs::read(&shot)
                    .ok()
                    .and_then(|b| aterm_png::decode_rgba8(&b, 8192))
                    .ok_or_else(|| format!("{}: not a PNG this lab decodes", shot.display()))?;
                let (vw, vh) = held_at(&output, &presented, view.x + ox, view.y + oy, live)
                    .map_err(|why| format!("{why} (frame {frame}, output {})", shot.display()))?;
                let held = if (vw, vh) == (w, h) {
                    format!("the presented {w}x{h} frame exactly")
                } else {
                    format!("the {vw}x{vh} of the presented {w}x{h} frame that is on it exactly")
                };
                let bands = if (view.width, view.height) == (w, h) {
                    String::new()
                } else {
                    let n = bands_held(&output, &presented, &view, (ox, oy)).map_err(|why| {
                        format!("{why} (frame {frame}, output {})", shot.display())
                    })?;
                    format!(
                        " at ({ox},{oy}) in it, and its {n} band pixels each the frame's nearest \
                         pixel"
                    )
                };
                Ok(format!(
                    "weston's {}x{} output (weston-screenshooter) shows {held}, where its scene \
                     graph puts the window's {}x{} {} buffer ({},{}){bands}",
                    output.1, output.2, view.width, view.height, view.format, view.x, view.y
                ))
            }
            Display::Mutter => {
                let shot = self.mutter_cast(results, suffix)?;
                let output = std::fs::read(&shot)
                    .ok()
                    .and_then(|b| aterm_png::decode_rgba8(&b, 8192))
                    .ok_or_else(|| format!("{}: not a PNG this lab decodes", shot.display()))?;
                let (fx, fy) = match frame_places(&output, &presented, live.clone()).as_slice() {
                    [one] => *one,
                    [] => {
                        let centred = held_at(
                            &output,
                            &presented,
                            (output.1 as i64 - w as i64) / 2,
                            (output.2 as i64 - h as i64) / 2,
                            live,
                        )
                        .err()
                        .unwrap_or_default();
                        return Err(format!(
                            "Mutter's {}x{} monitor (lane-bus cast) shows the presented {w}x{h} \
                             frame {frame} nowhere exactly; centred: {centred} (monitor {})",
                            output.1,
                            output.2,
                            shot.display()
                        ));
                    }
                    _ => {
                        return Err(format!(
                            "Mutter's monitor shows the presented {w}x{h} frame {frame} in more \
                             than one place (monitor {})",
                            shot.display()
                        ));
                    }
                };
                let d = self.expect(&["dims"], &|r| r.starts_with("OK "))?;
                let num = |k: &str| field(&d, k).and_then(|v| v.parse::<i64>().ok());
                let surface = (
                    num("surface_w").and_then(|v| usize::try_from(v).ok()),
                    num("surface_h").and_then(|v| usize::try_from(v).ok()),
                );
                let bands = match surface {
                    (Some(sw), Some(sh)) if (sw, sh) != (w, h) => {
                        let (ox, oy) = (num("offset_x").unwrap_or(0), num("offset_y").unwrap_or(0));
                        let view = WestonView {
                            x: fx as i64 - ox,
                            y: fy as i64 - oy,
                            width: sw,
                            height: sh,
                            format: "XRGB8888".into(),
                        };
                        let n =
                            bands_held(&output, &presented, &view, (ox, oy)).map_err(|why| {
                                format!("{why} (frame {frame}, monitor {})", shot.display())
                            })?;
                        format!(
                            ", {ox},{oy} into its {sw}x{sh} surface, whose {n} band pixels are \
                             each the frame's nearest pixel"
                        )
                    }
                    _ => String::new(),
                };
                Ok(format!(
                    "Mutter's {}x{} monitor (lane-bus cast, a ScreenCast frame through PipeWire) \
                     shows the presented {w}x{h} frame exactly at ({fx},{fy}){bands}",
                    output.1, output.2
                ))
            }
        }
    }

    fn mutter_cast(&self, results: &Path, suffix: &str) -> Result<PathBuf, String> {
        let pid = unit_pid(self.up, &format!("{}mutter", guest::UNIT_PREFIX))
            .ok_or_else(|| "Mutter's unit has no main pid".to_string())?;
        let guest_png = format!("{}/{}{suffix}-monitor.png", self.dir, self.name);
        let env = [
            (
                "DBUS_SESSION_BUS_ADDRESS".to_string(),
                format!("unix:path={}/bus", self.up.runtime),
            ),
            ("XDG_RUNTIME_DIR".to_string(), self.up.runtime.clone()),
        ];
        let cast = self.up.sh(&guest::with_env(
            &env,
            &[
                format!("{}/lane-bus", self.bin),
                "cast".into(),
                pid,
                guest_png.clone(),
            ],
        ));
        if !cast.ok() {
            return Err(format!("lane-bus cast: {}", cast.tail(3)));
        }
        let local = results.join(format!("{}{suffix}-monitor.png", self.name));
        let copied = self.up.lima.copy_out(&guest_png, &local);
        if !copied.ok() {
            return Err(format!("{guest_png} not copied out: {}", copied.tail(2)));
        }
        Ok(local)
    }

    fn x_grab(&self, display: Display, results: &Path, suffix: &str) -> Result<PathBuf, String> {
        let x = vec![display.client_env(&self.up.runtime)];
        let pid = unit_pid(self.up, &Self::unit_name())
            .ok_or_else(|| "aterm-gui's unit has no main pid".to_string())?;
        let found = self.up.sh(&guest::with_env(
            &x,
            &[
                "timeout".into(),
                "60".into(),
                "xdotool".into(),
                "search".into(),
                "--sync".into(),
                "--pid".into(),
                pid.clone(),
            ],
        ));
        if !found.ok() {
            return Err(format!(
                "xdotool search for aterm-gui (pid {pid}): {}",
                found.tail(3)
            ));
        }
        let win = found
            .text
            .lines()
            .find(|l| !l.trim().is_empty() && l.trim().bytes().all(|b| b.is_ascii_digit()))
            .map(|l| l.trim().to_string())
            .ok_or_else(|| format!("xdotool found no window for aterm-gui (pid {pid})"))?;
        let guest_ppm = format!("{}/{}{suffix}.ppm", self.dir, self.name);
        let grab = self.up.sh(&guest::with_env(
            &x,
            &[
                format!("{}/lane-bus", self.bin),
                "xgrab".into(),
                win.clone(),
                guest_ppm.clone(),
            ],
        ));
        if !grab.ok() {
            return Err(format!("lane-bus xgrab {win}: {}", grab.tail(3)));
        }
        let ppm = results.join(format!("{}{suffix}.ppm", self.name));
        let copied = self.up.lima.copy_out(&guest_ppm, &ppm);
        if !copied.ok() {
            return Err(format!("{guest_ppm} not copied out: {}", copied.tail(2)));
        }
        Ok(ppm)
    }

    fn weston_view(&self, display: Display) -> Result<WestonView, String> {
        let pid = unit_pid(self.up, &Self::unit_name())
            .ok_or_else(|| "aterm-gui's unit has no main pid".to_string())?;
        let dump = self.up.sh(&guest::with_env(
            &[display.client_env(&self.up.runtime)],
            &[
                "timeout".into(),
                "60".into(),
                "weston-debug".into(),
                "scene-graph".into(),
            ],
        ));
        read_weston_view(&dump, &pid)
    }

    fn weston_shot(
        &self,
        display: Display,
        results: &Path,
        suffix: &str,
    ) -> Result<PathBuf, String> {
        let dir = format!("{}/shots", self.dir);
        let shot = self.up.sh(&guest::with_env(
            &[display.client_env(&self.up.runtime)],
            &[
                "sh".into(),
                "-c".into(),
                format!(
                    "rm -rf {dir} && mkdir -p {dir} && cd {dir} && \
                     exec timeout 60 weston-screenshooter"
                ),
            ],
        ));
        if !shot.ok() {
            return Err(format!("weston-screenshooter: {}", shot.tail(3)));
        }
        let listed = self.up.sh(&["ls".into(), dir.clone()]);
        if !listed.ok() {
            return Err(format!(
                "listing weston-screenshooter output in {dir}: {}",
                listed.tail(3)
            ));
        }
        let name = listed
            .text
            .lines()
            .map(str::trim)
            .find(|l| l.ends_with(".png"))
            .ok_or_else(|| format!("weston-screenshooter wrote no PNG in {dir}"))?;
        let local = results.join(format!("{}{suffix}-output.png", self.name));
        let copied = self.up.lima.copy_out(&format!("{dir}/{name}"), &local);
        if !copied.ok() {
            return Err(format!("{dir}/{name} not copied out: {}", copied.tail(2)));
        }
        Ok(local)
    }

    fn judge(
        &self,
        display: Option<Display>,
        results: &Path,
    ) -> Result<(String, Option<String>), String> {
        let listening = self.up.sh(&guest::wait_socket(&self.sock(), 60));
        if !listening.ok() {
            return Err("the control socket never listened (60 s)".into());
        }
        self.expect(&["ready", "60000"], &|r| r.starts_with("OK ready"))?;
        let before = self.expect(&["dims"], &|r| r.starts_with("OK "))?;
        self.expect(&["send", TYPED], &|r| r.starts_with("OK"))?;
        self.expect(&["key", "enter"], &|r| r.starts_with("OK"))?;
        self.expect(&["await", "match", ECHO, "timeout=60000"], &|r| {
            r.starts_with("OK match")
        })?;
        self.expect(&["resize", "30", "100"], &|r| r.starts_with("OK"))?;
        self.expect(&["dims"], &|r| dims_are(r, 30, 100))?;
        let clipboard = match display {
            Some(d) if d.clipboard_scriptable() => {
                let (_, x) = d.client_env(&self.up.runtime);
                let screen = self.expect(&["text"], &|r| echo_row(r).is_some())?;
                let row = echo_row(&screen).unwrap_or_default().to_string();
                self.expect(&["select", "line", &row], &|r| r.starts_with("OK"))?;
                let copied = format!("OK {}", ECHO.len());
                self.expect(&["copy"], &|r| r == copied)?;
                let read = self
                    .up
                    .sh(&guest::read_clipboard(&x, guest::Selection::Clipboard, 60));
                if !read.ok() || read.text.lines().next() != Some(ECHO) {
                    return Err(format!(
                        "xclip read CLIPBOARD as {:?} after aterm's copy (exit {:?})",
                        crate::util::one_line(&read.text, 200),
                        read.code
                    ));
                }
                let owner = guest::clipboard_owner(&x, guest::Selection::Clipboard, FOREIGN);
                let started = self.up.sh(&guest::run_unit(&owner, None));
                if !started.ok() {
                    return Err(format!("xclip as owner: {}", started.tail(3)));
                }
                if !self
                    .up
                    .sh(&guest::wait_clipboard(
                        &x,
                        guest::Selection::Clipboard,
                        FOREIGN,
                        60,
                    ))
                    .ok()
                {
                    return Err(format!("xclip never owned CLIPBOARD with {FOREIGN} (60 s)"));
                }
                self.expect(&["send", PASTE_BEFORE], &|r| r.starts_with("OK"))?;
                self.expect(&["invoke", "unread=ok", "Paste"], &|r| r.starts_with("OK"))?;
                self.expect(&["send", PASTE_AFTER], &|r| r.starts_with("OK"))?;
                self.expect(&["key", "enter"], &|r| r.starts_with("OK"))?;
                self.expect(&["await", "match", PASTED, "timeout=60000"], &|r| {
                    r.starts_with("OK match")
                })?;
                format!(", clipboard {ECHO} copied and read by xclip, xclip's {FOREIGN} pasted")
            }
            Some(_) => ", clipboard not driven (a Wayland selection needs an input seat)".into(),
            None => String::new(),
        };
        let status = self.expect(&["status"], &|r| field(r, "render") == Some("ok"))?;
        let metrics = self.expect(&["metrics", "percentiles"], &|r| r.starts_with("OK"))?;
        let frame = format!("{}.png", self.name);
        let image = self.expect(&["image", &frame], &|r| r.starts_with("OK "))?;
        let kept = image
            .split_whitespace()
            .nth(3)
            .map(|guest_path| {
                let to = results.join(&frame);
                self.up.lima.copy_out(guest_path, &to).ok()
            })
            .unwrap_or(false);
        let summary = self.expect(&["metrics"], &|r| r.starts_with("OK "))?;
        let size = |r: &str| {
            let w: Vec<&str> = r.split_whitespace().collect();
            format!("{}x{}", w.get(1).unwrap_or(&"?"), w.get(2).unwrap_or(&"?"))
        };
        Ok((
            format!(
                "typed, {ECHO} echoed, resized {} -> 30x100{clipboard}; render=ok resizes={} phase={}; {}; frame {}",
                size(&before),
                field(&status, "resizes").unwrap_or("?"),
                field(&status, "phase").unwrap_or("?"),
                present_summary(&metrics),
                if kept {
                    frame
                } else {
                    format!("{frame} (not copied out)")
                }
            ),
            field(&summary, "backend").map(str::to_string),
        ))
    }
}

/// A binary PPM (P6, maxval 255): its RGB bytes, width and height.
pub(crate) fn parse_ppm(b: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while b.get(i)?.is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while !b.get(i)?.is_ascii_whitespace() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&b[start..i]).ok()?.to_string());
    }
    i += 1;
    if fields[0] != "P6" || fields[3] != "255" {
        return None;
    }
    let (w, h): (usize, usize) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let rgb = b.get(i..i.checked_add(w.checked_mul(h)?.checked_mul(3)?)?)?;
    Some((rgb.to_vec(), w, h))
}

/// Whether `server` (RGB) shows exactly the RGB of `rgba` (`w` x `h`), but for the rows in `live` (the stats Deck's, whose figures move while the frame is read twice: `Gui::held_to_server_but`).
pub(crate) fn compare_rgb(
    rgba: &[u8],
    w: usize,
    h: usize,
    server: &(Vec<u8>, usize, usize),
    live: std::ops::Range<usize>,
) -> Result<(), String> {
    let (rgb, sw, sh) = server;
    if (*sw, *sh) != (w, h) {
        return Err(format!(
            "the server's window is {sw}x{sh}, the frame {w}x{h}"
        ));
    }
    let mut differ = 0usize;
    let mut first = None;
    for (n, (p, q)) in rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rgb.as_chunks::<3>().0)
        .enumerate()
    {
        if p[..3] != *q && !live.contains(&(n / w.max(1))) {
            differ += 1;
            first.get_or_insert((
                n % w.max(1),
                n / w.max(1),
                [p[0], p[1], p[2]],
                [q[0], q[1], q[2]],
            ));
        }
    }
    match first {
        None => Ok(()),
        Some((x, y, want, got)) => Err(format!(
            "{differ} of {} pixels differ; first at ({x},{y}): presented {want:02x?}, server {got:02x?}",
            w * h
        )),
    }
}

/// A toplevel's view in weston's scene graph (`weston-debug scene-graph`): where the compositor shows it, and the buffer it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WestonView {
    pub(crate) x: i64,
    pub(crate) y: i64,
    pub(crate) width: usize,
    pub(crate) height: usize,
    /// The buffer's format line's name (`XRGB8888`).
    pub(crate) format: String,
}

/// The `xdg_toplevel` view of process `pid` in a `weston-debug scene-graph` dump.
pub(crate) fn weston_view(dump: &str, pid: &str) -> Option<WestonView> {
    let owner = format!("role xdg_toplevel, PID {pid},");
    let mut lines = dump.lines().map(str::trim);
    lines.find(|l| l.starts_with("View ") && l.contains(&owner))?;
    let (mut at, mut size, mut format) = (None, None, None);
    for l in lines.take_while(|l| !l.starts_with("View ") && !l.starts_with("Layer ")) {
        if let Some(rest) = l.strip_prefix("position: (") {
            let (x, rest) = rest.split_once(", ")?;
            let (y, _) = rest.split_once(')')?;
            at = Some((x.parse().ok()?, y.parse().ok()?));
        } else if let Some(rest) = l.strip_prefix("width: ") {
            let (w, h) = rest.split_once(", height: ")?;
            size = Some((w.parse().ok()?, h.parse().ok()?));
        } else if let Some(rest) = l.strip_prefix("format: ") {
            format = rest.split_whitespace().nth(1).map(str::to_string);
        }
    }
    let ((x, y), (width, height)) = (at?, size?);
    Some(WestonView {
        x,
        y,
        width,
        height,
        format: format?,
    })
}

fn read_weston_view(dump: &Out, pid: &str) -> Result<WestonView, String> {
    if !dump.ok() {
        return Err(format!("weston-debug scene-graph failed: {}", dump.tail(5)));
    }
    weston_view(&dump.text, pid).ok_or_else(|| {
        format!(
            "weston's scene graph has no xdg_toplevel of pid {pid}: {}",
            dump.tail(5)
        )
    })
}

/// Whether `screen` (RGBA, a display's whole output) shows `frame` (RGBA) exactly, by RGB, with the frame's top-left corner at `x`, `y` (where the compositor put the window).
pub(crate) fn held_at(
    screen: &(Vec<u8>, usize, usize),
    frame: &(Vec<u8>, usize, usize),
    x: i64,
    y: i64,
    live: std::ops::Range<usize>,
) -> Result<(usize, usize), String> {
    let (s, sw, sh) = (&screen.0, screen.1, screen.2);
    let (f, w, h) = (&frame.0, frame.1, frame.2);
    if s.len() < sw * sh * 4 || f.len() < w * h * 4 {
        return Err(format!("the output is {sw}x{sh}, the frame {w}x{h}"));
    }
    let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
        return Err(format!(
            "the window is at {x},{y}, above or left of the output"
        ));
    };
    let (vw, vh) = (w.min(sw.saturating_sub(x)), h.min(sh.saturating_sub(y)));
    if w == 0 || h == 0 || vw * vh * 2 < w * h {
        return Err(format!(
            "only {vw}x{vh} of the {w}x{h} frame at {x},{y} is on the {sw}x{sh} output"
        ));
    }
    let mut region = Vec::with_capacity(vw * vh * 3);
    let mut sub = Vec::with_capacity(vw * vh * 4);
    for row in 0..vh {
        let at = ((y + row) * sw + x) * 4;
        for p in s[at..at + vw * 4].as_chunks::<4>().0 {
            region.extend_from_slice(&p[..3]);
        }
        sub.extend_from_slice(&f[row * w * 4..(row * w + vw) * 4]);
    }
    compare_rgb(&sub, vw, vh, &(region, vw, vh), live)
        .map(|()| (vw, vh))
        .map_err(|why| format!("at {x},{y}, {vw}x{vh} of the frame on the output: {why}"))
}

/// Where on `screen` (RGBA) the whole of `frame` (RGBA) shows exactly, by RGB, its rows in `live` aside.
pub(crate) fn frame_places(
    screen: &(Vec<u8>, usize, usize),
    frame: &(Vec<u8>, usize, usize),
    live: std::ops::Range<usize>,
) -> Vec<(usize, usize)> {
    let (s, sw, sh) = (&screen.0, screen.1, screen.2);
    let (f, w, h) = (&frame.0, frame.1, frame.2);
    let rows: Vec<usize> = (0..h).filter(|r| !live.contains(r)).collect();
    if w == 0 || w > sw || h > sh || s.len() < sw * sh * 4 || f.len() < w * h * 4 {
        return Vec::new();
    }
    let (Some(&top), Some(&bottom)) = (rows.first(), rows.last()) else {
        return Vec::new();
    };
    let middle = rows[rows.len() / 2];
    let probes = [(0, top), (w - 1, bottom), (w / 2, middle)];
    let rgb = |buf: &[u8], stride: usize, x: usize, y: usize| {
        let at = (y * stride + x) * 4;
        [buf[at], buf[at + 1], buf[at + 2]]
    };
    let mut found = Vec::new();
    for y in 0..=sh - h {
        for x in 0..=sw - w {
            if probes
                .iter()
                .any(|&(px, py)| rgb(s, sw, x + px, y + py) != rgb(f, w, px, py))
            {
                continue;
            }
            let whole = rows.iter().all(|&r| {
                let (a, b) = (((y + r) * sw + x) * 4, r * w * 4);
                s[a..a + w * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(f[b..b + w * 4].as_chunks::<4>().0)
                    .all(|(p, q)| p[..3] == q[..3])
            });
            if whole {
                found.push((x, y));
                if found.len() == 2 {
                    return found;
                }
            }
        }
    }
    found
}

/// Whether the bands of a window's buffer that a frame smaller than it leaves (`frame` at `offset` in the buffer weston shows as `view`) are on `screen` (RGBA, weston's whole output) as the CPU presenter fills them.
pub(crate) fn bands_held(
    screen: &(Vec<u8>, usize, usize),
    frame: &(Vec<u8>, usize, usize),
    view: &WestonView,
    offset: (i64, i64),
) -> Result<usize, String> {
    let (s, sw, sh) = (&screen.0, screen.1, screen.2);
    let (f, w, h) = (&frame.0, frame.1, frame.2);
    if w == 0 || h == 0 || f.len() < w * h * 4 || s.len() < sw * sh * 4 {
        return Err(format!("the output is {sw}x{sh}, the frame {w}x{h}"));
    }
    let (fx, fy) = (view.x + offset.0, view.y + offset.1);
    let (mut held, mut differ, mut first) = (0usize, 0usize, None);
    for by in 0..view.height {
        for bx in 0..view.width {
            let (x, y) = (view.x + bx as i64, view.y + by as i64);
            let (dx, dy) = (x - fx, y - fy);
            let inside = (0..w as i64).contains(&dx) && (0..h as i64).contains(&dy);
            let (Ok(ux), Ok(uy)) = (usize::try_from(x), usize::try_from(y)) else {
                continue;
            };
            if inside || ux >= sw || uy >= sh {
                continue;
            }
            let (nx, ny) = (
                dx.clamp(0, w as i64 - 1) as usize,
                dy.clamp(0, h as i64 - 1) as usize,
            );
            let at = (uy * sw + ux) * 4;
            let want = (ny * w + nx) * 4;
            held += 1;
            if s[at..at + 3] != f[want..want + 3] {
                differ += 1;
                first.get_or_insert((
                    bx,
                    by,
                    [f[want], f[want + 1], f[want + 2]],
                    [s[at], s[at + 1], s[at + 2]],
                ));
            }
        }
    }
    match first {
        None => Ok(held),
        Some((x, y, want, got)) => Err(format!(
            "{differ} of {held} band pixels differ; first at ({x},{y}) of the buffer: the \
             frame's nearest {want:02x?}, the output {got:02x?}"
        )),
    }
}

/// `linux proof …`.
///
/// # Errors
/// A usage error, or the lane could not be brought up (NOT RUN).
pub fn cmd_proof(args: &[String]) -> Res {
    run(&parse_proof(args)?, "proof")
}

/// `linux gone …`: the proof's display-gone rows alone ([`crate::linux_gone`]).
///
/// # Errors
/// A usage error, or the lane could not be brought up (NOT RUN).
pub fn cmd_gone(args: &[String]) -> Res {
    run(&parse_gone(args)?, "gone")
}

/// `linux cpu …`: the proof's CPU presenter rows alone.
///
/// # Errors
/// A usage error, or the lane could not be brought up (NOT RUN).
pub fn cmd_cpu(args: &[String]) -> Res {
    run(&parse_cpu(args)?, "cpu")
}

/// `linux deck …`: the proof's stats Deck layout rows alone ([`crate::linux_baseline::deck_row`]).
///
/// # Errors
/// A usage error, or the lane could not be brought up (NOT RUN).
pub fn cmd_deck(args: &[String]) -> Res {
    run(&parse_deck(args)?, "deck")
}

/// `linux edges …`: the proof's untested-edge rows alone ([`crate::linux_edges`]).
///
/// # Errors
/// A usage error, or the lane could not be brought up (NOT RUN).
pub fn cmd_edges(args: &[String]) -> Res {
    run(&parse_edges(args)?, "edges")
}

/// `linux a11y …`: the proof's accessibility runtime rows alone ([`crate::linux_a11y`]).
///
/// # Errors
/// A usage error, or the lane could not be brought up (NOT RUN).
pub fn cmd_a11y(args: &[String]) -> Res {
    run(&parse_a11y(args)?, "a11y")
}

fn run(ask: &ProofArgs, kind: &str) -> Res {
    crate::stop::install();
    let _lock = linux::LaneLock::take()?;
    let results = linux::state_dir()?.join(format!(
        "{kind}-{}",
        crate::util::compact_stamp(crate::util::now_secs())
    ));
    std::fs::create_dir_all(&results)
        .map_err(|e| Fail::die(format!("{}: {e}", results.display())))?;
    let t0 = Instant::now();
    let mut up = match linux::bring_up() {
        Ok(up) => up,
        Err(f) => {
            let _ = std::fs::remove_dir(&results);
            return Err(f);
        }
    };
    up.weston_renderer = ask.weston_renderer;
    let mut rows: Vec<Row> = up
        .timings
        .iter()
        .map(|step| Row {
            name: format!("vm {}", step.name),
            verdict: if step.failed.is_some() {
                Verdict::NotRun
            } else {
                Verdict::Pass
            },
            secs: step.secs,
            detail: step.failed.clone().unwrap_or_default(),
        })
        .collect();
    let lane = up.lane_dir();
    let repo = linux::repo();
    let mut stopped = None;
    let baseline =
        if ask.gone_only || ask.cpu_only || ask.a11y_only || ask.deck_only || ask.edges_only {
            None
        } else {
            match crate::linux_baseline::Baseline::begin(&up, &results, false) {
                Ok(b) => Some(b),
                Err(row) => {
                    rows.push(row);
                    None
                }
            }
        };
    'arches: for &arch in &ask.arches {
        let tag = format!("{} ({})", arch.name(), arch.runs());
        if arch == Arch::X86_64 {
            let reg = up.sh(&["cat".into(), guest::ROSETTA_BINFMT.into()]);
            if !guest::rosetta_registered(&reg.text) {
                rows.push(Row {
                    name: format!("{tag} every row"),
                    verdict: Verdict::NotRun,
                    secs: reg.secs,
                    detail: format!(
                        "Rosetta's binfmt registration is not enabled in the guest ({}): {}",
                        guest::ROSETTA_BINFMT,
                        reg.tail(3)
                    ),
                });
                continue;
            }
        }
        let env = guest::cargo_env(arch, &format!("{lane}/target"), false);
        let oracle_env = guest::cargo_env(arch, &format!("{lane}/libc-oracle-target"), false);
        let bin = arch.bin_dir(&format!("{lane}/target"));
        let a11y =
            (!ask.gone_only && !ask.cpu_only && !ask.deck_only).then(|| crate::linux_a11y::Cell {
                up: &up,
                arch,
                tag: tag.clone(),
                repo: repo.clone(),
                env: env.clone(),
                bin: bin.clone(),
                results: &results,
            });
        let cargo = |env: &[(String, String)], manifest: &str, verb: &str, extra: &[&str]| {
            let mut argv = vec![
                "cargo".to_string(),
                verb.into(),
                "--locked".into(),
                "--manifest-path".into(),
                manifest.into(),
            ];
            argv.extend(arch.target_args());
            argv.extend(extra.iter().map(|s| (*s).to_string()));
            guest::with_env(env, &argv)
        };
        if !ask.gone_only && !ask.cpu_only && !ask.a11y_only && !ask.deck_only && !ask.edges_only {
            let mut tests = vec!["--no-fail-fast", "--tests"];
            for p in TESTED {
                tests.extend(["-p", p]);
            }
            tests.extend(["--", "--nocapture"]);
            if arch == Arch::X86_64 {
                tests.push(ROSETTA_ONE_AT_A_TIME);
            }
            println!("\n=== {tag}: aterm-uds, aterm-pty, aterm-libc tests ===");
            let manifest = format!("{repo}/Cargo.toml");
            let workspace = |args: &[&str]| cargo(&env, &manifest, "test", args);
            let out = test_run(&up, workspace, &tests);
            rows.push(translator_not_runs(
                test_verdict(&format!("{tag} tests: uds pty libc"), &out),
                &out.text,
                arch,
            ));
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
            println!("\n=== {tag}: {RENDER} library tests ===");
            let mut render = vec!["--no-fail-fast", "-p", RENDER, "--lib", "--", "--nocapture"];
            if arch == Arch::X86_64 {
                render.push(ROSETTA_ONE_AT_A_TIME);
            }
            let out = test_run(&up, workspace, &render);
            rows.push(test_verdict(&format!("{tag} tests: render lib"), &out));
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
            println!(
                "\n=== {tag}: libc oracle conformance (cell {}) ===",
                arch.triple()
            );
            let out = up.lima.shell_streamed(
                &up.home,
                &cargo(
                    &oracle_env,
                    &format!("{repo}/libc-oracle/Cargo.toml"),
                    "test",
                    &["--no-fail-fast", "-p", "libc-conformance"],
                ),
                "  | ",
            );
            rows.push(test_verdict(
                &format!("{tag} libc oracle conformance"),
                &out,
            ));
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
            println!("\n=== {tag}: {SHELLS} tests (bash, zsh, fish, pwsh) ===");
            let out = test_run(&up, workspace, &["--no-fail-fast", "--tests", "-p", SHELLS]);
            rows.push(test_verdict(
                &format!("{tag} tests: shell integration (bash, zsh, fish, pwsh)"),
                &out,
            ));
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
            println!("\n=== {tag}: install.sh's bootstrap verifier, static and embedded ===");
            rows.extend(verifier_rows(&up, arch, &tag, &repo, &lane));
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
            println!(
                "\n=== {tag}: aterm-dl, aterm-x11, aterm-wayland, aterm-xkb, aterm-window tests (Xvfb, weston, Mutter) ==="
            );
            let started = Instant::now();
            let name =
                format!("{tag} tests: dl x11 wayland xkb window, live on Xvfb, weston and Mutter");
            let bus = up.lima.shell_streamed(
                &up.home,
                &cargo(
                    &env,
                    &format!("{repo}/Cargo.toml"),
                    "build",
                    &["-p", "aterm-dbus", "--bin", "lane-bus"],
                ),
                "  | ",
            );
            let servers = if bus.ok() {
                linux::start_pipewire(&up);
                linux::start_display(&up, Display::Xvfb)
                    .and_then(|_| linux::start_display(&up, Display::Weston))
                    .and_then(|_| linux::start_display(&up, Display::Mutter))
                    .and_then(|_| {
                        unit_pid(&up, &format!("{}mutter", guest::UNIT_PREFIX))
                            .ok_or_else(|| "Mutter's unit has no main pid".to_string())
                    })
            } else {
                Err(format!("lane-bus did not build: {}", bus.tail(5)))
            };
            match servers {
                Err(why) => rows.push(Row {
                    name,
                    verdict: Verdict::NotRun,
                    secs: started.elapsed().as_secs_f64(),
                    detail: why,
                }),
                Ok(mutter) => {
                    let mut live = env.clone();
                    live.push(Display::Xvfb.client_env(&up.runtime));
                    live.push(Display::Weston.client_env(&up.runtime));
                    live.extend(seat_env(
                        &up.runtime,
                        &mutter,
                        &arch.bin_dir(&format!("{lane}/target")),
                    ));
                    let mut tests = vec!["--no-fail-fast", "--tests"];
                    for p in WINDOWING {
                        tests.extend(["-p", p]);
                    }
                    tests.extend(["--", "--nocapture"]);
                    let out = up.lima.shell_streamed(
                        &up.home,
                        &cargo(&live, &format!("{repo}/Cargo.toml"), "test", &tests),
                        "  | ",
                    );
                    rows.push(no_skips(test_verdict(&name, &out), &out.text));
                }
            }
            up.stop_units();
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
        }
        if let Some(cell) = a11y.as_ref().filter(|_| !ask.edges_only)
            && let Err(s) = crate::linux_a11y::test_rows(cell, &mut rows)
        {
            stopped = Some(s);
            break 'arches;
        }
        let oracle = if ask.wgpu_oracle {
            " (wgpu oracle)"
        } else {
            ""
        };
        println!("\n=== {tag}: build aterm-gui{oracle}, aterm-ctl ===");
        let mut build = vec![
            "-p",
            "aterm-gui",
            "--bin",
            "aterm-gui",
            "-p",
            "aterm-ctl",
            "--bin",
            "aterm-ctl",
            "-p",
            "aterm-dbus",
            "--bin",
            "lane-bus",
        ];
        if ask.wgpu_oracle {
            build.extend(["--features", "aterm-gpu/wgpu-oracle"]);
        }
        let out = up.lima.shell_streamed(
            &up.home,
            &cargo(&env, &format!("{repo}/Cargo.toml"), "build", &build),
            "  | ",
        );
        let built = out.ok();
        rows.push(Row {
            name: format!("{tag} build aterm-gui{oracle}"),
            verdict: if built { Verdict::Pass } else { Verdict::Fail },
            secs: out.secs,
            detail: if built {
                String::new()
            } else {
                tally(&out.text).error.unwrap_or_else(|| out.tail(5))
            },
        });
        let sessions: Vec<Option<Display>> =
            if ask.gone_only || ask.a11y_only || ask.deck_only || ask.edges_only {
                Vec::new()
            } else if ask.cpu_only {
                ask.displays.iter().copied().map(Some).collect()
            } else {
                std::iter::once(None)
                    .chain(ask.displays.iter().copied().map(Some))
                    .collect()
            };
        for display in sessions {
            let where_ = display.map_or_else(
                || "--headless".to_string(),
                |d| match up.weston_renderer {
                    "gl" => format!("{} ({})", d.name(), d.protocol()),
                    _ if !d.runs_weston() => format!("{} ({})", d.name(), d.protocol()),
                    r => format!("{} --renderer={r} ({})", d.name(), d.protocol()),
                },
            );
            let gui = Gui {
                up: &up,
                bin: bin.clone(),
                dir: format!(
                    "/tmp/{}{}-{}",
                    guest::UNIT_PREFIX,
                    arch.name(),
                    display.map_or("headless", Display::name)
                ),
                name: format!(
                    "{}-{}",
                    arch.name(),
                    display.map_or("headless", Display::name)
                ),
                cpu: false,
                deck: false,
            };
            if !built {
                let gpu = (!ask.cpu_only).then(|| format!("{tag} aterm-gui {where_}"));
                let cpu = display.map(|_| format!("{tag} aterm-gui --cpu {where_}"));
                for name in gpu.into_iter().chain(cpu) {
                    rows.push(Row {
                        name,
                        verdict: Verdict::NotRun,
                        secs: 0.0,
                        detail: "no aterm-gui: its build failed".into(),
                    });
                }
                continue;
            }
            if !ask.cpu_only {
                let name = format!("{tag} aterm-gui {where_}");
                println!("\n=== {name} ===");
                let started = Instant::now();
                let (verdict, detail) = match display.map(|d| linux::start_display(&up, d)) {
                    Some(Err(why)) => (Verdict::NotRun, why),
                    _ => gui.drive(display, &results),
                };
                up.stop_units();
                println!("  {} {detail}", verdict.label());
                rows.push(Row {
                    name,
                    verdict,
                    secs: started.elapsed().as_secs_f64(),
                    detail,
                });
                if let Err(s) = crate::stop::check() {
                    stopped = Some(s);
                    break 'arches;
                }
            }
            let Some(d) = display else {
                continue;
            };
            let name = format!("{tag} aterm-gui --cpu {where_}");
            println!("\n=== {name} ===");
            let started = Instant::now();
            let servers = if d == Display::Weston {
                linux::start_weston_capturing(&up)
            } else {
                linux::start_display(&up, d)
            };
            if let Err(why) = servers {
                rows.push(Row {
                    name,
                    verdict: Verdict::NotRun,
                    secs: started.elapsed().as_secs_f64(),
                    detail: why,
                });
                continue;
            }
            let gui = Gui {
                cpu: true,
                name: format!("{}-{}-cpu", arch.name(), d.name()),
                ..gui
            };
            let (verdict, detail) = gui.drive(display, &results);
            up.stop_units();
            println!("  {} {detail}", verdict.label());
            rows.push(Row {
                name,
                verdict,
                secs: started.elapsed().as_secs_f64(),
                detail,
            });
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
            if d != Display::Xvfb {
                continue;
            }
            let name = format!("{tag} aterm-gui --cpu xvfb without MIT-SHM (X11)");
            println!("\n=== {name} ===");
            let started = Instant::now();
            if let Err(why) = linux::start_xvfb_without_shm(&up) {
                rows.push(Row {
                    name,
                    verdict: Verdict::NotRun,
                    secs: started.elapsed().as_secs_f64(),
                    detail: why,
                });
                continue;
            }
            let gui = Gui {
                name: format!("{}-xvfb-noshm-cpu", arch.name()),
                ..gui
            };
            let (verdict, detail) = match no_shm(&up) {
                Ok(()) => gui.drive(display, &results),
                Err(why) => (Verdict::Fail, why),
            };
            up.stop_units();
            println!("  {} {detail}", verdict.label());
            rows.push(Row {
                name,
                verdict,
                secs: started.elapsed().as_secs_f64(),
                detail,
            });
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
        }
        let gone = ask.displays.iter().copied().filter(|d| {
            !ask.cpu_only
                && !ask.a11y_only
                && !ask.deck_only
                && !ask.edges_only
                && (ask.gone_only || crate::linux_gone::GONE.contains(d))
        });
        let gone = gone.flat_map(|d| {
            crate::linux_gone::End::ALL
                .into_iter()
                .filter(move |m| m.applies(d))
                .map(move |m| (d, m))
        });
        for (display, end) in gone {
            let name = format!(
                "{tag} {} under aterm-gui{oracle}: {} ({})",
                end.name(),
                display.name(),
                display.protocol()
            );
            if !built {
                rows.push(Row {
                    name,
                    verdict: Verdict::NotRun,
                    secs: 0.0,
                    detail: "no aterm-gui: its build failed".into(),
                });
                continue;
            }
            println!("\n=== {name} ===");
            let started = Instant::now();
            if let Err(why) = linux::start_display(&up, display) {
                rows.push(Row {
                    name,
                    verdict: Verdict::NotRun,
                    secs: started.elapsed().as_secs_f64(),
                    detail: why,
                });
                continue;
            }
            let gui = Gui {
                up: &up,
                bin: bin.clone(),
                dir: format!(
                    "/tmp/{}{}-{}-gone-{}",
                    guest::UNIT_PREFIX,
                    arch.name(),
                    display.name(),
                    end.slug()
                ),
                name: format!("{}-{}-gone-{}", arch.name(), display.name(), end.slug()),
                cpu: false,
                deck: false,
            };
            let (verdict, detail) =
                crate::linux_gone::drive(&gui, display, end, ask.wgpu_oracle, &results);
            up.stop_units();
            println!("  {} {detail}", verdict.label());
            rows.push(Row {
                name,
                verdict,
                secs: started.elapsed().as_secs_f64(),
                detail,
            });
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
        }
        if let Some(cell) = a11y.as_ref().filter(|_| !ask.edges_only)
            && let Err(s) = crate::linux_a11y::gui_rows(cell, built, &mut rows)
        {
            stopped = Some(s);
            break 'arches;
        }
        if let Some(cell) = a11y.as_ref().filter(|_| !ask.a11y_only)
            && let Err(s) = crate::linux_edges::rows(cell, built, &ask.edges, &mut rows)
        {
            stopped = Some(s);
            break 'arches;
        }
        let decks = guest::Screen::DECK
            .into_iter()
            .filter(|s| ask.displays.contains(&s.display.proof_display()))
            .filter(|_| !ask.gone_only && !ask.cpu_only && !ask.a11y_only && !ask.edges_only);
        for screen in decks {
            if !built {
                rows.push(Row {
                    name: crate::linux_baseline::deck_row_name(arch, screen),
                    verdict: Verdict::NotRun,
                    secs: 0.0,
                    detail: "no aterm-gui: its build failed".into(),
                });
                continue;
            }
            rows.push(crate::linux_baseline::deck_row(
                &up, arch, screen, &bin, &results,
            ));
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
        }
        let Some(baseline) = baseline.as_ref().filter(|_| built) else {
            continue;
        };
        for screen in guest::Screen::BASELINE {
            if !ask.displays.contains(&screen.display.proof_display()) {
                continue;
            }
            rows.push(baseline.session(&up, arch, screen, &bin));
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
        }
    }
    if let Some(b) = &baseline {
        b.end(&up);
    }
    let df = |up: &Up| {
        linux::parse_df(
            &up.sh(&[
                "df".into(),
                "-B1".into(),
                "--output=used,size".into(),
                "/".into(),
            ])
            .text,
        )
    };
    let mut disk = Vec::new();
    if let Some((used, size)) = df(&up) {
        disk.push(format!(
            "guest / after the run: {} used of {}",
            linux::gib(used),
            linux::gib(size)
        ));
    }
    let build_bytes = up.sh(&["du".into(), "-sb".into(), lane.clone()]);
    if let Some(n) = build_bytes
        .text
        .split_whitespace()
        .next()
        .and_then(|n| n.parse::<u64>().ok())
    {
        disk.push(format!("guest build output ({lane}): {}", linux::gib(n)));
    }
    if !ask.keep_build {
        match up.clean_build() {
            Ok(()) => {
                if let Some((used, _)) = df(&up) {
                    disk.push(format!("guest / after clean: {} used", linux::gib(used)));
                }
            }
            Err(e) => disk.push(format!("guest build output NOT removed: {e}")),
        }
    }
    if let Ok(Some(vm)) = up.lima.state()
        && let Some(bytes) = linux::host_disk_bytes(&vm)
    {
        disk.push(format!(
            "host: the VM disk holds {} (sparse, <= {} GiB; trimmed after a clean)",
            linux::gib(bytes),
            linux::DISK_GIB
        ));
    }
    let keep_up = ask.keep_up;
    if keep_up {
        up.stop_units();
    } else {
        up.release();
    }
    let report = render(&rows, &disk, t0.elapsed().as_secs_f64());
    print!("\n{report}");
    let path = results.join("report.txt");
    if let Err(e) = std::fs::write(&path, &report) {
        eprintln!("aterm-lab: linux: {}: {e}", path.display());
    } else {
        println!("report and frames: {}", results.display());
    }
    if let Some(s) = stopped {
        return Err(s.into());
    }
    let code = exit_code(&rows);
    if code == linux::NOT_RUN {
        let why = rows
            .iter()
            .find(|r| r.verdict == Verdict::NotRun)
            .map_or("no row ran".to_string(), |r| {
                format!("{}: {}", r.name, r.detail)
            });
        return Err(linux::not_run(crate::util::one_line(&why, 300)));
    }
    Ok(code)
}

/// The report: one row per step with its verdict and wall time, the details under it, then the disk use and the total.
#[must_use]
pub fn render(rows: &[Row], disk: &[String], total_secs: f64) -> String {
    render_as("LINUX LANE PROOF", rows, disk, total_secs)
}

/// [`render`] under another `title` (another verb's report).
#[must_use]
pub fn render_as(title: &str, rows: &[Row], disk: &[String], total_secs: f64) -> String {
    let width = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut s = format!("{title}\n");
    for r in rows {
        let _ = writeln!(
            s,
            "  {:<7}  {:<width$}  {:>7.1} s",
            r.verdict.label(),
            r.name,
            r.secs
        );
        for line in r.detail.lines().filter(|l| !l.trim().is_empty()) {
            let _ = writeln!(s, "             {line}");
        }
    }
    for d in disk {
        let _ = writeln!(s, "  {d}");
    }
    let _ = writeln!(
        s,
        "  total {total_secs:.0} s; verdict: {}",
        match exit_code(rows) {
            0 => "PASS",
            1 => "FAIL",
            _ => "NOT RUN",
        }
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(v: Verdict) -> Row {
        Row {
            name: "r".into(),
            verdict: v,
            secs: 1.0,
            detail: String::new(),
        }
    }

    /// A row's build takes its cargo arguments and none of the test
    /// binaries' (a `--nocapture` or a filter would be cargo's error there).
    #[test]
    fn a_rows_build_is_its_cargo_arguments_with_no_run() {
        let filtered = [
            "--no-fail-fast",
            "-p",
            "aterm-gui",
            "--lib",
            "--",
            "a11y",
            "--nocapture",
        ];
        assert_eq!(
            no_run(&filtered),
            ["--no-fail-fast", "-p", "aterm-gui", "--lib", "--no-run"]
        );
        let bench = [
            "-p",
            "aterm-a11y",
            "--bench",
            "live_atspi",
            "--profile",
            "test",
        ];
        assert_eq!(
            no_run(&bench),
            [
                "-p",
                "aterm-a11y",
                "--bench",
                "live_atspi",
                "--profile",
                "test",
                "--no-run"
            ]
        );
    }

    #[test]
    fn the_lane_builds_the_verifier_as_a_release_does() {
        for arch in [Arch::Aarch64, Arch::X86_64] {
            let triple = arch.triple();
            for (_, _, assertions) in VERIFIER_SHAPES {
                let argv = verifier_build_args("/r/Cargo.toml", triple, assertions);
                let pair = |a: &str, b: &str| argv.windows(2).any(|w| w[0] == a && w[1] == b);
                assert!(pair("--target", triple), "{argv:?}");
                assert!(pair("-p", VERIFIER), "{argv:?}");
                assert!(argv.iter().any(|a| a == "--release"), "{argv:?}");
                assert!(
                    pair(
                        "--config",
                        &aterm_bootstrap_verify::pin::static_build_config(triple)
                    ),
                    "{argv:?}"
                );
                assert_eq!(
                    pair("--config", "profile.release.debug-assertions=true"),
                    assertions,
                    "{argv:?}"
                );
            }
        }
        assert_ne!(VERIFIER_SHAPES[0].1, VERIFIER_SHAPES[1].1);
        assert!(
            !VERIFIER_SHAPES[0].2 && VERIFIER_SHAPES[1].2,
            "the shipped shape first"
        );
    }

    #[test]
    fn an_extension_is_listed_by_its_whole_name() {
        let listing = "number of extensions:    3\n    BIG-REQUESTS  (opcode: 133)\n    \
                       MIT-SCREEN-SAVER  (opcode: 145, base event: 85)\n    \
                       MIT-SHM  (opcode: 130, base event: 65, base error: 128)\n";
        assert!(lists_extension(listing, "MIT-SHM"));
        let without = listing.replace("MIT-SHM ", "XKEYBOARD ");
        assert!(!lists_extension(&without, "MIT-SHM"), "{without}");
    }

    #[test]
    fn a_refused_verb_says_how_it_ended() {
        let out = |code: Option<i32>, text: &str| Out {
            code,
            text: text.into(),
            secs: 0.0,
        };
        let verb = ["image", "plain", "deck-gpu-off"];
        assert_eq!(
            refused(&verb, &out(Some(255), "")),
            "`image plain deck-gpu-off` answered \"\" (exit 255, nothing said: a guest command \
             that a signal ended, or one that exited 255)"
        );
        assert_eq!(
            refused(&verb, &out(None, "\n(limactl ended on signal 15)\n")),
            "`image plain deck-gpu-off` answered \"(limactl ended on signal 15)\" (no exit status)"
        );
        assert_eq!(
            refused(&["ready", "60000"], &out(Some(0), "OK timeout\n")),
            "`ready 60000` answered \"OK timeout\" (exit 0)"
        );
    }

    #[test]
    fn a_partial_lane_never_passes() {
        assert_eq!(exit_code(&[row(Verdict::Pass), row(Verdict::Pass)]), 0);
        assert_eq!(exit_code(&[row(Verdict::Pass), row(Verdict::NotRun)]), 3);
        assert_eq!(exit_code(&[row(Verdict::NotRun), row(Verdict::Fail)]), 1);
        assert_eq!(exit_code(&[]), 3, "nothing ran is not a pass");
    }

    #[test]
    fn every_arch_and_display_unless_named() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        let all = parse_proof(&[]).expect("parses");
        assert_eq!(all.arches, Arch::ALL);
        assert_eq!(all.displays, Display::PROOF);
        let one = parse_proof(&s(&[
            "--arch",
            "x86_64",
            "--display",
            "xvfb",
            "--display",
            "xvfb",
            "--keep-up",
        ]))
        .expect("parses");
        assert_eq!(one.arches, [Arch::X86_64]);
        assert_eq!(one.displays, [Display::Xvfb]);
        assert!(one.keep_up && !one.keep_build);
        assert_eq!(
            parse_proof(&s(&["--display", "kwin"]))
                .map_err(|f| f.code)
                .err(),
            Some(2)
        );
        assert_eq!(
            parse_proof(&s(&["--display", "weston-x11"]))
                .map_err(|f| f.code)
                .err(),
            Some(2),
            "the baseline's weston with a seat is no proof display"
        );
    }

    #[test]
    fn gone_runs_its_displays_and_alone_takes_the_oracle() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        let gone = parse_gone(&[]).expect("parses");
        assert!(gone.gone_only && !gone.wgpu_oracle);
        assert_eq!(gone.arches, Arch::ALL);
        assert_eq!(gone.displays, crate::linux_gone::GONE);
        let oracle = parse_gone(&s(&["--wgpu-oracle", "--display", "mutter"])).expect("parses");
        assert!(oracle.wgpu_oracle);
        assert_eq!(oracle.displays, [Display::Mutter]);
        let proof = parse_proof(&[]).expect("parses");
        assert!(!proof.gone_only && !proof.wgpu_oracle);
        assert_eq!(
            parse_proof(&s(&["--wgpu-oracle"]))
                .map_err(|f| f.code)
                .err(),
            Some(2)
        );
    }

    #[test]
    fn a11y_runs_its_rows_alone_on_every_arch_unless_named() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        let a11y = parse_a11y(&[]).expect("parses");
        assert!(a11y.a11y_only && !a11y.gone_only && !a11y.cpu_only);
        assert_eq!(a11y.arches, Arch::ALL);
        let one = parse_a11y(&s(&["--arch", "aarch64", "--keep-build"])).expect("parses");
        assert_eq!(one.arches, [Arch::Aarch64]);
        assert!(one.keep_build);
        for refused in [
            &["--display", "xvfb"][..],
            &["--wgpu-oracle"][..],
            &["--weston-renderer", "pixman"][..],
        ] {
            assert_eq!(
                parse_a11y(&s(refused)).map_err(|f| f.code).err(),
                Some(2),
                "{refused:?}"
            );
        }
        assert!(!parse_proof(&[]).expect("parses").a11y_only);
    }

    #[test]
    fn the_weston_renderer_is_taken_where_it_starts_a_weston() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        let pixman = s(&["--weston-renderer", "pixman"]);
        for parse in [parse_proof, parse_gone, parse_cpu, parse_deck] {
            assert_eq!(parse(&pixman).expect("parses").weston_renderer, "pixman");
            assert_eq!(parse(&[]).expect("parses").weston_renderer, "gl");
        }
        assert_eq!(parse_edges(&pixman).map_err(|f| f.code).err(), Some(2));
        assert!(
            parse_edges(&s(&["--edge", "weston-pixman"]))
                .expect("parses")
                .edges_only
        );
    }

    #[test]
    fn the_echo_row_is_the_output_never_the_typed_line() {
        let screen = "user@lima:/w$ echo LANE-$((6*7))\nLANE-42\nuser@lima:/w$ \n\n";
        assert_eq!(echo_row(screen), Some(1));
        assert_eq!(
            echo_row("user@lima:/w$ echo LANE-$((6*7))\nuser@lima:/w$ \n"),
            None,
            "no output yet: no row, so the round trip refuses rather than copy a prompt"
        );
        assert!(!TYPED.contains(PASTED) && !PASTE_BEFORE.contains(PASTED));
        assert!(!PASTE_AFTER.contains(PASTED) && !FOREIGN.contains(PASTED));
        assert_ne!(ECHO.to_lowercase(), PASTED.trim_start_matches("x-"));
        assert_eq!(echo_row(reply("\n\n$ echo x\nLANE-42\n$ \n\n")), Some(3));
    }

    const STAGED: &str = "\
     Running unittests src/lib.rs (lane/target/debug/deps/aterm_uds-04e96a6b8de7de57)

running 3 tests
test a::ok ... ok
test spawnfd::staged ... FAILED
test hangup::shut ... FAILED

failures:

---- spawnfd::staged stdout ----
stdout:
running 1 test
test spawnfd::staged ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 90 filtered out; finished in 0.00s

failures:
    spawnfd::staged
    hangup::shut

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.02s

error: test failed, to rerun pass `-p aterm-uds --lib`
     Running tests/roundtrip.rs (lane/target/debug/deps/roundtrip-be61f7ab46a44740)

running 10 tests
test result: ok. 9 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.11s

error: 1 target failed:
";

    #[test]
    fn a_staged_childs_lines_are_not_counted_twice() {
        let t = tally(STAGED);
        assert_eq!((t.passed, t.failed, t.ignored), (10, 2, 1));
        assert_eq!(
            t.failures,
            ["aterm_uds: spawnfd::staged", "aterm_uds: hangup::shut"]
        );
        assert_eq!(
            t.error, None,
            "cargo's own test-failure lines are not build errors"
        );
    }

    #[test]
    fn a_build_error_is_named_and_a_clean_run_counts() {
        let broken = "   Compiling x v0.1.0\nerror[E0425]: cannot find value `y`\nerror: could not compile `x`\n";
        assert_eq!(
            tally(broken).error.as_deref(),
            Some("error[E0425]: cannot find value `y`")
        );
        let out = Out {
            code: Some(101),
            text: broken.into(),
            secs: 2.0,
        };
        let r = test_verdict("t", &out);
        assert_eq!(r.verdict, Verdict::Fail);
        assert!(r.detail.contains("E0425"), "{}", r.detail);
        let clean = Out {
            code: Some(0),
            text: "     Running tests/a.rs (t/debug/deps/a-0123)\ntest result: ok. 4 passed; 0 failed; 0 ignored; 0 measured\n".into(),
            secs: 1.0,
        };
        let r = test_verdict("t", &clean);
        assert_eq!(
            (r.verdict, r.detail.as_str()),
            (Verdict::Pass, "4 passed, 0 failed, 0 ignored")
        );
        let nothing = Out {
            code: Some(0),
            text: "     Running tests/a.rs (t/debug/deps/a-0123)\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out\n".into(),
            secs: 1.0,
        };
        assert_eq!(test_verdict("t", &nothing).verdict, Verdict::NotRun);
    }

    #[test]
    fn the_control_replies_are_read_by_field() {
        let status = "OK schema=1 sid=0 phase=idle render=ok resizes=1 flaps=0";
        assert_eq!(field(status, "render"), Some("ok"));
        assert_eq!(field(status, "resizes"), Some("1"));
        assert_eq!(field(status, "rend"), None);
        assert!(dims_are("OK 30 100 900 600 session=0", 30, 100));
        assert!(!dims_are("OK 24 80 720 480", 30, 100));
        assert!(!dims_are("ERR 30 100", 30, 100));
        assert_eq!(
            present_summary(
                "OK n_input=0 n_present=3 present_p50_ms=1.70 present_p95_ms=9.44 present_p99_ms=9.44"
            ),
            "n_present=3 present_p50/p95/p99_ms=1.70/9.44/9.44"
        );
        assert_eq!(
            renderer("x\naterm-gui: GPU rendering on llvmpipe (LLVM 20.1.2, 256 bits) (Vulkan)\n"),
            Some("llvmpipe (LLVM 20.1.2, 256 bits) (Vulkan)")
        );
        assert!(
            !TYPED.contains(ECHO),
            "the echo must not be the typed text itself"
        );
    }

    #[test]
    fn a_gpu_row_fails_when_its_session_drew_on_the_cpu() {
        let device = "aterm-gui: GPU rendering on llvmpipe (LLVM 20.1.2, 128 bits) (Gl)\n";
        assert_eq!(
            drew_on_gpu(device, Some("gpu")),
            Ok(Some("llvmpipe (LLVM 20.1.2, 128 bits) (Gl)"))
        );
        let fell_back = format!(
            "{device}aterm-gui: {SURFACE_FAILED}: gl: no libwayland-egl.so.1 on this machine; \
             falling back to the CPU renderer\n"
        );
        for backend in [Some("gpu"), Some("cpu"), None] {
            let why = drew_on_gpu(&fell_back, backend).expect_err("a fallback fails the row");
            assert!(why.contains("no libwayland-egl.so.1"), "{why}");
        }
        let no_device = "aterm-gui: GPU unavailable (no EGL display); using CPU renderer\n";
        let why = drew_on_gpu(no_device, Some("cpu")).expect_err("a CPU session fails the row");
        assert!(why.contains("backend=cpu"), "{why}");
        assert!(
            drew_on_gpu(device, None).is_err(),
            "no backend= is no answer"
        );
    }

    #[test]
    fn the_report_names_every_row_and_the_verdict() {
        let rows = [
            Row {
                name: "aarch64 (native) tests".into(),
                verdict: Verdict::Pass,
                secs: 12.0,
                detail: "3 passed".into(),
            },
            Row {
                name: "x86_64 (Rosetta) aterm-gui xvfb (X11)".into(),
                verdict: Verdict::NotRun,
                secs: 0.5,
                detail: "xvfb did not come up".into(),
            },
        ];
        let text = render(&rows, &["guest / 7.0 GiB used".into()], 30.0);
        assert!(text.contains("PASS     aarch64 (native) tests"), "{text}");
        assert!(
            text.contains("NOT RUN  x86_64 (Rosetta) aterm-gui xvfb (X11)"),
            "{text}"
        );
        assert!(text.contains("xvfb did not come up"));
        assert!(text.ends_with("total 30 s; verdict: NOT RUN\n"), "{text}");
    }

    #[test]
    fn a_ppm_reads_and_a_frame_compares_by_rgb() {
        let ppm = b"P6\n2 1\n255\n\x01\x02\x03\x04\x05\x06";
        let server = parse_ppm(ppm).expect("a 2x1 PPM");
        assert_eq!(server, (vec![1, 2, 3, 4, 5, 6], 2, 1));
        let frame = [1, 2, 3, 255, 4, 5, 6, 0];
        assert_eq!(compare_rgb(&frame, 2, 1, &server, 0..0), Ok(()));
        let off = [1, 2, 3, 255, 4, 5, 7, 255];
        let why = compare_rgb(&off, 2, 1, &server, 0..0).unwrap_err();
        assert!(
            why.starts_with("1 of 2 pixels differ; first at (1,0)"),
            "{why}"
        );
        assert!(compare_rgb(&frame, 1, 2, &server, 0..0).is_err());
        let two = (vec![1, 2, 3, 4, 5, 6], 1, 2);
        let moved = [9, 9, 9, 255, 4, 5, 6, 255];
        assert_eq!(compare_rgb(&moved, 1, 2, &two, 0..1), Ok(()));
        assert!(compare_rgb(&moved, 1, 2, &two, 1..2).is_err());
        assert_eq!(parse_ppm(b"P6\n2 1\n255\n\x01"), None, "short");
        assert_eq!(parse_ppm(b"P3\n1 1\n255\n1 2 3"), None, "not binary");
    }

    fn rgba(
        w: usize,
        h: usize,
        paint: impl Fn(usize, usize) -> [u8; 3],
    ) -> (Vec<u8>, usize, usize) {
        let mut v = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&paint(x, y));
                v.push(255);
            }
        }
        (v, w, h)
    }

    #[test]
    fn a_frame_is_held_where_the_compositor_shows_it() {
        let pixel = |x: usize, y: usize| [u8::try_from(x).unwrap(), u8::try_from(y).unwrap(), 7];
        let frame = rgba(4, 5, pixel);
        let place = |fx: usize, fy: usize, (x, y): (usize, usize)| {
            if (fx..fx + 4).contains(&x) && (fy..fy + 5).contains(&y) {
                pixel(x - fx, y - fy)
            } else {
                [40, 50, 60]
            }
        };
        let output = rgba(12, 10, |x, y| place(5, 3, (x, y)));
        assert_eq!(held_at(&output, &frame, 5, 3, 0..0), Ok((4, 5)));
        let past = rgba(12, 10, |x, y| place(5, 6, (x, y)));
        assert_eq!(held_at(&past, &frame, 5, 6, 0..0), Ok((4, 4)));
        let mut off = output.clone();
        off.0[((3 + 4) * 12 + 5 + 2) * 4] ^= 1;
        let why = held_at(&off, &frame, 5, 3, 0..0).unwrap_err();
        assert!(
            why.ends_with(
                "1 of 20 pixels differ; first at (2,4): presented [02, 04, 07], server [03, 04, 07]"
            ),
            "{why}"
        );
        let low = rgba(12, 10, |x, y| match (x, y) {
            (5..9, 6) => [0, 0, 0],
            (5..9, 7..10) => pixel(x - 5, y - 7),
            _ => [40, 50, 60],
        });
        assert_eq!(
            held_at(&low, &frame, 5, 7, 0..0),
            Ok((4, 3)),
            "content alone"
        );
        assert!(held_at(&low, &frame, 5, 6, 0..0).is_err());
        assert!(held_at(&output, &frame, 6, 3, 0..0).is_err());
        let why = held_at(&output, &frame, 9, 7, 0..0).unwrap_err();
        assert!(why.starts_with("only 3x3 of the 4x5 frame at 9,7"), "{why}");
        assert!(held_at(&output, &frame, -1, 3, 0..0).is_err());
    }

    #[test]
    fn a_frame_is_found_where_it_shows_whole() {
        let pixel = |x: usize, y: usize| [u8::try_from(x).unwrap(), u8::try_from(y).unwrap(), 7];
        let frame = rgba(4, 5, pixel);
        let place = |fx: usize, fy: usize, (x, y): (usize, usize)| {
            if (fx..fx + 4).contains(&x) && (fy..fy + 5).contains(&y) {
                pixel(x - fx, y - fy)
            } else {
                [40, 50, 60]
            }
        };
        let output = rgba(12, 10, |x, y| place(5, 3, (x, y)));
        assert_eq!(frame_places(&output, &frame, 0..0), [(5, 3)]);
        let mut off = output.clone();
        off.0[((3 + 4) * 12 + 5 + 2) * 4] ^= 1;
        assert_eq!(frame_places(&off, &frame, 0..0), []);
        let mut live = output.clone();
        live.0[((3 + 2) * 12 + 5 + 1) * 4] ^= 1;
        assert_eq!(frame_places(&live, &frame, 0..0), []);
        assert_eq!(frame_places(&live, &frame, 2..3), [(5, 3)]);
        let twice = rgba(12, 10, |x, y| {
            if x < 6 {
                place(1, 2, (x, y))
            } else {
                place(7, 4, (x, y))
            }
        });
        assert_eq!(frame_places(&twice, &frame, 0..0), [(1, 2), (7, 4)]);
        let small = rgba(3, 5, |_, _| [40, 50, 60]);
        assert_eq!(frame_places(&small, &frame, 0..0), []);
    }

    #[test]
    fn a_buffer_s_bands_continue_the_frame_s_edge() {
        let pixel = |x: usize, y: usize| [u8::try_from(x).unwrap(), u8::try_from(y).unwrap(), 7];
        let frame = rgba(4, 5, pixel);
        let view = WestonView {
            x: 3,
            y: 2,
            width: 7,
            height: 6,
            format: "XRGB8888".into(),
        };
        let output = rgba(12, 10, |x, y| {
            if !(3..10).contains(&x) || !(2..8).contains(&y) {
                return [40, 50, 60];
            }
            let (dx, dy) = (x as i64 - 5, y as i64 - 2);
            pixel(dx.clamp(0, 3) as usize, dy.clamp(0, 4) as usize)
        });
        assert_eq!(held_at(&output, &frame, 5, 2, 0..0), Ok((4, 5)));
        assert_eq!(
            bands_held(&output, &frame, &view, (2, 0)),
            Ok(7 * 6 - 4 * 5)
        );
        let mut off = output.clone();
        off.0[((7 * 12) + 9) * 4] ^= 1;
        let why = bands_held(&off, &frame, &view, (2, 0)).unwrap_err();
        assert!(
            why.starts_with("1 of 22 band pixels differ; first at (6,5)"),
            "{why}"
        );
        assert!(bands_held(&output, &frame, &view, (1, 0)).is_err());
    }

    #[test]
    fn a_toplevel_is_read_from_westons_scene_graph() {
        let dump = "\
Layer 4 (pos 0x80000000):
\tView 0 (role (null), PID 1480, surface ID 15, panel for output headless, 0xae632b24a420):
\t\tposition: (0, 0) -> (1280, 32)
\t\tSHM buffer
\t\t\tformat: 0x34325241 ARGB8888
\t\t\twidth: 1280, height: 32

Layer 5 (pos 0x50000000):
\tView 0 (role xdg_toplevel, PID 1501, surface ID 3, top-level window 'simple-shm' of org.freedesktop.weston.simple-shm, 0xae632b246f20):
\t\tposition: (273, 400) -> (523, 650)
\t\t[fully opaque]
\t\tSHM buffer
\t\t\tformat: 0x34325258 XRGB8888
\t\t\tmodifier: LINEAR (0x0)
\t\t\twidth: 250, height: 250
";
        let view = WestonView {
            x: 273,
            y: 400,
            width: 250,
            height: 250,
            format: "XRGB8888".into(),
        };
        assert_eq!(weston_view(dump, "1501"), Some(view.clone()));
        assert_eq!(weston_view(dump, "150"), None);
        assert_eq!(weston_view(dump, "1480"), None);
        let observation = |code| Out {
            code,
            text: dump.to_string(),
            secs: 0.0,
        };
        assert_eq!(read_weston_view(&observation(Some(0)), "1501"), Ok(view));
        assert!(read_weston_view(&observation(Some(0)), "150").is_err());
        let admitted: Vec<_> = [Some(1), None]
            .into_iter()
            .filter(|code| read_weston_view(&observation(*code), "1501").is_ok())
            .collect();
        assert!(
            admitted.is_empty(),
            "UNSUCCESSFUL_WINDOW_METADATA_MUST_NOT_VALIDATE: {admitted:?}"
        );
    }

    #[test]
    fn a_skipped_test_fails_a_passing_row() {
        let row = |verdict| Row {
            name: "tests".into(),
            verdict,
            secs: 1.0,
            detail: "3 passed, 0 failed, 0 ignored".into(),
        };
        let text = "test live ... ok\nSKIP: no DISPLAY\ntest other ... ok\n";
        let failed = no_skips(row(Verdict::Pass), text);
        assert_eq!(failed.verdict, Verdict::Fail);
        assert!(
            failed
                .detail
                .ends_with("1 skipped, which the lane counts as not run:\n    SKIP: no DISPLAY"),
            "{}",
            failed.detail
        );
        let kept = no_skips(row(Verdict::Pass), "test live ... ok\n");
        assert_eq!(kept.verdict, Verdict::Pass);
        assert_eq!(kept.detail, "3 passed, 0 failed, 0 ignored");
    }

    #[test]
    fn a_test_not_run_under_rosetta_is_listed_in_the_x86_64_cell_and_fails_any_other() {
        let row = || Row {
            name: "tests".into(),
            verdict: Verdict::Pass,
            secs: 1.0,
            detail: "3 passed, 0 failed, 0 ignored".into(),
        };
        let text = "test a ... ok\ntest m::t ... NOT RUN under Rosetta: m::t: why\nok\n";
        let listed = translator_not_runs(row(), text, Arch::X86_64);
        assert_eq!(listed.verdict, Verdict::Pass);
        assert!(
            listed.detail.ends_with(
                "1 not runnable under Rosetta, which a real x86_64 CPU decides:\n    \
                 NOT RUN under Rosetta: m::t: why"
            ),
            "{}",
            listed.detail
        );
        let native = translator_not_runs(row(), text, Arch::Aarch64);
        assert_eq!(native.verdict, Verdict::Fail, "{}", native.detail);
        let other = "NOT RUN under a translator (x86_64 code on an aarch64 kernel): m::t: why\n";
        assert_eq!(
            translator_not_runs(row(), other, Arch::X86_64).verdict,
            Verdict::Fail
        );
        assert_eq!(
            translator_not_runs(row(), "test a ... ok\n", Arch::X86_64),
            row()
        );
    }
}
