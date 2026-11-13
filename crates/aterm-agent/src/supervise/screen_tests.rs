// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// OF A LIVE COMPOSER'S STATES ONLY THE PROMPT-HISTORY SEARCH LEAVES THE CURSOR UNDER IT, over
/// every screen in `aterm-phase`'s fixtures that records its cursor (`· cursor=<row>,<col>`) and
/// whose program's reader reads a composer: Claude Code 2.1.318's search at every width
/// measured and Muse Code 1.4.14's, each read by its program's reader (`history_search`), which
/// reads it on no other of these screens; and, no live composer, the old prompt box a relaunch
/// left drawn above its new launch line (`claude-2.1.283-inline-relaunch-before-repl`), where a
/// draft typed is lost. Nowhere a gate reads such a composer empty, and it asks no cell there.
/// NEGATIVE CONTROLS: the rule taking the bottom rule's own row for under it, and the half-drawn
/// launch frames read under; it reading no rule between, and every search over nothing reads
/// empty to a gate; `phase::history_search` ending its footer at the first blank row, and the
/// 12-column match reads no search.
#[test]
fn of_a_live_composer_only_the_history_search_leaves_the_cursor_under_it() {
    let dir = aterm_tempfile::manifest_dir().join("../aterm-phase/src/fixtures");
    let mut under = Vec::new();
    let mut read = 0;
    for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
        let path = entry.expect("a fixture").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let Some((row, col)) = aterm_phase::prompt::fixtures::cursor(&text) else {
            continue;
        };
        if text.contains("\n## ") {
            continue;
        }
        let Some(program) = ["claude", "muse", "agy", "grok", "codex", "kimi"]
            .into_iter()
            .find(|p| name.starts_with(&format!("{p}-")))
        else {
            continue;
        };
        let rows = aterm_phase::prompt::fixtures::screen(&text);
        let Some((caret, lines)) = aterm_phase::read(Some(program), &rows, Some(col)).composer
        else {
            continue;
        };
        read += 1;
        let search = name.contains("-history-search") && !name.contains("-left");
        let reader = match program {
            "claude" => Some(aterm_phase::phase::history_search as fn(&[String]) -> bool),
            "muse" => Some(aterm_phase::muse::history_search as fn(&[String]) -> bool),
            _ => None,
        };
        if let Some(history_search) = reader {
            assert_eq!(history_search(&rows), search, "{name}: the search read");
        }
        if cursor_under_composer(&rows, Some(row), caret) {
            assert!(
                !composer_holds_no_draft(&rows, Some((row, col)), caret, &lines, || {
                    panic!("{name}: a cell asked under the composer")
                }),
                "{name}"
            );
            under.push(name);
        }
    }
    under.sort();
    assert!(read >= 40, "the corpus must exercise the rule: {read}");
    let searches: Vec<&String> = under
        .iter()
        .filter(|n| n.contains("-history-search") && !n.contains("-left"))
        .collect();
    assert_eq!(
        under.len(),
        searches.len() + 1,
        "under the composer: {under:#?}"
    );
    assert!(
        under.contains(&"claude-2.1.283-inline-relaunch-before-repl.txt".to_string()),
        "{under:#?}"
    );
    assert!(searches.len() >= 16, "{under:#?}");
}

/// [`Screen::take_cursor_of`]: a later read of the same content gives its cursor whole (row,
/// column and slice start); NEGATIVE CONTROLS: a read whose seq moved, or whose rows moved
/// at the same seq, gives nothing, and the screen keeps the cursor it was read with.
#[test]
fn a_later_reads_cursor_is_taken_whole_only_over_the_same_content() {
    let point = Screen {
        rows: vec!["⏺ Done.".to_string(), "❯ ".to_string()],
        cursor_row: 10,
        cursor_col: 0,
        seq: 7,
        first: 0,
        ..Screen::default()
    };
    let caret = Screen {
        cursor_row: 3,
        cursor_col: 2,
        first: 2,
        ..point.clone()
    };
    let mut taken = point.clone();
    assert!(taken.take_cursor_of(&caret));
    assert_eq!((taken.cursor_row, taken.cursor_col, taken.first), (3, 2, 2));
    assert_eq!(taken.rows, point.rows);
    for moved in [
        Screen {
            seq: 8,
            ..caret.clone()
        },
        Screen {
            rows: vec!["⏺ Done.".to_string(), "❯ hello".to_string()],
            ..caret.clone()
        },
    ] {
        let mut kept = point.clone();
        assert!(!kept.take_cursor_of(&moved), "{moved:?}");
        assert_eq!(kept, point, "{moved:?}");
    }
}

/// A `status` reply's stamp reads as a `text --json` one does, and [`HumanInput::within`]
/// is strictly under the grace.
#[test]
fn a_status_stamp_reads_as_the_one_person_fact() {
    let ok = "OK schema=1 hold=0 hand=- human_ms=5000 level=quiet";
    assert_eq!(HumanInput::of_status(ok), HumanInput::Ago(5000));
    assert_eq!(
        HumanInput::of_status(&ok.replace("5000", "-")),
        HumanInput::Never
    );
    assert_eq!(
        HumanInput::of_status("OK schema=1 hold=0"),
        HumanInput::Unknown,
        "an older server"
    );
    assert_eq!(
        HumanInput::of_status("ERR no such session human_ms=1"),
        HumanInput::Unknown
    );
    assert_eq!(
        HumanInput::of_status("OKAY human_ms=1"),
        HumanInput::Unknown
    );
    assert_eq!(status_field("OKAY human_ms=1", "human_ms"), None);
    assert_eq!(HumanInput::Ago(4999).within(5), Some(true));
    assert_eq!(HumanInput::Ago(5000).within(5), Some(false), "the edge");
    assert_eq!(HumanInput::Never.within(u32::MAX), Some(false));
    assert_eq!(HumanInput::Unknown.within(5), None);
    assert_eq!(HumanInput::Ago(u64::MAX).within(u32::MAX), Some(false));
}

/// The exact shape the server writes (see `cmd_text_json_opt`), with the
/// header line the wire carries and without it (the client strips it).
#[test]
fn parses_the_servers_text_json_shape() {
    let body = r#"{"rows":["line-zero","second \"quoted\" \\ tab\t","❯ caf\u00e9 \ud83d\ude00"],"cursor":{"row":60,"col":2,"visible":true,"style":"blinking_block"},"dims":{"rows":63,"cols":138},"seq":122571}"#;
    for input in [body.to_string(), format!("OK 1\n{body}\n")] {
        let s = parse_text_json(&input).expect("parses");
        assert_eq!(s.rows.len(), 3);
        assert_eq!(s.rows[0], "line-zero");
        assert_eq!(s.rows[1], "second \"quoted\" \\ tab\t");
        assert_eq!(s.rows[2], "❯ café 😀");
        assert_eq!((s.cursor_row, s.cursor_col, s.seq), (60, 2, 122571));
        assert_eq!(s.cols, Some(138), "the pane's width, from dims");
        assert_eq!(
            s.pane_rows,
            Some(63),
            "the PANE's height, from dims — not the 3 rows sent"
        );
    }
    let bare = r#"{"rows":[],"cursor":{"row":0,"col":0},"seq":5}"#;
    assert_eq!(parse_text_json(bare).expect("parses").cols, None);
    assert_eq!(parse_text_json(bare).expect("parses").pane_rows, None);
    let huge = r#"{"rows":[],"cursor":{"row":0,"col":0},"dims":{"rows":2,"cols":70000},"seq":5}"#;
    assert_eq!(parse_text_json(huge).expect("parses").cols, None);
    let trimmed =
        r#"{"rows":[],"cursor":{"row":0,"col":0},"dims":{"rows":2,"cols":2},"seq":5,"trimmed":2}"#;
    let s = parse_text_json(trimmed).expect("parses");
    assert!(s.rows.is_empty());
    assert_eq!(s.seq, 5);
    assert_eq!(s.generation, None, "an older host sends no gen");
    let dup = r#"{"rows":["old"],"rows":["new"],"cursor":{"row":1,"col":2},"seq":1,"seq":9}"#;
    let s = parse_text_json(dup).expect("parses duplicate keys");
    assert_eq!(s.rows, ["new"]);
    assert_eq!(s.seq, 9);
}

/// The screen generation (`"gen":"<epoch>.<seq>"`) is read as the fence a
/// press names; a malformed one is not a fence at all.
#[test]
fn the_screen_generation_is_read_and_a_malformed_one_is_none() {
    let with = |g: &str| {
        parse_text_json(&format!(
            r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"{g}"}}"#
        ))
        .expect("parses")
        .generation
    };
    assert_eq!(with("2.15").as_deref(), Some("2.15"));
    for bad in [
        "15",
        "2.",
        ".15",
        "2.15.1",
        "-2.15",
        "a.b",
        "",
        "18446744073709551616.15",
    ] {
        assert_eq!(with(bad), None, "{bad:?}");
    }
}

/// `"title"`: a string is the program's title as sent, `""` included; NEGATIVE CONTROLS: an
/// older host's (or a tape's) reply without the key, and a key whose value is no string, say
/// nothing, and neither moves another field.
#[test]
fn the_title_is_read_and_its_absence_says_nothing() {
    let with = |tail: &str| {
        parse_text_json(&format!(
            r#"OK 1
{{"rows":["⏺ streaming"],"cursor":{{"row":0,"col":2}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"2.15"{tail},"human_ms":null,"human_seq":3,"launch":null}}"#
        ))
        .expect("parses")
    };
    let sent = with(r#","title":"◐ Claude Code""#);
    assert_eq!(sent.title.as_deref(), Some("◐ Claude Code"));
    assert_eq!(
        with(r#","title":"say \"hi\" \\ ✳""#).title.as_deref(),
        Some(r#"say "hi" \ ✳"#),
        "escapes decode"
    );
    assert_eq!(
        with(r#","title":"""#).title.as_deref(),
        Some(""),
        "a host that sends the key with no title set"
    );
    let absent = with("");
    assert_eq!(absent.title, None, "an older host sends no title");
    for bad in [r#","title":null"#, r#","title":7"#, r#","title":["◐"]"#] {
        let malformed = with(bad);
        assert_eq!(malformed.title, None, "{bad:?}");
        assert_eq!(malformed, absent, "{bad:?}: nothing else moves");
    }
    assert_eq!(
        Screen {
            title: None,
            ..sent
        },
        absent,
        "the title is the only field it adds"
    );
}

/// `"human_typed_ms"`: a number is the age of the person's last typing and `null` is none,
/// apart from `"human_ms"`; absent, the window reports one stamp, which both read.
#[test]
fn the_persons_typing_is_read_apart_from_their_gestures() {
    let with = |tail: &str| {
        parse_text_json(&format!(
            r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"2.15"{tail}}}"#
        ))
        .expect("parses")
        .typed
    };
    assert_eq!(
        with(r#","human_ms":5,"human_typed_ms":7200000"#),
        HumanInput::Ago(7_200_000)
    );
    assert_eq!(
        with(r#","human_ms":5,"human_typed_ms":null"#),
        HumanInput::Never
    );
    assert_eq!(with(r#","human_ms":5"#), HumanInput::Ago(5));
}

/// `"human_ms"`: a number is its age, `null` is never, and absent or any other value is
/// unknown.
#[test]
fn the_persons_stamp_is_read_and_its_absence_is_unknown() {
    let with = |tail: &str| {
        parse_text_json(&format!(
            r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"2.15"{tail}}}"#
        ))
        .expect("parses")
        .human
    };
    assert_eq!(with(r#","human_ms":12345"#), HumanInput::Ago(12_345));
    assert_eq!(with(r#","human_ms":0,"trimmed":1"#), HumanInput::Ago(0));
    assert_eq!(with(r#","human_ms":null"#), HumanInput::Never);
    assert_eq!(
        with(r#","human_ms":null,"trimmed":1,"unkeyed_ms":5000"#),
        HumanInput::NeverFor(5000),
        "a never that says since when"
    );
    assert_eq!(
        with(r#","human_ms":null,"unkeyed_ms":"5""#),
        HumanInput::Never
    );
    assert_eq!(with(""), HumanInput::Unknown, "an older host");
    assert_eq!(with(r#","human_ms":"12""#), HumanInput::Unknown);
    assert_eq!(with(r#","human_ms":-3"#), HumanInput::Unknown);
}

/// [`HumanInput::quiet_since`] (the review of LC-4, KR-2): an age is
/// quiet when the key came before the instant; a never that says since
/// when is quiet when the host served the session from the instant or
/// before, and cannot say when it began after; a bare never and an
/// unknown cannot say.
#[test]
fn a_never_that_says_since_when_is_quiet_from_before_the_instant() {
    let (since, now) = (1_000_000, 1_600_000);
    assert_eq!(HumanInput::Ago(700_000).quiet_since(since, now), Some(true));
    assert_eq!(
        HumanInput::Ago(500_000).quiet_since(since, now),
        Some(false)
    );
    assert_eq!(
        HumanInput::NeverFor(600_000).quiet_since(since, now),
        Some(true),
        "served from the instant"
    );
    assert_eq!(HumanInput::NeverFor(599_999).quiet_since(since, now), None);
    assert_eq!(HumanInput::Never.quiet_since(since, now), None);
    assert_eq!(HumanInput::Unknown.quiet_since(since, now), None);
    assert_eq!(HumanInput::NeverFor(1).within(u32::MAX), Some(false));
    assert_eq!(HumanInput::NeverFor(1).ago(), None);
}

/// `"submits"` (aterm-gui's `submit_count`, last in the object): a count
/// is read; absent (an older host) and any other value are none.
#[test]
fn the_submission_count_is_read_and_its_absence_is_none() {
    let with = |tail: &str| {
        parse_text_json(&format!(
            r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"2.15","human_ms":null,"unkeyed_ms":5000{tail}}}"#
        ))
        .expect("parses")
        .submits
    };
    assert_eq!(with(r#","submits":7"#), Some(7));
    assert_eq!(with(r#","submits":0"#), Some(0));
    assert_eq!(with(""), None, "an older host");
    assert_eq!(with(r#","submits":"7""#), None);
    assert_eq!(with(r#","submits":-1"#), None);
}

/// `"frame":"open"` (aterm-gui's `text --json`, last in the object, only while the
/// program's DEC 2026 frame is unfinished) is an open frame; absent (a whole frame, or an
/// older host) and any other value are whole.
#[test]
fn an_open_frame_is_read_only_from_the_servers_open() {
    let with = |tail: &str| {
        parse_text_json(&format!(
            r#"{{"rows":["❯"],"cursor":{{"row":0,"col":2}},"dims":{{"rows":1,"cols":2}},"seq":15,"submits":3{tail}}}"#
        ))
        .expect("parses")
        .frame_open
    };
    assert!(with(r#","frame":"open""#));
    assert!(!with(""), "a whole frame, or an older host");
    assert!(!with(r#","frame":"whole""#));
    assert!(!with(r#","frame":true"#));
    assert!(!with(r#","frame":null"#));
}

/// `"inputs"` (aterm-gui's input writes, before and after the rows): both
/// counts are read, a `null` first (a write in flight) is none before and
/// the count after; absent (an older host) and any other shape are none.
#[test]
fn the_input_counts_are_read_and_a_write_in_flight_is_none_before() {
    let with = |tail: &str| {
        let s = parse_text_json(&format!(
            r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"2.15","human_ms":null{tail},"submits":1}}"#
        ))
        .expect("parses");
        (s.inputs_before, s.inputs_after)
    };
    assert_eq!(with(r#","inputs":[3,4]"#), (Some(3), Some(4)));
    assert_eq!(with(r#","inputs":[null,4]"#), (None, Some(4)));
    assert_eq!(with(""), (None, None), "an older host");
    assert_eq!(with(r#","inputs":[3]"#), (None, None));
    assert_eq!(with(r#","inputs":[3,4,5]"#), (None, None));
    assert_eq!(with(r#","inputs":7"#), (None, None));
    assert_eq!(with(r#","inputs":["3",-1]"#), (None, None));
}

/// `"launch"` (the server's launch witness, in the shape aterm-gui's
/// `launch_witness` writes it): a `person` witness is read whole; every
/// other word, `null`, a missing field and an older host that sends none
/// are no witness.
#[test]
fn only_a_whole_person_launch_witness_is_read() {
    let with = |tail: &str| {
        parse_text_json(&format!(
            r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"2.15","human_ms":900,"human_seq":4{tail}}}"#
        ))
        .expect("parses")
        .launch
    };
    assert_eq!(
        with(r#","launch":{"id":3,"seq":4,"word":"claude","why":"person"},"trimmed":1"#),
        Some(LaunchWitness {
            id: 3,
            seq: 4,
            word: "claude".to_string()
        })
    );
    for other in [
        r#","launch":{"why":"since"}"#,
        r#","launch":{"why":"typed"}"#,
        r#","launch":{"id":3,"seq":4,"word":"claude","why":"pending"}"#,
        r#","launch":{"id":3,"word":"claude","why":"person"}"#,
        r#","launch":{"id":3,"seq":"4","word":"claude","why":"person"}"#,
        r#","launch":null"#,
        "",
    ] {
        assert_eq!(with(other), None, "{other}");
    }
}

/// A `tail=` read's `first` maps the cursor's grid row onto the rows read; a full read
/// starts at 0, and a cursor above a tail is on none.
#[test]
fn a_tail_reads_first_row_places_the_cursor() {
    let tail = r#"{"rows":["a","❯ hi","b"],"cursor":{"row":61,"col":4},"dims":{"rows":63,"cols":80},"seq":9,"first":60}"#;
    let s = parse_text_json(tail).expect("parses");
    assert_eq!((s.first, s.cursor_row), (60, 61));
    assert_eq!(s.cursor_index(), Some(1));
    let full =
        r#"{"rows":["a","❯ hi"],"cursor":{"row":1,"col":4},"dims":{"rows":2,"cols":80},"seq":9}"#;
    let s = parse_text_json(full).expect("parses");
    assert_eq!((s.first, s.cursor_index()), (0, Some(1)));
    let above = Screen {
        rows: vec!["a".to_string()],
        cursor_row: 3,
        first: 10,
        ..Screen::default()
    };
    assert_eq!(above.cursor_index(), None);
}

#[test]
fn rejects_what_is_not_a_screen() {
    assert!(parse_text_json("ERR usage: text").is_err());
    assert!(parse_text_json(r#"{"rows":"no"}"#).is_err());
    assert!(parse_text_json(r#"{"rows":[1]}"#).is_err());
    assert!(parse_text_json(r#"{"rows":[]} x"#).is_err());
    assert!(parse_text_json("").is_err());
}

#[test]
fn json_reader_handles_the_value_vocabulary() {
    let v = Json::parse(r#" {"a":[true,false,null,-1.5e3,"x"],"b":{}} "#).expect("parses");
    let a = v.get("a").and_then(Json::as_array).expect("array");
    assert_eq!(a[0], Json::Bool(true));
    assert_eq!(a[2], Json::Null);
    assert_eq!(a[3], Json::Number("-1.5e3".to_string()));
    assert_eq!(a[4].as_str(), Some("x"));
    assert_eq!(v.get("b"), Some(&Json::Object(vec![])));
    assert_eq!(
        Json::parse(r#""\uD83D\uDE00 \uD800\u0041 \uD800\uD83D\uDE00 \uDE00""#)
            .expect("surrogate escapes")
            .as_str(),
        Some("😀 \u{FFFD}A \u{FFFD}😀 \u{FFFD}")
    );
    assert!(Json::parse("[1,]").is_err());
    assert!(Json::parse("{\"a\" 1}").is_err());
    assert!(Json::parse("tru").is_err());
    assert!(Json::parse("\"\\q\"").is_err());
    assert!(Json::parse(r#""\u+041""#).is_err());
}

/// [`cell_fg_is`] reads a well-formed cell's colour; a cell of an unfinished frame is no evidence.
#[test]
fn a_cell_reply_names_its_colour_only_off_a_whole_frame() {
    assert!(cell_fg_is("OK x 8A8F99 111318 none", "8a8f99"));
    assert!(!cell_fg_is("OK x d0d0d0 111318 none", "8a8f99"));
    assert!(!cell_fg_is("OK x 8a8f99 111318 none frame=open", "8a8f99"));
    assert!(!cell_fg_is("ERR out of range", "8a8f99"));
}

/// [`past_drawn_caret`] over the `cell` replies of Claude Code 2.1.310's composer, read in the
/// real-Claude lab with the suggestion `keep going` drawn: its caret cell sends the read one
/// glyph on, two past a wide glyph. NEGATIVE CONTROLS: the dim glyphs after it, a plain one,
/// an unfinished frame's caret and a refusal send it nowhere.
#[test]
fn a_drawn_caret_sends_the_placeholder_read_to_the_glyph_after_it() {
    assert_eq!(past_drawn_caret("OK k 111318 d0d0d0 inverse", 2), Some(3));
    assert_eq!(
        past_drawn_caret("OK %E4%BD%A0 111318 d0d0d0 inverse,wide", 2),
        Some(4)
    );
    assert_eq!(
        past_drawn_caret("OK k 111318 d0d0d0 bold,inverse link=https%3A%2F%2Fa", 5),
        Some(6)
    );
    for not_caret in [
        "OK e 999999 111318 dim",
        "OK %20 999999 111318 dim",
        "OK k d0d0d0 111318 none",
        "OK k 111318 d0d0d0 inverse frame=open",
        "OK k 111318 d0d0d0",
        "ERR out of range",
    ] {
        assert_eq!(past_drawn_caret(not_caret, 2), None, "{not_caret}");
    }
    assert_eq!(
        cell_is_placeholder("OK e 999999 111318 dim", None),
        Some(true)
    );
    assert_eq!(
        cell_is_placeholder("OK k 111318 d0d0d0 inverse", None),
        Some(false),
        "the caret's own cell reads as typed text"
    );
}

/// [`composer_glyph_cell`] reads column 2, and past a drawn caret the column after it, and
/// answers that cell. NEGATIVE CONTROLS: a plain first glyph is read alone, and a caret whose
/// next cell does not answer gives nothing (never the caret's own cell, a draft's look).
#[test]
fn the_composers_glyph_is_read_past_a_drawn_caret() {
    let cells = |row: &'static [(usize, &'static str)]| {
        let mut asked = Vec::new();
        let got = composer_glyph_cell(|col| {
            asked.push(col);
            row.iter()
                .find(|(c, _)| *c == col)
                .map(|(_, line)| (*line).to_string())
        });
        (got, asked)
    };
    assert_eq!(
        cells(&[
            (2, "OK k 111318 d0d0d0 inverse"),
            (3, "OK e 999999 111318 dim")
        ]),
        (Some("OK e 999999 111318 dim".to_string()), vec![2, 3])
    );
    assert_eq!(
        cells(&[
            (2, "OK %E4%BD%A0 111318 d0d0d0 inverse,wide"),
            (4, "OK e 999999 111318 dim")
        ]),
        (Some("OK e 999999 111318 dim".to_string()), vec![2, 4])
    );
    assert_eq!(
        cells(&[
            (2, "OK k d0d0d0 111318 none"),
            (3, "OK e 999999 111318 dim")
        ]),
        (Some("OK k d0d0d0 111318 none".to_string()), vec![2])
    );
    assert_eq!(
        cells(&[(2, "OK k 111318 d0d0d0 inverse")]),
        (None, vec![2, 3])
    );
    assert_eq!(cells(&[]), (None, vec![2]));
}
