// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Overview "On the fabric" group, brief parsing, and polling lifecycle.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::WindowId;
use crate::app_introspect::AuxTarget;
use crate::overview::{OverviewAct, OverviewState};

const CAPTURED: &str = concat!(
    r#"{"v":2,"fleet":"f1","head":1000,"from":0,"scanned":0,"pages":0,"partial":0,"#,
    r#""opaque":0,"foreign":0,"dup":0,"reordered":0,"budget":2000,"used":243,"more":0,"#,
    r#""sessions":["#,
    r#"{"node":"n-2","sid":"s-mine","this":true,"ep":1,"need":"failure","detail":null,"#,
    r#""open":[],"mail_in":0,"mail_out":0,"last_seq":3,"age_s":19,"partial":[]},"#,
    r#"{"node":"n-1","sid":"s-quiet","this":false,"ep":1,"need":null,"detail":null,"#,
    r#""open":[],"mail_in":0,"mail_out":0,"last_seq":1,"age_s":30,"partial":[]},"#,
    r#"{"node":"n-1","sid":"s-appr","this":false,"ep":1,"need":"approval:bash","detail":null,"#,
    r#""open":[{"level":"run","kind":"agent"},{"level":"turn","kind":"turn"},"#,
    r#"{"level":"step","kind":"approval"}],"mail_in":0,"mail_out":0,"last_seq":4,"age_s":59,"#,
    r#""partial":[]},"#,
    r#"{"node":"n-3","sid":"s-fail","this":false,"ep":1,"need":"failure","detail":null,"#,
    r#""open":[],"mail_in":0,"mail_out":0,"last_seq":3,"age_s":49,"partial":[]},"#,
    r#"{"node":"n-3","sid":"s-old","this":false,"ep":1,"need":"approval:edit","detail":null,"#,
    r#""open":[{"level":"run","kind":"agent"},{"level":"turn","kind":"turn"},"#,
    r#"{"level":"step","kind":"approval"}],"mail_in":0,"mail_out":0,"last_seq":4,"age_s":399,"#,
    r#""partial":[]}]}"#,
);

const READ_AT: u64 = 1_760_000_400_000;

fn captured_with_old_gone() -> String {
    let gone = CAPTURED.replace(
        r#""age_s":399,"partial":[]"#,
        r#""age_s":399,"partial":["node-gone"]"#,
    );
    assert_ne!(gone, CAPTURED);
    gone
}

fn need(node: &str, sid: &str, word: &str, detail: Option<&str>, age_s: Option<u64>) -> FabricNeed {
    FabricNeed {
        node: node.to_string(),
        sid: sid.to_string(),
        need: word.to_string(),
        detail: detail.map(str::to_string),
        age_s,
        gone: false,
    }
}

fn group(chips: Vec<FabricChip>) -> crate::overview::FabricGroup {
    crate::overview::FabricGroup {
        chips,
        failed: None,
    }
}

fn chip(
    node: &str,
    sid: &str,
    mark: ChipMark,
    word: &str,
    words: &str,
    age_s: Option<u64>,
    older: bool,
) -> FabricChip {
    FabricChip {
        node: node.to_string(),
        sid: sid.to_string(),
        mark,
        need: word.to_string(),
        words: words.to_string(),
        age_s,
        older,
    }
}

#[test]
fn the_read_keeps_each_remote_need_and_drops_this_nodes() {
    let snap = parse(CAPTURED, READ_AT).expect("the brief's object");
    assert_eq!(snap.read_at_ms, READ_AT);
    assert_eq!(
        snap.needs,
        vec![
            need("n-1", "s-appr", "approval:bash", None, Some(59)),
            need("n-3", "s-fail", "failure", None, Some(49)),
            need("n-3", "s-old", "approval:edit", None, Some(399)),
        ]
    );
    let gone = parse(&captured_with_old_gone(), READ_AT).expect("the brief's object");
    assert_eq!(
        gone.needs
            .iter()
            .map(|n| (n.sid.as_str(), n.gone))
            .collect::<Vec<_>>(),
        [("s-appr", false), ("s-fail", false), ("s-old", true)]
    );
    let spaced = CAPTURED.replace(
        r#""node":"n-1","sid":"s-appr""#,
        r#""node":"n 1","sid":"s-appr""#,
    );
    assert_ne!(spaced, CAPTURED);
    let snap = parse(&spaced, READ_AT).expect("the brief's object");
    assert!(
        snap.needs.iter().all(|n| n.sid != "s-appr"),
        "{:?}",
        snap.needs
    );
    assert_eq!(snap.needs.len(), 2);
    assert_eq!(
        parse("ERR fabric no bridge", READ_AT),
        Err(ReadFail::NoBrief)
    );
    assert_eq!(parse(r#"{"v":2}"#, READ_AT), Err(ReadFail::NoBrief));
}

/// [`CAPTURED`] with `s-fail`'s need and detail respelled (`detail` a JSON
/// string body, escapes and all, or `null`).
fn captured_with_fail_need(need: &str, detail: &str) -> String {
    let respelled = CAPTURED.replace(
        r#""sid":"s-fail","this":false,"ep":1,"need":"failure","detail":null"#,
        &format!(r#""sid":"s-fail","this":false,"ep":1,"need":"{need}","detail":{detail}"#),
    );
    assert_ne!(respelled, CAPTURED);
    respelled
}

/// A WALL'S RESET READS AS THE BRIEF SAYS IT (PLAN1-1): the brief's `detail`
/// keeps its spaces from parse to the chip's words, so a usage wall's chip says
/// when the session carries on (`limit · in 1m`), as its own tab's clause
/// does. A detail holding a control character, or only blanks, is dropped and
/// the chip says the bare `limit`; a long one is clipped as a word is.
#[test]
fn a_walls_reset_keeps_its_spaces_from_the_brief_to_the_chip() {
    let rows = [
        (r#""in 1m""#, "limit \u{b7} in 1m"),
        (r#""9am tomorrow""#, "limit \u{b7} 9am tomorrow"),
        (
            r#""3pm (America/Los_Angeles)""#,
            "limit \u{b7} 3pm (America/Los_Angeles)",
        ),
        (r#""in  1m""#, "limit \u{b7} in 1m"),
        (r#""in\u001b[31m 1m""#, "limit"),
        (r#""in 1m\n""#, "limit"),
        (r#""   ""#, "limit"),
        ("null", "limit"),
    ];
    let mut failed = Vec::new();
    for (detail, want) in rows {
        let body = captured_with_fail_need("wall:usage-weekly", detail);
        let snap = parse(&body, READ_AT).expect("the brief's object");
        let words = compose(&snap, READ_AT)
            .into_iter()
            .find(|c| c.sid == "s-fail")
            .map(|c| c.words);
        if words.as_deref() != Some(want) {
            failed.push(format!("detail {detail}: {words:?}, want {want:?}"));
        }
    }
    let long = format!("\"in {}\"", "1".repeat(200));
    let snap = parse(
        &captured_with_fail_need("wall:usage-weekly", &long),
        READ_AT,
    )
    .expect("the brief's object");
    let clipped = snap
        .needs
        .iter()
        .find(|n| n.sid == "s-fail")
        .and_then(|n| n.detail.clone());
    if !clipped
        .as_deref()
        .is_some_and(|d| d.len() <= 64 && d.starts_with("in 1") && d.ends_with('\u{2026}'))
    {
        failed.push(format!("a long detail: {clipped:?}"));
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// A SESSION THE BRIEF JUDGES GONE IS OLDER AND UNCOUNTED (PLAN1-2): for every
/// reason the brief gives a session gone without its close
/// ([`aterm_slog::bus::Gone`], `node-gone` and `no-close`, alone or after a
/// seed's word), its chip is listed `older=1` and the title's count leaves it
/// out. A partial fold for a seed's reason alone (`no-start`, `lost`) is still
/// answerable there, so it stays fresh and counted.
#[test]
fn every_reason_the_brief_gives_a_session_gone_lists_it_older_and_uncounted() {
    let appr = |partial: &str| {
        let body = CAPTURED.replace(
            r#""age_s":59,"partial":[]"#,
            &format!(r#""age_s":59,"partial":{partial}"#),
        );
        assert!(partial == "[]" || body != CAPTURED, "{partial}");
        body
    };
    let mut rows: Vec<(String, bool)> = vec![
        (r#"["no-close"]"#.to_string(), true),
        (r#"["no-start","no-close"]"#.to_string(), true),
        (r#"["node-gone"]"#.to_string(), true),
        (r#"["no-start"]"#.to_string(), false),
        (r#"["lost"]"#.to_string(), false),
        ("[]".to_string(), false),
    ];
    rows.extend(
        aterm_slog::bus::Gone::ALL
            .iter()
            .map(|g| (format!(r#"["{}"]"#, g.word()), true)),
    );
    let mut failed = Vec::new();
    for (partial, gone) in rows {
        let snap = parse(&appr(&partial), READ_AT).expect("the brief's object");
        let chips = compose(&snap, READ_AT + 30_000);
        let older = chips.iter().find(|c| c.sid == "s-appr").map(|c| c.older);
        let mut card = OverviewState::new(WindowId(0), Vec::new(), Vec::new());
        card.set_fabric(group(chips));
        let counted = if gone { 2 } else { 3 };
        let title = card.title();
        let line = card
            .controls_lines()
            .into_iter()
            .find(|l| l.contains(" sid=s-appr "))
            .unwrap_or_default();
        if older != Some(gone)
            || !title.ends_with(&format!(" \u{b7} {counted} need you"))
            || !line.contains(&format!(" older={} ", u8::from(gone)))
        {
            failed.push(format!(
                "partial {partial}: older {older:?}, {title:?}, {line:?}"
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

type Want<'a> = Option<(ChipMark, &'a str, Option<u64>, bool)>;

type Row<'a> = (&'a str, Option<&'a str>, Option<u64>, bool, u64, Want<'a>);

#[test]
fn the_group_composes_each_need_in_the_clauses_words() {
    let snap = parse(CAPTURED, READ_AT).expect("the brief's object");
    assert_eq!(
        compose(&snap, READ_AT + 30_000),
        vec![
            chip(
                "n-1",
                "s-appr",
                ChipMark::Decide,
                "approval:bash",
                "approval \u{b7} bash",
                Some(89),
                false
            ),
            chip(
                "n-3",
                "s-fail",
                ChipMark::Stuck,
                "failure",
                "failed",
                Some(79),
                false
            ),
            chip(
                "n-3",
                "s-old",
                ChipMark::Decide,
                "approval:edit",
                "approval \u{b7} edit",
                Some(429),
                false
            ),
        ]
    );
    let rows: [Row<'_>; 11] = [
        (
            "question",
            None,
            Some(0),
            false,
            0,
            Some((ChipMark::Decide, "question", Some(0), false)),
        ),
        (
            "failure",
            Some("2"),
            Some(300),
            false,
            0,
            Some((ChipMark::Stuck, "failed \u{b7} exit 2", Some(300), false)),
        ),
        (
            "approval:edit",
            None,
            Some(399),
            false,
            0,
            Some((ChipMark::Decide, "approval \u{b7} edit", Some(399), false)),
        ),
        (
            "wall:auth",
            None,
            Some(3600),
            false,
            0,
            Some((ChipMark::Stuck, "logged out", Some(3600), false)),
        ),
        (
            "wall:usage-weekly",
            Some("3pm"),
            Some(5),
            false,
            0,
            Some((ChipMark::Stuck, "limit \u{b7} 3pm", Some(5), false)),
        ),
        (
            "wall:context",
            None,
            Some(5),
            false,
            0,
            Some((ChipMark::Stuck, "wall \u{b7} context", Some(5), false)),
        ),
        (
            "unresponsive",
            None,
            None,
            false,
            0,
            Some((ChipMark::Stuck, "not responding", None, false)),
        ),
        (
            "approval:bash",
            None,
            Some(10),
            true,
            0,
            Some((ChipMark::Decide, "approval \u{b7} bash", Some(10), true)),
        ),
        (
            "question",
            None,
            Some(10),
            false,
            300_000,
            Some((ChipMark::Decide, "question", Some(310), false)),
        ),
        (
            "question",
            None,
            Some(10),
            false,
            300_001,
            Some((ChipMark::Decide, "question", Some(310), true)),
        ),
        ("busy", None, Some(1), false, 0, None),
    ];
    for (word, detail, age, gone, read_ago, want) in rows {
        let snap = FabricSnapshot {
            read_at_ms: READ_AT,
            needs: vec![FabricNeed {
                gone,
                ..need("n-9", "s-row", word, detail, age)
            }],
        };
        let want: Vec<FabricChip> = want
            .into_iter()
            .map(|(mark, words, age_s, older)| {
                chip("n-9", "s-row", mark, word, words, age_s, older)
            })
            .collect();
        assert_eq!(
            compose(&snap, READ_AT + read_ago),
            want,
            "{word} gone={gone} read {read_ago} ms ago"
        );
    }
}

#[test]
fn a_future_fabric_read_keeps_its_needs_without_claiming_their_age_or_count() {
    let snap = parse(CAPTURED, READ_AT).expect("the brief's object");
    let chips = compose(&snap, READ_AT - 1);
    assert_eq!(chips.len(), 3, "the historical needs remain visible");
    assert!(
        chips.iter().all(|c| c.older && c.age_s.is_none()),
        "{chips:?}"
    );
    let mut card = OverviewState::new(WindowId(0), Vec::new(), Vec::new());
    assert!(card.set_fabric(group(chips)));
    assert!(!card.title().contains("need you"), "{}", card.title());
    assert!(compose(&snap, READ_AT).iter().all(|c| !c.older));
}

#[test]
fn the_card_lists_the_group_counts_the_fresh_and_acts_on_none() {
    let snap = parse(&captured_with_old_gone(), READ_AT).expect("the brief's object");
    let chips = compose(&snap, READ_AT + 30_000);
    let mut card = OverviewState::new(WindowId(0), Vec::new(), Vec::new());
    assert!(card.set_fabric(group(chips.clone())), "the group arrived");
    assert!(
        !card.set_fabric(group(chips.clone())),
        "the same read moves nothing"
    );
    assert!(
        card.title().ends_with(" \u{b7} 2 need you"),
        "{}",
        card.title()
    );
    let lines = card.controls_lines();
    assert!(lines[0].ends_with(" fabric=3 fabric_error=-"), "{lines:?}");
    assert_eq!(lines[1], r#"overview group label="On the fabric""#);
    assert_eq!(
        lines[2],
        "overview fabric chip node=n-1 sid=s-appr need=approval:bash age=89 mark=decide \
         older=0 selected=true"
    );
    assert!(
        lines[4].contains(" sid=s-old ") && lines[4].contains(" older=1 "),
        "{lines:?}"
    );

    card.move_selection(1);
    assert_eq!(
        card.selected_fabric().map(|c| c.sid.as_str()),
        Some("s-fail")
    );
    for act in [
        card.activate(),
        card.take_over_pressed(),
        card.read_result_pressed(),
        card.stop_pressed(false),
        card.stop_pressed(false),
    ] {
        assert_eq!(act, OverviewAct::None);
    }
    assert!(
        card.controls_lines()[0].contains(" confirm=- "),
        "no confirm armed"
    );

    assert!(card.set_fabric(group(chips[1..].to_vec())));
    assert_eq!(
        card.selected_fabric().map(|c| c.sid.as_str()),
        Some("s-fail")
    );
    assert!(
        card.title().ends_with(" \u{b7} 1 need you"),
        "{}",
        card.title()
    );
    assert!(crate::overview::fabric_hint_of("n-3").contains("on n-3, answered there"));
}

#[test]
fn a_seconds_age_repaints_nothing_until_its_words_move() {
    let snap = FabricSnapshot {
        read_at_ms: READ_AT,
        needs: vec![need("n-1", "s-appr", "approval:bash", None, Some(100))],
    };
    let mut card = OverviewState::new(WindowId(0), Vec::new(), Vec::new());
    assert!(card.set_fabric(group(compose(&snap, READ_AT))));
    let painted = card.fingerprint();
    assert!(!card.set_fabric(group(compose(&snap, READ_AT + 1_000))));
    assert_eq!(card.fingerprint(), painted, "nothing to repaint");
    let lines = card.controls_lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains(" sid=s-appr ") && l.contains(" age=101 ")),
        "{lines:?}"
    );
    assert!(
        card.set_fabric(group(compose(&snap, READ_AT + 20_000))),
        "1m to 2m"
    );
    assert_ne!(card.fingerprint(), painted);
}

struct Answers(Mutex<VecDeque<ReadAnswer>>);

impl Answers {
    fn of(bodies: &[&str]) -> Arc<dyn FabricRunner> {
        Arc::new(Self(Mutex::new(
            bodies.iter().map(|b| Ok((*b).to_string())).collect(),
        )))
    }
}

impl FabricRunner for Answers {
    fn run(&self) -> ReadAnswer {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(ReadFail::Refused))
    }
}

struct Held {
    go: Mutex<Receiver<()>>,
    body: String,
}

impl FabricRunner for Held {
    fn run(&self) -> ReadAnswer {
        let _ = self.go.lock().unwrap().recv();
        Ok(self.body.clone())
    }
}

fn await_read(app: &crate::App, n: u64) {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the fabric read", || {
        let slot = app.agents.overview_fabric.copy();
        (slot.reads >= n && !slot.reading).then_some(())
    });
}

fn title(app: &crate::App) -> String {
    app.windows[&WindowId(0)]
        .overview()
        .map(OverviewState::title)
        .expect("open")
}

fn fabric_app(runner: Arc<dyn FabricRunner>, clock_ms: u64) -> crate::App {
    let mut app = crate::App::headless_for_test();
    app.agents.overview_fabric.configured = true;
    app.agents.overview_fabric.runner = Some(runner);
    app.agents.overview_fabric.clock_ms = Some(clock_ms);
    app
}

#[test]
fn a_read_lands_on_the_open_card_and_a_closed_need_leaves_it() {
    let closed = CAPTURED.replace(r#""need":"approval:bash""#, r#""need":null"#);
    assert_ne!(closed, CAPTURED);
    let mut app = fabric_app(Answers::of(&[CAPTURED, &closed]), READ_AT);
    assert!(app.open_overview().is_ok());
    assert_eq!(
        app.agents.overview_fabric.started, 1,
        "the open kicked a read"
    );
    await_read(&app, 1);
    app.on_fabric_read();
    let lines = app.read_aux_controls(AuxTarget::Overview);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("overview fabric chip node=n-1 sid=s-appr need=approval:bash ")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains(" sid=s-fail ")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("s-mine")),
        "this node's own: {lines:?}"
    );
    assert!(
        title(&app).contains(" \u{b7} 3 need you"),
        "{}",
        title(&app)
    );

    app.kick_fabric_read();
    assert_eq!(app.agents.overview_fabric.started, 2);
    await_read(&app, 2);
    app.on_fabric_read();
    let lines = app.read_aux_controls(AuxTarget::Overview);
    assert!(
        !lines.iter().any(|l| l.contains(" sid=s-appr ")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains(" sid=s-fail ")),
        "{lines:?}"
    );
    assert!(
        title(&app).contains(" \u{b7} 2 need you"),
        "{}",
        title(&app)
    );
}

#[test]
fn an_open_card_reads_the_fabric_again_a_minute_on_with_no_paint() {
    let mut app = fabric_app(Answers::of(&[CAPTURED, CAPTURED]), READ_AT);
    assert_eq!(app.overview_fabric_wake(), None, "no read was kicked");
    assert!(app.open_overview().is_ok());
    await_read(&app, 1);
    app.on_fabric_read();
    let began = app
        .agents
        .overview_fabric
        .kicked_at
        .expect("the open's read began");
    let step = crate::app_overview::OVERVIEW_AGE_STEP;
    app.overview_refresh_all();
    assert_eq!(app.overview_fabric_wake(), Some(began + step));
    app.tick_overview_fabric(began + step - Duration::from_secs(1));
    assert_eq!(app.agents.overview_fabric.started, 1, "not a minute on yet");
    app.tick_overview_fabric(began + step);
    assert_eq!(app.agents.overview_fabric.started, 2, "read again");
    let second = app.agents.overview_fabric.kicked_at.expect("began");
    assert!(second >= began);
    await_read(&app, 2);
    assert_eq!(app.overview_fabric_wake(), Some(second + step));

    app.overview_exit(WindowId(0));
    assert_eq!(app.overview_fabric_wake(), None);
    app.tick_overview_fabric(second + step * 2);
    assert_eq!(app.agents.overview_fabric.started, 2);

    assert!(app.open_overview().is_ok());
    await_read(&app, 3);
    app.agents.overview_fabric.configured = false;
    let third = app.agents.overview_fabric.kicked_at.expect("began");
    app.tick_overview_fabric(third + step);
    assert_eq!(app.agents.overview_fabric.started, 3);
    assert_eq!(app.agents.overview_fabric.kicked_at, None);
    assert_eq!(app.overview_fabric_wake(), None);
}

#[test]
fn a_kick_while_a_read_is_out_is_that_reads() {
    let (go, wait) = channel();
    let held = Arc::new(Held {
        go: Mutex::new(wait),
        body: CAPTURED.to_string(),
    });
    let mut app = fabric_app(held, READ_AT);
    assert!(app.open_overview().is_ok());
    assert!(app.agents.overview_fabric.copy().reading, "the read is out");
    app.kick_fabric_read();
    app.kick_fabric_read();
    let began = app.agents.overview_fabric.kicked_at.expect("began");
    assert_eq!(app.overview_fabric_wake(), None, "a read is out");
    app.tick_overview_fabric(began + crate::app_overview::OVERVIEW_AGE_STEP * 2);
    assert_eq!(app.agents.overview_fabric.started, 1, "one read");
    go.send(()).expect("the read waits");
    await_read(&app, 1);
    let slot = app.agents.overview_fabric.copy();
    assert_eq!((slot.reads, slot.error), (1, None));
}

#[test]
fn a_failed_read_keeps_the_last_good_one_and_says_so() {
    struct Panics;
    impl FabricRunner for Panics {
        fn run(&self) -> ReadAnswer {
            panic!("injected fabric reader panic");
        }
    }
    let mut app = fabric_app(Answers::of(&[CAPTURED]), READ_AT);
    assert!(app.open_overview().is_ok());
    await_read(&app, 1);
    app.on_fabric_read();
    app.kick_fabric_read();
    await_read(&app, 2);
    app.on_fabric_read();
    let slot = app.agents.overview_fabric.copy();
    assert_eq!(slot.error, Some(ReadFail::Refused));
    let lines = app.read_aux_controls(AuxTarget::Overview);
    assert!(
        lines[0].ends_with(" fabric=3 fabric_error=refused"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(" sid=s-appr ") && l.contains(" older=0 ")),
        "the last good read's chips stand: {lines:?}"
    );
    assert!(
        title(&app).contains(" \u{b7} 3 need you"),
        "{}",
        title(&app)
    );
    let header = app.windows[&WindowId(0)]
        .overview()
        .expect("open")
        .line_texts()
        .into_iter()
        .find(|l| l.contains(crate::overview::FABRIC_GROUP))
        .expect("the group's header");
    assert!(header.ends_with("the last read failed"), "{header}");

    app.agents.overview_fabric.clock_ms = Some(READ_AT + 301_000);
    app.overview_refresh_all();
    let lines = app.read_aux_controls(AuxTarget::Overview);
    assert!(
        lines
            .iter()
            .filter(|l| l.starts_with("overview fabric chip "))
            .all(|l| l.contains(" older=1 ")),
        "{lines:?}"
    );
    assert!(!title(&app).contains("need you"), "{}", title(&app));

    let before = app.agents.overview_fabric.copy().snapshot;
    app.agents.overview_fabric.runner = Some(Arc::new(Panics));
    app.kick_fabric_read();
    await_read(&app, 3);
    app.on_fabric_read();
    let slot = app.agents.overview_fabric.copy();
    assert_eq!(slot.error, Some(ReadFail::Run));
    assert_eq!(slot.snapshot, before, "an unwind keeps the last good read");
    assert!(
        app.overview_fabric_wake().is_some(),
        "the card can read again"
    );
    let lines = app.read_aux_controls(AuxTarget::Overview);
    assert!(
        lines[0].ends_with(" fabric=3 fabric_error=run"),
        "{lines:?}"
    );

    let closed = CAPTURED.replace(r#""need":"approval:bash""#, r#""need":null"#);
    assert_ne!(closed, CAPTURED);
    app.agents.overview_fabric.runner = Some(Answers::of(&[&closed]));
    app.kick_fabric_read();
    await_read(&app, 4);
    app.on_fabric_read();
    let slot = app.agents.overview_fabric.copy();
    assert_eq!((slot.reads, slot.error, slot.reading), (4, None, false));
    let lines = app.read_aux_controls(AuxTarget::Overview);
    assert!(lines[0].ends_with(" fabric=2 fabric_error=-"), "{lines:?}");
    assert!(
        !lines.iter().any(|l| l.contains(" sid=s-appr ")),
        "{lines:?}"
    );
    assert!(
        title(&app).contains(" \u{b7} 2 need you"),
        "{}",
        title(&app)
    );
}

/// ⏎ AND EVERY KEY ON A FABRIC CHIP REACH NO PROGRAM (App level, the
/// session's own PTY observed): the card stays open on the chip, and no
/// byte is written to the session.
#[cfg(unix)]
#[test]
fn every_key_on_a_fabric_chip_writes_nothing_anywhere() {
    use std::io::{ErrorKind, Read};
    use std::os::unix::net::UnixStream;

    use aterm_session::sink::SinkWriter;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    use crate::input::InputEvent;

    fn drained(wire: &mut UnixStream) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            match wire.read(&mut buf) {
                Ok(0) => return out,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => return out,
                Err(e) => panic!("reading the observer: {e}"),
            }
        }
    }

    let (mut wire, writer) = UnixStream::pair().expect("an observer");
    wire.set_nonblocking(true).unwrap();
    let sink = Arc::new(SinkWriter::new_owned(writer.into()));
    let mut app = crate::App::headless_for_test_with_sink(sink);
    let wid = WindowId(0);
    app.agents.overview_fabric.configured = true;
    app.agents.overview_fabric.clock_ms = Some(READ_AT);
    app.agents.overview_fabric.slot.lock().unwrap().snapshot =
        Some(Arc::new(parse(CAPTURED, READ_AT).expect("brief")));
    let _ = drained(&mut wire);
    assert!(app.open_overview().is_ok());
    let target = app.windows[&wid]
        .overview()
        .expect("open")
        .controls_lines()
        .iter()
        .filter(|l| l.contains(" chip "))
        .position(|l| l.contains(" sid=s-appr "))
        .expect("the fabric chip is listed");
    app.overview_move(wid, target as isize);
    let key = |key: Key| InputEvent::Key {
        key,
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    for ev in [
        key(Key::Named(NamedKey::Enter)),
        InputEvent::Text("t".to_string()),
        InputEvent::Text("r".to_string()),
        key(Key::Named(NamedKey::Backspace)),
        key(Key::Named(NamedKey::Backspace)),
    ] {
        app.overview_input_event(wid, &ev);
        let card = app.windows[&wid].overview().expect("the card stays open");
        assert_eq!(
            card.selected_fabric().map(|c| c.sid.as_str()),
            Some("s-appr")
        );
    }
    assert!(drained(&mut wire).is_empty(), "no byte reached the session");
    await_read(&app, 1);
}

#[test]
fn with_no_fabric_configured_no_read_is_spawned() {
    let mut app = crate::App::headless_for_test();
    app.agents.overview_fabric.runner = Some(Answers::of(&[CAPTURED]));
    app.agents.overview_fabric.clock_ms = Some(READ_AT);
    assert!(app.open_overview().is_ok());
    app.kick_fabric_read();
    assert_eq!(app.agents.overview_fabric.started, 0);
    assert_eq!(app.overview_fabric_wake(), None);
    let slot = app.agents.overview_fabric.copy();
    assert!(!slot.reading && slot.reads == 0 && slot.snapshot.is_none());
    let lines = app.read_aux_controls(AuxTarget::Overview);
    assert!(lines[0].ends_with(" fabric=0 fabric_error=-"), "{lines:?}");
    assert!(
        !lines.iter().any(|l| l.contains("On the fabric")),
        "{lines:?}"
    );
    app.agents.overview_fabric.slot.lock().unwrap().snapshot =
        Some(Arc::new(parse(CAPTURED, READ_AT).expect("brief")));
    assert_eq!(
        app.overview_fabric_group(),
        crate::overview::FabricGroup::default()
    );
}

#[test]
fn the_read_asks_for_the_whole_machine_form() {
    let read = ChildRunner::brief();
    assert_eq!(
        read.args,
        ["fabric", "brief", "--json", "--budget", "100000"]
    );
    assert!(
        read.program
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("aterm")),
        "{}",
        read.program.display()
    );
}

/// THE REAL READ RUNS A CHILD AND SAYS WHY IN A WORD (`ChildRunner::run`, a
/// stand-in program for `aterm`): what it prints on a clean exit is the
/// answer; a non-zero exit, or nothing printed, is `refused` (no text of
/// its stderr kept); a program that does not start is `run`.
#[cfg(unix)]
#[test]
fn the_real_read_runs_a_child_and_says_why_in_a_word() {
    let sh = |script: &str| {
        ChildRunner {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), script.to_string()],
            cap: READ_CAP,
        }
        .run()
    };
    assert!(!CAPTURED.contains('\''));
    assert_eq!(
        sh(&format!("printf '%s' '{CAPTURED}'")),
        Ok(CAPTURED.to_string())
    );
    assert_eq!(
        sh("echo QZP6SECRET5 >&2; echo half; exit 2"),
        Err(ReadFail::Refused)
    );
    assert_eq!(sh("exit 0"), Err(ReadFail::Refused));
    let none = ChildRunner {
        program: PathBuf::from("/private/tmp/x/no-such-aterm"),
        args: Vec::new(),
        cap: READ_CAP,
    }
    .run();
    assert_eq!(none, Err(ReadFail::Run));
}
