// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_record_reads_back_as_each_capture_in_the_order_what_it_shows_came() {
    let text = "captures 1000 every=50\n\
                capture 2600 stage-workstation 01-stage-workstation.png 1800x1288 trigger=2300 begun=2320 Workstation started\n\
                capture 2200 first-present lane-first-present.png 1800x1288 trigger=2000 begun=2000 the first tab's prompt\n\
                capture-error 14100 before-nehelper trigger=3700 aterm ctl image: ERR image: no window to capture\n\
                capture 2700 torn\n\
                capture x stage-spotlight 02.png 1x1 trigger=1 begun=1\n";
    let shots = read_shots(text);
    assert_eq!(
        shots
            .iter()
            .map(|s| (s.key.as_str(), s.file.as_deref(), s.trigger_ms))
            .collect::<Vec<_>>(),
        [
            ("first-present", Some("lane-first-present.png"), 2000),
            ("stage-workstation", Some("01-stage-workstation.png"), 2300),
            ("before-nehelper", None, 3700),
        ],
        "torn lines are no capture"
    );
    assert_eq!(shots[1].begun_ms, 2320);
    assert_eq!(shots[1].done_ms, 2600);
    assert_eq!(shots[1].size, "1800x1288");
    assert_eq!(shots[1].words, "Workstation started");
    assert_eq!(
        shots[2].words,
        "aterm ctl image: ERR image: no window to capture"
    );
}

#[test]
fn capture_records_cannot_choose_shell_words_or_host_paths() {
    for file in [
        "../outside.png",
        "/tmp/outside.png",
        "01-nested/frame.png",
        "01-nested\\frame.png",
        "01-stage'quoted.png",
        "01-stage\"quoted.png",
        "01-stage-$(id).png",
        "01-stage-\0.png",
        "01-stage-\u{1b}.png",
        "01-stage.jpg",
        "01-stage.PNG",
        "01-stage.png.bak",
        ".png",
        "01-.png",
        "01-stage--workstation.png",
        "contact.png",
    ] {
        let shots = read_shots(&format!(
            "capture 20 first 01-stage-workstation.png 9x9 trigger=10 begun=11 valid\n\
             capture 40 rejected {file} 9x9 trigger=30 begun=31 guest\n\
             capture 60 last lane-first-present.png 9x9 trigger=50 begun=51 valid\n"
        ));
        assert_eq!(shots.len(), 3, "{file:?}: retain the missed capture");
        let rejected = &shots[1];
        assert!(rejected.file.is_none(), "{file:?}");
        assert!(rejected.size.is_empty(), "{file:?}");
        assert!(!rejected.words.is_empty(), "{file:?}: visible failure");
        assert_eq!(
            (rejected.trigger_ms, rejected.begun_ms, rejected.done_ms),
            (30, 31, 40)
        );
        let admitted: Vec<_> = frames(&shots).into_iter().map(|(file, _)| file).collect();
        assert_eq!(
            admitted,
            ["01-stage-workstation.png", "lane-first-present.png"],
            "{file:?}: only producer filenames reach frame copying"
        );
    }
}

/// A frame shows every milestone whose shot names its file; a shot of a file of its own is a
/// frame of its own (the negative control); a failed shot is no frame.
#[test]
fn a_frame_shows_each_milestone_that_names_it_and_no_other() {
    let text = "capture 8700 stage-permissions-2 04-stage-permissions-2.png 9x9 trigger=5492 begun=8669 a\n\
                capture 8700 before-nehelper 04-stage-permissions-2.png 9x9 trigger=6835 begun=8669 b\n\
                capture 6200 stage-secrets 03-stage-secrets.png 9x9 trigger=5148 begun=6195 c\n\
                capture-error 9900 after-nehelper trigger=9380 refused\n\
                capture 5100 first-present lane-first-present.png 9x9 trigger=5077 begun=5077 d\n";
    let shots = read_shots(text);
    let keys = |shots: &[Shot]| -> Vec<(String, Vec<String>)> {
        frames(shots)
            .into_iter()
            .map(|(file, its)| {
                (
                    file.to_owned(),
                    its.into_iter().map(|s| s.key.clone()).collect(),
                )
            })
            .collect()
    };
    assert_eq!(
        keys(&shots),
        [
            ("lane-first-present.png", vec!["first-present"]),
            ("03-stage-secrets.png", vec!["stage-secrets"]),
            (
                "04-stage-permissions-2.png",
                vec!["stage-permissions-2", "before-nehelper"]
            ),
        ]
        .map(|(file, keys)| (
            file.to_owned(),
            keys.into_iter().map(str::to_owned).collect()
        ))
    );
    let apart: Vec<Shot> = shots
        .iter()
        .cloned()
        .map(|mut s| {
            s.file = s.file.map(|_| format!("{}.png", s.key));
            s
        })
        .collect();
    assert_eq!(
        keys(&apart).len(),
        4,
        "each shot of a file of its own is its own frame"
    );
}

#[test]
fn frames_go_beside_the_record_else_in_the_state_folder() {
    let state = Path::new("/state");
    assert_eq!(
        frames_dir(
            Some(Path::new("/runs/g2-first-launch.md")),
            state,
            "first-launch",
            "person-noclt",
            "20261031-120000"
        ),
        Path::new("/runs/g2-first-launch-frames/first-launch-person-noclt-20261031-120000")
    );
    assert_eq!(
        frames_dir(None, state, "first-launch", "", "20261031-120000"),
        Path::new("/state/mac-nux/frames/first-launch-20261031-120000")
    );
}
