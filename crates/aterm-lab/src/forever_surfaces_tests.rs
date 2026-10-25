// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's readers over lines a headless instance printed, each beside a
//! line it must not read the same way.

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
text key="harness/session/0/today" role=Group label="today: 1 need, carried on"
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
    assert_eq!(of("Notify Me").section, "look");
    assert_eq!(of("Notify Me").at, "10");
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
    assert_eq!(landings_of(&journal[1..3], |_| true), 1);
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
             today=today: 1 need, carried on"
        )
    );
    assert_eq!(
        page_of(&rows, "land-ab").as_deref(),
        Some(
            "head=land-ab: Window 2, tab 3 · Claude Code · supervised · autopilot land\nstate=landed 03:20"
        )
    );
    assert_eq!(page_of(&rows, "land"), None);
    assert_eq!(page_of(&rows, "Show land-a"), None);
}

/// The text lists only what the page's screen shows, so a head drawn with no
/// state row and nothing of the page after its own rows is the screen's last
/// line, and its state line is unread: the row reads as off the page's screen.
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
/// read, and the one `before` lacks is named.
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
    assert_eq!(
        new_window(&[], Duration::ZERO, || one.to_string()).as_deref(),
        Some("0")
    );
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
    let t = tab_with(&[(now - 9000, act), (now - 8000, took), (now - 5000, waiting)]);
    assert_eq!(
        due_and_quiet(std::slice::from_ref(&t), now),
        (Some(now + 240_000), now - 5000)
    );
    let t = tab_with(&[(now - 9000, act), (now - 8000, took)]);
    assert_eq!(due_and_quiet(std::slice::from_ref(&t), now).0, Some(now));
    let t = tab_with(&[(now - 9000, waiting), (now - 8000, act)]);
    assert_eq!(due_and_quiet(std::slice::from_ref(&t), now).0, None);
    let t = tab_with(&[
        (now - 9000, act),
        (now - 8000, waiting),
        (now - 7000, "DONE seq=3 rule=land@v1 x"),
    ]);
    assert_eq!(due_and_quiet(std::slice::from_ref(&t), now).0, None);
}

/// A journal line the same as the line before it is no move and no news: the
/// idle window's span and its quiet stand.
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
/// and land-k's ends to stand in it.
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
    assert_eq!(unread_why(true, &[Chain::Wall, Chain::CruiseN]), None);
}

/// `chrome`'s menu with a need's submenu and a manager's: a row one level in is its submenu's,
/// a row two levels in and a row after a top-level separator are no one's.
const SUBMENUS: &str = r#"statusmenu "Needs you (1)" enabled=no at=0
statusmenu "◇ worker needs you: approve the notes · 2m" enabled=yes submenu at=1
statusmenu   "● worker — asked 2m" enabled=yes checked=no at=1.0
statusmenu --- at=2
statusmenu "This Mac — 1 landing · 3 shells" enabled=no at=3
statusmenu "mgr — 1 worker: 1 landing" enabled=yes submenu at=4
statusmenu   "Go to mgr" enabled=yes checked=no at=4.0
statusmenu   --- at=4.1
statusmenu   "● worker — landing 3m" enabled=yes checked=no tip="not landed yet: no “Nothing left:” defense yet · 1 nudge" at=4.2
statusmenu     "● worker — deeper 1m" enabled=no at=4.2.0
statusmenu --- at=5
statusmenu   "● stray — working 1s" enabled=yes checked=no at=6.0"#;

/// The worker's row is read one level into its MANAGER's submenu (a need's submenu naming it is
/// not its manager's), ages masked, its tip whole; the flat reader passes all of them over.
#[test]
fn a_workers_row_is_read_under_its_managers_submenu() {
    let sub = sub_rows(SUBMENUS);
    assert_eq!(
        sub.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
        ["● worker — asked 2m", "Go to mgr", "● worker — landing 3m"]
    );
    assert_eq!(
        worker_of(&sub, "worker"),
        "mgr|mgr — 1 worker: 1 landing\nrow|● worker — landing #\ntip|not landed yet: no \u{201c}Nothing left:\u{201d} defense yet \u{b7} 1 nudge"
    );
    assert_eq!(worker_of(&sub, "mgr"), "-");
    assert_eq!(worker_of(&sub, "stray"), "-");
    assert!(
        !menu_rows(SUBMENUS)
            .iter()
            .any(|r| r.label.contains("worker \u{2014}")),
        "the flat reader passes submenu rows over"
    );
    assert!(is_manager("lab-sweep — 3 workers: 1 landing, 2 landed"));
    assert!(!is_manager("◇ worker needs you: approve the notes · 2m"));
    let mut w = Window {
        label: "worker".into(),
        ..Window::default()
    };
    let said = w.note(&Glance::of(5, SUBMENUS, ""), "s-w");
    assert!(said.iter().any(|(what, _)| *what == "worker"), "{said:?}");
    assert_eq!(
        w.worker.last().map(|(_, e)| worker_field(e, "row")),
        Some(Some("● worker — landing #"))
    );
}

/// A debug build's ` title=` is split off the chip line: the chip's reads stay as they were, and
/// the title is a read of its own (`-` where none is drawn, `?` where none is printed).
#[test]
fn the_title_is_split_off_the_chip_line() {
    let line = "window=2 program=claude face=cruise drawn=\"∞\" spoken=\"Autopilot: cruise.\" \
                description=- title=\"autopilot: cruise — keeps going for ever, finding more to \
                do · click: land · right-click: more\"";
    let (chip_line, title) = split_title(line);
    assert_eq!(
        chip_line,
        "window=2 program=claude face=cruise drawn=\"∞\" spoken=\"Autopilot: cruise.\" description=-"
    );
    assert_eq!(
        title.as_deref(),
        Some(
            "autopilot: cruise — keeps going for ever, finding more to do · click: land · right-click: more"
        )
    );
    assert_eq!(
        split_title("face=cruise drawn=- why=not-drawn spoken=- description=- title=-")
            .1
            .as_deref(),
        Some("-")
    );
    assert_eq!(
        split_title("face=cruise drawn=- why=not-drawn spoken=-").1,
        None
    );
    let chrome = format!(
        "autopilot sid=s-c {line}\nautopilot sid=s-d window=2 program=claude face=land drawn=- why=not-drawn spoken=- description=- title=-"
    );
    let g = Glance::of(7, &chrome, "");
    let mut front = Window::default();
    let _ = front.note(&g, "s-c");
    assert_eq!(front.chip, [(7, chip_line)]);
    assert_eq!(front.title.len(), 1);
    assert!(
        front.title[0]
            .1
            .starts_with("autopilot: cruise — keeps going")
    );
    let mut behind = Window::default();
    let _ = behind.note(&g, "s-d");
    assert_eq!(behind.title, [(7, "-".to_string())]);
    let mut old = Window::default();
    let _ = old.note(
        &Glance::of(
            7,
            "autopilot sid=s-e window=2 program=claude face=off drawn=\"○ off\" spoken=\"x\"",
            "",
        ),
        "s-e",
    );
    assert_eq!(old.title, [(7, "?".to_string())]);
}

/// A chip line drawn in front with `title`.
fn drawn_title(program: &str, face: &str, drawn: &str, title: &str) -> String {
    format!(
        "window=2 program={program} face={face} drawn={drawn} spoken=\"x\" description=- title={title}"
    )
}

/// A title as `chrome` prints it.
fn qt(t: &str) -> String {
    format!("{t:?}")
}

fn title_seen(opening: Option<String>, front: Option<String>) -> Seen {
    Seen {
        window: Window {
            read: true,
            label: "x".into(),
            opening: opening.map(|l| (900, l)),
            front: front.map(|l| (9_000, l)),
            ..Window::default()
        },
        ..Seen::default()
    }
}

fn title_cx(chain: Chain, program: &'static str) -> Ctx<'static> {
    Ctx {
        chain,
        program,
        t0: 1_000,
    }
}

/// THE TITLE'S WORDS (5d): every chain's chip title says its mode's sentence from MODE_WORDS
/// before its first prompt; a spawn supplies its worker's task before the opening sweep.
/// Under Land a read hold's words stand in its place, and a landed face says none. Each mode's
/// title is the others' negative control.
#[test]
fn every_chip_title_says_its_modes_sentence_from_mode_words() {
    use aterm_agent::supervise::Autopilot;
    use aterm_agent::supervise::mode_words::mode_words;
    let title = |mode: &str, m: Autopilot| {
        format!(
            "autopilot: {mode} \u{2014} {} \u{b7} click: x \u{b7} right-click: more",
            mode_words(m).chip
        )
    };
    let (cruise, land, off) = (
        title("cruise", Autopilot::Cruise),
        title("land", Autopilot::Land),
        title("off", Autopilot::Off),
    );
    assert_eq!(title_says(&cruise, "cruise"), TitleSays::Sentence);
    assert_eq!(title_says(&land, "land"), TitleSays::Sentence);
    assert_eq!(title_says(&off, "off"), TitleSays::Sentence);
    for (t, mode) in [(&cruise, "land"), (&land, "off"), (&off, "cruise")] {
        assert!(
            matches!(title_says(t, mode), TitleSays::Wrong(_)),
            "{t} as {mode}"
        );
    }
    let old = "autopilot: cruise \u{2014} keeps the work moving \u{b7} click: land";
    assert!(matches!(title_says(old, "cruise"), TitleSays::Wrong(_)));
    let cut = format!(
        "autopilot: land \u{2014} {}",
        &mode_words(Autopilot::Land).chip[..20]
    );
    assert!(matches!(title_says(&cut, "land"), TitleSays::Wrong(_)));
    let held = "autopilot: land \u{2014} not landed yet: no \u{201c}Nothing left:\u{201d} defense yet \u{b7} landing since 14:02 \u{b7} click: off";
    assert_eq!(title_says(held, "land"), TitleSays::Held);
    assert!(matches!(title_says(held, "cruise"), TitleSays::Wrong(_)));
    let unread = format!(
        "autopilot: land \u{2014} {} \u{b7} landing since 14:02",
        mode_words(Autopilot::Land).chip
    );
    assert_eq!(title_says(&unread, "land"), TitleSays::Sentence);
    let landed = format!(
        "autopilot: land, landed 14:05 \u{2014} {} \u{b7} click: off",
        aterm_agent::supervise::mode_words::LANDED_WORDS
    );
    assert_eq!(title_says(&landed, "land"), TitleSays::Past("landed"));
    for wrong in [
        "autopilot: land, landed 14:05 \u{2014} all of the work is on the remote main branch",
        "autopilot: land, landed 14:05",
        &landed[..landed.len() - 20],
    ] {
        assert!(
            matches!(title_says(wrong, "land"), TitleSays::Wrong(_)),
            "a landed title that does not say LANDED_WORDS whole: {wrong}"
        );
    }

    let ok = |s: &Seen, c: Chain, p: &'static str| title_words(s, &title_cx(c, p));
    let n = title_seen(
        Some(drawn_title("claude", "cruise", "\"∞\"", &qt(&cruise))),
        None,
    );
    assert!(
        matches!(ok(&n, Chain::CruiseN, "claude"), Verdict::Pass(_)),
        "{:?}",
        ok(&n, Chain::CruiseN, "claude")
    );
    assert!(
        matches!(ok(&n, Chain::LandB, "claude"), Verdict::Fail(_)),
        "a cruise title on a Land chain"
    );
    assert!(
        matches!(ok(&n, Chain::Worker, "claude"), Verdict::Fail(_)),
        "a spawned worker still needs its own mode's words"
    );
    let b = title_seen(
        Some(drawn_title("claude", "land", "\"⤓ land\"", &qt(&land))),
        Some(drawn_title("claude", "land", "\"⤓ land\"", &qt(held))),
    );
    assert!(
        matches!(ok(&b, Chain::LandB, "claude"), Verdict::Pass(_)),
        "{:?}",
        ok(&b, Chain::LandB, "claude")
    );
    let early = title_seen(
        Some(drawn_title("claude", "land", "\"⤓ land\"", &qt(held))),
        None,
    );
    assert!(
        matches!(ok(&early, Chain::LandB, "claude"), Verdict::Fail(_)),
        "a hold before its first prompt"
    );
    assert!(
        matches!(ok(&early, Chain::Worker, "claude"), Verdict::Pass(_)),
        "the spawn already supplied the worker's task"
    );
    let blank = title_seen(Some(drawn_title("claude", "off", "\"○ off\"", "-")), None);
    assert!(
        matches!(ok(&blank, Chain::Off, "claude"), Verdict::Fail(_)),
        "a drawn chip with no title"
    );
    let o = title_seen(
        Some(drawn_title("claude", "off", "\"○ off\"", &qt(&off))),
        None,
    );
    assert!(matches!(ok(&o, Chain::Off, "claude"), Verdict::Pass(_)));
    let not_yet =
        "window=2 program=claude face=land drawn=- why=not-drawn spoken=- description=- title=-";
    let v = ok(
        &title_seen(Some(not_yet.into()), None),
        Chain::Worker,
        "claude",
    );
    assert!(
        matches!(v, Verdict::Unobserved(_)),
        "a chip not drawn yet has no title to judge: {v:?}"
    );
    let later = title_seen(
        Some(not_yet.into()),
        Some(drawn_title("claude", "land", "\"⤓ land\"", &qt(&land))),
    );
    let v = ok(&later, Chain::Worker, "claude");
    assert!(
        matches!(v, Verdict::Pass(ref w) if w.contains("not drawn yet")),
        "{v:?}"
    );
    let drawn_blank = title_seen(Some(drawn_title("claude", "land", "\"⤓ land\"", "-")), None);
    assert!(
        matches!(ok(&drawn_blank, Chain::Worker, "claude"), Verdict::Fail(_)),
        "a drawn chip with no title still fails"
    );
    let g = title_seen(
        Some(
            "window=2 program=gemini face=- drawn=- why=no-footer spoken=- description=- title=-"
                .into(),
        ),
        None,
    );
    let v = ok(&g, Chain::LandB, "gemini");
    assert!(
        matches!(v, Verdict::Pass(ref w) if w.contains("no footer")),
        "{v:?}"
    );
    let agy = title_seen(
        Some(
            "window=2 program=agy face=master-off drawn=- why=switch-only spoken=- description=- \
             title=-"
                .into(),
        ),
        None,
    );
    assert!(
        matches!(ok(&agy, Chain::Off, "agy"), Verdict::Pass(_)),
        "{:?}",
        ok(&agy, Chain::Off, "agy")
    );
    let release = title_seen(
        Some("window=2 program=claude face=off drawn=\"○ off\" spoken=\"x\" description=-".into()),
        None,
    );
    assert!(matches!(
        ok(&release, Chain::Off, "claude"),
        Verdict::Unobserved(_)
    ));
    for c in [Chain::Off, Chain::Worker] {
        assert!(matches!(
            ok(&title_seen(None, None), c, "claude"),
            Verdict::Unobserved(_)
        ));
    }
    let mut drawn = title_seen(None, None);
    drawn.window.chip = vec![(
        2_000,
        "window=2 program=claude face=cruise drawn=\"∞\" spoken=\"x\" description=-".into(),
    )];
    drawn.window.title = vec![(2_000, cruise.clone())];
    assert!(matches!(
        ok(&drawn, Chain::Cruise, "claude"),
        Verdict::Pass(_)
    ));
    drawn.window.title.push((3_000, off.clone()));
    assert!(
        matches!(ok(&drawn, Chain::Cruise, "claude"), Verdict::Fail(_)),
        "a later read in front says Off's"
    );
    let w = WANTS
        .iter()
        .find(|w| w.name.starts_with("the chip's title"))
        .unwrap();
    for c in TITLED {
        let s = c.scenarios()[0];
        assert!(w.of(c, s), "{c:?}");
    }
    assert!(!w.of(Chain::Wall, Scenario::W1) && !w.of(Chain::CruiseN, Scenario::N2));
}

/// L7's Landed row (4c, WHAT THE HARNESS DID): today the run's landings, the week at least that.
#[test]
fn the_landed_row_counts_every_landing() {
    let rest = |did: &str| {
        format!(
            "here=\"This Mac — 5 landed\" landed_tabs=5 landing_tabs=7 landings=5 today=\"5 landed today\" every=\"x\" did=\"{did}\""
        )
    };
    let judge = |r: Option<String>| {
        let s = Seen {
            window: Window {
                read: true,
                rest: r,
                ..Window::default()
            },
            ..Seen::default()
        };
        l7_did(&s, &title_cx(Chain::LandE, "claude"))
    };
    assert!(matches!(
        judge(Some(rest("Landed: 5 today \u{b7} 5 in 7 days"))),
        Verdict::Pass(_)
    ));
    assert!(matches!(
        judge(Some(rest("Landed: 5 today \u{b7} 9 in 7 days"))),
        Verdict::Pass(_)
    ));
    for bad in [
        "Landed: 4 today \u{b7} 5 in 7 days",
        "Landed: 5 today \u{b7} 4 in 7 days",
        "Land prompts: 5 today \u{b7} 5 in 7 days",
        "Landed: five today",
        "None in 7 days: declined \u{b7} landed \u{b7} upgraded",
        "None in 7 days: declined \u{b7} land prompts",
    ] {
        assert!(matches!(judge(Some(rest(bad))), Verdict::Fail(_)), "{bad}");
    }
    assert!(matches!(judge(Some(rest("-"))), Verdict::Unobserved(_)));
    assert!(matches!(judge(None), Verdict::Unobserved(_)));
    let rows = page_rows(
        "text key=\"harness/did/row/land-prompts\" role=Group label=\"Land prompts: 9 today · 9 in 7 days\"\ntext key=\"harness/did/row/landed\" role=Group label=\"Landed: 5 today · 5 in 7 days\"",
    );
    assert_eq!(
        did_landed_of(&rows).as_deref(),
        Some("Landed: 5 today · 5 in 7 days")
    );
    let none = "None in 7 days: declined \u{b7} landed \u{b7} upgraded";
    let rows = page_rows(&format!(
        "text key=\"harness/did/row/land-prompts\" role=Group label=\"Land prompts: 9 today · 9 in 7 days\"\ntext key=\"harness/did/row/none-in-7-days\" role=Group label=\"{none}\""
    ));
    assert_eq!(did_landed_of(&rows).as_deref(), Some(none));
    let s = |did: &str| Seen {
        window: Window {
            read: true,
            rest: Some(format!(
                "here=\"x\" landed_tabs=0 landing_tabs=1 landings=0 today=\"x\" every=\"x\" did=\"{did}\""
            )),
            ..Window::default()
        },
        ..Seen::default()
    };
    assert!(matches!(
        l7_did(&s(none), &title_cx(Chain::LandE, "claude")),
        Verdict::Pass(_)
    ));
    // A None row that does not name `landed`, with no Landed row over it, is a page that dropped
    // the count (6a: a week with counts always says landed): read in the Landed row's place, so
    // the wheel stops there and L7 fails it, never a skip.
    let dropped = "None in 7 days: land prompts \u{b7} upgraded";
    let rows = page_rows(&format!(
        "text key=\"harness/did/row/none-in-7-days\" role=Group label=\"{dropped}\""
    ));
    assert_eq!(
        did_landed_of(&rows).as_deref(),
        Some(dropped),
        "land prompts is not landed"
    );
    assert_eq!(did_step(&rows), DidStep::Read(dropped.into()));
    assert!(matches!(
        l7_did(&s(dropped), &title_cx(Chain::LandE, "claude")),
        Verdict::Fail(_)
    ));
    let rows = page_rows(&format!(
        "text key=\"harness/did/row/landed\" role=Group label=\"Landed: 5 today · 5 in 7 days\"\ntext key=\"harness/did/row/none-in-7-days\" role=Group label=\"{dropped}\""
    ));
    assert_eq!(
        did_landed_of(&rows).as_deref(),
        Some("Landed: 5 today · 5 in 7 days"),
        "the Landed row first"
    );
}

/// L7 widened (§14.6): This Mac's `landing` is every Land tab that holds at rest, less those
/// whose need it counts first. CONTROL: `b42ae72fc3`'s menu, which calls the held tabs `resting`.
/// It never passes on `-` (§14.0 limit 7): no held tab, a hold no `status` names, or a wait This
/// Mac counts first is UNOBSERVED, main's row included.
#[test]
fn this_mac_counts_every_held_landing() {
    let rest = |here: &str, held: usize, needs: usize, unread: usize| {
        format!(
            "here=\"{here}\" landed_tabs=5 landing_tabs=7 held_landing={held} held_needs={needs} held_unread={unread} landings=5 today=\"5 landed today\" every=\"x\" did=\"-\""
        )
    };
    let judge = |r: Option<String>| {
        let s = Seen {
            window: Window {
                read: true,
                rest: r,
                ..Window::default()
            },
            ..Seen::default()
        };
        l7_here(&s, &title_cx(Chain::LandE, "claude"))
    };
    const NOW: &str = "This Mac \u{2014} 1 need you \u{b7} 6 landing \u{b7} 1 resting \u{b7} 5 landed \u{b7} 3 shells";
    const MAIN: &str =
        "This Mac \u{2014} 1 need you \u{b7} 7 resting \u{b7} 5 landed \u{b7} 3 shells";
    let pass = judge(Some(rest(NOW, 7, 1, 0)));
    assert!(matches!(pass, Verdict::Pass(_)), "{pass:?}");
    let control = judge(Some(rest(MAIN, 7, 1, 0)));
    assert!(
        matches!(&control, Verdict::Fail(why) if why.contains("0 landing, not 6")),
        "{control:?}"
    );
    for (here, held, needs) in [
        (NOW, 7, 2),
        (NOW, 7, 0),
        (
            "This Mac \u{2014} 7 landing \u{b7} 1 resting \u{b7} 5 landed \u{b7} 3 shells",
            7,
            1,
        ),
        ("This Mac \u{2014} 2 landing \u{b7} 5 landed", 1, 0),
    ] {
        assert!(
            matches!(judge(Some(rest(here, held, needs, 0))), Verdict::Fail(_)),
            "{here} against {held} held, {needs} with a need"
        );
    }
    for (why, r) in [
        ("main's row, no tab held", Some(rest(MAIN, 0, 0, 0))),
        (
            "no tab held",
            Some(rest("This Mac \u{2014} 5 landed", 0, 0, 0)),
        ),
        ("a hold no status named", Some(rest(MAIN, 0, 0, 8))),
        ("one hold unread", Some(rest(NOW, 7, 1, 1))),
        (
            "a wait counted first",
            Some(rest(
                "This Mac \u{2014} 1 need you \u{b7} 1 waiting \u{b7} 6 landing \u{b7} 5 landed",
                8,
                1,
                0,
            )),
        ),
        ("no rest read", None),
        (
            "no held_needs",
            Some(rest(NOW, 7, 1, 0).replace(" held_needs=1", "")),
        ),
        (
            "no held_unread",
            Some(rest(NOW, 7, 1, 0).replace(" held_unread=0", "")),
        ),
        ("no This Mac row", Some(rest("", 7, 1, 0))),
    ] {
        let got = judge(r);
        assert!(matches!(got, Verdict::Unobserved(_)), "{why}: {got:?}");
    }
    assert_eq!(
        held_at_rest("autopilot=land task=open land_held=repo land_nudges=2 land_since=1792159320"),
        Some(true)
    );
    assert_eq!(
        held_at_rest("autopilot=land task=open land_held=- land_nudges=0 land_since=1792159320"),
        Some(true)
    );
    assert_eq!(
        held_at_rest("autopilot=land task=open land_held=- land_nudges=- land_since=-"),
        Some(false)
    );
    assert_eq!(held_at_rest("autopilot=land task=open"), None);
}

/// END's wheel down the page reads the Landed row where a screen shows it, turns again while the
/// row is further down, and stops once HARNESS UPDATES shows with no Landed row, since a step
/// never moves past a row unseen.
#[test]
fn the_wheel_stops_at_the_landed_row_or_past_it() {
    let row = |key: &str, label: &str| (key.to_string(), label.to_string());
    let landed = row("harness/did/row/landed", "Landed: 5 today · 5 in 7 days");
    let prompts = row(
        "harness/did/row/land-prompts",
        "Land prompts: 9 today · 9 in 7 days",
    );
    let updates = row("harness/updates/heading", "HARNESS UPDATES");
    let sessions = row("harness/session/0", "land-a: land");
    assert_eq!(
        did_step(&[prompts.clone(), landed.clone(), updates.clone()]),
        DidStep::Read("Landed: 5 today · 5 in 7 days".into())
    );
    assert_eq!(did_step(&[prompts.clone(), updates]), DidStep::Past);
    assert_eq!(did_step(&[sessions, prompts]), DidStep::Wheel);
    assert_eq!(did_step(&[]), DidStep::Wheel);
}

/// `inspect app/v1 tabs` with Settings in the page's window (1) and, in front of the chains'
/// window (2), the Settings tab END opened; the chains' window's Settings view only while it is
/// that window's front tab.
#[test]
fn the_opened_page_is_the_chains_windows_front_settings_tab() {
    let tabs = |active: &str| {
        format!(
            "app-inspection version=app/v1 revision=1 subject=tabs
window id=1 focused=false tabs=2 active=3
  tab id=2 index=0 kind=terminal title=\"proj\" state=clean active=false
    leaf path=/ view=2 rect_px=0.0,0.0,1008.0,700.0 focused=true kind=terminal session=1
  tab id=3 index=1 kind=app title=\"Settings\" state=clean active=true
    leaf path=/ view=3 rect_px=0.0,0.0,1008.0,700.0 focused=true kind=app app=settings title=\"Settings\"
window id=2 focused=true tabs=2 active=9
  tab id=4 index=0 kind=terminal title=\"land-a\" state=busy active={}
    leaf path=/ view=4 rect_px=0.0,0.0,1008.0,700.0 focused=true kind=terminal session=2
  tab id=9 index=1 kind=app title=\"Settings\" state=clean active={}
    leaf path=/ view=7 rect_px=0.0,0.0,1008.0,700.0 focused=true kind=app app=settings title=\"Settings\"
",
            active == "terminal",
            active == "settings"
        )
    };
    assert_eq!(front_page_of(&tabs("settings"), "2").as_deref(), Some("7"));
    assert_eq!(front_page_of(&tabs("terminal"), "2"), None);
    assert_eq!(front_page_of(&tabs("settings"), "1").as_deref(), Some("3"));
    assert_eq!(front_page_of(&tabs("settings"), "5"), None);
}

/// A spawned worker is read in front only once its chip names its program and the strip lists its
/// title: a strip a headless window has not drawn since the title was set lacks it.
#[test]
fn a_worker_is_read_in_front_once_its_strip_lists_its_title() {
    assert!(worker_shown(CHROME, "s-aa", "land-a"));
    assert!(
        !worker_shown(CHROME, "s-aa", "worker"),
        "the strip lacks the title"
    );
    assert!(
        !worker_shown(CHROME, "s-zz", "land-a"),
        "no chip for that tab"
    );
    let unnamed = CHROME.replace(
        "autopilot sid=s-aa window=2 program=claude face=land drawn=\"⤓ land\" spoken=\"Autopilot: land.\"",
        "autopilot sid=s-aa window=2 program=claude face=- drawn=- why=not-drawn spoken=-",
    );
    assert_ne!(unnamed, CHROME);
    assert!(
        !worker_shown(&unnamed, "s-aa", "land-a"),
        "its chip names no program yet"
    );
    let bare = unnamed.replace("why=not-drawn", "why=no-footer");
    assert!(
        worker_shown(&bare, "s-aa", "land-a"),
        "a program with no footer draws no chip"
    );
}

/// The strip marks a spawned worker `↳ worker` and its manager `mgr ⇉1`; the lab reads each by
/// the title it gave the tab, and leaves a title that only looks like a mark as it is.
#[test]
fn the_strip_is_read_by_title_without_its_team_marks() {
    let chrome = "toolbar-tabs count=3 selected=0 labels=[\"off\", \"mgr \u{21c9}1\", \"\u{21b3} worker\"] icons=[None, None, None] states=[[\"selected\"], [\"busy\"], [\"busy\"]] tooltips=[None, None, None]\nautopilot sid=s-ww window=2 program=claude face=land drawn=- why=not-drawn spoken=-";
    let labels: Vec<String> = strip_tabs(chrome).into_iter().map(|t| t.label).collect();
    assert_eq!(labels, ["off", "mgr", "worker"]);
    assert!(worker_shown(chrome, "s-ww", "worker"));
    for kept in [
        "mgr \u{21c9}",
        "mgr \u{21c9}1a",
        "\u{21b3}worker",
        "a\u{21b3} b",
    ] {
        assert_eq!(team_unmarked(kept), kept);
    }
    assert_eq!(team_unmarked("\u{21b3} mgr \u{21c9}12"), "mgr");
}

/// A worker's opening read waits only on a chip its footer would draw and the last frame did not:
/// a drawn chip, and a program with no footer, are read at once.
#[test]
fn only_a_chip_the_frame_left_undrawn_is_waited_on() {
    assert!(undrawn(
        "autopilot sid=s-ww window=2 program=claude face=land drawn=- why=not-drawn spoken=- description=- title=-"
    ));
    for read in [
        "autopilot sid=s-ww window=2 program=claude face=land drawn=\"⤓ land\" spoken=\"Autopilot: land.\" description=- title=\"autopilot: land\"",
        "autopilot sid=s-ww window=2 program=gemini face=- drawn=- why=no-footer spoken=- description=- title=-",
        "autopilot sid=s-ww window=2 program=agy face=off drawn=- why=switch-only spoken=- description=- title=-",
    ] {
        assert!(!undrawn(read), "{read}");
    }
}

/// The wheel turns over the middle of the page's list, read off `settings/list`'s rect.
#[test]
fn the_wheel_turns_over_the_lists_middle() {
    let controls = "ui key=\"settings/list/scroll\" role=Group label=\"\" value=- action=- state=- rect=0.0,0.0,10.0,10.0
ui key=\"settings/list\" role=List label=\"Harness\" value=- action=- state=- rect=234.0,106.0,798.0,582.0";
    assert_eq!(list_middle(controls), Some((633, 397)));
    assert_eq!(
        list_middle("ui key=\"settings/list/scroll\" role=Group rect=0.0,0.0,10.0,10.0"),
        None
    );
    assert_eq!(
        list_middle("ui key=\"settings/list\" role=List rect=1.0,2.0"),
        None
    );
}

/// END's holds are the Land hold itself (`land_since=`), read off each tab's last `status`,
/// never a pause: a Cruise chain paused in a back-off holds nothing.
#[test]
fn ends_holds_are_read_off_the_hold() {
    let tab = |chain: Chain, status: &str| Tab {
        chain,
        seen: Seen {
            status: vec![(1, status.to_string())],
            journal: vec![(
                1,
                "WAITING seq=2 until=2099-10-21T10:24:00Z 2 short turn(s) in a row: 4 min back-off"
                    .to_string(),
            )],
            ..Seen::default()
        },
        ..tab_with(&[])
    };
    let tabs = [
        tab(
            Chain::LandB,
            "task=open land_held=open land_nudges=1 land_since=1792000060",
        ),
        tab(
            Chain::Cruise,
            "task=open land_held=- land_nudges=- land_since=-",
        ),
        tab(
            Chain::LandA,
            "task=landed land_held=- land_nudges=- land_since=-",
        ),
        tab(
            Chain::LandC,
            "task=open land_held=- land_nudges=0 land_since=1792000070",
        ),
        tab(Chain::LandD, "task=open needs=ask"),
    ];
    assert_eq!(
        holds_of(&tabs),
        [
            "land-b(open, since 1792000060)",
            "land-c(-, since 1792000070)"
        ]
    );
}

/// Every chain whose chip title is judged has a window want, so a run of it alone (`--chains
/// land-t`, a red rerun alone) reads the window: 5d gave the cover chains wants of their own
/// (P5's and L12's defense hold, L15's worker row, the title), and a run that read none of them
/// would pass them as skips. The wall's chain alone still has none.
#[test]
fn a_chain_with_a_window_want_reads_its_window_when_run_alone() {
    for c in TITLED {
        assert_eq!(unread_why(true, &[c]), None, "{} alone", c.name());
    }
    assert_eq!(unread_why(true, &[Chain::Mgr, Chain::Worker]), None);
    assert_eq!(
        unread_why(true, &[Chain::Wall]),
        Some("no chain of this run has a window want")
    );
    assert_eq!(unread_why(false, &[Chain::LandT]), Some("--no-surfaces"));
}

/// The front tab's chip drawn mid-run with no title fails (`title=-` on a drawn chip is a
/// title lost), as the opening's and END's reads do; a read whose chip is not drawn names none.
#[test]
fn a_drawn_chip_that_loses_its_title_mid_run_fails() {
    use aterm_agent::supervise::Autopilot;
    use aterm_agent::supervise::mode_words::mode_words;
    let cruise = format!(
        "autopilot: cruise \u{2014} {} \u{b7} click: land \u{b7} right-click: more",
        mode_words(Autopilot::Cruise).chip
    );
    let opening = Some(drawn_title("claude", "cruise", "\"∞\"", &qt(&cruise)));
    let drawn = "window=2 program=claude face=cruise drawn=\"∞\" spoken=\"x\" description=-";
    let undrawn =
        "window=2 program=claude face=cruise drawn=- why=not-drawn spoken=- description=-";
    let judge = |chip: Vec<(i64, String)>, title: Vec<(i64, String)>| {
        let mut s = title_seen(opening.clone(), None);
        s.window.chip = chip;
        s.window.title = title;
        title_words(&s, &title_cx(Chain::CruiseN, "claude"))
    };
    let lost = judge(vec![(2_000, drawn.into())], vec![(2_000, "-".into())]);
    assert!(matches!(lost, Verdict::Fail(_)), "drawn, title=-: {lost:?}");
    let became = judge(
        vec![(2_000, undrawn.into()), (3_000, drawn.into())],
        vec![(2_000, "-".into())],
    );
    assert!(
        matches!(became, Verdict::Fail(_)),
        "drawn at +2 s with the title still -: {became:?}"
    );
    let kept = judge(vec![(2_000, drawn.into())], vec![(2_000, cruise.clone())]);
    assert!(matches!(kept, Verdict::Pass(_)), "drawn, titled: {kept:?}");
    let hidden = judge(vec![(2_000, undrawn.into())], vec![(2_000, "-".into())]);
    assert!(
        matches!(hidden, Verdict::Pass(_)),
        "not drawn, no title: {hidden:?}"
    );
}

/// A hold's words or a landed or quiet title stand only over the face that says them: a
/// title says `land, landed` only over `face=landed`, `land, quiet` only over `face=theirs`,
/// a hold only over `face=land`. A title out of step with the chip it hovers fails.
#[test]
fn a_past_or_held_title_is_judged_against_its_own_chips_face() {
    use aterm_agent::supervise::Autopilot;
    use aterm_agent::supervise::mode_words::mode_words;
    let land = format!(
        "autopilot: land \u{2014} {} \u{b7} click: off \u{b7} right-click: more",
        mode_words(Autopilot::Land).chip
    );
    let landed: &str = &format!(
        "autopilot: land, landed 06:12 \u{2014} {}",
        aterm_agent::supervise::mode_words::LANDED_WORDS
    );
    let quiet = "autopilot: land, quiet \u{2014} a person's own exchange";
    let held = "autopilot: land \u{2014} not landed yet: no \u{201c}Nothing left:\u{201d} defense yet \u{b7} landing since 14:02";
    let at_end = |face: &str, drawn: &str, title: &str| {
        let s = title_seen(
            Some(drawn_title("claude", "land", "\"⤓ land\"", &qt(&land))),
            Some(drawn_title("claude", face, drawn, &qt(title))),
        );
        title_words(&s, &title_cx(Chain::LandB, "claude"))
    };
    let at_opening = |chain: Chain, face: &str, drawn: &str, title: &str| {
        let s = title_seen(Some(drawn_title("claude", face, drawn, &qt(title))), None);
        title_words(&s, &title_cx(chain, "claude"))
    };
    for (face, drawn, title) in [
        ("landed", "\"✓ landed\"", landed),
        ("theirs", "\"⤓ quiet\"", quiet),
        ("land", "\"⤓ land\"", held),
    ] {
        let v = at_end(face, drawn, title);
        assert!(matches!(v, Verdict::Pass(_)), "{face}: {v:?}");
        let v = at_opening(Chain::Worker, face, drawn, title);
        assert!(matches!(v, Verdict::Pass(_)), "spawned {face}: {v:?}");
        let v = at_opening(Chain::LandB, face, drawn, title);
        assert!(matches!(v, Verdict::Fail(_)), "before prompt {face}: {v:?}");
    }
    for (face, drawn, title) in [
        ("land", "\"⤓ land\"", landed),
        ("land", "\"⤓ land\"", quiet),
        ("theirs", "\"⤓ quiet\"", landed),
        ("landed", "\"✓ landed\"", quiet),
        ("landed", "\"✓ landed\"", held),
    ] {
        let v = at_end(face, drawn, title);
        assert!(
            matches!(v, Verdict::Fail(_)),
            "{title:?} over face={face}: {v:?}"
        );
        let v = at_opening(Chain::Worker, face, drawn, title);
        assert!(
            matches!(v, Verdict::Fail(_)),
            "spawned {title:?} over face={face}: {v:?}"
        );
    }
}
