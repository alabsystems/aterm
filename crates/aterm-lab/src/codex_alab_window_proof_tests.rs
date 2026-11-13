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

/// This Mac's first count sits right after its lead, so the row is read past ` — ` under
/// either lead; a row that is not This Mac's counts nothing.
#[test]
fn this_mac_s_first_count_is_read_past_its_lead() {
    assert_eq!(
        landing_on_this_mac("This Mac \u{2014} 1 landing \u{b7} 1 resting \u{b7} 3 shells"),
        1
    );
    assert_eq!(
        landing_on_this_mac("m0 Heron \u{2014} 2 landing \u{b7} 2 shells"),
        2
    );
    assert_eq!(
        landing_on_this_mac("m8 Heron \u{2014} 1 resting \u{b7} 2 landing"),
        2
    );
    assert_eq!(landing_on_this_mac("m8 Heron \u{2014} 3 shells"), 0);
    assert_eq!(landing_on_this_mac("-"), 0, "CONTROL: never read");
    assert_eq!(
        surfaces::count_in("This Mac \u{2014} 1 landing", "landing"),
        0,
        "CONTROL: the whole row hides its first count"
    );
}

#[test]
fn the_pad_is_the_row_over_the_last_prompt() {
    let text = format!("> hello\n\n  {OWN}  \n\u{203a} Ask Codex\n\n  ? for shortcuts\n");
    assert_eq!(pad_of(&text).as_deref(), Some(OWN));
    assert_eq!(
        pad_of("> hello\n\n\u{203a} Ask Codex\n").as_deref(),
        Some(""),
        "a blank pad"
    );
    assert_eq!(pad_of("no prompt here\n"), None, "CONTROL: no prompt row");
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

#[test]
fn a_workers_rows_are_judged_by_the_hold_it_was_told() {
    let row =
        |w: &str| format!("mgr|manager \u{2014} 1 worker: 1 {w}\nrow|\u{25cf} worker \u{2014} {w}");
    let at = |t: i64, w: &str| (t, row(w));
    let went = [at(0, "working"), at(2, "landing"), at(6, "landed")];
    assert_eq!(worker_rows(&went, true), Ok("goes from landing to landed"));
    let skipped = [at(0, "working"), at(6, "landed")];
    assert_eq!(
        worker_rows(&skipped, false),
        Err(None),
        "no hold told: a named skip"
    );
    assert!(
        matches!(worker_rows(&skipped, true), Err(Some(_))),
        "CONTROL: a hold told and no landing row"
    );
    assert!(
        matches!(
            worker_rows(&[at(0, "landed"), at(2, "landing")], true),
            Err(Some(_))
        ),
        "CONTROL: landed before landing"
    );
    assert!(
        matches!(worker_rows(&[at(0, "working")], false), Err(Some(_))),
        "CONTROL: never landed"
    );
}

#[test]
fn a_workers_row_says_landing_then_landed() {
    let landing = "mgr|manager \u{2014} 1 worker: landing\nrow|\u{25cf} worker \u{2014} landing";
    let landed = "mgr|manager \u{2014} 1 worker: landed\nrow|\u{2713} worker \u{2014} landed #";
    assert!(under_says(landing, "landing") && !under_says(landing, "landed"));
    assert!(under_says(landed, "landed") && !under_says(landed, "landing"));
    assert!(!under_says("-", "landed"), "CONTROL: no row");
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
            here: "-".into(),
        },
    )
}

/// A LANDING RESTS ON WHAT IT TOLD, NOT ON A CLOCK: its Done for you row is waited for up to
/// [`ROW_HANG`] after `end=` told the end; a landing a later read still shows with `end=-` is
/// at rest at once (its missing end is judged); a read of the landing alone is not; its row
/// read rests it. CONTROL: before [`ROW_HANG`] with the end told and no row, it is not at rest.
#[test]
fn a_landing_rests_on_its_end_and_its_row() {
    let landed = |end: &str| format!("task=landed end={end}");
    let mut s = Seen {
        reads: vec![read(0, "task=open end=-"), read(10_000, &landed("-"))],
        last_t: 10_000,
        ..Seen::default()
    };
    assert!(
        !landing_at_rest(&s, "land", 10_000),
        "the landing's first read alone"
    );
    s.last_t = 12_000;
    assert!(
        landing_at_rest(&s, "land", 12_000),
        "a later read still says end=-"
    );
    s.reads[1] = read(10_000, &landed("1793451264"));
    assert!(
        !landing_at_rest(&s, "land", 10_000 + ms(ROW_HANG) - 1),
        "CONTROL: the end told, its row not yet due"
    );
    assert!(
        landing_at_rest(&s, "land", 10_000 + ms(ROW_HANG)),
        "the row's hang detector"
    );
    s.rows
        .push((11_000, "done|\u{2713} land \u{2014} landed 1m".to_string()));
    assert!(landing_at_rest(&s, "land", 11_000), "its row read");
    assert!(
        !landing_at_rest(&s, "reland", 11_000),
        "CONTROL: another tab's row"
    );
    assert_eq!(landed_run(&s).len(), 1);
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
