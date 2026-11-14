// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const LAND_NOT_DRAWN: &str =
    "window=0 program=codex face=land drawn=- why=no-footer spoken=- description=- title=-";
const LAND_DRAWN: &str = "window=0 program=codex face=land drawn=\"\u{2913} land\" spoken=\"Autopilot: land.\" description=- title=-";
const OFF_SWITCH: &str =
    "window=0 program=codex face=off drawn=- why=no-footer spoken=- description=- title=-";
/// 14249's line under Land (`window-branch-14249-final.log`).
const OWN: &str = "codex-alab 0.1.0-dev.14249 \u{b7} based on OpenAI Codex 0.166.0 \
                   \u{2500}\u{2500} \u{2913} land \u{2500}[Cruise]\u{2500}[Land]\u{2500}[Off]\
                   \u{2500}\u{2500}\u{2500}[ALab on]";
/// 13916's line under Land, landed alike (`window-13916-try1.log`): the mode's button lit,
/// no word.
const OWN_13916: &str = "codex-alab 0.1.0-dev.13916 \u{b7} based on OpenAI Codex 0.165.0 \
                         \u{2500}\u{2500}\u{2500}[Cruise]\u{2500}[Land]\u{2500}[Off]\u{2500}\
                         \u{2500}\u{2500}[ALab on]";

#[test]
fn the_pad_is_the_row_over_the_last_prompt() {
    let text = format!("> hello\n\n  {OWN}  \n» Ask Codex\n\n  ? for shortcuts\n");
    assert_eq!(pad_of(&text).as_deref(), Some(OWN));
    assert_eq!(
        pad_of("> hello\n\n» Ask Codex\n").as_deref(),
        Some(""),
        "a blank pad"
    );
    assert_eq!(pad_of("no prompt here\n"), None, "CONTROL: no prompt row");
    assert_eq!(pad_of(&text.replace('»', "›")).as_deref(), Some(OWN));
}

#[test]
fn a_build_says_what_it_tells() {
    assert_eq!(
        dev_build("0.1.0-dev.14249+codex.0.166.0.g76c628303"),
        Some(14_249)
    );
    assert_eq!(dev_build("0.1.0-dev.13916"), Some(13_916));
    assert_eq!(dev_build("0.164.0"), None, "CONTROL: no dev build");
    let b = Build::of("0.1.0-dev.14249+codex.0.166.0.g76c628303".into());
    assert!(b.tells_hold && b.says_mode);
    let b = Build::of("0.1.0-dev.14157".into());
    assert!(b.tells_hold && !b.says_mode, "the hold before the word");
    let b = Build::of("0.1.0-dev.13916+codex.0.165.0.gabc".into());
    assert!(!b.tells_hold && !b.says_mode, "the owner's installed build");
    assert!(
        !Build::of("?".into()).tells_hold,
        "CONTROL: no version read"
    );
}

/// THE CHIP NEVER COVERS THE PROGRAM'S OWN LINE: a chip drawn while the pad holds
/// codex-alab's own text fails whatever its face. A chip on a blank pad fails too.
#[test]
fn a_chip_is_judged_against_the_programs_own_pad() {
    assert!(judge_line(LAND_NOT_DRAWN, Some(OWN), "land", true).is_ok());
    assert!(
        judge_line(
            &LAND_NOT_DRAWN.replace("face=land", "face=-"),
            Some(OWN),
            "land",
            true
        )
        .is_ok(),
        "a native mode needs no host-rendered face"
    );
    assert!(judge_line(LAND_DRAWN, Some(""), "land", true).is_err());
    let covered = judge_line(LAND_DRAWN, Some(OWN), "land", true).expect_err("covered");
    assert!(covered.contains("aterm drew Codex controls"), "{covered}");
    assert!(
        judge_line(LAND_NOT_DRAWN, Some(""), "land", true).is_err(),
        "CONTROL: no chip and nothing of the program's on a blank pad"
    );
    assert!(
        judge_line(LAND_DRAWN, Some(""), "landed", true).is_err(),
        "CONTROL: the wrong face"
    );
    assert!(
        judge_line(
            &LAND_DRAWN.replace("\u{2913} land", "\u{221e}"),
            Some(""),
            "land",
            true
        )
        .is_err(),
        "CONTROL: drawn cells that are not its face"
    );
    assert!(judge_line(OFF_SWITCH, Some(""), "off", true).is_err());
    let off = OWN.replace("\u{2913} land", "\u{25cb} off");
    assert!(judge_line(OFF_SWITCH, Some(&off), "off", true).is_ok());
    assert!(
        judge_line(
            "window=0 program=codex face=off drawn=\"\u{25cb} off\" spoken=- description=-",
            Some(""),
            "off",
            true
        )
        .is_err(),
        "CONTROL: a chip where none belongs"
    );
    assert!(judge_line("", Some(OWN), "land", true).is_err(), "no line");
}

/// A BUILD THAT NAMES ITS MODE MUST NAME THE FACE: beside codex-alab's own line, a build from
/// [`SAYS_ITS_MODE`] on fails where its line lacks the face's words; 13916's line, which
/// lights a button and names no mode, passes only for a build before it.
#[test]
fn its_own_line_names_the_face_from_the_build_that_says_it() {
    let landed = LAND_NOT_DRAWN.replace("face=land", "face=landed");
    let said = OWN.replace("\u{2913} land", "\u{2713} landed");
    assert!(judge_line(&landed, Some(&said), "landed", true).is_ok());
    let stale = judge_line(&landed, Some(OWN), "landed", true).expect_err("stale");
    assert!(stale.contains("names no landed"), "{stale}");
    assert!(
        judge_line(&landed, Some(OWN_13916), "landed", true).is_err(),
        "CONTROL: 13916's line under a build that should say it"
    );
    let old = judge_line(&landed, Some(OWN_13916), "landed", false).expect("old");
    assert!(old.contains("no host footer"), "{old}");
}

fn read(t: i64, words: &str) -> (i64, Read) {
    (
        t,
        Read {
            line: String::new(),
            words: words.into(),
            pad: None,
            state: "-".into(),
            hover: "-".into(),
        },
    )
}

#[test]
fn a_workers_status_is_judged_by_the_hold_it_was_told() {
    let working = "task=open land_since=-";
    let holding = "task=open land_since=1793451264";
    let landed = "task=landed land_since=-";
    let went = [read(0, working), read(2, holding), read(6, landed)];
    assert_eq!(
        worker_went(&went, true),
        Ok("goes from its hold told to landed")
    );
    let skipped = [read(0, working), read(6, landed)];
    assert_eq!(
        worker_went(&skipped, false),
        Err(None),
        "no hold told: a named skip"
    );
    assert!(
        matches!(worker_went(&skipped, true), Err(Some(_))),
        "CONTROL: a hold told and never read"
    );
    assert!(
        matches!(
            worker_went(&[read(0, landed), read(2, holding)], true),
            Err(Some(_))
        ),
        "CONTROL: landed before its hold"
    );
    assert!(
        matches!(worker_went(&[read(0, working)], false), Err(Some(_))),
        "CONTROL: never landed"
    );
}

/// A LANDING RESTS ON A READ PAST IT, NOT ON A CLOCK: the read that first shows the landing is
/// not rest, since its `end=` may be from just before; any later read is.
#[test]
fn a_landing_rests_on_a_read_past_it() {
    let mut s = Seen {
        reads: vec![
            read(0, "task=open end=-"),
            read(10_000, "task=landed end=-"),
        ],
        last_t: 10_000,
    };
    assert!(!landing_at_rest(&s), "the landing's first read alone");
    s.last_t = 12_000;
    assert!(landing_at_rest(&s), "a later read");
    assert_eq!(landed_run(&s).len(), 1);
    s.reads.truncate(1);
    assert!(!landing_at_rest(&s), "CONTROL: never landed");
}

/// THE OPEN PAGE'S NEXT TURN MAY ARM, NEVER FIRE: with Settings ▸ Harness open and every leg
/// at rest, the page's next turn of its words ([`PAGE_TURN`], hours away) is the loop's only
/// deadline and arms at each wake (this tree's END, page open: `autopilot_timer:14/0`); END
/// passes it there, and only there. CONTROL: the page shut, the same arms fail; a fire of it
/// fails either way.
#[test]
fn the_open_pages_next_turn_may_arm_in_ends_idle_but_never_fire() {
    const OPEN: &str = "timer_wakes=0 event_wakes=17 deadline_owner=autopilot_timer \
                        deadline_arms_by_owner=autopilot_timer:14/0 deadline_fires_by_owner=none \
                        wakes_by_variant=ReadSessionStatuses:11,ReadSessionStatus:4,HostNote:1";
    const FIRED: &str = "timer_wakes=1 event_wakes=17 deadline_owner=autopilot_timer \
                         deadline_arms_by_owner=autopilot_timer:14/0 \
                         deadline_fires_by_owner=autopilot_timer:1 \
                         wakes_by_variant=ReadSessionStatuses:11,ReadSessionStatus:4";
    let p = "n_render=0";
    assert_eq!(
        surfaces::idle_counts_beside(OPEN, p, &[PAGE_TURN]),
        Vec::<String>::new()
    );
    assert!(
        !surfaces::idle_counts_beside(OPEN, p, &[]).is_empty(),
        "CONTROL: the page shut"
    );
    assert!(
        !surfaces::idle_counts_beside(FIRED, p, &[PAGE_TURN]).is_empty(),
        "CONTROL: a fire in the window"
    );
}

#[test]
fn the_state_line_and_the_pages_tab_are_found() {
    let rows = vec![
        (
            "harness/session/7".to_string(),
            "land: Window 2, tab 1".to_string(),
        ),
        (
            "harness/session/7/state".to_string(),
            "codex-alab: landing since 09:12".to_string(),
        ),
        (
            "harness/session/all".to_string(),
            "Every session".to_string(),
        ),
    ];
    assert_eq!(state_of(&rows, "land"), "codex-alab: landing since 09:12");
    assert_eq!(state_of(&rows, "reland"), "-", "CONTROL: no row for it");
    let tabs = "window id=1\n  tab index=0 active=false\n    leaf kind=term sid=s-a\n  \
                tab index=1 active=true\n    leaf kind=app app=settings view=9\nwindow id=2\n  \
                tab index=0 active=true\n    leaf kind=term sid=s-b\n";
    assert_eq!(tab_of_view(tabs, "1", "9").as_deref(), Some("1"));
    assert_eq!(tab_of_view(tabs, "2", "9"), None, "CONTROL: another window");
}
