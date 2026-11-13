// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `ax-texts` prints one line the lab reads back: the grant, and each string whole, quotes and
/// line breaks included; with no grant, no string.
#[test]
fn the_trees_reading_is_one_line_of_its_grant_and_its_strings() {
    let texts = vec![
        "Row 2, password: hidden value".to_owned(),
        "a \"b\"\nc".to_owned(),
    ];
    let line = tree_report(true, &texts).unwrap();
    assert!(!line.contains('\n'), "{line}");
    let read = aterm_json::from_str::<aterm_json::Value>(&line).unwrap();
    assert_eq!(
        read.get("trusted").and_then(aterm_json::Value::as_bool),
        Some(true)
    );
    let back: Vec<&str> = read
        .get("texts")
        .and_then(aterm_json::Value::as_array)
        .unwrap()
        .iter()
        .filter_map(aterm_json::Value::as_str)
        .collect();
    assert_eq!(back, texts);
    let none = tree_report(false, &[]).unwrap();
    assert_eq!(none, "{\"texts\":[],\"trusted\":false}");
    let mut printed = Vec::new();
    assert!(ax_texts("no-pid", &mut printed).is_err());
    assert!(printed.is_empty());
}

#[test]
fn the_reader_writes_each_dialogs_words_once_per_change_and_reads_no_settings_page() {
    let alert = "AXWindow; AXStaticText \"Background Items Added\"";
    assert_eq!(
        reader_line(None, alert).as_deref(),
        Some(alert),
        "a new alert"
    );
    assert_eq!(
        reader_line(Some(alert), alert),
        None,
        "the same words again"
    );
    let next = "AXWindow; AXStaticText \"aterm would like to\"";
    assert_eq!(
        reader_line(Some(alert), next).as_deref(),
        Some(next),
        "changed"
    );
    assert_eq!(reader_line(None, ""), None, "nothing read");
    assert!(read_by_reader("UserNotificationCenter"));
    assert!(read_by_reader("CoreServicesUIAgent"));
    assert!(!read_by_reader("System Settings"), "a page, not a question");
    assert!(!read_by_reader("aterm"), "the app's own windows");
}

#[test]
fn the_person_waits_for_the_idle_clock_to_run_its_quiet_before_an_input() {
    let ns = |d: Duration| Some(u64::try_from(d.as_nanos()).expect("in range"));
    assert_eq!(
        quiet_left(ns(Duration::from_millis(350))),
        Some(QUIET_BEFORE - Duration::from_millis(350)),
        "just after a reset: the rest of the quiet"
    );
    assert_eq!(quiet_left(Some(0)), Some(QUIET_BEFORE));
    assert_eq!(quiet_left(ns(QUIET_BEFORE)), None, "quiet enough");
    assert_eq!(quiet_left(ns(Duration::from_secs(120))), None);
    assert_eq!(
        quiet_left(None),
        None,
        "an unreadable clock is not waited on"
    );
}

/// A control under `parent`: a text field's text is its value.
fn node(role: &str, subrole: &str, text: &str, parent: usize) -> ax::Node {
    let field = role == "AXTextField";
    ax::Node {
        role: role.to_owned(),
        subrole: subrole.to_owned(),
        text: text.to_owned(),
        value: None,
        value_text: (field && subrole != "AXSecureTextField").then(|| text.to_owned()),
        enabled: true,
        secure: subrole == "AXSecureTextField",
        parent,
        frame: (0.0, 0.0, 0.0, 0.0),
    }
}

/// A System Settings window with a search field, the authorization sheet a standard account's
/// Full Disk Access switch raised in a Golden Gate guest (`lab mac nux --standard-user
/// --person`, 2026-10-28: its controls as the person described them, with `words` for its
/// second line and its name field showing `name`, `None` for no name field) and a text field
/// of the pane behind the sheet.
fn sheet(words: &str, name: Option<&str>) -> (Vec<ax::Node>, usize) {
    let mut nodes = vec![
        node(
            "AXWindow",
            "AXStandardWindow",
            "System Settings",
            usize::MAX,
        ),
        node("AXTextField", "AXSearchField", "", 0),
        node("AXSheet", "", "", 0),
        node("AXGroup", "AXHostingView", "", 2),
        node("AXImage", "", "Lock", 3),
        node(
            "AXStaticText",
            "",
            "Privacy & Security is trying to modify your system settings.",
            3,
        ),
        node("AXStaticText", "", words, 3),
    ];
    if let Some(name) = name {
        nodes.push(node("AXTextField", "", name, 3));
    }
    let secure = nodes.len();
    nodes.push(node("AXTextField", "AXSecureTextField", "•••", 3));
    nodes.push(node("AXButton", "", "Modify Settings", 3));
    nodes.push(node("AXButton", "", "Cancel", 3));
    nodes.push(node("AXButton", "", "General", 0));
    nodes.push(node("AXTextField", "", "Full Disk Access", 0));
    (nodes, secure)
}

/// A System Settings window with `buttons` (each a title and whether it is enabled) under the
/// words of the sheet macOS shows once an access switch for a running app changed, in an
/// `AXSheet` (`in_sheet`) or straight in a group of the window, and the pane's aterm switch
/// behind it. The words are macOS's form of them, not a recording: no row had read the sheet
/// before this fixture was written.
fn reopen_fixture(buttons: &[(&str, bool)], in_sheet: bool) -> Vec<ax::Node> {
    let mut nodes = vec![
        node(
            "AXWindow",
            "AXStandardWindow",
            "System Settings",
            usize::MAX,
        ),
        node("AXTextField", "AXSearchField", "", 0),
        node(if in_sheet { "AXSheet" } else { "AXGroup" }, "", "", 0),
        node(
            "AXStaticText",
            "",
            "“aterm” won’t have full disk access until it is quit.",
            2,
        ),
        node(
            "AXStaticText",
            "",
            "You can choose to quit “aterm” now, or do it on your own later.",
            2,
        ),
    ];
    for (title, enabled) in buttons {
        let mut button = node("AXButton", "", title, 2);
        button.enabled = *enabled;
        nodes.push(button);
    }
    nodes.push(node("AXCheckBox", "AXSwitch", "aterm", 0));
    nodes
}

#[test]
fn the_person_finds_the_quit_and_reopen_sheet_by_its_role_and_its_buttons() {
    let who = standard();
    let nodes = reopen_fixture(&[("Later", true), ("Quit & Reopen", true)], true);
    let found = reopen_sheet(&nodes).expect("the sheet");
    assert_eq!(
        found,
        ReopenSheet {
            dialog: 2..7,
            reopen: 6,
            later: 5
        }
    );
    assert_eq!(
        describe(&nodes, found.dialog.clone(), &who),
        "AXSheet; AXStaticText \"“aterm” won’t have full disk access until it is quit.\"; \
         AXStaticText \"You can choose to quit “aterm” now, or do it on your own later.\"; \
         AXButton \"Later\"; AXButton \"Quit & Reopen\""
    );
    assert_eq!(found.button(Sheet::Reopen), 6, "the default answer");
    assert_eq!(found.button(Sheet::Later), 5, "--sheet later");
    // The host reads the press by these words (`mac_nux_guest::SHEET_PRESSES`).
    assert_eq!(one_line(Sheet::Reopen.title()), "Quit_&_Reopen");
    assert_eq!(one_line(Sheet::Later.title()), "Later");
    assert!(
        reopen_sheet(&reopen_fixture(
            &[("later", true), ("Quit  &  Reopen", true)],
            true
        ))
        .is_some(),
        "spacing and case aside"
    );
    // Negative controls: each one thing short of the sheet.
    for (why, buttons, in_sheet) in [
        (
            "the buttons in a window, no sheet",
            &[("Later", true), ("Quit & Reopen", true)][..],
            false,
        ),
        ("no Later", &[("Quit & Reopen", true)][..], true),
        ("no Quit & Reopen", &[("Later", true)][..], true),
        ("a Quit alone", &[("Later", true), ("Quit", true)][..], true),
        (
            "Quit & Reopen disabled",
            &[("Later", true), ("Quit & Reopen", false)][..],
            true,
        ),
        (
            "Later disabled",
            &[("Later", false), ("Quit & Reopen", true)][..],
            true,
        ),
    ] {
        assert_eq!(
            reopen_sheet(&reopen_fixture(buttons, in_sheet)),
            None,
            "{why}"
        );
    }
    let (password, _) = sheet("Enter your password to continue.", None);
    assert_eq!(
        reopen_sheet(&password),
        None,
        "the authorization sheet is not it"
    );
}

/// The app's alert with `words` and `buttons` (each a title and whether it is enabled), in its
/// own window (an `AXWindow`), beside the app's terminal window. Not a recording: no row had
/// read the alert before this fixture was written, and its body is longer than [`describe`]
/// keeps, to show the cut.
fn quit_fixture(words: &str, buttons: &[(&str, bool)]) -> Vec<ax::Node> {
    let mut nodes = vec![
        node("AXWindow", "AXStandardWindow", "aterm", usize::MAX),
        // The terminal window's own Quit-titled control is no alert: its window does not read
        // the question.
        node("AXButton", "", "Quit", 0),
        node("AXWindow", "AXDialog", "", usize::MAX),
        node("AXImage", "", "", 2),
        node("AXStaticText", "", words, 2),
        node(
            "AXStaticText",
            "",
            "A body longer than describe keeps: terminal sessions will end, and windows and tabs reopen next time.",
            2,
        ),
    ];
    for (title, enabled) in buttons {
        let mut button = node("AXButton", "", title, 2);
        button.enabled = *enabled;
        nodes.push(button);
    }
    nodes
}

#[test]
fn the_person_answers_the_apps_quit_question_only_after_its_own_quit_and_reopen() {
    let who = standard();
    let asked = quit_fixture(QUIT_QUESTION, &[("Quit", true), ("Cancel", true)]);
    let found = quit_question(&asked).expect("the question");
    assert_eq!(
        found,
        QuitQuestion {
            dialog: 2..8,
            quit: 6
        }
    );
    assert_eq!(
        describe(&asked, found.dialog.clone(), &who),
        "AXWindow/AXDialog; AXImage; AXStaticText \"Quit aterm?\"; AXStaticText \"A body longer than describe keeps: termi\
         nal sessions will end, and windows and t\"; AXButton \"Quit\"; AXButton \"Cancel\"",
        "each control's text cut at 80 characters"
    );
    let mut twice = quit_fixture(QUIT_QUESTION, &[("Cancel", true), ("quit", true)]);
    twice[4].text = format!("{QUIT_QUESTION} {QUIT_QUESTION}");
    assert_eq!(
        quit_question(&twice).map(|q| q.quit),
        Some(7),
        "case aside, the question read from its title and its value"
    );
    // Negative controls: each one thing short of the question.
    for (why, words, buttons) in [
        (
            "a Cancel-only alert",
            QUIT_QUESTION,
            &[("Cancel", true)][..],
        ),
        ("no Cancel", QUIT_QUESTION, &[("Quit", true)][..]),
        (
            "Quit disabled",
            QUIT_QUESTION,
            &[("Quit", false), ("Cancel", true)][..],
        ),
        (
            "Cancel disabled",
            QUIT_QUESTION,
            &[("Quit", true), ("Cancel", false)][..],
        ),
        (
            "another question",
            "Close this window?",
            &[("Quit", true), ("Cancel", true)][..],
        ),
        (
            "more words than the question",
            "Quit aterm? It is not.",
            &[("Quit", true), ("Cancel", true)][..],
        ),
        (
            "the app's own page: its Continue and Open",
            QUIT_QUESTION,
            &[("Continue", true), ("Open", true), ("Cancel", true)][..],
        ),
        (
            "a Quit & Reopen, not a Quit",
            QUIT_QUESTION,
            &[("Quit & Reopen", true), ("Cancel", true)][..],
        ),
    ] {
        assert_eq!(quit_question(&quit_fixture(words, buttons)), None, "{why}");
    }
    assert_eq!(
        quit_question(&reopen_fixture(
            &[("Later", true), ("Quit & Reopen", true)],
            true
        )),
        None,
        "System Settings' sheet is not the app's question"
    );
    // When: within a minute of the person's own Quit & Reopen press, and never with Later.
    let soon = Some(Duration::from_secs(6));
    assert!(answers_quit(Sheet::Reopen, soon));
    assert!(!answers_quit(Sheet::Reopen, None), "no earlier sheet press");
    assert!(!answers_quit(Sheet::Later, soon), "--sheet later");
    assert!(!answers_quit(Sheet::Reopen, Some(QUIT_ASKED_WITHIN)));
    let at = Instant::now();
    let mut pressed: BTreeMap<String, Instant> = [
        ("812 sheet Later".to_owned(), at),
        ("812 Allow".to_owned(), at),
        ("812 switch 10,20".to_owned(), at),
        ("793 quit-confirm".to_owned(), at),
    ]
    .into();
    assert_eq!(
        reopen_pressed(&pressed),
        None,
        "a Later, an Allow, a switch and the question itself are no Quit & Reopen press"
    );
    pressed.insert(sheet_key(812, Sheet::Reopen), at);
    assert_eq!(reopen_pressed(&pressed), Some(at));
    // Native dialog passes never act in the app. Its receipt-bound setup PTY is separate.
    assert!(!ACTS_ON.iter().any(|o| o.to_lowercase().contains("aterm")));
    assert!(!ALLOW.contains(&"Quit"));
}

/// `n` with its frame at `frame` (x, y, width, height; top-left origin, as AX gives it).
fn framed(mut n: ax::Node, frame: (f64, f64, f64, f64)) -> ax::Node {
    n.frame = frame;
    n
}

/// System Settings' window on a page with words `words` above a list holding `rows` (each an
/// app's name and its switch's value), its window titled `title`, with the sidebar's outline
/// at the left, the buttons `buttons` (each a title) just below the list, and the page's
/// Processor Trace switch, off, below them. The words and layout are macOS's form of the
/// Developer Tools page, not a recording: no row had read the page before this fixture was
/// written.
fn devtools_fixture(
    title: &str,
    words: &str,
    rows: &[(&str, i64)],
    buttons: &[&str],
) -> Vec<ax::Node> {
    let mut nodes = vec![
        framed(
            node("AXWindow", "AXStandardWindow", title, usize::MAX),
            (100.0, 50.0, 715.0, 600.0),
        ),
        framed(node("AXGroup", "", "", 0), (100.0, 50.0, 715.0, 600.0)),
        // The sidebar: an outline whose rows name the pages, Privacy & Security among them.
        framed(
            node("AXScrollArea", "", "", 1),
            (100.0, 100.0, 200.0, 540.0),
        ),
        framed(node("AXOutline", "", "", 2), (100.0, 100.0, 200.0, 540.0)),
        node("AXRow", "", "", 3),
        node("AXStaticText", "", "Privacy & Security", 4),
        // The page, in a scroll area that starts above its words.
        framed(node("AXScrollArea", "", "", 1), (310.0, 90.0, 500.0, 550.0)),
        framed(node("AXHeading", "", title, 6), (330.0, 100.0, 200.0, 20.0)),
        framed(
            node("AXStaticText", "", words, 6),
            (330.0, 130.0, 460.0, 32.0),
        ),
        framed(
            node("AXScrollArea", "", "", 6),
            (330.0, 170.0, 460.0, 120.0),
        ),
        framed(node("AXTable", "", "", 9), (330.0, 170.0, 460.0, 120.0)),
    ];
    let mut y = 170.0;
    for (name, on) in rows {
        let row = nodes.len();
        nodes.push(framed(node("AXRow", "", "", 10), (330.0, y, 460.0, 30.0)));
        nodes.push(node("AXCell", "", "", row));
        nodes.push(node("AXImage", "", "", row + 1));
        nodes.push(node("AXStaticText", "", name, row + 1));
        nodes.push(node("AXCell", "", "", row));
        let mut switch = framed(
            node("AXCheckBox", "AXSwitch", "", row + 4),
            (750.0, y + 5.0, 30.0, 20.0),
        );
        switch.value = Some(*on);
        nodes.push(switch);
        y += 30.0;
    }
    let mut x = 332.0;
    for title in buttons {
        nodes.push(framed(
            node("AXButton", "", title, 6),
            (x, 292.0, 22.0, 22.0),
        ));
        x += 24.0;
    }
    nodes.push(framed(
        node("AXStaticText", "", "Processor Trace", 6),
        (330.0, 340.0, 200.0, 20.0),
    ));
    let mut trace = framed(
        node("AXCheckBox", "AXSwitch", "Processor Trace", 6),
        (750.0, 340.0, 30.0, 20.0),
    );
    trace.value = Some(0);
    nodes.push(trace);
    nodes
}

const DEVTOOLS_LINE: &str = "Allow the applications below to run software locally that does \
                             not meet the system’s security policy.";

#[test]
fn the_person_finds_the_developer_tools_page_its_list_and_the_plus_below_it() {
    let lacks = devtools_fixture(
        "Developer Tools",
        DEVTOOLS_LINE,
        &[("Terminal", 1)],
        &["Add", "Remove"],
    );
    let pane = devtools_pane(&lacks).expect("the page");
    assert_eq!(
        pane,
        DevtoolsPane {
            anchor: 8,
            list: Some(9),
            holds: false,
            switch: None,
            plus: Some(17),
        },
        "the list's scroll area, under the words; Terminal's switch is not aterm's"
    );
    assert_eq!(lacks[17].text, "Add");
    assert_eq!(pane_state(&lacks, &pane), "list=lacks switch=none");
    // aterm on the list, its switch off: its row's switch, not Terminal's or Processor Trace's.
    let holds = devtools_fixture(
        "Developer Tools",
        DEVTOOLS_LINE,
        &[("Terminal", 1), ("aterm", 0)],
        &["+", "−"],
    );
    let pane = devtools_pane(&holds).expect("the page");
    assert!(pane.holds);
    let switch = pane.switch.expect("aterm's switch");
    assert_eq!(holds[holds[switch].parent].role, "AXCell");
    assert_eq!(holds[switch].frame.1, 205.0, "the second row's");
    assert_eq!(pane.plus.map(|k| holds[k].text.as_str()), Some("+"));
    assert_eq!(pane_state(&holds, &pane), "list=holds switch=off");
    // Untitled + and −: the left one.
    let bare = devtools_fixture("Developer Tools", DEVTOOLS_LINE, &[], &["", ""]);
    let plus = devtools_pane(&bare).and_then(|p| p.plus).expect("a +");
    assert_eq!(bare[plus].frame.0, 332.0);
    // Words that read otherwise: the window's title and its heading.
    let titled = devtools_fixture("Developer Tools", "", &[("Terminal", 1)], &["Add"]);
    assert_eq!(
        devtools_pane(&titled).map(|p| (p.anchor, p.list)),
        Some((7, Some(9)))
    );
    assert!(names_aterm("“aterm”") && names_aterm("aterm.app") && names_aterm("ATERM"));
    // Negative controls.
    assert!(!names_aterm("aterms") && !names_aterm("Waterm") && !names_aterm("aterm-lab"));
    let fda = devtools_fixture(
        "Full Disk Access",
        "Allow the applications below to access data like Mail, Messages, Safari, Home, Time \
         Machine backups and certain administrative settings for all users on this Mac.",
        &[("aterm", 0)],
        &["Add", "Remove"],
    );
    assert_eq!(devtools_pane(&fda), None, "Full Disk Access is not it");
    let mut privacy = devtools_fixture("Privacy & Security", "", &[], &[]);
    privacy.push(node("AXButton", "", "Developer Tools", 6));
    privacy.push(node("AXStaticText", "", "Developer Tools", 6));
    assert_eq!(
        devtools_pane(&privacy),
        None,
        "Privacy & Security's own row named Developer Tools is not it"
    );
    let removes = devtools_fixture("Developer Tools", DEVTOOLS_LINE, &[], &["Remove", "−"]);
    assert_eq!(
        devtools_pane(&removes).map(|p| p.plus),
        Some(None),
        "a titled Remove or − is never +"
    );
    let mut far = devtools_fixture("Developer Tools", DEVTOOLS_LINE, &[], &["Add"]);
    let add = far.len() - 3;
    far[add].frame.1 = 600.0;
    assert_eq!(
        devtools_pane(&far).map(|p| p.plus),
        Some(None),
        "an Add far below the list"
    );
    let mut disabled = devtools_fixture("Developer Tools", DEVTOOLS_LINE, &[], &["Add"]);
    let add = disabled.len() - 3;
    disabled[add].enabled = false;
    assert_eq!(devtools_pane(&disabled).map(|p| p.plus), Some(None));
    // No list under the words: the page reads, its list does not.
    let mut listless = devtools_fixture("Developer Tools", DEVTOOLS_LINE, &[], &[]);
    listless.truncate(9);
    let pane = devtools_pane(&listless).expect("the page");
    assert_eq!((pane.list, pane.plus), (None, None));
    assert_eq!(pane_state(&listless, &pane), "list=unread switch=none");
}

/// aterm's windows: its terminal window with its alert's icon, and its helper panel headed
/// `heading` with the tile of aterm (an `AXImage` labelled aterm, as
/// `aterm_gui::devtools_helper`'s tile sets it) at `tile`.
fn helper_fixture(heading: &str, tile: (f64, f64, f64, f64)) -> Vec<ax::Node> {
    vec![
        node("AXWindow", "AXStandardWindow", "aterm", usize::MAX),
        framed(
            node("AXImage", "", "aterm alert", 0),
            (10.0, 10.0, 64.0, 64.0),
        ),
        node("AXWindow", "AXFloatingWindow", "aterm", usize::MAX),
        node("AXStaticText", "", heading, 2),
        node("AXStaticText", "", "Step 1 of 2", 2),
        framed(node("AXImage", "", "aterm", 2), tile),
        node(
            "AXStaticText",
            "",
            "Drag aterm into the Developer Tools list",
            2,
        ),
    ]
}

#[test]
fn the_person_drags_aterm_only_from_the_helper_panels_developer_tools_page() {
    let shown = (850.0, 200.0, 64.0, 64.0);
    let nodes = helper_fixture(DEVTOOLS_HELPER_TITLE, shown);
    assert_eq!(devtools_tile(&nodes), Some(5));
    assert_eq!(centre_words(nodes[5].frame), "882,232");
    // Negative controls: the Full Disk Access page's tile, a tile with no frame, the app's
    // alert icon.
    assert_eq!(
        devtools_tile(&helper_fixture("Turn on Full Disk Access for aterm", shown)),
        None
    );
    assert_eq!(
        devtools_tile(&helper_fixture(DEVTOOLS_HELPER_TITLE, (0.0, 0.0, 0.0, 0.0))),
        None
    );
    let mut alert_only = helper_fixture(DEVTOOLS_HELPER_TITLE, shown);
    alert_only[5].text = "aterm alert".to_owned();
    assert_eq!(devtools_tile(&alert_only), None);
    // The person adds at most ADDS times, each ADD_SETTLE after the last and after a password
    // typed since it.
    let s = |secs| Some(Duration::from_secs(secs));
    assert!(may_add(0, None, None));
    assert!(
        may_add(0, None, s(1)),
        "the Full Disk Access password before the page"
    );
    assert!(!may_add(1, s(5), None), "too soon after a try");
    assert!(may_add(1, s(25), None));
    assert!(!may_add(1, s(25), s(5)), "too soon after a password");
    assert!(may_add(2, s(25), s(21)));
    assert!(!may_add(ADDS, s(600), None), "no more tries");
}

/// An open panel as a sheet of System Settings' window: its file view, a Go to field and its
/// buttons (each a title and whether it is enabled). macOS's form, not a recording.
fn panel_fixture(view: &str, buttons: &[(&str, bool)]) -> Vec<ax::Node> {
    let mut nodes = vec![
        node(
            "AXWindow",
            "AXStandardWindow",
            "Developer Tools",
            usize::MAX,
        ),
        node("AXSheet", "", "", 0),
        node("AXGroup", "AXHostingView", "", 1),
        node("AXPopUpButton", "", "Applications", 2),
        node(view, "", "", 2),
        node("AXTextField", "AXSearchField", "", 2),
    ];
    for (title, enabled) in buttons {
        let mut button = node("AXButton", "", title, 2);
        button.enabled = *enabled;
        nodes.push(button);
    }
    nodes
}

#[test]
fn the_person_finds_the_open_panel_and_types_the_path_only_in_a_field_in_clear() {
    let panel = panel_fixture("AXBrowser", &[("Cancel", true), ("Open", false)]);
    assert_eq!(
        open_panel(&panel),
        Some(OpenPanel {
            dialog: 1..8,
            open: 7,
            cancel: 6
        }),
        "Open is off until something is chosen"
    );
    assert!(
        open_panel(&panel_fixture(
            "AXOutline",
            &[("Open", true), ("Cancel", true)]
        ))
        .is_some()
    );
    // Negative controls: Gatekeeper's alert has Open and Cancel and no files; Cancel off; no
    // Cancel; the app's quit question.
    assert_eq!(
        open_panel(&panel_fixture(
            "AXStaticText",
            &[("Cancel", true), ("Open", true)]
        )),
        None
    );
    assert_eq!(
        open_panel(&panel_fixture(
            "AXBrowser",
            &[("Cancel", false), ("Open", true)]
        )),
        None
    );
    assert_eq!(
        open_panel(&panel_fixture("AXBrowser", &[("Open", true)])),
        None
    );
    assert_eq!(
        open_panel(&quit_fixture(
            QUIT_QUESTION,
            &[("Quit", true), ("Cancel", true), ("Open", true)]
        )),
        None,
        "the quit question, were it to show an Open: no files"
    );
    assert_eq!(
        open_panel(&devtools_fixture(
            "Developer Tools",
            DEVTOOLS_LINE,
            &[],
            &["Add", "Open"]
        )),
        None,
        "the page itself, were it to show an Open: its table is no panel's, with no Cancel"
    );
    let field = node("AXTextField", "", "/Applications", 0);
    assert!(goes_to(Some(&field)));
    assert!(goes_to(Some(&node("AXComboBox", "", "", 0))));
    // Negative controls: a secure field, a button, an unreadable focus.
    assert!(!goes_to(Some(&node(
        "AXTextField",
        "AXSecureTextField",
        "",
        0
    ))));
    assert!(!goes_to(Some(&node("AXButton", "", "Open", 0))));
    assert!(!goes_to(None));
    assert!(
        !goes_to(Some(&node("AXTextField", "AXSearchField", "", 0))),
        "the panel's search field: Command-Shift-G did not take"
    );
    assert_eq!(
        bundle_of("/Applications/aterm.app/Contents/MacOS/aterm"),
        Some("/Applications/aterm.app")
    );
    assert_eq!(bundle_of("/usr/local/bin/aterm"), None);
    assert_eq!(
        bundle_of("/Applications/aterm.app/Contents/MacOS/x/aterm"),
        None
    );
    assert_eq!(bundle_of("/opt/aterm/Contents/MacOS/aterm"), None);
    // The host reads the person's acts by these words (`mac_nux_guest::DEVTOOLS_*`).
    assert_eq!(
        [
            DEVTOOLS_DRAG,
            DEVTOOLS_PLUS,
            DEVTOOLS_GOTO,
            DEVTOOLS_OPEN,
            DEVTOOLS_CANCEL,
            DEVTOOLS_SWITCH
        ],
        [
            "devtools-drag",
            "devtools-plus",
            "devtools-goto",
            "devtools-open",
            "devtools-cancel",
            "devtools-switch"
        ]
    );
}

#[test]
fn a_sheet_over_the_page_is_never_its_list_its_switch_or_its_plus() {
    // The open panel as a sheet of System Settings' window, its file view listing aterm.app
    // between the page's words and its list, with a switch named aterm and an Add of its own.
    let with_sheet = |role: &str, buttons: &[&str]| {
        let mut nodes = devtools_fixture(
            "Developer Tools",
            DEVTOOLS_LINE,
            &[("Terminal", 1)],
            buttons,
        );
        let sheet = nodes.len();
        nodes.push(framed(node(role, "", "", 0), (300.0, 60.0, 520.0, 400.0)));
        nodes.push(framed(
            node("AXScrollArea", "", "", sheet),
            (330.0, 164.0, 460.0, 120.0),
        ));
        nodes.push(framed(
            node("AXOutline", "", "", sheet + 1),
            (330.0, 164.0, 460.0, 120.0),
        ));
        nodes.push(framed(
            node("AXRow", "", "", sheet + 2),
            (330.0, 164.0, 460.0, 20.0),
        ));
        nodes.push(node("AXStaticText", "", "aterm.app", sheet + 3));
        let mut switch = framed(
            node("AXCheckBox", "AXSwitch", "aterm", sheet + 3),
            (750.0, 166.0, 30.0, 16.0),
        );
        switch.value = Some(0);
        nodes.push(switch);
        nodes.push(framed(
            node("AXButton", "", "Add", sheet),
            (332.0, 292.0, 22.0, 22.0),
        ));
        nodes
    };
    let nodes = with_sheet("AXSheet", &[]);
    let pane = devtools_pane(&nodes).expect("the page");
    assert_eq!(
        (pane.list, pane.holds, pane.switch, pane.plus),
        (Some(9), false, None, None),
        "the page's own list, which lacks aterm; the sheet's switch and Add are not its"
    );
    assert_eq!(pane_state(&nodes, &pane), "list=lacks switch=none");
    // Negative control: the same controls in a group of the window are the page's.
    let nodes = with_sheet("AXGroup", &[]);
    let pane = devtools_pane(&nodes).expect("the page");
    assert!(pane.holds && pane.switch.is_some() && pane.plus.is_some());
    assert_ne!(pane.list, Some(9));
}

#[test]
fn the_person_cancels_the_go_to_sheet_before_the_open_panel_it_stands_in() {
    // The open panel with the Go to sheet still up inside it, ahead of the panel's buttons.
    let mut nodes = panel_fixture("AXBrowser", &[]);
    nodes.push(node("AXSheet", "", "", 1));
    nodes.push(node("AXTextField", "", "/Applications/aterm.app", 6));
    nodes.push(node("AXButton", "", "Cancel", 6));
    nodes.push(node("AXButton", "", "Go", 6));
    nodes.push(node("AXButton", "", "Cancel", 2));
    nodes.push(node("AXButton", "", "Open", 2));
    let panel = open_panel(&nodes).expect("the panel");
    assert_eq!(
        (panel.dialog.clone(), panel.open, panel.cancel),
        (1..12, 11, 10),
        "the panel's own Cancel, not the Go to sheet's"
    );
    assert_eq!(inner_cancel(&nodes, &panel), Some(8));
    // Negative control: with the Go to sheet gone, nothing inside the panel to cancel first.
    let bare = panel_fixture("AXBrowser", &[("Cancel", true), ("Open", false)]);
    let panel = open_panel(&bare).expect("the panel");
    assert_eq!(inner_cancel(&bare, &panel), None);
}

#[test]
fn the_person_takes_the_panel_for_gone_only_on_two_misses_or_no_window() {
    let mut misses = 0;
    assert_eq!(
        panel_read(Some(false), true, true, &mut misses),
        PanelRead::Unsure
    );
    assert_eq!(
        panel_read(None, true, true, &mut misses),
        PanelRead::Unsure,
        "one miss"
    );
    assert_eq!(
        panel_read(Some(false), true, true, &mut misses),
        PanelRead::Unsure
    );
    assert_eq!(misses, 0, "a match starts the count again");
    assert_eq!(panel_read(None, true, true, &mut misses), PanelRead::Unsure);
    assert_eq!(
        panel_read(None, false, true, &mut misses),
        PanelRead::Unsure,
        "empty read"
    );
    assert_eq!(panel_read(None, true, true, &mut misses), PanelRead::Gone);
    assert_eq!(
        panel_read(Some(true), true, true, &mut misses),
        PanelRead::Open
    );
    let mut misses = 0;
    assert_eq!(
        panel_read(None, false, false, &mut misses),
        PanelRead::Gone,
        "its process shows no window"
    );
}

#[test]
fn the_person_acts_on_the_developer_tools_page_only_when_nothing_stands_over_it() {
    let s = |secs| Some(Duration::from_secs(secs));
    let lacks = DevSweep {
        list: true,
        ..DevSweep::default()
    };
    assert_eq!(next(&lacks), DevAct::Add);
    // No add while a sheet or the panel service's window stands, or the list did not read.
    assert_eq!(
        next(&DevSweep {
            sheet_up: true,
            ..lacks
        }),
        DevAct::Wait
    );
    assert_eq!(
        next(&DevSweep {
            list: false,
            ..lacks
        }),
        DevAct::Wait
    );
    assert_eq!(
        next(&DevSweep {
            adds: 1,
            last_add: s(5),
            ..lacks
        }),
        DevAct::Wait,
        "too soon after a try"
    );
    // An open panel: waited on, then cancelled once it has stood PANEL_STRAY, again only
    // PANEL_STRAY after that Cancel.
    let panel = |stood, cancelled| DevSweep {
        panel_for: Some(stood),
        cancelled,
        sheet_up: true,
        ..lacks
    };
    assert_eq!(next(&panel(Duration::from_secs(2), None)), DevAct::Wait);
    assert_eq!(next(&panel(PANEL_STRAY, None)), DevAct::CancelPanel);
    assert_eq!(next(&panel(PANEL_STRAY, s(2))), DevAct::Wait);
    assert_eq!(
        next(&panel(PANEL_STRAY, Some(PANEL_STRAY))),
        DevAct::CancelPanel
    );
    // aterm on the list: its switch pressed while it reads off, once per AGAIN; never under a
    // sheet; and no add.
    let holds = DevSweep {
        holds: true,
        switch_off: true,
        ..lacks
    };
    assert_eq!(next(&holds), DevAct::Switch);
    assert_eq!(
        next(&DevSweep {
            switched: s(1),
            ..holds
        }),
        DevAct::Wait
    );
    assert_eq!(
        next(&DevSweep {
            switched: Some(AGAIN),
            ..holds
        }),
        DevAct::Switch
    );
    assert_eq!(
        next(&DevSweep {
            sheet_up: true,
            ..holds
        }),
        DevAct::Wait
    );
    assert_eq!(
        next(&DevSweep {
            switch_off: false,
            ..holds
        }),
        DevAct::Wait,
        "on already, or no switch: and no add, though none was made"
    );
    // How it adds: a drag on the first try that finds the tile, waiting for it on the first
    // try up to TILE_WITHIN, else +.
    let early = Duration::from_secs(1);
    assert_eq!(add_how(0, false, true, early), Some(AddBy::Drag));
    assert_eq!(add_how(0, false, false, early), None, "the tile not up yet");
    assert_eq!(add_how(0, false, false, TILE_WITHIN), Some(AddBy::Plus));
    assert_eq!(
        add_how(1, false, true, TILE_WITHIN),
        Some(AddBy::Drag),
        "a later try that finds the tile"
    );
    assert_eq!(
        add_how(1, true, true, TILE_WITHIN),
        Some(AddBy::Plus),
        "one drag at most"
    );
    assert_eq!(add_how(1, false, false, early), Some(AddBy::Plus));
}

#[test]
fn the_person_answers_a_dialog_that_asks_for_an_administrator_as_the_administrator() {
    let account = ["nux".to_owned(), "NUX Standard".to_owned()];
    let asks = "Enter an administrator’s name and password to continue with Privacy & Security.";
    let own = "Enter your password to continue with Privacy & Security.";
    let admin = Some("admin");
    let whose = |words: &str, name: Option<&str>, admin: Option<&str>| {
        let (nodes, secure) = sheet(words, name);
        answer(&nodes, secure, &account, admin)
    };
    let rename = Answer::Administrator { rename: true };
    // The administrator's name goes into an empty name field (as the guest showed it) and over
    // the account's, were the dialog to show it though it asks for an administrator.
    assert_eq!(whose(asks, Some(""), admin), (rename, Some(7)));
    assert_eq!(whose(asks, Some("NUX Standard"), admin), (rename, Some(7)));
    assert_eq!(whose(own, Some(""), admin), (rename, Some(7)));
    assert_eq!(whose(own, Some("someone"), admin), (rename, Some(7)));
    // A name field already showing the administrator keeps it.
    assert_eq!(
        whose(asks, Some("Admin"), admin),
        (Answer::Administrator { rename: false }, Some(7))
    );
    // The account's own password: a prompt for it (no name field, or one showing the account,
    // its words naming no administrator), and every dialog without an administrator at hand.
    assert_eq!(
        whose(own, Some("NUX Standard"), admin),
        (Answer::Account, Some(7))
    );
    assert_eq!(whose(own, Some("nux"), admin), (Answer::Account, Some(7)));
    assert_eq!(
        whose(asks, None, admin),
        (Answer::Account, None),
        "the window's fields outside the sheet are no name field"
    );
    assert_eq!(
        whose(asks, Some("NUX Standard"), None),
        (Answer::Account, Some(7))
    );
    assert_eq!(whose(own, None, None), (Answer::Account, None));
    // A name field whose text did not read shows neither the account nor the administrator:
    // the words alone decide.
    let unread = |words: &str| {
        let (mut nodes, secure) = sheet(words, Some(""));
        nodes[7].value_text = None;
        answer(&nodes, secure, &account, admin)
    };
    assert_eq!(unread(own), (Answer::Account, Some(7)));
    assert_eq!(unread(asks), (rename, Some(7)));
    // A dialog window of its own (SecurityAgent's) is the dialog, whole.
    let window = vec![
        node("AXWindow", "AXDialog", "", usize::MAX),
        node("AXStaticText", "", asks, 0),
        node("AXTextField", "", "NUX Standard", 0),
        node("AXTextField", "AXSecureTextField", "", 0),
        node("AXButton", "", "OK", 0),
    ];
    assert_eq!(answer(&window, 3, &account, admin), (rename, Some(2)));
    assert_eq!(dialog_around(&window, 3), 0..5);
}

/// A standard account's person, with the image's administrator at hand.
fn standard() -> Credentials {
    Credentials {
        password: "nux-lane-2026".to_owned(),
        admin: Some(("admin".to_owned(), "admin".to_owned())),
        account: vec!["nux".to_owned(), "NUX Standard".to_owned()],
    }
}

#[test]
fn a_described_dialog_never_shows_a_secure_fields_text() {
    let who = standard();
    let (nodes, secure) = sheet("Enter an administrator’s name", Some(""));
    assert_eq!(dialog_around(&nodes, secure), 2..11);
    assert_eq!(
        describe(&nodes, dialog_around(&nodes, secure), &who),
        "AXSheet; AXGroup/AXHostingView; AXImage \"Lock\"; AXStaticText \"Privacy & Security is \
         trying to modify your system settings.\"; AXStaticText \"Enter an administrator’s \
         name\"; AXTextField; AXTextField/AXSecureTextField (secure); AXButton \"Modify \
         Settings\"; AXButton \"Cancel\""
    );
    assert_eq!(token(Some("NUX Standard"), &who), "NUX_Standard");
    assert_eq!(token(Some(" "), &who), "(empty)");
    assert_eq!(token(None, &who), "(unread)");
}

#[test]
fn a_password_that_went_into_a_field_not_secure_is_never_recorded() {
    let who = standard();
    // Keys that went astray: the administrator's password after its name, the account's after
    // its full name, the administrator's over a search.
    for astray in [
        "adminadmin",
        "NUX Standardnux-lane-2026",
        "Full Disk Accessadmin",
    ] {
        assert!(who.holds_password(astray), "{astray}");
        assert_eq!(token(Some(astray), &who), "(a password)", "{astray}");
        let (mut nodes, secure) = sheet("Enter an administrator’s name", Some(astray));
        nodes[1].value_text = Some(astray.to_owned());
        nodes[1].text = astray.to_owned();
        let field = describe(&nodes, dialog_around(&nodes, secure), &who);
        assert!(field.contains("AXTextField \"(a password)\""), "{field}");
        assert!(!field.contains(astray), "{field}");
        assert_eq!(
            part(&nodes[1], &who),
            "AXTextField/AXSearchField \"(a password)\""
        );
    }
    // The names the person reads and types are shown, though the administrator's is its
    // password too; the dialog's words are no field, so "administrator" stays.
    for name in ["admin", "NUX Standard", "nux", "Managed via Tart", ""] {
        assert!(!who.holds_password(name), "{name}");
    }
    let (mut nodes, secure) = sheet("Enter an administrator’s name", Some("admin"));
    // A static text's value is its words, as the Accessibility read gives it.
    nodes[6].value_text = Some(nodes[6].text.clone());
    assert_eq!(
        describe(&nodes, dialog_around(&nodes, secure), &who),
        "AXSheet; AXGroup/AXHostingView; AXImage \"Lock\"; AXStaticText \"Privacy & Security is \
         trying to modify your system settings.\"; AXStaticText \"Enter an administrator’s \
         name\"; AXTextField \"admin\"; AXTextField/AXSecureTextField (secure); AXButton \
         \"Modify Settings\"; AXButton \"Cancel\""
    );
    // The admin lane's person: the account is the administrator, `admin`.
    let admin = Credentials {
        password: "admin".to_owned(),
        admin: None,
        account: vec!["admin".to_owned(), "Managed via Tart".to_owned()],
    };
    assert!(!admin.holds_password("admin"));
    assert!(!admin.holds_password("Managed via Tart"));
    assert!(admin.holds_password("Managed via Tartadmin"));
}

#[test]
fn the_person_types_a_password_only_where_the_focus_is_not_a_field_in_clear() {
    let at = |role: &str, subrole: &str, frame: (f64, f64, f64, f64)| ax::Node {
        frame,
        ..node(role, subrole, "", usize::MAX)
    };
    let secure = at(
        "AXTextField",
        "AXSecureTextField",
        (100.0, 200.0, 220.0, 24.0),
    );
    let name = at("AXTextField", "", (100.0, 160.0, 220.0, 24.0));
    let search = at("AXTextField", "AXSearchField", (10.0, 10.0, 180.0, 28.0));
    // The keys go into the secure field, or where the focus did not read, or a control that
    // takes no text: never into the name field or the window's search field.
    assert!(may_type(Some(&secure)));
    assert!(may_type(None));
    assert!(may_type(Some(&at("AXButton", "", (0.0, 0.0, 80.0, 20.0)))));
    assert!(!may_type(Some(&name)));
    assert!(!may_type(Some(&search)));
    assert!(!may_type(Some(&at("AXTextArea", "", (0.0, 0.0, 9.0, 9.0)))));
    // The focus is in the field clicked when it reads as secure as it, at its place.
    assert!(is_at(&secure, &secure));
    assert!(is_at(&name, &name));
    assert!(
        !is_at(&name, &secure),
        "the name field is not the secure field"
    );
    assert!(!is_at(&secure, &name));
    assert!(
        !is_at(&search, &name),
        "the search field is not the name field"
    );
    let unplaced = at("AXTextField", "", (0.0, 0.0, 0.0, 0.0));
    assert!(
        !is_at(&unplaced, &unplaced),
        "a frame that did not read places nothing"
    );
}

#[test]
fn an_administrators_password_replaces_what_an_earlier_try_left_in_the_field() {
    let rename = Answer::Administrator { rename: true };
    let kept = Answer::Administrator { rename: false };
    for answer in [rename, kept] {
        assert_eq!(
            password_keys(answer, "admin"),
            [Key::SelectAll, Key::Text("admin"), Key::Return]
        );
    }
    // The account's own password, typed once per dialog: its keys alone, its dialog not
    // watched nor answered again.
    assert_eq!(
        password_keys(Answer::Account, "nux-lane-2026"),
        [Key::Text("nux-lane-2026"), Key::Return]
    );
    assert!(!Answer::Account.watched());
    assert!(rename.watched() && kept.watched());
}

#[test]
fn the_person_types_an_administrators_name_and_password_only_as_the_administrator() {
    // The image's administrator is `admin`, password `admin`: told apart here, so a name typed
    // as a password, or one person's password for another's, shows.
    let who = Credentials {
        password: "nux-lane-2026".to_owned(),
        admin: Some(("boss".to_owned(), "boss-pw".to_owned())),
        account: vec!["nux".to_owned(), "NUX Standard".to_owned()],
    };
    let rename = Answer::Administrator { rename: true };
    let kept = Answer::Administrator { rename: false };
    assert_eq!(
        credentials(Answer::Account, &who),
        (None, "nux-lane-2026"),
        "the account's own password, its name field left alone"
    );
    assert_eq!(credentials(rename, &who), (Some("boss"), "boss-pw"));
    assert_eq!(
        credentials(kept, &who),
        (None, "boss-pw"),
        "a name field already showing the administrator keeps it"
    );
    // With no administrator at hand, the account's own password whatever the dialog asks.
    let alone = Credentials { admin: None, ..who };
    for answer in [Answer::Account, rename, kept] {
        assert_eq!(
            credentials(answer, &alone),
            (None, "nux-lane-2026"),
            "{answer:?}"
        );
    }
}

#[test]
fn a_password_dialog_went_when_its_windows_left_the_screen_or_its_secure_field_went() {
    let dialog = [41, 42];
    let listed = |ids: &[u64]| ids.iter().copied().collect::<BTreeSet<u64>>();
    let (asking, _) = sheet("Enter an administrator’s name", Some(""));
    let window = vec![
        node(
            "AXWindow",
            "AXStandardWindow",
            "System Settings",
            usize::MAX,
        ),
        node("AXTextField", "AXSearchField", "", 0),
    ];
    // SecurityAgent's dialog, its only window: once it closes no window of it is listed and
    // its controls read nothing.
    assert!(gone(&dialog, Some(&listed(&[7, 99])), &[]));
    assert!(gone(&dialog, Some(&listed(&[])), &[]));
    // A dialog the app shows after it is another, with windows of its own.
    assert!(gone(&dialog, Some(&listed(&[43])), &asking));
    // System Settings' sheet: its window stays listed, its controls read with no secure field.
    assert!(gone(&dialog, Some(&listed(&[41, 42])), &window));
    assert!(gone(&dialog, None, &window));
    // No sign it went: a secure field still read, or nothing read while a window of it is
    // listed (a busy app) or the list did not read.
    assert!(!gone(&dialog, Some(&listed(&[41, 42])), &asking));
    assert!(
        !gone(&dialog, Some(&listed(&[42, 99])), &[]),
        "an empty read while one of its windows is listed"
    );
    assert!(!gone(&dialog, None, &[]), "a window list that did not read");
    assert!(!gone(&dialog, None, &asking));
}

#[test]
fn a_dialogs_going_is_timed_from_its_return_not_from_the_read_that_showed_the_return() {
    // The idle clock can show a Return long after it went out (17.5 s at load 62).
    let ago = |d: Duration| {
        Instant::now()
            .checked_sub(d)
            .expect("the clock ran that long")
    };
    let ms = gone_after(ago(Duration::from_secs(2)), || true);
    assert!(ms.is_some_and(|ms| ms >= 2000), "{ms:?}");
    // The watch ends GONE_WITHIN after the Return, after one look at least: after a Return
    // shown later than that the person looks once, and a dialog that look sees gone went.
    let late = GONE_WITHIN + Duration::from_secs(1);
    let mut looks = 0;
    let stayed = gone_after(ago(late), || {
        looks += 1;
        false
    });
    assert_eq!((stayed, looks), (None, 1));
    let ms = gone_after(ago(late), || true);
    assert!(ms.is_some_and(|ms| ms >= late.as_millis()), "{ms:?}");
}
