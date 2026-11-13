// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn walk(text: &str) -> String {
    format!(
        "apps 1\n\
         node depth=0 path=/org/a11y/atspi/accessible/root role=application name=\"aterm-gui\" \
         description=\"\" states=- children=1\n\
         node depth=1 path=/org/a11y/atspi/accessible/1 role=frame name=\"~\" description=\"\" \
         states=active,enabled children=1\n\
         node depth=2 path=/org/a11y/atspi/accessible/2 role=terminal name=\"aterm terminal\" \
         description=\"\" states=focused characters=30 caret=20 text=\"{text}\"\n"
    )
}

#[test]
fn a_walk_reads_back_the_frame_when_the_terminal_holds_the_echo() {
    let w = walk("lane$ echo A11Y-$((6*7))\\nA11Y-42\\nlane$");
    assert_eq!(read_back(&w, BEFORE_ECHO), Ok("~".to_string()));
    assert!(
        read_back(&w, AFTER_ECHO)
            .unwrap_err()
            .contains("does not hold REJOINED-42")
    );
    assert!(
        read_back("walk failed (exit Some(1)): no application", BEFORE_ECHO)
            .unwrap_err()
            .contains("no application named aterm-gui")
    );
    let other = w.replace("name=\"aterm-gui\"", "name=\"gnome-terminal\"");
    assert!(read_back(&other, BEFORE_ECHO).is_err());
}

fn log(lost: &str) -> String {
    format!(
        "2026-10-19T10:00:00Z [INFO] {JOINED} :1.7\n\
         2026-10-19T10:00:05Z [WARN] {lost} (a write waited 5 s); screen readers lose this process\n\
         2026-10-19T10:00:05Z [INFO] accessibility: left the AT-SPI bus: why\n\
         2026-10-19T10:00:31Z [INFO] {JOINED} :1.9\n"
    )
}

#[test]
fn a_rejoin_is_the_join_the_loss_and_the_join_again_in_order() {
    let stopped = log(STOPPED);
    let [joined, lost, again] = rejoined(&stopped, STOPPED).unwrap();
    assert!(joined.ends_with(":1.7") && again.ends_with(":1.9"));
    assert!(lost.contains(STOPPED));
    assert!(rejoined(&log(FAILED), STOPPED).is_err());
    let cut: String = log(STOPPED).lines().take(3).collect::<Vec<_>>().join("\n");
    assert!(
        rejoined(&cut, STOPPED)
            .unwrap_err()
            .contains("never that the publisher rejoined")
    );
    let late = format!("{STOPPED}\n{JOINED} :1.2\n");
    assert!(rejoined(&late, STOPPED).is_err());
}

fn evidence(ordeal: Ordeal) -> Evidence {
    Evidence {
        before: walk("lane$ echo A11Y-$((6*7))\\nA11Y-42"),
        after: walk("A11Y-42\\nlane$ echo REJOINED-$((6*7))\\nREJOINED-42"),
        log: log(ordeal.said()),
        stderr: "aterm-gui: GPU rendering on llvmpipe\n".into(),
        alive: true,
    }
}

#[test]
fn a_rejoin_passes_only_read_back_alive_and_without_a_panic() {
    for ordeal in Ordeal::ALL {
        let ok = judge(&evidence(ordeal), ordeal).unwrap();
        assert!(ok.contains(ordeal.said()), "{ok}");
        let mut e = evidence(ordeal);
        e.stderr
            .push_str("thread 'atspi' panicked at publisher.rs:1:1:\n");
        assert!(judge(&e, ordeal).unwrap_err().contains("panicked"));
        let mut e = evidence(ordeal);
        e.alive = false;
        assert!(judge(&e, ordeal).unwrap_err().contains("not running"));
        let mut e = evidence(ordeal);
        e.after = walk("lane$");
        assert!(
            judge(&e, ordeal)
                .unwrap_err()
                .starts_with("after the rejoin")
        );
    }
    assert!(judge(&evidence(Ordeal::Killed), Ordeal::Frozen).is_err());
}

fn bench(rows: &[(&str, &str, &str)], summary: &str) -> String {
    let mut text = String::from("[DEBUG] accessibility: publishing on the AT-SPI bus as :1.4\n");
    for (name, status, detail) in rows {
        text.push_str(&format!(
            "{MEASURE_ROW}a11y-atspi::{name} {status}\n{detail}\n"
        ));
    }
    text + summary
}

#[test]
fn a_bench_passes_only_a_complete_matrix_of_passes_and_exit_0() {
    let passed = [
        ("rejoin", "PASS", "ran"),
        ("frozen", "PASS", "left the frozen bus after 4389 changes"),
        ("control", "PASS", "never let go"),
    ];
    let all = "measure result: a11y-atspi planned=3 passed=3 failed=0 refused=0\n";
    let (verdict, detail) = measured("a11y-atspi", &bench(&passed, all), true);
    assert_eq!(verdict, Verdict::Pass, "{detail}");
    assert!(detail.contains("frozen PASS: left the frozen bus after 4389 changes"));
    assert_eq!(
        measured("a11y-atspi", &bench(&passed, all), false).0,
        Verdict::Fail
    );
    assert_eq!(
        measured("a11y-other", &bench(&passed, all), true).0,
        Verdict::Fail
    );
    let failed = "measure result: a11y-atspi planned=3 passed=1 failed=1 refused=1\n";
    assert_eq!(
        measured("a11y-atspi", &bench(&passed, failed), false).0,
        Verdict::Fail
    );
    let refused = "measure result: a11y-atspi planned=3 passed=0 failed=0 refused=3\n";
    assert_eq!(
        measured("a11y-atspi", &bench(&passed, refused), false).0,
        Verdict::NotRun
    );
    for summary in [
        "error: could not compile `aterm-a11y`\n",
        &format!("{all}{all}"),
        "measure result: a11y-atspi planned=3 passed=2 failed=0 refused=0\n",
        "measure result: a11y-atspi planned=0 passed=0 failed=0 refused=0\n",
        "measure result: a11y-atspi planned=3 passed=3 failed=0\n",
    ] {
        let (verdict, detail) = measured("a11y-atspi", &bench(&passed, summary), true);
        assert_eq!(verdict, Verdict::Fail, "{summary}: {detail}");
    }
}

#[test]
fn only_rosettas_own_abort_of_a_dumped_aterm_gui_is_not_run() {
    let stderr = "\
        aterm-gui: GPU rendering on llvmpipe (LLVM 20.1.2, 256 bits) (Gl)\n\
        libEGL warning: DRI3 error: Could not get DRI3 device\n\
        assertion failed [!readlink_result_self.is_error]: Could not readlinkat /proc/13425/exe\n\
        (ThreadContextFcntl.cpp:142 was_rosetta_process_invoked_directly)\n";
    let dumped = "ActiveState=failed Result=core-dump ExecMainCode=3 ExecMainStatus=5";
    let abort = translator_abort(Arch::X86_64, dumped, stderr).unwrap();
    assert!(abort.contains("readlinkat") && abort.contains("ThreadContextFcntl.cpp:142"));
    let (verdict, detail) = translated_away(Arch::X86_64, dumped, stderr, "why".into());
    assert_eq!(verdict, Verdict::NotRun);
    assert!(detail.contains("Rosetta aborted aterm-gui") && detail.ends_with("why"));
    assert_eq!(translator_abort(Arch::Aarch64, dumped, stderr), None);
    for alive in [
        "ActiveState=active Result=success ExecMainCode=0 ExecMainStatus=0",
        "ActiveState=failed Result=exit-code ExecMainCode=1 ExecMainStatus=101",
        "",
    ] {
        assert_eq!(
            translator_abort(Arch::X86_64, alive, stderr),
            None,
            "{alive}"
        );
    }
    let panicked = "thread 'main' panicked at crates/aterm-a11y/src/atspi/publisher.rs:1:1:\n\
                    assertion failed: joined\n";
    assert_eq!(translator_abort(Arch::X86_64, dumped, panicked), None);
    assert_eq!(
        translated_away(Arch::X86_64, dumped, panicked, "why".into()),
        (Verdict::Fail, "why".to_string())
    );
}

fn queued(at: &str, ssml: &str) -> String {
    format!("[Mon Oct 19 {at} 2026 : 1] speechd:     {QUEUED}{ssml}| with priority 2\n")
}

#[test]
fn speech_is_the_queued_messages_unmarked_and_unescaped() {
    let log = [
        queued("21:59:06", "<speak>Screen reader on.</speak>"),
        "[Mon Oct 19 21:59:08 2026 : 2] speechd:     Incoming text: |<speak>x</speak>|\n".into(),
        queued(
            "21:59:08",
            "<speak>a &amp; &lt;b&gt; &quot;c&quot; &apos;d&apos;</speak>",
        ),
    ]
    .concat();
    assert_eq!(spoken(&log), ["Screen reader on.", "a & <b> \"c\" 'd'"]);
}

#[test]
fn orca_must_speak_the_frame_the_text_and_the_typed_output() {
    let log = [
        queued("21:59:06", "<speak><mark name=\"0:6\"/>Screen <mark name=\"7:13\"/>reader <mark name=\"14:17\"/>on.</speak>"),
        queued("21:59:07", "<speak><mark name=\"0:1\"/>~ <mark name=\"2:8\"/>frame.</speak>"),
        queued("21:59:07", "<speak><mark name=\"0:5\"/>lane$\n</speak>"),
        queued("21:59:07", "<speak><mark name=\"0:5\"/>space</speak>"),
        queued("21:59:07", "<speak><mark name=\"0:4\"/>left <mark name=\"5:10\"/>shift</speak>"),
        queued("21:59:07", "<speak> <mark name=\"1:5\"/>echo <mark name=\"6:7\"/>O</speak>"),
        queued("21:59:07", "-"),
        queued("21:59:08", "6"),
        queued("21:59:08", "<speak><mark name=\"0:6\"/>return</speak>"),
        queued("21:59:08", ")"),
        queued("21:59:08", "<speak><mark name=\"0:7\"/>ORCA-42\n<mark name=\"8:13\"/>lane$</speak>"),
    ]
    .concat();
    let speech = spoken(&log);
    assert_eq!(
        speech,
        [
            "Screen reader on.",
            "~ frame.",
            "lane$\n",
            "space",
            "left shift",
            " echo O",
            "-",
            "6",
            "return",
            ")",
            "ORCA-42\nlane$",
        ]
    );
    let passed = heard(&speech, "~").unwrap();
    assert!(passed.contains("the terminal lane$ and"), "{passed}");
    let without = |gone: &dyn Fn(&str) -> bool| -> Vec<String> {
        speech.iter().filter(|s| !gone(s)).cloned().collect()
    };
    assert!(
        heard(&without(&|s| s.starts_with("ORCA-42")), "~")
            .unwrap_err()
            .contains("typed line's output")
    );
    assert!(
        heard(&speech, "lane-frame")
            .unwrap_err()
            .contains("frame's name")
    );
    assert!(
        heard(&without(&|s| s == "lane$\n"), "~")
            .unwrap_err()
            .contains("terminal's text")
    );
    let frame_and_output = ["~ frame.".to_string(), "ORCA-42\nlane$".into()];
    assert!(
        heard(&frame_and_output, "~")
            .unwrap_err()
            .contains("terminal's text")
    );
    let echoed = [
        "~ frame.".to_string(),
        "lane$ echo O".into(),
        "ORCA-42\nlane$".into(),
    ];
    assert!(heard(&echoed, "~").unwrap_err().contains("terminal's text"));
}

#[test]
fn where_orca_hears_no_key_an_unspoken_output_must_be_for_want_of_one() {
    let speech: Vec<String> = [
        "Screen reader on.",
        "~ frame.",
        "lane$\n",
        " e",
        "echo ORCA-$((6*7))",
        "~",
        "lane$",
    ]
    .map(String::from)
    .to_vec();
    let deaf = "22:57:33.180230 - EVENT MANAGER: Processing window:activate\n\
                22:57:37.397468 - DEFAULT: Not speaking inserted string due to lack of cause\n";
    let passed = heard_keyless(&speech, "~", deaf).unwrap();
    assert!(
        passed.contains("left the output ORCA-42 unspoken"),
        "{passed}"
    );
    assert!(
        heard(&speech, "~")
            .unwrap_err()
            .contains("typed line's output")
    );
    let heard_keys = format!("{deaf}22:57:36.1 - TERMINAL: Presenting keyboard event e\n");
    assert!(
        heard_keyless(&speech, "~", &heard_keys)
            .unwrap_err()
            .contains("though it heard 1 keys")
    );
    assert!(
        heard_keyless(&speech, "~", "22:57:33.18 - EVENT MANAGER: Processing\n")
            .unwrap_err()
            .contains("never says")
    );
    assert!(
        heard_keyless(&speech, "lane-frame", deaf)
            .unwrap_err()
            .contains("frame's name")
    );
    let no_text: Vec<String> = speech.iter().filter(|s| *s != "lane$\n").cloned().collect();
    assert!(
        heard_keyless(&no_text, "~", deaf)
            .unwrap_err()
            .contains("terminal's text")
    );
    let mut spoken_out = speech.clone();
    spoken_out.push("ORCA-42".into());
    assert_eq!(
        heard_keyless(&spoken_out, "~", deaf),
        heard(&spoken_out, "~")
    );
    assert!(heard(&spoken_out, "~").is_ok());
}

#[test]
fn an_output_spoken_with_its_whole_line_is_spoken() {
    let log = [
        queued("23:31:12", "<speak><mark name=\"0:6\"/>Screen <mark name=\"7:13\"/>reader <mark name=\"14:17\"/>on.</speak>"),
        queued("23:31:13", "<speak><mark name=\"0:1\"/>~ <mark name=\"2:8\"/>frame.</speak>"),
        queued("23:31:13", "<speak><mark name=\"0:5\"/>lane$\n</speak>"),
        queued("23:31:13", "<speak> <mark name=\"1:5\"/>echo <mark name=\"6:18\"/>ORCA-$((6*7)</speak>"),
        queued("23:31:13", "<speak><mark name=\"0:5\"/>lane$ <mark name=\"6:10\"/>echo <mark name=\"11:24\"/>ORCA-$((6*7))\n<mark name=\"25:32\"/>ORCA-42\n<mark name=\"33:38\"/>lane$</speak>"),
    ]
    .concat();
    let speech = spoken(&log);
    assert_eq!(
        speech,
        [
            "Screen reader on.",
            "~ frame.",
            "lane$\n",
            " echo ORCA-$((6*7)",
            "lane$ echo ORCA-$((6*7))\nORCA-42\nlane$",
        ]
    );
    let passed = heard(&speech, "~").unwrap();
    assert!(
        passed.contains("the output lane$ echo ORCA-$((6*7)) ORCA-42 lane$"),
        "{passed}"
    );
    assert_eq!(heard_keyless(&speech, "~", ""), heard(&speech, "~"));
    let unanswered = [
        "~ frame.".to_string(),
        "lane$\n".into(),
        "lane$ echo ORCA-$((6*7))\n".into(),
        "echo ORCA-$((6*7))".into(),
    ];
    assert!(
        heard(&unanswered, "~")
            .unwrap_err()
            .contains("typed line's output")
    );
}

#[test]
fn mutter_s_command_names_its_xwayland_and_its_socket() {
    let given = "WAYLAND_DISPLAY=aterm-lane-mutter-orca\nDISPLAY=:0\n\
                 XAUTHORITY=/run/user/501/.mutter-Xwaylandauth.86VRX3\n";
    assert_eq!(
        mutter_env(given, "/run/user/501").unwrap(),
        (
            vec![
                ("DISPLAY".to_string(), ":0".to_string()),
                (
                    "XAUTHORITY".to_string(),
                    "/run/user/501/.mutter-Xwaylandauth.86VRX3".to_string()
                ),
            ],
            (
                "WAYLAND_DISPLAY".to_string(),
                "/run/user/501/aterm-lane-mutter-orca".to_string()
            ),
        )
    );
    assert!(
        mutter_env("WAYLAND_DISPLAY=aterm-lane-mutter-orca\n", "/run/user/501")
            .unwrap_err()
            .contains("no DISPLAY")
    );
    assert!(
        mutter_env("DISPLAY=:0\nWAYLAND_DISPLAY=w\nXAUTHORITY=\n", "/r")
            .unwrap_err()
            .contains("no XAUTHORITY")
    );
}

#[test]
fn the_seat_types_the_line_a_key_at_a_time() {
    assert_eq!(
        seat_keys("echo O-$(("),
        [
            "tap:e",
            "tap:c",
            "tap:h",
            "tap:o",
            "tap:space",
            "tap:O",
            "tap:-",
            "tap:$",
            "tap:(",
            "tap:("
        ]
    );
    assert_eq!(seat_keys(ORCA_TYPED).len(), ORCA_TYPED.len());
}

/// Whether `steps` have Mutter's frames client on the registry before Orca and aterm-gui start
/// (`frames`), so Orca never hears the window that brings it there, or never wait for it
/// (`!frames`).
fn frames_first(steps: &[OrcaStep], frames: bool) -> Result<(), String> {
    let at = |step| steps.iter().position(|s| *s == step);
    let orca = at(OrcaStep::Orca).ok_or_else(|| format!("Orca never starts: {steps:?}"))?;
    let gui = at(OrcaStep::Gui).ok_or_else(|| format!("aterm-gui never starts: {steps:?}"))?;
    match (frames, at(OrcaStep::Frames)) {
        (true, Some(f)) if f < orca && f < gui => Ok(()),
        (true, Some(f)) if f > gui => Err(format!(
            "the frames wait follows aterm-gui's start: {steps:?}"
        )),
        (true, Some(_)) => Err(format!("the frames wait follows Orca's start: {steps:?}")),
        (true, None) => Err(format!("no frames wait: {steps:?}")),
        (false, None) => Ok(()),
        (false, Some(_)) => Err(format!("a frames wait with no Mutter: {steps:?}")),
    }
}

#[test]
fn under_mutter_the_frames_client_joins_the_registry_before_orca_and_aterm_gui_start() {
    for screen in OrcaScreen::ALL {
        let steps = orca_steps(screen);
        assert_eq!(frames_first(&steps, screen.mutter()), Ok(()), "{screen:?}");
        assert_eq!(
            steps.iter().filter(|s| **s == OrcaStep::Frames).count(),
            usize::from(screen.mutter()),
            "{screen:?}: {steps:?}"
        );
    }
    assert!(OrcaScreen::MutterX11.mutter() && OrcaScreen::MutterWayland.mutter());
    assert!(!OrcaScreen::Xvfb.mutter());
    let mutter = orca_steps(OrcaScreen::MutterX11);
    let without: Vec<OrcaStep> = mutter
        .iter()
        .copied()
        .filter(|s| *s != OrcaStep::Frames)
        .collect();
    assert!(
        frames_first(&without, true)
            .unwrap_err()
            .contains("no frames wait")
    );
    let mut late = without.clone();
    late.push(OrcaStep::Frames);
    assert!(
        frames_first(&late, true)
            .unwrap_err()
            .contains("aterm-gui's start")
    );
    let mut heard = without.clone();
    let gui = heard.iter().position(|s| *s == OrcaStep::Gui).unwrap();
    heard.insert(gui, OrcaStep::Frames);
    assert!(
        frames_first(&heard, true)
            .unwrap_err()
            .contains("Orca's start")
    );
    assert!(frames_first(&mutter, false).is_err());
}

#[test]
fn speech_dispatcher_uses_libao_so_dummy_loads_without_a_pulse_socket() {
    assert!(
        SPEECHD_CONF
            .lines()
            .any(|l| l == "AudioOutputMethod \"libao\""),
        "{SPEECHD_CONF}"
    );
}
