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

/// `gone [--arch A]... [--display D]... [--keep-build] [--keep-up]`.
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
    p.arches = crate::linux_gpu::arches(std::mem::take(&mut p.arches), verb)?;
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

/// A test run's row: PASS on cargo's exit 0 when a test passed, NOT RUN when none did (a selection that matched nothing decided nothing) or the guest was lost under it (what then failed decided nothing), else FAIL naming what failed.
#[must_use]
pub fn test_verdict(name: &str, out: &Out) -> Row {
    let t = tally(&out.text);
    let counts = format!(
        "{} passed, {} failed, {} ignored",
        t.passed, t.failed, t.ignored
    );
    let (verdict, detail) = if let Some(why) = &out.lost {
        (
            Verdict::NotRun,
            format!("the guest was lost under the tests ({counts} before it): {why}"),
        )
    } else if out.ok() && t.passed > 0 {
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
/// detector, since a cold build on a loaded host is slow, not hung, then run under the row's
/// 30-minute one, which only this names. A failed build is the row's output; the seconds are both
/// steps'.
#[must_use]
pub fn test_run(up: &Up, cargo: impl Fn(&[&str]) -> Vec<String>, args: &[&str]) -> Out {
    const TEST_ROW_HANG_SECS: u32 = 1800;
    let built = up
        .guest
        .shell_streamed(&up.home, &cargo(&no_run(args)), "  | ");
    if !built.ok() {
        return built;
    }
    let mut argv = hang_detector(TEST_ROW_HANG_SECS);
    argv.extend(cargo(args));
    let mut out = up.guest.shell_streamed(&up.home, &argv, "  | ");
    out.secs += built.secs;
    out
}

/// The packages whose Linux tests the proof runs.
pub const TESTED: [&str; 4] = ["aterm-uds", "aterm-pty", "libc", "amacs-files"];

/// The system readers' packages (P7): `/proc` and `/sys`, the job probe, the walker and the vitals
/// engine over them, with their live examples built alongside their tests.
pub const SYSTEM: [&str; 3] = ["aterm-sysprobe", "aterm-vitals", "aterm-diskwalk"];

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
        // A build has no hang detector: on a loaded host it is slow, not hung (as in test_run).
        let argv = guest::with_env(
            &up.cargo_env(arch, &target_dir, false),
            &verifier_build_args(&manifest, arch.triple(), assertions),
        );
        let out = up.guest.shell_streamed(&up.home, &argv, "  | ");
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
    let mut env = up.cargo_env(arch, &format!("{lane}/target"), false);
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
    let cargo = |args: &[&str]| {
        let mut cargo = vec![
            "cargo".to_string(),
            "test".into(),
            "--locked".into(),
            "--no-fail-fast".into(),
            "--manifest-path".into(),
            manifest.clone(),
        ];
        cargo.extend(arch.target_args());
        cargo.extend(args.iter().map(|s| (*s).to_string()));
        guest::with_env(&env, &cargo)
    };
    for (what, selection) in suites {
        let out = test_run(up, cargo, selection);
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

/// What [`SHELLS`]' tests get beside the arch's cargo environment: the guest's GNU bash 5.3 ([`guest::BASH_5_3`]) named as the `bash-5.3` tool, so each bash test runs on it as well as on the distro's bash, and one that is missing or answers no 5.3 fails the row rather than leaving the 5.3 paths unrun.
#[must_use]
pub fn shells_bash() -> (String, String) {
    (
        aterm_uds::host::tool("bash-5.3").var(),
        guest::BASH_5_3.path(),
    )
}

/// The [`SHELLS`] row's name for the arch `tag`: each shell its tests run.
#[must_use]
pub fn shells_row(tag: &str) -> String {
    format!(
        "{tag} tests: shell integration (bash, bash {}, zsh, fish, pwsh)",
        guest::BASH_5_3.version
    )
}

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

/// `n_input` and the input percentiles (a key to its frame) of a `metrics percentiles` reply.
#[must_use]
pub fn input_summary(reply: &str) -> String {
    let get = |k: &str| field(reply, k).unwrap_or("?");
    format!(
        "n_input={} input_p50/p95/p99_ms={}/{}/{}",
        get("n_input"),
        get("input_p50_ms"),
        get("input_p95_ms"),
        get("input_p99_ms")
    )
}

/// What the guest's journal says aterm-gui renders on (`GPU rendering on …`).
#[must_use]
pub fn renderer(journal: &str) -> Option<&str> {
    journal
        .lines()
        .find_map(|l| l.split_once("GPU rendering on ").map(|(_, r)| r.trim()))
}

/// What the GL arm's windows draw on, where aterm-gpu named it (`aterm-gpu (gl arm): <platform>
/// windows draw on …`): a window system's driver can be another than the headless probe's that
/// [`renderer`] reads (on krunkit, Mesa's EGL on Wayland falls back to software where its DRM screen
/// fails).
#[must_use]
pub fn window_renderer(journal: &str) -> Option<&str> {
    journal.lines().find_map(|l| {
        let note = l.split_once("aterm-gpu (gl arm): ")?.1;
        note.split_once(" windows draw on ").map(|(_, r)| r.trim())
    })
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

/// Retain a fresh, decodable frame before its row may pass.
fn copy_frame(image: &str, to: &Path, copy: impl FnOnce(&str, &Path) -> Out) -> Result<(), String> {
    let source = image
        .split_whitespace()
        .nth(3)
        .ok_or("image named no frame path")?;
    match std::fs::symlink_metadata(to) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => {
            return Err(format!(
                "{} already exists or cannot be checked",
                to.display()
            ));
        }
    }
    let copied = copy(source, to);
    if !copied.ok() {
        return Err(format!("{source} not copied out: {}", copied.tail(3)));
    }
    let bytes = crate::util::read_artifact(to)
        .map_err(|error| format!("{} cannot be read: {error}", to.display()))?;
    aterm_png::decode_rgba8(&bytes, 8192)
        .ok_or_else(|| format!("{}: not a PNG this lab decodes", to.display()))?;
    Ok(())
}

/// Allocate one results directory exclusively, keeping it for the report's readers.
fn fresh_results(parent: &Path, prefix: &str) -> Result<PathBuf, Fail> {
    aterm_tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent)
        .map(aterm_tempfile::TempDir::keep)
        .map_err(|error| Fail::die(format!("{}: {error}", parent.display())))
}

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
                // The GUI refuses a group/world-writable config, and the
                // guest umask would make `tee` hand it one (mode 664).
                vec![
                    "install".into(),
                    "-m".into(),
                    "600".into(),
                    "/dev/stdin".into(),
                    format!("{d}/cfg/aterm/aterm.toml"),
                ],
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
            let out = self.up.guest.shell(&self.up.home, &argv, stdin);
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
            // The clipboard rows' `invoke Paste` stands in for the person's Edit ▸ Paste: a
            // control client's own paste reads no clipboard.
            ("ATERM_DEBUG_PERSON_INVOKE".into(), "1".into()),
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
        env.extend(crate::linux_gpu::client_env());
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
            if self
                .up
                .guest
                .copy_out(guest_path, &results.join(&name))
                .ok()
            {
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
        let copied = self.up.guest.copy_out(guest_png, &png);
        if !copied.ok() {
            return Err(format!("{guest_png} not copied out: {}", copied.tail(2)));
        }
        let presented = crate::util::read_artifact(&png)
            .ok()
            .and_then(|b| aterm_png::decode_rgba8(&b, 8192))
            .ok_or_else(|| format!("{}: not a PNG this lab decodes", png.display()))?;
        let (w, h) = (presented.1, presented.2);
        match display {
            Display::Xvfb | Display::Xwayland => {
                let ppm = self.x_grab(display, results, suffix)?;
                let server = crate::util::read_artifact(&ppm)
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
                let output = crate::util::read_artifact(&shot)
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
                let output = crate::util::read_artifact(&shot)
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
        let copied = self.up.guest.copy_out(&guest_png, &local);
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
        let copied = self.up.guest.copy_out(&guest_ppm, &ppm);
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
        let copied = self.up.guest.copy_out(&format!("{dir}/{name}"), &local);
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
        copy_frame(&image, &results.join(&frame), |guest_path, to| {
            self.up.guest.copy_out(guest_path, to)
        })?;
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
                frame
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
    let mut lane = linux::LaneLock::boot()?;
    let prefix = format!(
        "{kind}-{}-",
        crate::util::compact_stamp(crate::util::now_secs())
    );
    let results = fresh_results(&linux::state_dir()?, &prefix)?;
    let t0 = Instant::now();
    let mut up = match linux::bring_up(&mut lane) {
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
        let env = up.cargo_env(arch, &format!("{lane}/target"), false);
        let oracle_env = up.cargo_env(arch, &format!("{lane}/libc-oracle-target"), false);
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
            println!("\n=== {tag}: aterm-uds, aterm-pty, aterm-libc, amacs-files tests ===");
            let manifest = format!("{repo}/Cargo.toml");
            let workspace = |args: &[&str]| cargo(&env, &manifest, "test", args);
            let out = test_run(&up, workspace, &tests);
            rows.push(translator_not_runs(
                test_verdict(&format!("{tag} tests: uds pty libc files"), &out),
                &out.text,
                arch,
            ));
            if let Err(s) = crate::stop::check() {
                stopped = Some(s);
                break 'arches;
            }
            // `--examples` builds their live checks too (the sweep, the job probe and the walk
            // held to `du`), which `--tests` alone never compiles.
            let mut system = vec!["--no-fail-fast", "--tests", "--examples"];
            for p in SYSTEM {
                system.extend(["-p", p]);
            }
            system.extend(["--", "--nocapture"]);
            if arch == Arch::X86_64 {
                system.push(ROSETTA_ONE_AT_A_TIME);
            }
            println!("\n=== {tag}: aterm-sysprobe, aterm-vitals, aterm-diskwalk tests ===");
            let out = test_run(&up, workspace, &system);
            rows.push(translator_not_runs(
                test_verdict(&format!("{tag} tests: sysprobe vitals diskwalk"), &out),
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
            let out = up.guest.shell_streamed(
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
            let name = shells_row(&tag);
            println!("\n=== {name}: {SHELLS} ===");
            let mut shells_env = env.clone();
            shells_env.push(shells_bash());
            let out = test_run(
                &up,
                |args: &[&str]| cargo(&shells_env, &manifest, "test", args),
                &["--no-fail-fast", "--tests", "-p", SHELLS],
            );
            rows.push(test_verdict(&name, &out));
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
            let bus = up.guest.shell_streamed(
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
                    let out = up.guest.shell_streamed(
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
        println!("\n=== {tag}: build aterm-gui, aterm-ctl ===");
        let build = [
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
        let out = up.guest.shell_streamed(
            &up.home,
            &cargo(&env, &format!("{repo}/Cargo.toml"), "build", &build),
            "  | ",
        );
        let built = out.ok();
        // The GUI rows that follow are timed: they run on the quiet machine, from here to the
        // end of this arch ([`crate::admission`]), sharing the native-window lane
        // with both Timing and functional Instances in other lab runs.
        let quiet = if built {
            crate::admission::window_lane()
                .and_then(|()| crate::admission::quiet(&format!("{tag} GUI rows")))
                .map(Some)
        } else {
            Ok(None)
        };
        rows.push(Row {
            name: format!("{tag} build aterm-gui"),
            verdict: if built { Verdict::Pass } else { Verdict::Fail },
            secs: out.secs,
            detail: if built {
                String::new()
            } else {
                tally(&out.text).error.unwrap_or_else(|| out.tail(5))
            },
        });
        // An interrupt that ended its wait is the run's ([`crate::stop::note`]): no GUI row
        // starts, and the run ends NOT RUN.
        let _quiet = match quiet {
            Ok(quiet) => quiet,
            Err(f) => {
                if let Some(m) = f.msg {
                    eprintln!("aterm-lab: {m}");
                }
                stopped = f.stopped.map(crate::stop::Stopped);
                break 'arches;
            }
        };
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
                "{tag} {} under aterm-gui: {} ({})",
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
            let (verdict, detail) = crate::linux_gone::drive(&gui, display, end, &results);
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
    if let Some(held) = up.guest.host_disk() {
        disk.push(format!(
            "host: the VM disk holds {} (sparse, <= {} GiB; trimmed after a clean)",
            linux::gib(held.bytes),
            linux::DISK_GIB
        ));
    }
    up.end(ask.keep_up);
    let report = render(&rows, &disk, t0.elapsed().as_secs_f64());
    print!("\n{report}");
    let path = results.join("report.txt");
    if let Err(e) = crate::util::write_fresh(&path, &report) {
        eprintln!("aterm-lab: linux: {}: {e}", path.display());
    } else {
        println!("report and frames: {}", results.display());
    }
    if let Some(s) = stopped {
        // A failure found before an interrupt ended a wait stays the run's verdict
        // ([`crate::stop::status`]).
        if crate::stop::ended_a_wait() && exit_code(&rows) == 1 {
            return Ok(1);
        }
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
#[path = "linux_proof_tests.rs"]
mod tests;
