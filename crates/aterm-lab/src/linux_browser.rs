// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux guest browser verification lane (`STOCK EXCEPTION`: the modules build on the checkout's
//! stock pin's wasm32 std, as `verify browser`'s do, and the guest builds `aterm-verify` with
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
    p.arches = crate::linux_gpu::arches(std::mem::take(&mut p.arches), "browser")?;
    Ok(p)
}

/// What `browser-drive` runs for one arch.
#[derive(Debug, PartialEq, Eq)]
pub struct Sessions {
    /// Its sessions, as `--engine` names them.
    pub engines: &'static [&'static str],
    /// The sessions whose worker row the set leaves out (`--no-worker`).
    pub no_worker: &'static [&'static str],
    /// Whether they need an X display (Firefox's; WebKit's WPE draws headless).
    pub display: bool,
}

impl Sessions {
    /// `browser-drive`'s flags for the set: each `--engine`, then each `--no-worker`.
    #[must_use]
    pub fn flags(&self) -> Vec<String> {
        let flag = |name: &str, list: &[&str]| -> Vec<String> {
            list.iter()
                .flat_map(|e| [name.to_string(), (*e).to_string()])
                .collect()
        };
        let mut flags = flag("--engine", self.engines);
        flags.extend(flag("--no-worker", self.no_worker));
        flags
    }
}

/// What `browser-drive` runs for `arch` on the lane's VM or, `gpu`, on the GPU instance
/// (`docs/measured/linux-gpu-fixes-2026-10-30.md`). WPE gives a worker no WebGL2 on either: the
/// lane's VM has no DRM render node, and beside the GPU instance's virtio-gpu one it made none
/// either, so WebKit's worker row is out of the VM's set. The GPU instance drives Firefox alone:
/// under its client environment (Venus alone, zink) WPE aborts at start, finding no CPU Vulkan
/// device for its surfaceless EGL display, and without those overrides it draws on llvmpipe, as on
/// the lane's VM.
#[must_use]
pub const fn sessions(arch: Arch, gpu: bool) -> Sessions {
    match (arch, gpu) {
        (Arch::X86_64, _) => Sessions {
            engines: &["swiftshader"],
            no_worker: &[],
            display: false,
        },
        (Arch::Aarch64, false) => Sessions {
            engines: &["firefox", "webkit"],
            no_worker: &["webkit"],
            display: true,
        },
        (Arch::Aarch64, true) => Sessions {
            engines: &["firefox"],
            no_worker: &[],
            display: true,
        },
    }
}

/// The browsers `verify provision` places in the guest for `arch`'s sessions: Playwright's
/// WebKit is pinned for Linux on arm64 alone.
#[must_use]
pub const fn provisioned(arch: Arch) -> &'static [&'static str] {
    match arch {
        Arch::X86_64 => &["chrome-headless-shell", "firefox"],
        Arch::Aarch64 => &["chrome-headless-shell", "firefox", "webkit"],
    }
}

/// The row's name for `arch` on the lane's VM or, `gpu`, on the GPU instance ([`sessions`]).
#[must_use]
pub fn row_name(arch: Arch, gpu: bool) -> String {
    let what = match (arch, gpu) {
        (Arch::X86_64, _) => "chrome-headless-shell linux64, WebGL2 on SwiftShader",
        (Arch::Aarch64, false) => {
            "Firefox under Xvfb and WebKit's WPE, headless (Playwright, Ubuntu 24.04); WPE's \
             worker row is out, as WPE gives a worker no WebGL2 here"
        }
        (Arch::Aarch64, true) => {
            "Firefox under Xvfb (Playwright, Ubuntu 24.04), on the GPU instance's virtio-gpu; WPE \
             runs on the lane's VM, as it aborts under zink on Venus"
        }
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
                || l.starts_with("webkit: ")
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
    let mut waited = std::time::Duration::ZERO;
    let mut rows = Vec::new();
    let site = results.join("site");
    let laid = lay_site(&site);
    let ok = laid.verdict == Verdict::Pass;
    rows.push(laid);
    if ok {
        let mut lock = linux::LaneLock::boot()?;
        waited = lock.waited();
        let up = linux::bring_up(&mut lock)?;
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
        up.end(ask.keep_up);
        if let Err(s) = stopped {
            return Err(s.into());
        }
    }
    let report = crate::linux_proof::render_as(
        "LINUX LANE BROWSER",
        &rows,
        &[],
        t0.elapsed().saturating_sub(waited).as_secs_f64(),
    );
    print!("\n{report}");
    let path = results.join("report.txt");
    if let Err(e) = crate::util::write_fresh(&path, &report) {
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

/// `verify browser-site <site>` here, by the targo that runs the lab, in the
/// shim's build cache (`crate::util::targo`), its output shown as it comes: the
/// row.
fn lay_site(site: &Path) -> Row {
    println!("\n=== this Mac: both web modules built and bound (verify browser-site) ===");
    let t0 = Instant::now();
    let mut cmd = crate::util::targo(&["--unverified", "verify", "browser-site"]);
    cmd.arg(site)
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
            detail: format!("could not run {}: {e}", cmd.get_program().to_string_lossy()),
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
    let copied = up.guest.copy_dir_in(site, &lane);
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
        let gpu = crate::linux_gpu::on();
        let name = row_name(arch, gpu);
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
        let mut env = up.cargo_env(arch, &format!("{lane}/target"), false);
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
            .guest
            .shell_streamed(&up.home, &guest::with_env(&env, &build), "  | ");
        if !built.ok() {
            rows.push(row(
                Verdict::Fail,
                format!("aterm-verify did not build:\n{}", built.tail(15)),
            ));
            continue;
        }
        let verify = format!("{}/aterm-verify", arch.bin_dir(&format!("{lane}/target")));
        let mut provision = vec![verify.clone(), "provision".into()];
        provision.extend(provisioned(arch).iter().map(|b| (*b).to_string()));
        let fetched = up
            .guest
            .shell_streamed(&up.home, &guest::with_env(&env, &provision), "  | ");
        if !fetched.ok() {
            rows.push(row(
                Verdict::NotRun,
                format!("verify provision:\n{}", fetched.tail(8)),
            ));
            continue;
        }
        let set = sessions(arch, gpu);
        if set.display {
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
        argv.extend(set.flags());
        let out = up.guest.shell_streamed(&up.home, &argv, "  | ");
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
    let copied = up.guest.copy_dir_out(out_dir, parent);
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
    /// Firefox, which needs one, and WebKit, each provisioned only where pinned.
    /// WebKit's worker row is out of the lane's VM's set alone, and every
    /// session the set names reaches browser-drive.
    #[test]
    fn each_arch_runs_its_pinned_browser() {
        let x86 = sessions(Arch::X86_64, false);
        assert_eq!((x86.engines, x86.display), (&["swiftshader"][..], false));
        assert!(x86.no_worker.is_empty());
        let vm = sessions(Arch::Aarch64, false);
        assert_eq!((vm.engines, vm.display), (&["firefox", "webkit"][..], true));
        assert_eq!(vm.no_worker, ["webkit"]);
        let gpu = sessions(Arch::Aarch64, true);
        assert_eq!((gpu.engines, gpu.display), (&["firefox"][..], true));
        assert!(gpu.no_worker.is_empty());
        assert_eq!(
            sessions(Arch::Aarch64, false).flags(),
            [
                "--engine",
                "firefox",
                "--engine",
                "webkit",
                "--no-worker",
                "webkit"
            ]
        );
        assert_eq!(
            sessions(Arch::Aarch64, true).flags(),
            ["--engine", "firefox"]
        );
        assert!(!provisioned(Arch::X86_64).contains(&"webkit"));
        assert!(provisioned(Arch::Aarch64).contains(&"webkit"));
        assert!(row_name(Arch::X86_64, false).starts_with("x86_64 (Rosetta) browser"));
        assert_ne!(
            row_name(Arch::Aarch64, false),
            row_name(Arch::Aarch64, true)
        );
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
