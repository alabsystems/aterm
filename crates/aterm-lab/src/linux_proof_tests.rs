// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_frame_requires_a_fresh_successful_copy_of_a_decodable_png() {
    let top = aterm_tempfile::tempdir().unwrap();
    let image = "OK 1 1 /private/guest/frame.png";
    let mut png = Vec::new();
    {
        let mut encoder = aterm_png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(aterm_png::ColorType::Rgb);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[1, 2, 3]).unwrap();
        writer.finish().unwrap();
    }
    let outcome = |code| Out {
        code: Some(code),
        text: "copy outcome".into(),
        secs: 0.0,
        signal: None,
        not_started: None,
        lost: None,
    };
    let good = top.path().join("good.png");
    copy_frame(image, &good, |source, to| {
        assert_eq!(source, "/private/guest/frame.png");
        std::fs::write(to, &png).unwrap();
        outcome(0)
    })
    .unwrap();
    assert_eq!(std::fs::read(&good).unwrap(), png);
    assert!(
        copy_frame(image, &good, |_, _| panic!(
            "a stale destination was reused"
        ))
        .is_err()
    );
    assert_eq!(std::fs::read(&good).unwrap(), png);
    assert!(
        copy_frame("OK 1 1", &top.path().join("missing-path.png"), |_, _| {
            panic!("an image reply without a path reached the copier")
        })
        .is_err()
    );
    for (name, bytes, code) in [
        ("failed.png", Some(png.as_slice()), 1),
        ("absent.png", None, 0),
        ("invalid.png", Some(b"not a PNG".as_slice()), 0),
        ("truncated.png", Some(b"\x89PNG\r\n\x1a\n".as_slice()), 0),
    ] {
        let to = top.path().join(name);
        let result = copy_frame(image, &to, |_, to| {
            if let Some(bytes) = bytes {
                std::fs::write(to, bytes).unwrap();
            }
            outcome(code)
        });
        assert!(result.is_err(), "{name}: {result:?}");
    }
    assert!(
        copy_frame(image, &top.path().join("directory.png"), |_, to| {
            std::fs::create_dir(to).unwrap();
            outcome(0)
        })
        .is_err()
    );
    #[cfg(unix)]
    {
        let link = top.path().join("linked.png");
        assert!(
            copy_frame(image, &link, |_, to| {
                std::os::unix::fs::symlink(&good, to).unwrap();
                outcome(0)
            })
            .is_err(),
            "a copied symlink does not establish a retained frame"
        );
        assert!(copy_frame(image, &link, |_, _| panic!("followed an existing link")).is_err());
        assert_eq!(std::fs::read(&good).unwrap(), png);
    }
}

#[test]
fn proof_results_are_exclusive_and_leave_previous_artifacts_intact() {
    let top = aterm_tempfile::tempdir().unwrap();
    let first = fresh_results(top.path(), "proof-fixed-stamp-").unwrap();
    std::fs::write(first.join("frame.png"), b"previous evidence").unwrap();
    let second = fresh_results(top.path(), "proof-fixed-stamp-").unwrap();
    assert_ne!(first, second);
    assert_eq!(
        std::fs::read(first.join("frame.png")).unwrap(),
        b"previous evidence"
    );
    assert_eq!(std::fs::read_dir(&second).unwrap().count(), 0);
    let not_directory = top.path().join("not-a-directory");
    std::fs::write(&not_directory, b"keep").unwrap();
    assert!(fresh_results(&not_directory, "proof-fixed-stamp-").is_err());
    assert_eq!(std::fs::read(&not_directory).unwrap(), b"keep");
}

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
        signal: None,
        not_started: None,
        lost: None,
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
fn gone_runs_its_displays_alone() {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    let gone = parse_gone(&[]).expect("parses");
    assert!(gone.gone_only);
    assert_eq!(gone.arches, Arch::ALL);
    assert_eq!(gone.displays, crate::linux_gone::GONE);
    let one = parse_gone(&s(&["--display", "mutter"])).expect("parses");
    assert_eq!(one.displays, [Display::Mutter]);
    assert!(!parse_proof(&[]).expect("parses").gone_only);
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
        signal: None,
        not_started: None,
        lost: None,
    };
    let r = test_verdict("t", &out);
    assert_eq!(r.verdict, Verdict::Fail);
    assert!(r.detail.contains("E0425"), "{}", r.detail);
    let clean = Out {
        code: Some(0),
        text: "     Running tests/a.rs (t/debug/deps/a-0123)\ntest result: ok. 4 passed; 0 failed; 0 ignored; 0 measured\n".into(),
        secs: 1.0,
        signal: None,
        not_started: None,
        lost: None,
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
        signal: None,
        not_started: None,
        lost: None,
    };
    assert_eq!(test_verdict("t", &nothing).verdict, Verdict::NotRun);
    let cut = Out {
        code: Some(255),
        text: "test a ... ok\ntest b ... FAILED\n".into(),
        secs: 1.0,
        signal: None,
        not_started: None,
        lost: Some("krunkit aborted: SIGABRT in f (lib)".into()),
    };
    let r = test_verdict("t", &cut);
    assert_eq!(r.verdict, Verdict::NotRun, "{}", r.detail);
    assert!(
        r.detail.ends_with("krunkit aborted: SIGABRT in f (lib)"),
        "{}",
        r.detail
    );
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
    let windows = "aterm-gui: GPU rendering on zink Vulkan 1.3(Venus) (Gl)\n\
                   aterm-gpu (gl arm): the window's presenter cannot attach (x); posting it\n\
                   aterm-gpu (gl arm): Wayland windows draw on llvmpipe (LLVM 20.1.2, 128 bits)\n";
    assert_eq!(
        window_renderer(windows),
        Some("llvmpipe (LLVM 20.1.2, 128 bits)"),
        "the windows' renderer, not the probe's nor another GL-arm note"
    );
    assert_eq!(
        window_renderer("aterm-gui: GPU rendering on x (Gl)\n"),
        None
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

fn rgba(w: usize, h: usize, paint: impl Fn(usize, usize) -> [u8; 3]) -> (Vec<u8>, usize, usize) {
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
        signal: None,
        not_started: None,
        lost: None,
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
