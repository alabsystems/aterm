// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        "GPU rendering on Microsoft Basic Render Driver (D3d11)\r\n",
    );
    let (answers, stderr) = parse_session(&out);
    assert_eq!(answers.len(), VERBS.len());
    assert_eq!(answers[2].verb, format!("send {TYPED}"));
    let (detail, frame) = judge(&answers, &stderr).unwrap_or_else(|e| unreachable!("{e}"));
    assert!(
        detail.contains("drew on Microsoft Basic Render Driver (D3d11)"),
        "{detail}"
    );
    assert_eq!(frame.as_deref(), Some("C:\\lane\\smoke-aarch64\\frame.png"));
    let local = Path::new("/captured/aarch64-headless.png");
    let copied = |code| Out {
        code: Some(code),
        text: "captured scp result".into(),
        secs: 0.0,
        signal: None,
        not_started: None,
        lost: None,
    };
    assert!(copied_frame(&detail, local, &copied(0)).is_ok());
    assert!(
        copied_frame(&detail, local, &copied(1))
            .is_err_and(|why| why.contains("required frame was not retained"))
    );
}

#[test]
fn a_session_fails_on_a_cpu_frame_a_real_gpu_or_a_wrong_answer() {
    let warp = "GPU rendering on Microsoft Basic Render Driver (D3d11)\n";
    let mut cpu = PASSING;
    cpu[10] = "OK backend=cpu rows=30";
    let (a, e) = parse_session(&session(&cpu, warp));
    assert!(judge(&a, &e).is_err_and(|e| e.contains("backend=cpu")));
    let (a, e) = parse_session(&session(
        &PASSING,
        "GPU rendering on Some Real GPU (D3d11)\n",
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
