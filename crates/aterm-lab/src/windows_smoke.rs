// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `windows smoke [--arch A]...`: the Windows lane's acceptance on each
//! architecture, in the guest ([`crate::windows`]):
//!
//! 1. `cargo build -p aterm -p aterm-gui -p aterm-ctl` for the arch (MSVC;
//!    x86_64 cross-built on the arm64 guest);
//! 2. the shipped `aterm.exe --version`, which must print this checkout's
//!    build number (`build: <HEAD's committer time>`: the lane pins the build
//!    facts, the copy having no `.git`);
//! 3. one `aterm-gui --headless` in a scratch config with the automatic lanes
//!    off ([`crate::instance::CONFIG_OFF`]) and Windows PowerShell as its
//!    shell, driven over its control socket by `aterm-ctl` (AF_UNIX over
//!    winsock): `ready`, a typed line whose output ([`ECHO`]) only running it
//!    prints, `resize 30 100` and `dims` showing it, `status` with
//!    `render=ok`, `metrics percentiles`, one `image` copied to this Mac; and
//!    the session must have drawn on the GPU (`metrics` says `backend=gpu`
//!    once the frame is taken) on WARP, Windows' software Direct3D (the VM
//!    has no GPU: its device line names the Microsoft Basic Render Driver).
//!
//! x86_64 runs under Prism. Exit 0 every row passed, 1 a row failed, 3 NOT
//! RUN (no VM could be brought up). Frames and the report:
//! `$LAB_STATE/windows/`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::linux::Out;
use crate::linux_proof::{ECHO, dims_are, drew_on_gpu, field, present_summary, reply};
use crate::paths::paths;
use crate::windows::{self, Layout, Up, lane_lock, not_run};
use crate::windows_guest::{self as guest, Arch};
use crate::{Fail, LAB, Res};

/// What the session types: Windows PowerShell prints [`ECHO`] only by
/// running it (the typed text never holds it). No double quote: Windows
/// PowerShell 5.1 passes one to `aterm-ctl` unescaped.
pub const TYPED: &str = "Write-Output ('LANE-' + 6*7)";

/// The adapter WARP presents as (DXGI's software adapter).
pub const WARP: &str = "Microsoft Basic Render Driver";

/// The directory `cargo` puts `arch`'s binaries in, in the guest.
#[must_use]
pub fn bin_dir(arch: Arch) -> String {
    match arch {
        Arch::Aarch64 => format!("{}\\debug", guest::TARGET),
        Arch::X86_64 => format!("{}\\{}\\debug", guest::TARGET, arch.triple()),
    }
}

/// The scratch directory of `arch`'s session in the guest.
#[must_use]
pub fn session_dir(arch: Arch) -> String {
    format!("{}\\smoke-{}", guest::LANE, arch.name())
}

/// The control verbs the session sends, in order.
pub const VERBS: [&[&str]; 11] = [
    &["ready", "60000"],
    &["dims"],
    &["send", TYPED],
    &["key", "enter"],
    &["await", "match", ECHO, "timeout=60000"],
    &["resize", "30", "100"],
    &["dims"],
    &["status"],
    &["metrics", "percentiles"],
    &["image", "frame.png"],
    &["metrics"],
];

/// The PowerShell that runs `arch`'s session in one SSH call: aterm-gui
/// started headless with its stderr kept, each of [`VERBS`] sent by
/// `aterm-ctl` (every one, a failure included), each answer under a
/// `@@ <exit> <verb>` line, then `@@ stderr` and aterm-gui's stderr; aterm-gui
/// stopped last.
#[must_use]
pub fn session_script(arch: Arch) -> String {
    let q = guest::ps_quote;
    let mut s = String::new();
    let _ = writeln!(s, "$ProgressPreference = 'SilentlyContinue'");
    let _ = writeln!(s, "$bin = {}", q(&bin_dir(arch)));
    let _ = writeln!(s, "$d = {}", q(&session_dir(arch)));
    s.push_str(
        "if (Test-Path $d) { Remove-Item -Recurse -Force $d }\n\
         New-Item -ItemType Directory -Force -Path \"$d\\cfg\\aterm\", \"$d\\state\" | Out-Null\n",
    );
    let _ = writeln!(
        s,
        "Set-Content -Encoding ascii -Path \"$d\\cfg\\aterm\\aterm.toml\" -Value {}",
        q(crate::instance::CONFIG_OFF)
    );
    s.push_str(
        "$env:XDG_CONFIG_HOME = \"$d\\cfg\"\n\
         $env:ATERM_STATE_HOME = \"$d\\state\"\n\
         $sock = \"$d\\aterm.sock\"\n\
         $p = Start-Process -FilePath \"$bin\\aterm-gui.exe\" -ArgumentList '--headless','--control-sock',$sock,'-e','powershell.exe','-NoLogo','-NoProfile' \
         -PassThru -NoNewWindow -RedirectStandardOutput \"$d\\stdout.log\" -RedirectStandardError \"$d\\stderr.log\"\n\
         $deadline = (Get-Date).AddSeconds(60)\n\
         while (-not (Test-Path $sock) -and -not $p.HasExited -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 250 }\n\
         function Ctl([string[]] $words) {\n\
           $out = & \"$bin\\aterm-ctl.exe\" --sock $sock --timeout 90 @words 2>&1 | Out-String\n\
           Write-Output \"@@ $LASTEXITCODE $($words -join ' ')\"\n\
           Write-Output $out.TrimEnd()\n\
         }\n",
    );
    for verb in VERBS {
        let words: Vec<String> = verb.iter().map(|w| q(w)).collect();
        let _ = writeln!(s, "Ctl @({})", words.join(", "));
    }
    s.push_str(
        "if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force; $p.WaitForExit(30000) | Out-Null }\n\
         Write-Output '@@ stderr'\n\
         Get-Content -Raw \"$d\\stderr.log\"\n",
    );
    s
}

/// One verb's answer in a session's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub code: Option<i32>,
    pub verb: String,
    pub text: String,
}

/// A session's output read back: each verb's answer in order, then
/// aterm-gui's stderr.
#[must_use]
pub fn parse_session(out: &str) -> (Vec<Answer>, String) {
    let mut answers: Vec<Answer> = Vec::new();
    let mut stderr: Option<String> = None;
    for line in out.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(s) = stderr.as_mut() {
            s.push_str(line);
            s.push('\n');
        } else if line == "@@ stderr" {
            stderr = Some(String::new());
        } else if let Some(head) = line.strip_prefix("@@ ") {
            let (code, verb) = head.split_once(' ').unwrap_or((head, ""));
            answers.push(Answer {
                code: code.parse().ok(),
                verb: verb.to_string(),
                text: String::new(),
            });
        } else if let Some(a) = answers.last_mut() {
            a.text.push_str(line);
            a.text.push('\n');
        }
    }
    (answers, stderr.unwrap_or_default())
}

/// A verb's name and what its answer must satisfy.
type Want<'a> = (&'a str, &'a dyn Fn(&str) -> bool);

/// The session's verdict from its answers and stderr: the detail of a
/// pass, the guest path of its frame, and the device it named.
///
/// # Errors
/// The first verb whose answer was not what it must be, a session that did
/// not draw on the GPU, or one whose device is not WARP.
pub fn judge(answers: &[Answer], stderr: &str) -> Result<(String, Option<String>), String> {
    let want: [Want<'_>; 11] = [
        ("ready", &|r| r.starts_with("OK ready")),
        ("dims", &|r| r.starts_with("OK ")),
        ("send", &|r| r.starts_with("OK")),
        ("key", &|r| r.starts_with("OK")),
        ("await", &|r| r.starts_with("OK match")),
        ("resize", &|r| r.starts_with("OK")),
        ("dims", &|r| dims_are(r, 30, 100)),
        ("status", &|r| field(r, "render") == Some("ok")),
        ("metrics", &|r| r.starts_with("OK")),
        ("image", &|r| r.starts_with("OK ")),
        ("metrics", &|r| r.starts_with("OK ")),
    ];
    if answers.len() != want.len() {
        return Err(format!(
            "the session answered {} of {} verbs",
            answers.len(),
            want.len()
        ));
    }
    for (a, (verb, ok)) in answers.iter().zip(want) {
        let r = reply(&a.text);
        if !a.verb.starts_with(verb) || a.code != Some(0) || !ok(r) {
            return Err(format!(
                "`{}` answered {:?} (exit {:?})",
                a.verb,
                crate::util::one_line(r, 200),
                a.code
            ));
        }
    }
    let r = |i: usize| answers.get(i).map_or("", |a| reply(&a.text));
    let device = drew_on_gpu(stderr, field(r(10), "backend"))?;
    let device = device.unwrap_or("(no device line)");
    if !device.contains(WARP) {
        return Err(format!("drew on {device}, not on WARP ({WARP})"));
    }
    let frame = r(9)
        .split_whitespace()
        .nth(3)
        .ok_or_else(|| "the image reply named no frame to retain".to_string())?
        .to_string();
    Ok((
        format!(
            "typed, {ECHO} echoed, resized to 30x100; render=ok resizes={}; {}; drew on {device}",
            field(r(7), "resizes").unwrap_or("?"),
            present_summary(r(8)),
        ),
        Some(frame),
    ))
}

/// What `smoke` was asked.
fn parse_args(args: &[String]) -> Result<Vec<Arch>, Fail> {
    let mut arches = Vec::new();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--arch" => {
                let w = args.get(i + 1).map_or("", String::as_str);
                arches.push(Arch::parse(w).ok_or_else(|| {
                    Fail::usage(format!("windows smoke: --arch {w}: aarch64 or x86_64"))
                })?);
                i += 2;
            }
            other => {
                return Err(Fail::usage(format!(
                    "windows smoke: unknown argument '{other}' ({LAB} windows help)"
                )));
            }
        }
    }
    if arches.is_empty() {
        arches = vec![Arch::Aarch64, Arch::X86_64];
    }
    Ok(arches)
}

/// `$LAB_STATE/windows`, made when missing.
fn state_dir() -> Result<PathBuf, Fail> {
    let dir = paths().need_state()?.join("windows");
    std::fs::create_dir_all(&dir).map_err(|e| Fail::die(format!("{}: {e}", dir.display())))?;
    Ok(dir)
}

/// One row: its name, and a pass's detail or a failure's.
type Row = (String, Result<String, String>);

fn arch_rows(up: &Up, arch: Arch, repo: &Path, results: &Path) -> Vec<Row> {
    let mut rows = Vec::new();
    let name = |what: &str| format!("{} {what}", arch.name());
    let mut env = guest::cargo_env(arch);
    env.extend(windows::build_facts(repo));
    let build = [
        "cargo",
        "build",
        "-p",
        "aterm",
        "-p",
        "aterm-gui",
        "-p",
        "aterm-ctl",
    ]
    .map(String::from);
    println!("windows: {} ({}) build …", arch.name(), arch.runs());
    let out = up.ssh.streamed(
        &guest::exec_command(guest::SRC, &env, &build),
        &format!("windows: {}: ", arch.name()),
    );
    if !out.ok() {
        rows.push((
            name("build"),
            Err(format!("cargo build failed: {}", out.tail(10))),
        ));
        return rows;
    }
    rows.push((
        name("build"),
        Ok(format!(
            "aterm, aterm-gui, aterm-ctl built ({:.0} s)",
            out.secs
        )),
    ));
    let version = up
        .ssh
        .run(&format!("{}\\aterm.exe --version", bin_dir(arch)));
    let epoch = env
        .iter()
        .find(|(k, _)| k == "SOURCE_DATE_EPOCH")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    rows.push((
        name("aterm --version"),
        version_names_build(&version.text, version.ok(), &epoch),
    ));
    println!("windows: {} headless aterm-gui on WARP …", arch.name());
    let session = up.ssh.ps(&session_script(arch));
    let (answers, stderr) = parse_session(&session.text);
    let verdict = judge(&answers, &stderr).and_then(|(detail, frame)| {
        let guest_path = frame.ok_or_else(|| "the session named no frame to retain".to_string())?;
        let local = results.join(format!("{}-headless.png", arch.name()));
        let copied = up.ssh.get(&guest_path.replace('\\', "/"), &local);
        copied_frame(&detail, &local, &copied)
    });
    if verdict.is_err() {
        let _ = std::fs::write(
            results.join(format!("{}-session.txt", arch.name())),
            &session.text,
        );
    }
    rows.push((name("headless aterm-gui (WARP)"), verdict));
    rows
}

/// A frame is required evidence, not an optional warning on a passing row.
fn copied_frame(detail: &str, local: &Path, copied: &Out) -> Result<String, String> {
    if copied.ok() {
        Ok(format!("{detail}; frame {}", local.display()))
    } else {
        Err(format!(
            "required frame was not retained: {}",
            copied.tail(2)
        ))
    }
}

/// `aterm --version`'s verdict: its identity line, and the `build:` line this
/// checkout's build number names (`SOURCE_DATE_EPOCH`, HEAD's committer time).
fn version_names_build(text: &str, ok: bool, epoch: &str) -> Result<String, String> {
    let identity = text.lines().next().unwrap_or("").trim();
    let want = format!("build: {epoch}");
    if ok
        && identity.starts_with("aterm ")
        && !epoch.is_empty()
        && text.lines().any(|l| l.trim() == want)
    {
        Ok(format!("{identity}, {want}"))
    } else {
        Err(format!(
            "answered {:?} (ok={ok}), want `aterm …` and `{want}`",
            text.trim()
        ))
    }
}

/// `windows smoke [--arch A]...`.
///
/// # Errors
/// A usage error, or the lane could not be brought up (NOT RUN).
pub fn cmd_smoke(args: &[String]) -> Res {
    let arches = parse_args(args)?;
    let l = Layout::find()?;
    let _lock = lane_lock(&l)?;
    let results = state_dir()?;
    let up = windows::bring_up(false)?;
    let repo = crate::paths::physical(&paths().repo);
    if let Err(e) = windows::copy_tree(&up.layout, &up.ssh, &repo) {
        up.release().map_err(Fail::die)?;
        return Err(not_run(format!("the checkout did not copy: {e}")));
    }
    let mut rows = Vec::new();
    for arch in arches {
        rows.extend(arch_rows(&up, arch, &repo, &results));
    }
    let cleanup = up.release();
    let mut report = String::new();
    let mut failed = 0;
    for (row, verdict) in &rows {
        let line = match verdict {
            Ok(d) => format!("PASS {row}: {d}"),
            Err(e) => {
                failed += 1;
                format!("FAIL {row}: {e}")
            }
        };
        println!("{line}");
        report.push_str(&line);
        report.push('\n');
    }
    let path = results.join("smoke.txt");
    let _ = std::fs::write(&path, &report);
    println!("windows: report {}", path.display());
    windows::released(i32::from(failed > 0), cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(replies: &[&str], stderr: &str) -> String {
        let mut s = String::new();
        for (verb, r) in VERBS.iter().zip(replies) {
            let _ = writeln!(s, "@@ 0 {}\r\n{r}\r", verb.join(" "));
        }
        let _ = write!(s, "@@ stderr\r\n{stderr}");
        s
    }

    const PASSING: [&str; 11] = [
        "OK ready",
        "OK 24 80",
        "OK",
        "OK",
        "OK match LANE-42",
        "OK",
        "OK 30 100",
        "OK phase=idle render=ok resizes=1",
        "OK n_present=3 present_p50_ms=1.0 present_p95_ms=2.0 present_p99_ms=3.0",
        "OK 800 600 C:\\lane\\smoke-aarch64\\frame.png",
        "OK backend=gpu rows=30",
    ];

    #[test]
    fn a_session_on_warp_passes_and_names_its_frame() {
        let out = session(
            &PASSING,
            "GPU rendering on Microsoft Basic Render Driver (Dx12)\r\n",
        );
        let (answers, stderr) = parse_session(&out);
        assert_eq!(answers.len(), VERBS.len());
        assert_eq!(answers[2].verb, format!("send {TYPED}"));
        let (detail, frame) = judge(&answers, &stderr).unwrap_or_else(|e| unreachable!("{e}"));
        assert!(
            detail.contains("drew on Microsoft Basic Render Driver (Dx12)"),
            "{detail}"
        );
        assert_eq!(frame.as_deref(), Some("C:\\lane\\smoke-aarch64\\frame.png"));
        let local = Path::new("/captured/aarch64-headless.png");
        let copied = |code| Out {
            code: Some(code),
            text: "captured scp result".into(),
            secs: 0.0,
        };
        assert!(copied_frame(&detail, local, &copied(0)).is_ok());
        assert!(
            copied_frame(&detail, local, &copied(1))
                .is_err_and(|why| why.contains("required frame was not retained"))
        );
    }

    #[test]
    fn a_session_fails_on_a_cpu_frame_a_real_gpu_or_a_wrong_answer() {
        let warp = "GPU rendering on Microsoft Basic Render Driver (Dx12)\n";
        let mut cpu = PASSING;
        cpu[10] = "OK backend=cpu rows=30";
        let (a, e) = parse_session(&session(&cpu, warp));
        assert!(judge(&a, &e).is_err_and(|e| e.contains("backend=cpu")));
        let (a, e) = parse_session(&session(
            &PASSING,
            "GPU rendering on Some Real GPU (Dx12)\n",
        ));
        assert!(judge(&a, &e).is_err_and(|e| e.contains("not on WARP")));
        let mut no_echo = PASSING;
        no_echo[4] = "ERR timeout";
        let (a, e) = parse_session(&session(&no_echo, warp));
        assert!(judge(&a, &e).is_err_and(|e| e.starts_with("`await match LANE-42")));
        let mut no_frame = PASSING;
        no_frame[9] = "OK 800 600";
        let (a, e) = parse_session(&session(&no_frame, warp));
        assert!(judge(&a, &e).is_err_and(|why| why.contains("no frame")));
        let (a, e) = parse_session(&session(&PASSING[..5], warp));
        assert!(judge(&a, &e).is_err());
    }

    #[test]
    fn the_version_names_this_checkouts_build_number() {
        let text =
            "aterm 0.107.0\r\nby Andrew Yates\r\nbuild: 1792535837\r\nrunning: C:\\aterm.exe\r\n";
        assert_eq!(
            version_names_build(text, true, "1792535837").as_deref(),
            Ok("aterm 0.107.0, build: 1792535837")
        );
        assert!(version_names_build(text, true, "1792535838").is_err());
        assert!(version_names_build("aterm 0.107.0\nrunning: x\n", true, "1792535837").is_err());
        assert!(version_names_build(text, false, "1792535837").is_err());
        assert!(version_names_build(text, true, "").is_err());
    }

    #[test]
    fn the_typed_line_never_holds_its_echo_and_the_script_sends_every_verb() {
        assert!(!TYPED.contains(ECHO));
        assert!(!TYPED.contains('"'));
        let s = session_script(Arch::X86_64);
        assert!(s.contains("$bin = 'C:\\lane\\target\\x86_64-pc-windows-msvc\\debug'"));
        assert_eq!(s.matches("\nCtl @(").count(), VERBS.len());
        assert!(s.contains("Ctl @('send', 'Write-Output (''LANE-'' + 6*7)')"));
        assert!(s.contains("'--headless'"));
        assert_eq!(bin_dir(Arch::Aarch64), "C:\\lane\\target\\debug");
    }
}
