// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
