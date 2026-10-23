// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's readers over lines a headless instance printed (
//! exploration lab), each beside a line it must not read the
//! same way.

use super::*;

/// `chrome` as a headless instance printed it with one supervised tab
/// needing its person: the strip, the chip, the menu, the presence line.
const CHROME: &str = r#"toolbar (none)
toolbar-tabs count=2 selected=1 labels=["aterm", "land-a"] icons=[None, None] states=[[], ["selected", "busy", "attention"]] tooltips=[None, Some("land-a\nactivity: idle\nNeeds you: rotate the deploy key\ndriven by aterm (land)")]
menu (none)
autopilot sid=s-aa window=2 program=claude face=land drawn="⤓ land" spoken="Autopilot: land."
autopilot sid=s-bb window=2 program=claude face=cruise drawn=- why=not-drawn spoken=-
statusitem title=❯1 needs=1 stuck=0 exceptions=4 done=0 fleet=off
statusmenu "Needs you (1)" enabled=no at=0
statusmenu "◇ land-a — rotate the deploy key · 6s" enabled=yes checked=no at=1
statusmenu --- at=2
statusmenu "This Mac — 1 need you · 2 shells" enabled=no at=3
statusmenu --- at=4
statusmenu "Done for you (1)" enabled=no at=5
statusmenu "✓ land-ab — landed 2m" enabled=yes checked=no at=6
statusmenu --- at=7
statusmenu "To look at (1)" enabled=no at=8
statusmenu "Claude Code is not installed" enabled=yes checked=no at=9
statusmenu "Notify Me When I’m Needed" enabled=yes checked=no tip="One notice when something needs you, never for a wait." at=10
presence rim=decide pressing=@s-aa pill="""#;

/// `inspect app/v1 view <id> text` of Settings ▸ Harness, two sessions.
const PAGE: &str = r#"text key="harness/status/row/sessions" role=Group label="Sessions: 2 of 2 supervised · 1 need you"
text key="harness/session/0" role=Group label="land-a: Window 2, tab 2 · Claude Code · supervised · autopilot land"
text key="harness/session/0/show" role=Button label="Show land-a"
text key="harness/session/0/state" role=Status label="needs you: rotate the deploy key; carrying on · pausing until 03:24 (a short turn)"
text key="harness/session/0/today" role=Group label="today: 1 need told"
text key="harness/session/11" role=Group label="land-ab: Window 2, tab 3 · Claude Code · supervised · autopilot land"
text key="harness/session/11/state" role=Status label="landed 03:20""#;

#[test]
fn the_strip_is_read_tab_by_tab_through_its_quoted_lists() {
    let tabs = strip_tabs(CHROME);
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[1].label, "land-a");
    assert!(tabs[1].selected && !tabs[0].selected);
    assert_eq!(tabs[1].states, ["selected", "busy", "attention"]);
    assert!(tabs[0].states.is_empty());
    assert_eq!(
        tabs[1].words(),
        "index=1 selected=1 states=selected,busy,attention"
    );
    assert_eq!(tabs[0].words(), "index=0 selected=0 states=-");
    // a quote and a bracket inside a label are the label's
    let odd = r#"toolbar-tabs count=1 selected=0 labels=["a \"b\" ]c"] icons=[None] states=[["done"]] tooltips=[None]"#;
    let t = strip_tabs(odd);
    assert_eq!(t[0].label, "a \"b\" ]c");
    assert_eq!(t[0].states, ["done"]);
    assert!(strip_tabs("toolbar (none)").is_empty());
}

#[test]
fn the_menu_is_read_row_by_row_under_its_sections() {
    let rows = menu_rows(CHROME);
    let of = |label: &str| rows.iter().find(|r| r.label.starts_with(label)).unwrap();
    assert_eq!(of("◇ land-a").section, "needs");
    assert_eq!(of("◇ land-a").at, "1");
    assert_eq!(of("This Mac").section, "here");
    assert_eq!(of("✓ land-ab").section, "done");
    assert_eq!(of("Claude Code is not").section, "look");
    // past a separator with no header, no section; a tip's words are no `at=`
    assert_eq!(of("Notify Me").section, "look");
    assert_eq!(of("Notify Me").at, "10");
    // a row names its tab by the label whole, before ` — `
    assert!(row_names("◇ land-a — rotate the deploy key · 6s", "land-a"));
    assert!(!row_names("✓ land-ab — landed 2m", "land-a"));
    assert!(!row_names("◇ land-a — x", "land"));
    assert!(row_names("● fmt (worker of mgr) — working 2m", "fmt"));
}

/// A tab's menu entry carries the header of each section its rows are in,
/// with the rows the menu lists under it (every tab's), and never This
/// Mac's (whose counts move with every tab).
#[test]
fn a_tabs_menu_carries_its_sections_headers_and_their_rows() {
    let rows = menu_rows(CHROME);
    assert_eq!(
        menu_of(&rows, "land-a"),
        "needs-head|Needs you (1) rows=1\nneeds|◇ land-a — rotate the deploy key · #"
    );
    assert_eq!(
        menu_of(&rows, "land-ab"),
        "done-head|Done for you (1) rows=1\ndone|✓ land-ab — landed #"
    );
    assert_eq!(menu_of(&rows, "land"), "");
    // two needs under one header: each tab's entry counts both rows; an
    // `and N more` row is no need's
    let two = CHROME.replace(
        "statusmenu --- at=2",
        "statusmenu \"◇ cruise-n — grant it · 2s\" enabled=yes checked=no at=11\n\
         statusmenu \"and 3 more — Overview…\" enabled=yes checked=no at=12\nstatusmenu --- at=2",
    );
    let rows = menu_rows(&two);
    assert_eq!(
        menu_of(&rows, "cruise-n"),
        "needs-head|Needs you (1) rows=2\nneeds|◇ cruise-n — grant it · #"
    );
    // the rows' own reader never takes a header for a row
    assert_eq!(
        rows_in(&menu_of(&rows, "land-a"), "needs"),
        ["◇ land-a — rotate the deploy key · #"]
    );
    assert_eq!(
        head_of(&menu_of(&rows, "land-a"), "needs"),
        Some((Some(1), 2))
    );
    assert_eq!(head_of("needs|◇ x — y", "needs"), None);
}

/// The page's counts are read by part: `<n> <word>` between `,`, `:` and
/// `·`, a zero part being one never said.
#[test]
fn a_page_count_is_read_by_its_part() {
    let every = "Every session, 16 sessions: 4 cruising (2 timed), 6 landing, 5 landed, 1 off";
    assert_eq!(count_in(every, "landed"), 5);
    assert_eq!(count_in(every, "landing"), 6);
    assert_eq!(count_in(every, "cruising"), 4);
    assert_eq!(count_in(every, "quiet"), 0);
    assert_eq!(count_in(every, "sessions"), 16);
    let today = "5 landed today · 3 kept going · 7 land prompts · 2 choices made for you";
    assert_eq!(count_in(today, "landed"), 5);
    assert_eq!(count_in(today, "land"), 7);
    assert_eq!(count_in("nothing yet today", "landed"), 0);
    let rows = page_rows(
        "text key=\"harness/status/today\" role=Group label=\"5 landed today · 3 kept going\"\n\
         text key=\"harness/session/all\" role=Group label=\"Every session, 2 sessions: 1 landing, 1 landed\"\n\
         text key=\"harness/session/all/timer\" role=Button label=\"Timer for every session, 1 session\"",
    );
    assert_eq!(
        today_of(&rows).as_deref(),
        Some("5 landed today · 3 kept going")
    );
    assert_eq!(
        every_of(&rows).as_deref(),
        Some("Every session, 2 sessions: 1 landing, 1 landed")
    );
    assert_eq!(today_of(&page_rows(PAGE)), None);
    assert_eq!(every_of(&page_rows(PAGE)), None);
}

/// A tab's landings as the A/B book counts them: a `DONE … rule=land@v1`
/// note where it is the tab's first or a land act came since the last;
/// a note again with no act before it counts nothing, nor another rule's
/// DONE, nor one of another day.
#[test]
fn a_landing_is_counted_once_a_land_act() {
    let j = |t: i64, l: &str| (t, l.to_string());
    let journal = [
        j(
            1,
            "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
        ),
        j(
            2,
            "DONE seq=2 rule=land@v1 the worker defended that nothing is left",
        ),
        j(
            3,
            "DONE seq=2 rule=land@v1 the landing holds through the task's own wake",
        ),
        j(
            4,
            "CONTINUED seq=3 rule=land-nudge@v1 [aterm harness] Still landing:",
        ),
        j(
            5,
            "DONE seq=4 rule=land@v1 the worker defended that nothing is left",
        ),
        j(
            6,
            "CONTINUED seq=5 rule=continue@v1 [aterm harness] Autopilot is Cruise",
        ),
        j(
            7,
            "DONE seq=6 rule=land@v1 a note with no land act before it",
        ),
        j(8, "DONE seq=7 rule=continue@v1 not a landing"),
    ];
    assert_eq!(landings_of(&journal, |_| true), 2);
    // a session's first note counts (a cut may have dropped its act)
    assert_eq!(landings_of(&journal[1..3], |_| true), 1);
    // only the day asked for counts
    assert_eq!(landings_of(&journal, |t| t >= 4), 1);
    assert_eq!(landings_of(&[], |_| true), 0);
}

#[test]
fn an_age_is_masked_and_nothing_else_is() {
    assert_eq!(
        masked("◇ land-a — rotate the deploy key · 6s"),
        "◇ land-a — rotate the deploy key · #"
    );
    assert_eq!(
        masked("✓ land-e — landed 12m · 2.1 KB"),
        "✓ land-e — landed # · 2.1 KB"
    );
    for keep in ["landed", "ds", "0.48.0", "3", "s", "12ms"] {
        assert_eq!(masked(keep), keep, "{keep}");
    }
}

#[test]
fn the_page_is_read_by_key_and_a_session_by_its_label() {
    let rows = page_rows(PAGE);
    assert_eq!(rows.len(), 7);
    assert_eq!(
        page_of(&rows, "land-a").as_deref(),
        Some(
            "head=land-a: Window 2, tab 2 · Claude Code · supervised · autopilot land\n\
             state=needs you: rotate the deploy key; carrying on · pausing until 03:24 (a short turn)\n\
             today=today: 1 need told"
        )
    );
    // another label's rows are never this one's, nor a button's
    assert_eq!(
        page_of(&rows, "land-ab").as_deref(),
        Some(
            "head=land-ab: Window 2, tab 3 · Claude Code · supervised · autopilot land\nstate=landed 03:20"
        )
    );
    assert_eq!(page_of(&rows, "land"), None);
    assert_eq!(page_of(&rows, "Show land-a"), None);
}

/// A ROW CUT OFF BY THE PAGE'S BOTTOM EDGE (r5's Gemini CLI L2): the text
/// lists only what the page's screen shows, so a head drawn with no state
/// row and nothing of the page after its own rows is the screen's last line,
/// and its state line is unread: the row reads as off the page's screen.
/// NEGATIVE CONTROLS: a head with no state row and another session's row
/// below it is a row the page drew with no state line; the last row with its
/// state drawn is read whole.
#[test]
fn a_row_cut_off_by_the_pages_bottom_edge_is_off_its_screen() {
    let clipped = page_rows(
        "text key=\"harness/session/0\" role=Group label=\"land-a: Window 2, tab 2 · Claude Code · supervised · autopilot land\"\n\
         text key=\"harness/session/0/state\" role=Status label=\"landed 03:20\"\n\
         text key=\"harness/session/4\" role=Group label=\"land-b: Window 2, tab 3 · Claude Code · supervised · autopilot land\"\n\
         text key=\"harness/session/4/show\" role=Button label=\"Show land-b\"",
    );
    assert_eq!(
        page_of(&clipped, "land-b"),
        None,
        "its state line is below the edge"
    );
    assert_eq!(
        page_of(&clipped, "land-a").as_deref(),
        Some(
            "head=land-a: Window 2, tab 2 · Claude Code · supervised · autopilot land\nstate=landed 03:20"
        ),
        "the row above it is whole"
    );
    let stateless = page_rows(
        "text key=\"harness/session/4\" role=Group label=\"land-b: Window 2, tab 3 · Claude Code · supervised · autopilot land\"\n\
         text key=\"harness/session/4/show\" role=Button label=\"Show land-b\"\n\
         text key=\"harness/session/5\" role=Group label=\"land-c: Window 2, tab 4 · Claude Code · supervised · autopilot land\"",
    );
    assert_eq!(
        page_of(&stateless, "land-b").as_deref(),
        Some("head=land-b: Window 2, tab 3 · Claude Code · supervised · autopilot land"),
        "a row below it: the page drew no state line"
    );
    assert_eq!(
        page_of(&stateless, "land-c"),
        None,
        "the last head alone is cut off"
    );
}

#[test]
fn a_chip_line_is_read_and_its_cells_held_to_its_face() {
    let a = chip_of(CHROME, "s-aa").unwrap();
    assert_eq!(
        a,
        "window=2 program=claude face=land drawn=\"⤓ land\" spoken=\"Autopilot: land.\""
    );
    let c = chip(&a);
    assert_eq!(
        (c.face.as_str(), c.drawn.as_deref(), c.why.as_deref()),
        ("land", Some("⤓ land"), None)
    );
    assert!(drawn_is_face(&c));
    let b = chip(&chip_of(CHROME, "s-bb").unwrap());
    assert_eq!(
        (b.drawn.as_deref(), b.why.as_deref()),
        (None, Some("not-drawn"))
    );
    assert!(!drawn_is_face(&b), "no cells are no face");
    assert_eq!(chip_of(CHROME, "s-a"), None, "a sid is read whole");
    // a stale frame: the face moved, the cells did not
    assert!(!drawn_is_face(&chip("face=landed drawn=\"⤓ land\"")));
    assert!(drawn_is_face(&chip("face=cruise-until drawn=\"∞ ⤓18:00\"")));
    assert!(drawn_is_face(&chip("face=unhosted drawn=\"⊘ off\"")));
    assert!(!drawn_is_face(&chip("face=cruise drawn=\"∞ cruise\"")));
    let g = chip("window=2 program=gemini face=- drawn=- why=no-footer spoken=-");
    assert!(no_footer(&g) && footless("gemini") && !footless("claude"));
    assert!(!no_footer(&chip("face=land drawn=- why=no-footer")));
}

#[test]
fn quoted_values_read_their_escapes_back() {
    assert_eq!(
        quoted(r#"a pill="x \"y\" \\ z" b=1"#, "pill").as_deref(),
        Some(r#"x "y" \ z"#)
    );
    assert_eq!(
        quoted(r#"label="a\nb\u{2014}c""#, "label").as_deref(),
        Some("a\nb\u{2014}c")
    );
    // a key is a whole word: `drawn` is not `undrawn`'s
    assert_eq!(
        quoted(r#"undrawn="x" drawn="y""#, "drawn").as_deref(),
        Some("y")
    );
    assert_eq!(quoted("drawn=-", "drawn"), None);
}

#[test]
fn a_read_stands_from_before_its_span_and_settles_at_its_end() {
    let list = vec![
        (1, "a".to_string()),
        (5, "b".to_string()),
        (9, "c".to_string()),
    ];
    fn words(v: Vec<&(i64, String)>) -> Vec<&str> {
        v.into_iter().map(|(_, s)| s.as_str()).collect()
    }
    assert_eq!(words(standing(&list, 3, 9)), ["a", "b"]);
    assert_eq!(words(standing(&list, 5, 100)), ["b", "c"]);
    assert_eq!(words(standing(&list, 0, 2)), ["a"]);
    assert_eq!(words(standing(&list, 0, 1)), Vec::<&str>::new());
    assert_eq!(settled(&list, 9).map(|(_, s)| s.as_str()), Some("b"));
    assert_eq!(settled(&list, 1), None);
}

#[test]
fn the_windows_and_their_sessions_are_named_by_id() {
    let tabs = "app-inspection version=app/v1 revision=1 subject=tabs
window id=0 focused=false tabs=1 active=1
  tab id=1 index=0 kind=terminal title=\"alpha\" state=busy active=true
    leaf path=/ view=1 rect_px=0.0,0.0,1008.0,700.0 focused=true kind=terminal session=0
window id=1 focused=true tabs=2 active=3
  tab id=2 index=0 kind=terminal title=\"/p\" state=clean active=false
    leaf path=/ view=2 rect_px=0.0,0.0,1008.0,700.0 focused=true kind=terminal session=1
  tab id=3 index=1 kind=app title=\"Settings\" state=clean active=true
    leaf path=/ view=3 rect_px=0.0,0.0,1008.0,700.0 focused=true kind=app app=settings title=\"Settings\"";
    let (windows, page) = windows_of(tabs);
    assert_eq!(windows, ["0", "1"]);
    assert_eq!(page, Some(("1".to_string(), "3".to_string())));
    assert_eq!(windows_of("window id=0 focused=true").1, None);
    let ls = "0 0 s-aa - alive alpha meta=1 window=0 active=1\n0 1 s-bb - alive /p meta=0 window=2 active=1";
    assert_eq!(window_of(ls, "s-bb").as_deref(), Some("2"));
    assert_eq!(window_of(ls, "s-b"), None);
}

/// `invoke NewWindow` answers before its window is made (the window's first
/// shell spawns off the event loop): the window is waited for, read after
/// read, and the one `before` lacks is named. NEGATIVE CONTROLS: windows
/// that stood before are never the new one, and a window that never comes
/// is none once the wait is over.
#[test]
fn a_new_window_is_waited_for_until_it_is_made() {
    let one = "app-inspection version=app/v1 revision=1 subject=tabs
window id=0 focused=true tabs=1 active=1
  tab id=1 index=0 kind=app title=\"Settings\" state=clean active=true
    leaf path=/ view=1 rect_px=0.0,0.0,1008.0,700.0 focused=true kind=app app=settings title=\"Settings\"";
    let two = format!(
        "{one}\nwindow id=1 focused=true tabs=1 active=2\n  tab id=2 index=0 kind=terminal title=\"/p\" state=clean active=true"
    );
    let before = vec!["0".to_string()];
    // the window comes on the third read, as on a loaded machine
    let mut reads = 0;
    let made = new_window(&before, STEP_WAIT, || {
        reads += 1;
        if reads < 3 {
            one.to_string()
        } else {
            two.clone()
        }
    });
    assert_eq!(made.as_deref(), Some("1"));
    assert_eq!(reads, 3, "read until it showed, and no more");
    // from no window at all, the first one made is new
    assert_eq!(
        new_window(&[], Duration::ZERO, || one.to_string()).as_deref(),
        Some("0")
    );
    // NEGATIVE CONTROLS: the windows that stood before, and one that never comes
    assert_eq!(
        new_window(&["0".to_string(), "1".to_string()], Duration::ZERO, || two
            .clone()),
        None
    );
    assert_eq!(
        new_window(&before, Duration::from_millis(200), || one.to_string()),
        None
    );
}

#[test]
fn a_stamp_is_read_to_its_unix_ms() {
    assert_eq!(unix_ms_of("1970-01-01T00:00:01Z"), Some(1000));
    assert_eq!(unix_ms_of("2026-10-21T10:24:06Z"), Some(1_792_578_246_000));
    assert_eq!(unix_ms_of("x"), None);
}

#[test]
fn the_metrics_name_their_owners_and_wakes() {
    assert_eq!(
        owners("presence:4/0,session_status:2/0"),
        ["presence", "session_status"]
    );
    assert_eq!(owners("title_drift:2"), ["title_drift"]);
    assert!(owners("none").is_empty() && owners("-").is_empty() && owners("").is_empty());
    assert_eq!(wakes_of("Output:25,HostNote:6", "Output"), 25);
    assert_eq!(wakes_of("HostNote:6", "Output"), 0);
    assert_eq!(
        wakes_of("OutputLate:3", "Output"),
        0,
        "a variant is read whole"
    );
}

/// A tab whose journal's last word after its last act is `line`, at `t`.
fn tab_with(journal: &[(i64, &str)]) -> Tab {
    Tab {
        chain: Chain::Cruise,
        sid: "s-x".into(),
        dir: std::path::PathBuf::new(),
        seen: Seen {
            journal: journal
                .iter()
                .map(|(t, l)| (*t, (*l).to_string()))
                .collect(),
            ..Seen::default()
        },
        t0: 0,
        ask_tried: false,
        over: false,
    }
}

#[test]
fn the_next_act_is_the_last_wait_after_the_last_act() {
    let now = unix_ms_of("2026-10-21T10:20:00Z").unwrap();
    let waiting =
        "WAITING seq=2 until=2026-10-21T10:24:00Z 2 short turn(s) in a row: 4 min back-off";
    let took = "WAITING seq=2 until=2026-10-21T10:19:50Z the worker to take the act";
    let act = "CONTINUED seq=2 rule=continue@v1 x";
    // a back-off standing: due at its until
    let t = tab_with(&[(now - 9000, act), (now - 8000, took), (now - 5000, waiting)]);
    assert_eq!(
        due_and_quiet(std::slice::from_ref(&t), now),
        (Some(now + 240_000), now - 5000)
    );
    // the take window's wait before it is not the next act
    let t = tab_with(&[(now - 9000, act), (now - 8000, took)]);
    assert_eq!(due_and_quiet(std::slice::from_ref(&t), now).0, Some(now));
    // an act after the wait spends it; a landing ends the ladder
    let t = tab_with(&[(now - 9000, waiting), (now - 8000, act)]);
    assert_eq!(due_and_quiet(std::slice::from_ref(&t), now).0, None);
    let t = tab_with(&[
        (now - 9000, act),
        (now - 8000, waiting),
        (now - 7000, "DONE seq=3 rule=land@v1 x"),
    ]);
    assert_eq!(due_and_quiet(std::slice::from_ref(&t), now).0, None);
}

/// A journal line the same as the line before it (the loop writing one
/// decision again: measured on Gemini CLI, a `WAITING` every 20 s) is no
/// move and no news: the idle window's span and its quiet stand. NEGATIVE
/// CONTROLS: the same line after another, a new seq, an act.
#[test]
fn a_line_written_again_is_no_move() {
    let now = unix_ms_of("2026-10-21T10:20:00Z").unwrap();
    let waiting =
        "WAITING seq=2 until=2026-10-21T10:36:00Z 4 short turn(s) in a row: 16 min back-off";
    let act = "CONTINUED seq=2 rule=continue-done-check@v1 x";
    let j = |lines: &[(i64, &str)]| {
        lines
            .iter()
            .map(|(t, l)| (*t, (*l).to_string()))
            .collect::<Vec<_>>()
    };
    let again = j(&[
        (now - 60_000, act),
        (now - 40_000, waiting),
        (now - 20_000, waiting),
        (now, waiting),
    ]);
    let (moved, relogged) = moves_in("cruise", &again, now - 30_000, 30_000);
    assert!(moved.is_empty(), "{moved:?}");
    assert_eq!(relogged.len(), 2, "{relogged:?}");
    let t = tab_with(&[
        (now - 60_000, act),
        (now - 40_000, waiting),
        (now - 20_000, waiting),
        (now, waiting),
    ]);
    assert_eq!(due_and_quiet(std::slice::from_ref(&t), now).1, now - 40_000);
    // a new seq, an act, or the same line after another line: a move
    let next = waiting.replace("seq=2", "seq=3");
    let other = "EVENT idle seq=2 x";
    for (lines, n) in [
        (j(&[(now - 40_000, waiting), (now - 20_000, &next)]), 1),
        (j(&[(now - 40_000, waiting), (now - 20_000, act)]), 1),
        (
            j(&[
                (now - 40_000, waiting),
                (now - 25_000, other),
                (now - 20_000, waiting),
            ]),
            2,
        ),
    ] {
        let (moved, relogged) = moves_in("cruise", &lines, now - 30_000, 30_000);
        assert_eq!((moved.len(), relogged.len()), (n, 0), "{lines:?}");
    }
}

/// The window is read where a chain of the run has a window want (the cover
/// and cap modes' for their Off control), and END runs only with land-j's
/// and land-k's ends to stand in it. NEGATIVE CONTROLS: `--no-surfaces`; the
/// wall modes' chains; a run holding only one ending chain, or none.
#[test]
fn the_window_is_read_for_the_main_pass_and_end_needs_both_ends() {
    use super::super::Mode;
    let both = Chain::of_mode(Mode::Both);
    assert_eq!(unread_why(true, &both), None);
    assert!(end_runs(&both));
    assert_eq!(unread_why(false, &both), Some("--no-surfaces"));
    for mode in [Mode::Cover, Mode::Cap] {
        let chains = Chain::of_mode(mode);
        assert_eq!(unread_why(true, &chains), None, "{mode:?}");
        assert!(!end_runs(&chains), "{mode:?}");
    }
    for mode in [Mode::Wall, Mode::WallHid] {
        let chains = Chain::of_mode(mode);
        assert_eq!(
            unread_why(true, &chains),
            Some("no chain of this run has a window want"),
            "{mode:?}"
        );
        assert!(!end_runs(&chains), "{mode:?}");
    }
    let cruise = Chain::of_mode(Mode::Cruise);
    assert_eq!(unread_why(true, &cruise), None);
    assert!(!end_runs(&cruise));
    assert!(end_runs(&Chain::of_mode(Mode::Land)));
    assert!(end_runs(&[Chain::LandK, Chain::LandJ]));
    assert!(!end_runs(&[Chain::LandJ, Chain::CruiseN]));
    // one chain of the main pass beside a wall reads the window
    assert_eq!(unread_why(true, &[Chain::Wall, Chain::CruiseN]), None);
}
