// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! trail-diff tests: the comparator's policy (§15.0.2) on canned dump output, the closure check
//! on canned git output, the base default, the plant and the exit codes.

use super::*;

/// One frame as `trail_dump` prints it, with no arm and its light bits off.
const FRAME: &str = "\
F 0 t=0.000 light=0 arms=-
  streams glow_under=2:1111 glow_under_ramps=1:2222 cursor_glow_add=3:3333 cursor_glow_add_ramps=1:4444 glow_halo=0:5555 fx_layers=1:6666 word_decorations=0:7777 cursor_fill_override=0:8888
  px 80x40:9999
  deadline 16.666
  sounds -
  state n=12 stardust=aaaa ribbon=bbbb bed=cccc
  light n=0
";

/// [`FRAME`] with each `(from, to)` replaced once.
fn frame(edits: &[(&str, &str)]) -> String {
    let mut f = FRAME.to_string();
    for (from, to) in edits {
        assert!(f.contains(from), "{from} is in the frame");
        f = f.replacen(from, to, 1);
    }
    f
}

/// A dump of one script with one run of `frames`, renumbered from 0.
fn dump(frames: &[String]) -> String {
    let mut out = String::from("P dark/typing.trail\nS -\n");
    for (n, f) in frames.iter().enumerate() {
        out.push_str(&f.replacen("F 0 t=0.000", &format!("F {n} t={n}.000"), 1));
    }
    out
}

/// The comparison of two dumps' texts.
fn report(head: &str, base: &str) -> Report {
    compare(
        &parse_dump(head).expect("the head's dump parses"),
        &parse_dump(base).expect("the base's dump parses"),
    )
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

#[test]
fn equal_dumps_report_nothing_and_exit_0() {
    let d = dump(&[frame(&[]), frame(&[]), frame(&[])]);
    let r = report(&d, &d);
    assert_eq!(
        r.totals(),
        "scripts=1 frames=3 differing_compared=0 arm_frames=0 state_differing=0"
    );
    assert!(r.firsts.is_empty() && r.arms.is_empty(), "{r:?}");
    assert_eq!(verdict(&r, None), (0, None));
}

#[test]
fn a_differing_frame_on_which_an_arm_drew_passes_when_the_difference_is_confined_to_its_streams() {
    let armed = "light=1 arms=glass:glow_under+glow_under_ramps";
    let head = dump(&[
        frame(&[]),
        frame(&[
            ("light=0 arms=-", armed),
            ("glow_under=2:1111", "glow_under=5:abcd"),
            ("glow_under_ramps=1:2222", "glow_under_ramps=1:dcba"),
            ("px 80x40:9999", "px 80x40:0001"),
            ("deadline 16.666", "deadline 8.333"),
        ]),
    ]);
    let base = dump(&[frame(&[]), frame(&[])]);
    let r = report(&head, &base);
    assert_eq!(r.differing_compared, 0, "{r:?}");
    assert_eq!(r.arm_frames, 1, "{r:?}");
    assert_eq!(r.state_differing, 0, "{r:?}");
    assert_eq!(
        r.arms,
        ["arms dark/typing.trail S - glass:glow_under+glow_under_ramps: F1"]
    );
    assert_eq!(verdict(&r, None).0, 0);
}

#[test]
fn on_an_arm_frame_a_difference_in_a_stream_no_arm_touched_fails() {
    let head = dump(&[frame(&[
        ("light=0 arms=-", "light=1 arms=glass:glow_under"),
        ("fx_layers=1:6666", "fx_layers=2:6667"),
    ])]);
    let base = dump(&[frame(&[])]);
    let r = report(&head, &base);
    assert_eq!((r.differing_compared, r.arm_frames), (1, 1), "{r:?}");
    assert_eq!(
        r.firsts,
        ["differs dark/typing.trail S - F0 t=0.000: fx_layers head=2:6667 base=1:6666"]
    );
    assert_eq!(verdict(&r, None).0, 1);
}

#[test]
fn an_arm_the_base_drew_counts_as_the_head_s_does() {
    let head = dump(&[frame(&[("glow_halo=0:5555", "glow_halo=0:5556")])]);
    let base = dump(&[frame(&[("light=0 arms=-", "light=1 arms=lip:glow_halo")])]);
    let r = report(&head, &base);
    assert_eq!((r.differing_compared, r.arm_frames), (0, 1), "{r:?}");
}

#[test]
fn a_differing_frame_with_no_arm_fails_even_when_its_light_bits_are_on() {
    for edit in [
        ("px 80x40:9999", "px 80x40:9998"),
        ("deadline 16.666", "deadline -"),
        ("cursor_fill_override=0:8888", "cursor_fill_override=1:8889"),
    ] {
        let head = dump(&[frame(&[("light=0", "light=7"), edit])]);
        let base = dump(&[frame(&[])]);
        let r = report(&head, &base);
        assert_eq!(
            (r.differing_compared, r.arm_frames, r.state_differing),
            (1, 0, 0),
            "{edit:?}: {r:?}"
        );
        assert_eq!(verdict(&r, None).0, 1, "{edit:?}");
    }
}

#[test]
fn light_bits_and_light_words_alone_are_never_a_difference() {
    let head = dump(&[frame(&[("light=0", "light=3"), ("light n=0", "light n=9")])]);
    let base = dump(&[frame(&[])]);
    let r = report(&head, &base);
    assert!(!r.differs(), "{r:?}");
}

#[test]
fn any_state_difference_fails_on_every_frame_arms_or_none() {
    let every = "light=1 arms=glass:glow_under+glow_under_ramps+cursor_glow_add+\
                 cursor_glow_add_ramps+glow_halo+fx_layers+word_decorations+cursor_fill_override";
    for (arms, edit) in [
        ("light=0 arms=-", ("ribbon=bbbb", "ribbon=bbbc")),
        (every, ("ribbon=bbbb", "ribbon=bbbc")),
        (every, ("bed=cccc", "bed=cccd")),
        (every, ("state n=12", "state n=13")),
        (every, ("sounds -", "sounds 1:0123")),
    ] {
        let head = dump(&[frame(&[("light=0 arms=-", arms), edit])]);
        let base = dump(&[frame(&[("light=0 arms=-", arms)])]);
        let r = report(&head, &base);
        assert_eq!(
            (r.state_differing, r.differing_compared),
            (1, 0),
            "{edit:?} under {arms}: {r:?}"
        );
        assert_eq!(verdict(&r, None).0, 1, "{edit:?}");
    }
}

#[test]
fn a_frame_one_build_holds_or_lacks_differs_as_its_shape() {
    let held = "F 0 t=0.000 held\n".to_string();
    let r = report(
        &dump(&[held.clone(), frame(&[])]),
        &dump(&[frame(&[]), frame(&[])]),
    );
    assert_eq!((r.differing_compared, r.state_differing), (1, 0), "{r:?}");
    assert!(
        r.firsts[0].ends_with("F0 t=0.000: held head=held base=drawn"),
        "{r:?}"
    );
    let held = dump(std::slice::from_ref(&held));
    let r = report(&held, &held);
    assert!(!r.differs(), "{r:?}");
    let r = report(&dump(&[frame(&[]), frame(&[])]), &dump(&[frame(&[])]));
    assert_eq!((r.frames, r.differing_compared), (2, 1), "{r:?}");
    assert!(
        r.firsts[0].contains("frame head=F1 t=1.000 base=-"),
        "{r:?}"
    );
}

#[test]
fn a_held_frame_compares_main_s_state_under_the_hold() {
    let held = |state: &str| format!("F 0 t=0.000 held\n  state n=12 {state}\n  light n=0\n");
    let same = dump(&[held("stardust=aaaa ribbon=bbbb bed=cccc"), frame(&[])]);
    assert!(!report(&same, &same).differs());
    let moved = dump(&[held("stardust=aaaa ribbon=bbbc bed=cccc"), frame(&[])]);
    let r = report(&moved, &same);
    assert_eq!(
        (r.state_differing, r.differing_compared),
        (1, 0),
        "a state word the hold moved fails: {r:?}"
    );
    assert!(
        r.firsts[0].ends_with("F0 t=0.000: state.ribbon head=bbbc base=bbbb"),
        "{r:?}"
    );
}

#[test]
fn each_script_prints_its_first_difference_and_its_arm_frames_as_ranges() {
    let armed = |f: &str| frame(&[("light=0 arms=-", f)]);
    let glass = "light=1 arms=glass:glow_under";
    let mut head = dump(&[
        armed(glass),
        armed(glass),
        armed(glass),
        frame(&[("stardust=aaaa", "stardust=aaab")]),
        armed(glass),
        frame(&[("px 80x40:9999", "px 80x40:1")]),
    ]);
    let mut base = dump(&[
        armed(glass),
        armed(glass),
        armed(glass),
        frame(&[]),
        armed(glass),
        frame(&[]),
    ]);
    let other = "P light/flip.trail\nS Default Light\n".to_string() + &frame(&[]);
    head.push_str(&other.replacen("px 80x40:9999", "px 80x40:7", 1));
    base.push_str(&other);
    let r = report(&head, &base);
    assert_eq!(
        r.firsts,
        [
            "differs dark/typing.trail S - F3 t=3.000: state.stardust head=aaab base=aaaa",
            "differs light/flip.trail S Default Light F0 t=0.000: px head=80x40:7 base=80x40:9999",
        ]
    );
    assert_eq!(
        r.arms,
        ["arms dark/typing.trail S - glass:glow_under: F0-F2 F4"]
    );
    assert_eq!(
        r.totals(),
        "scripts=2 frames=7 differing_compared=2 arm_frames=4 state_differing=1"
    );
    let printed = r.render();
    assert!(
        printed
            .ends_with("scripts=2 frames=7 differing_compared=2 arm_frames=4 state_differing=1\n"),
        "{printed}"
    );
}

#[test]
fn a_run_or_script_one_build_lacks_differs() {
    let one = dump(&[frame(&[])]);
    let two = format!("{one}S Default\n{FRAME}");
    let r = report(&two, &one);
    assert_eq!(r.differing_compared, 1, "{r:?}");
    assert!(r.firsts[0].contains("run head=Default base=-"), "{r:?}");
    let r = report(&one, &one.replacen("dark/typing", "dark/other", 1));
    assert_eq!(r.differing_compared, 1, "{r:?}");
    assert!(
        r.firsts[0].contains("script head=dark/typing.trail"),
        "{r:?}"
    );
}

#[test]
fn a_dump_line_the_format_does_not_have_is_an_error() {
    assert!(parse_dump("  px 1x1:0\n").unwrap_err().contains("line 1"));
    assert!(
        parse_dump("P a\nF 0 t=0.000 light=0 arms=-\n")
            .unwrap_err()
            .contains("a frame before its run")
    );
    assert!(
        parse_dump("P a\nS -\nF 0 t=0.000 light=0\n")
            .unwrap_err()
            .contains("not a frame line")
    );
    assert!(
        parse_dump("P a\nS -\nbogus\n")
            .unwrap_err()
            .contains("not a dump line")
    );
    let full =
        format!("P a\nS -\n{FRAME}  W glow_under[0] Quad {{ .. }}\n  W state ribbon.x 0001\n");
    let parsed = parse_dump(&full).expect("the full detail parses");
    assert_eq!(
        parsed[0].runs[0].frames[0],
        parse_dump(&dump(&[frame(&[])])).expect("a dump")[0].runs[0].frames[0]
    );
}

#[test]
fn the_closure_is_the_dump_its_support_the_rig_every_state_words_and_the_profile() {
    let changed = "crates/aterm-gui/examples/trail_dump.rs\0\
                   crates/aterm-gui/src/lib.rs\0\
                   crates/aterm-effects/src/rainbow_kitty/stardust/state_words.rs\0\
                   crates/aterm-gui/src/trail_dump_tests.rs\0\
                   crates/aterm-gui/src/trail_rig.rs\0\
                   crates/aterm-gui/src/trail_frame_reads.rs\0\
                   crates/aterm-gui/src/light_fork_pin_tests.rs\0\
                   crates/aterm-gui/examples/objc_event_drive.rs\0\
                   crates/aterm-effects/src/my_state_words.rs\0";
    let untracked = "crates/aterm-gui/examples/support/trail_script_more.rs\0notes.txt\0";
    assert_eq!(
        closure_changes(changed, untracked),
        [
            "crates/aterm-effects/src/rainbow_kitty/stardust/state_words.rs",
            "crates/aterm-gui/examples/support/trail_script_more.rs",
            "crates/aterm-gui/examples/trail_dump.rs",
            "crates/aterm-gui/src/trail_frame_reads.rs",
            "crates/aterm-gui/src/trail_rig.rs",
        ]
    );
    assert!(
        !in_closure("crates/aterm-gui/src/trail_rig_tests.rs"),
        "the rig's own path, not a prefix of it"
    );
    assert!(closure_changes("crates/aterm-lab/src/trail_diff.rs\0", "").is_empty());
    assert!(closure_changes("", "").is_empty());
    assert!(
        in_closure("state_words.rs") && in_closure("crates/aterm-gui/examples/support/x/y.trail")
    );

    let toml = "[profile.opt-test]\ninherits = \"release\"\n\n# The dump's profile.\n\
                [profile.trail-diff]\ninherits = \"dev\"\nopt-level = 2\n\n[patch.crates-io]\nx = 1\n";
    assert_eq!(
        profile_table(toml),
        Some(vec![
            "inherits = \"dev\"".to_string(),
            "opt-level = 2".to_string()
        ])
    );
    let recommented = toml.replace("opt-level = 2\n", "# faster\nopt-level = 2 # draws\n");
    assert!(closure_differs("", "", toml, &recommented).is_empty());
    let changed_level = toml.replace("opt-level = 2", "opt-level = 3");
    assert_eq!(
        closure_differs("", "", toml, &changed_level),
        ["[profile.trail-diff]"]
    );
    let dropped = toml.replace("[profile.trail-diff]", "[profile.other]");
    assert_eq!(profile_table(&dropped), None);
    assert_eq!(
        closure_differs("", "", &dropped, toml),
        ["[profile.trail-diff]"]
    );

    let refusal = closure_refusal(&closure_differs(changed, "", toml, &changed_level));
    assert_eq!(refusal.code, 3, "a closure that differs is NOT RUN");
    let msg = refusal.msg.unwrap_or_default();
    assert!(
        msg.contains("crates/aterm-gui/examples/trail_dump.rs")
            && msg.contains("[profile.trail-diff]"),
        "{msg}"
    );
}

#[test]
fn the_base_is_the_parent_unless_named() {
    let a = parse_args(&[]).expect("no arguments");
    assert_eq!(a.base, Base::Ref("HEAD~1".into()));
    assert_eq!((a.control, a.keep, a.jobs), (None, false, None));
    assert!(a.scripts.is_empty());
    let a = parse_args(&args(&["--base", "merge-base", "x.trail", "--keep", "dir"])).expect("ok");
    assert_eq!(a.base, Base::MergeBase);
    assert!(a.keep);
    assert_eq!(a.scripts, ["x.trail", "dir"]);
    let a = parse_args(&args(&["--base", "abc123", "--jobs", "3"])).expect("ok");
    assert_eq!((a.base, a.jobs), (Base::Ref("abc123".into()), Some(3)));
    let a = parse_args(&args(&["--control", "plant"])).expect("ok");
    assert_eq!(a.control, Some(Control::Plant));
}

#[test]
fn usage_errors_exit_2() {
    for words in [
        &["--control", "both"][..],
        &["--control", "same", "--base", "HEAD~2"],
        &["--jobs", "0"],
        &["--jobs", "many"],
        &["--base"],
        &["--base", ""],
        &["--frobnicate"],
    ] {
        let f = parse_args(&args(words)).expect_err(&format!("{words:?}"));
        assert_eq!(f.code, 2, "{words:?}: {:?}", f.msg);
    }
}

#[test]
fn the_exit_says_same_differs_or_whether_a_control_held() {
    let same = Report::default();
    let stream = Report {
        differing_compared: 2,
        ..Report::default()
    };
    let state = Report {
        state_differing: 1,
        ..Report::default()
    };
    assert_eq!(verdict(&same, None), (0, None));
    assert_eq!(verdict(&stream, None), (1, None));
    assert_eq!(verdict(&state, None), (1, None));
    let held = |r: &Report, c| verdict(r, Some(c)).0;
    assert_eq!(held(&same, Control::Same), 0);
    assert_eq!(held(&stream, Control::Same), 1);
    assert_eq!(held(&state, Control::Same), 1);
    assert_eq!(
        held(&same, Control::Plant),
        1,
        "a plant nothing reports fails"
    );
    assert_eq!(held(&stream, Control::Plant), 0);
    assert!(
        verdict(&stream, Some(Control::Plant))
            .1
            .is_some_and(|l| l.starts_with("control plant: held")),
    );
    assert_eq!(not_run("x").code, 3);
}

#[test]
fn the_plant_lowers_body_frame_top_by_one() {
    let text = "/// The body's top.\nconst BODY_FRAME_TOP: f32 = 251.0;\nconst X: f32 = 1.0;\n";
    assert_eq!(
        plant(text).as_deref(),
        Some("/// The body's top.\nconst BODY_FRAME_TOP: f32 = 250.0;\nconst X: f32 = 1.0;\n")
    );
    assert_eq!(plant("const X: f32 = 1.0;\n"), None);
    assert_eq!(plant("const BODY_FRAME_TOP: f32 = top();\n"), None);
    let ribbon = include_str!("../../aterm-effects/src/rainbow_kitty/ribbon.rs");
    let planted = plant(ribbon).expect("ribbon.rs declares BODY_FRAME_TOP");
    assert_ne!(planted, ribbon);
    assert_eq!(planted.len(), ribbon.len(), "one value, one digit lower");
}

#[test]
fn frame_numbers_print_as_ranges() {
    let f = |ns: &[&str]| ranges(&ns.iter().map(|n| (*n).to_string()).collect::<Vec<_>>());
    assert_eq!(f(&["3", "4", "5", "9", "11", "12"]), "F3-F5 F9 F11-F12");
    assert_eq!(f(&["0"]), "F0");
    assert_eq!(f(&[]), "");
}
