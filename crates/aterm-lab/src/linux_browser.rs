// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux guest browser verification lane (`STOCK EXCEPTION`: guest builds `aterm-verify` with
//! stock Rust).

use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use aterm_uds::spawnfd::Stdio;

use crate::linux::{self, Up};
use crate::linux_guest::{self as guest, Arch, Display};
use crate::linux_proof::{Row, Verdict, hang_detector};
use crate::{Fail, LAB, Res};

/// What `browser` was asked.
#[derive(Debug, PartialEq, Eq)]
pub struct BrowserArgs {
    pub arches: Vec<Arch>,
    pub keep_build: bool,
    pub keep_up: bool,
}

/// Parses `linux browser` command-line arguments.
///
/// # Errors
/// A usage error.
pub fn parse(args: &[String]) -> Result<BrowserArgs, Fail> {
    let mut p = BrowserArgs {
        arches: Vec::new(),
        keep_build: false,
        keep_up: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--arch" => {
                let w = it.next().map_or("", String::as_str);
                let arch = Arch::parse(w).ok_or_else(|| {
                    Fail::usage(format!("linux browser: --arch {w}: aarch64 or x86_64"))
                })?;
                if !p.arches.contains(&arch) {
                    p.arches.push(arch);
                }
            }
            "--keep-build" => p.keep_build = true,
            "--keep-up" => p.keep_up = true,
            other => {
                return Err(Fail::usage(format!(
                    "linux browser: unknown argument '{other}' ({LAB} linux help)"
                )));
            }
        }
    }
    if p.arches.is_empty() {
        p.arches = Arch::ALL.to_vec();
    }
    Ok(p)
}

/// Returns the `browser-drive` session names for `arch` and whether they require an X display.
#[must_use]
pub const fn sessions(arch: Arch) -> (&'static [&'static str], bool) {
    match arch {
        Arch::X86_64 => (&["swiftshader"], false),
        Arch::Aarch64 => (&["firefox"], true),
    }
}

/// The row's name for `arch`.
#[must_use]
pub fn row_name(arch: Arch) -> String {
    let what = match arch {
        Arch::X86_64 => "chrome-headless-shell linux64, WebGL2 on SwiftShader",
        Arch::Aarch64 => "Firefox (Playwright, Ubuntu 24.04) under Xvfb",
    };
    format!("{} ({}) browser: {what}", arch.name(), arch.runs())
}

/// Maps `browser-drive`'s exit status to a [`Verdict`].
#[must_use]
pub const fn verdict_of(code: Option<i32>) -> Verdict {
    match code {
        Some(0) => Verdict::Pass,
        Some(linux::NOT_RUN) => Verdict::NotRun,
        _ => Verdict::Fail,
    }
}

/// Extracts summary and failure lines from `browser-drive` output.
#[must_use]
pub fn detail_of(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("ok ")
                || t.starts_with("FAIL ")
                || t.starts_with("NOT RUN ")
                || l.starts_with("browser: ")
                || l.starts_with("firefox: ")
                || [
                    "GPU adapter",
                    "against the recording",
                    "egative control",
                    "byte-identical",
                ]
                .iter()
                .any(|k| l.contains(k))
                || (l.contains(": ") && l.contains(" > "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `linux browser …`.
///
/// # Errors
/// A usage error, or the lane could not be brought up (NOT RUN).
pub fn cmd_browser(args: &[String]) -> Res {
    let ask = parse(args)?;
    crate::stop::install();
    let results = linux::state_dir()?.join(format!(
        "browser-{}",
        crate::util::compact_stamp(crate::util::now_secs())
    ));
    std::fs::create_dir_all(&results)
        .map_err(|e| Fail::die(format!("{}: {e}", results.display())))?;
    let t0 = Instant::now();
    let mut rows = Vec::new();
    let site = results.join("site");
    let laid = lay_site(&site);
    let ok = laid.verdict == Verdict::Pass;
    rows.push(laid);
    if ok {
        let _lock = linux::LaneLock::take()?;
        let up = linux::bring_up()?;
        rows.extend(up.timings.iter().map(|step| Row {
            name: format!("vm {}", step.name),
            verdict: if step.failed.is_some() {
                Verdict::NotRun
            } else {
                Verdict::Pass
            },
            secs: step.secs,
            detail: step.failed.clone().unwrap_or_default(),
        }));
        let stopped = drive(&up, &ask, &site, &results, &mut rows);
        if !ask.keep_build
            && let Err(e) = up.clean_build()
        {
            eprintln!("aterm-lab: linux: build output NOT removed: {e}");
        }
        if ask.keep_up {
            up.stop_units();
        } else {
            up.release();
        }
        if let Err(s) = stopped {
            return Err(s.into());
        }
    }
    let report =
        crate::linux_proof::render_as("LINUX LANE BROWSER", &rows, &[], t0.elapsed().as_secs_f64());
    print!("\n{report}");
    let path = results.join("report.txt");
    if let Err(e) = std::fs::write(&path, &report) {
        eprintln!("aterm-lab: linux: {}: {e}", path.display());
    } else {
        println!("report and frames: {}", results.display());
    }
    let code = crate::linux_proof::exit_code(&rows);
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

/// `verify browser-site <site>` here, by the targo that runs the lab
/// (`$CARGO`; the lab's own PATH holds no toolchain), its output shown as it
/// comes: the row.
fn lay_site(site: &Path) -> Row {
    println!("\n=== this Mac: both web modules built and bound (verify browser-site) ===");
    let t0 = Instant::now();
    let targo = std::env::var_os("CARGO").unwrap_or_else(|| "targo".into());
    let mut cmd = aterm_uds::spawnfd::command(&targo);
    cmd.args(["--unverified", "verify", "browser-site"])
        .arg(site)
        .current_dir(&crate::paths::paths().repo)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = cmd.status();
    let secs = t0.elapsed().as_secs_f64();
    let name = "this Mac: verify browser-site (both modules, wasm32 dev)".to_string();
    match status {
        Ok(s) => Row {
            name,
            verdict: verdict_of(s.code()),
            secs,
            detail: if s.success() {
                site.display().to_string()
            } else {
                format!("verify browser-site exited {s} (above)")
            },
        },
        Err(e) => Row {
            name,
            verdict: Verdict::NotRun,
            secs,
            detail: format!("could not run {}: {e}", targo.to_string_lossy()),
        },
    }
}

/// Copy the site into the guest and run each arch's row.
fn drive(
    up: &Up,
    ask: &BrowserArgs,
    site: &Path,
    results: &Path,
    rows: &mut Vec<Row>,
) -> Result<(), crate::stop::Stopped> {
    let lane = up.lane_dir();
    let repo = linux::repo();
    let guest_site = format!("{lane}/site");
    let _ = up.sh(&["rm".into(), "-rf".into(), guest_site.clone()]);
    let copied = up.lima.copy_dir_in(site, &lane);
    if !copied.ok() {
        rows.push(Row {
            name: "the site copied into the guest".into(),
            verdict: Verdict::NotRun,
            secs: copied.secs,
            detail: copied.tail(5),
        });
        return Ok(());
    }
    for &arch in &ask.arches {
        let name = row_name(arch);
        println!("\n=== {name} ===");
        let started = Instant::now();
        let row = |verdict: Verdict, detail: String| Row {
            name: name.clone(),
            verdict,
            secs: started.elapsed().as_secs_f64(),
            detail,
        };
        if arch == Arch::X86_64 {
            let reg = up.sh(&["cat".into(), guest::ROSETTA_BINFMT.into()]);
            if !guest::rosetta_registered(&reg.text) {
                rows.push(row(
                    Verdict::NotRun,
                    format!(
                        "Rosetta's binfmt registration is not enabled in the guest ({}): {}",
                        guest::ROSETTA_BINFMT,
                        reg.tail(3)
                    ),
                ));
                continue;
            }
        }
        let mut env = guest::cargo_env(arch, &format!("{lane}/target"), false);
        let mut build = vec![
            "cargo".to_string(),
            "build".into(),
            "--locked".into(),
            "--manifest-path".into(),
            format!("{repo}/Cargo.toml"),
        ];
        build.extend(arch.target_args());
        build.extend(["-p", "aterm-verify", "--bin", "aterm-verify"].map(String::from));
        let built = up
            .lima
            .shell_streamed(&up.home, &guest::with_env(&env, &build), "  | ");
        if !built.ok() {
            rows.push(row(
                Verdict::Fail,
                format!("aterm-verify did not build:\n{}", built.tail(15)),
            ));
            continue;
        }
        let verify = format!("{}/aterm-verify", arch.bin_dir(&format!("{lane}/target")));
        let fetched = up.lima.shell_streamed(
            &up.home,
            &guest::with_env(
                &env,
                &[
                    verify.clone(),
                    "provision".into(),
                    "chrome-headless-shell".into(),
                    "firefox".into(),
                ],
            ),
            "  | ",
        );
        if !fetched.ok() {
            rows.push(row(
                Verdict::NotRun,
                format!("verify provision:\n{}", fetched.tail(8)),
            ));
            continue;
        }
        let (engines, display) = sessions(arch);
        if display {
            if let Err(why) = linux::start_display(up, Display::Xvfb) {
                rows.push(row(Verdict::NotRun, why));
                continue;
            }
            env.push(Display::Xvfb.client_env(&up.runtime));
        }
        let out_dir = format!("{lane}/browser-{}", arch.name());
        let _ = up.sh(&["rm".into(), "-rf".into(), out_dir.clone()]);
        let mut argv = hang_detector(DRIVE_HANG_SECS);
        argv.extend(guest::with_env(
            &env,
            &[
                verify,
                "browser-drive".into(),
                guest_site.clone(),
                out_dir.clone(),
            ],
        ));
        argv.extend(
            engines
                .iter()
                .flat_map(|e| ["--engine".to_string(), (*e).to_string()]),
        );
        let out = up.lima.shell_streamed(&up.home, &argv, "  | ");
        up.stop_units();
        let kept = keep(up, &out_dir, &results.join(arch.name()));
        let mut detail = detail_of(&out.text);
        if let Some(why) = kept {
            let _ = write!(detail, "\nthe frames were NOT copied out: {why}");
        }
        if matches!(out.code, None | Some(124 | 137)) {
            let _ = write!(
                detail,
                "\nbrowser-drive did not end within {DRIVE_HANG_SECS} s"
            );
        }
        rows.push(row(verdict_of(out.code), detail));
        crate::stop::check()?;
    }
    Ok(())
}

/// Hang-detector timeout in seconds for a `browser-drive` run.
pub const DRIVE_HANG_SECS: u32 = 900;

/// Copy the guest's `out_dir` (each session's frames, timings and browser
/// log) to `to`: why not, when it was not copied.
fn keep(up: &Up, out_dir: &str, to: &Path) -> Option<String> {
    let _ = std::fs::remove_dir_all(to);
    let parent = to.parent()?;
    let copied = up.lima.copy_dir_out(out_dir, parent);
    if !copied.ok() {
        return Some(copied.tail(3));
    }
    let landed = parent.join(Path::new(out_dir).file_name()?);
    std::fs::rename(&landed, to)
        .err()
        .map(|e| format!("{} -> {}: {e}", landed.display(), to.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn both_arches_unless_one_is_named() {
        let p = parse(&[]).expect("parse");
        assert_eq!(p.arches, Arch::ALL.to_vec());
        assert!(!p.keep_build && !p.keep_up);
        let p = parse(&s(&["--arch", "x86_64", "--keep-build"])).expect("parse");
        assert_eq!(p.arches, vec![Arch::X86_64]);
        assert!(p.keep_build);
        assert!(parse(&s(&["--arch", "riscv"])).is_err());
        assert!(parse(&s(&["--display", "xvfb"])).is_err());
    }

    /// x86_64 runs Chromium's SwiftShader with no display; aarch64 runs
    /// Firefox, which needs one.
    #[test]
    fn each_arch_runs_its_pinned_browser() {
        assert_eq!(sessions(Arch::X86_64), (&["swiftshader"][..], false));
        assert_eq!(sessions(Arch::Aarch64), (&["firefox"][..], true));
        assert!(row_name(Arch::X86_64).starts_with("x86_64 (Rosetta) browser"));
    }

    /// A partial drive never passes: NOT RUN stays NOT RUN, and a kill or a
    /// usage error fails.
    #[test]
    fn the_drive_s_exit_is_the_row_s_verdict() {
        assert_eq!(verdict_of(Some(0)), Verdict::Pass);
        assert_eq!(verdict_of(Some(1)), Verdict::Fail);
        assert_eq!(verdict_of(Some(3)), Verdict::NotRun);
        assert_eq!(verdict_of(Some(124)), Verdict::Fail);
        assert_eq!(verdict_of(None), Verdict::Fail);
    }

    #[test]
    fn the_row_keeps_the_ladder_and_the_adapter() {
        let text = "  Compiling x\n\n=== browser-drive (s) ===\nbrowser: /c/chs (pinned)\n\
                    browser [swiftshader]: HeadlessChrome/155; GPU adapter ANGLE (SwiftShader)\n\
                    browser [swiftshader]: noise\n  ok    browser (…), WebGL2 on ANGLE swiftshader\n\
                    browser [swiftshader]: diff-6: 440x168, max byte delta 1 against the recording\n\
                    \x20 FAIL  browser (…), init_offscreen in a worker\n\
                    parity-2: differs from the recorded frame by 64 > 8 (1 bytes)\n";
        let d = detail_of(text);
        assert!(d.contains("GPU adapter ANGLE (SwiftShader)"), "{d}");
        assert!(d.contains("diff-6: 440x168"), "{d}");
        assert!(d.contains("by 64 > 8"), "{d}");
        assert!(d.contains("ok    browser (…), WebGL2"), "{d}");
        assert!(d.contains("FAIL  browser (…), init_offscreen"), "{d}");
        assert!(!d.contains("Compiling") && !d.contains("noise"), "{d}");
    }
}
