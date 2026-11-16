// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The judges of `lab claude-wall-panel` over kept evidence: the screens are aterm-phase's
//! measured fixtures, the logs and transcript rows Claude Code's and the stub's own lines.

use super::*;

/// A measured fixture's rows (its provenance line dropped).
fn frame(name: &str) -> Vec<String> {
    let path = aterm_tempfile::manifest_dir()
        .join("../aterm-phase/src/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines().skip(1).map(str::to_string).collect()
}

/// A transcript row of the weekly limit Claude Code wrote, at `ts`.
fn wall_row(ts: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","uuid":"u","timestamp":"{ts}","message":{{"id":"m","model":"<synthetic>","role":"assistant","stop_reason":"stop_sequence","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":"You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)"}}]}},"requestId":"req_0","quotaLimits":{{"status":"rejected","resetsAt":1791046800,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","isUsingOverage":false}},"error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429,"version":"2.1.280"}}"#
    )
}

/// A reply the model served after it, which lifts the standing wall.
fn served_row(ts: &str) -> String {
    format!(
        r#"{{"parentUuid":"v","isSidechain":false,"type":"assistant","uuid":"w","timestamp":"{ts}","message":{{"id":"m2","model":"claude-opus-5-5","role":"assistant","stop_reason":"end_turn","type":"message","usage":{{"input_tokens":1,"output_tokens":1}},"content":[{{"type":"text","text":"Done."}}]}},"requestId":"req_1","version":"2.1.280"}}"#
    )
}

/// A person's prompt row.
fn user_row(ts: &str) -> String {
    format!(
        r#"{{"parentUuid":"u","isSidechain":false,"type":"user","uuid":"v","timestamp":"{ts}","message":{{"role":"user","content":"carry on"}},"version":"2.1.280"}}"#
    )
}

const WALL_TS: &str = "2026-10-02T17:00:00.000Z";

/// The debug log's line for one `/usage` read (2.1.308's, as `lab debuglog` shows it).
const FETCH: &str =
    "2026-10-21T06:08:00.000Z [DEBUG] fetchUtilization: GET /api/oauth/usage (attempt 1)";

/// The frames `lab claude-wall-panel` kept of 2.1.325 (2026-11-16): the five-hour wall with
/// Claude Code's own continuation armed, the Usage tab `/usage` opened over it, the wall after
/// one Escape, and the weekly wall under the usage-limit dialog Claude Code opened itself.
const MEASURED: &str = "claude-2.1.325-wall-panel";

/// One of the measured 2.1.325 frames.
fn measured(what: &str) -> Vec<String> {
    frame(&format!("{MEASURED}-{what}.txt"))
}

/// The five-hour wall's evidence as the 2.1.325 run kept it: its three measured screens, as
/// drawn; the debug log's one usage read and an empty stub log (the jail refused the read). The
/// transcript rows are composed: the verb keeps no transcript on a PASS.
fn opened() -> Evidence {
    Evidence {
        before: measured("session-wall"),
        typed: Ok(()),
        panel: Some(measured("session-usage")),
        escaped: true,
        after: measured("session-escaped"),
        transcript_before: wall_row(WALL_TS),
        transcript_after: wall_row(WALL_TS),
        stub_log: String::new(),
        debug_log: FETCH.to_string(),
        unanswered: Vec::new(),
    }
}

/// The weekly wall's evidence as the 2.1.325 run kept it: the dialog Claude Code opened itself,
/// nothing typed into it.
fn week_dialog() -> Evidence {
    let week = measured("week-wall");
    Evidence {
        before: week.clone(),
        typed: Err("the reset program's dialog stands".into()),
        after: week,
        transcript_before: wall_row(WALL_TS),
        transcript_after: wall_row(WALL_TS),
        ..Evidence::default()
    }
}

/// `rows` with the composer's empty row holding `text`.
fn composing(rows: &[String], text: &str) -> Vec<String> {
    let at = rows
        .iter()
        .position(|r| r.trim_end() == "❯")
        .expect("an empty composer row");
    let mut out = rows.to_vec();
    out[at] = format!("❯ {text}");
    out
}

#[test]
fn the_usage_tab_reads_open_and_its_figures_are_read() {
    let p = panel_of(&frame(
        "claude-2.1.308-usage-header-readings-fullscreen.txt",
    ));
    let Panel::Opens { read, windows } = &p else {
        panic!("the measured Usage tab: {p:?}");
    };
    // The lab's usage body is refused by the black hole: the tab says `Could not refresh`.
    assert_eq!(*read, "refused");
    let got: Vec<(&str, u32)> = windows
        .iter()
        .map(|w| (w.name.as_str(), w.used_pct))
        .collect();
    assert_eq!(got, [("five_hour", 42), ("seven_day", 61)]);
    // Negative controls: the panel dismissed, and a wall with no panel, read shut.
    for name in [
        "claude-2.1.308-usage-dismissed-fullscreen.txt",
        "claude-2.1.304-wall-session-auto-continue-armed-statusline-login-words.txt",
    ] {
        assert!(matches!(panel_of(&frame(name)), Panel::Shut(_)), "{name}");
    }
}

#[test]
fn the_reset_programs_dialog_is_seen_and_a_wall_alone_is_none() {
    assert_eq!(
        dialog_of(&frame("claude-2.1.301-limit-dialog-no-reset-rows.txt")),
        Some("dialog")
    );
    assert_eq!(
        dialog_of(&frame(
            "claude-2.1.304-wall-session-auto-continue-armed-statusline-login-words.txt"
        )),
        None
    );
}

#[test]
fn reads_and_turns_are_counted_from_the_logs() {
    let stub = "\
12:00:00.000 POST /v1/messages -> 429 body=10B mode=limit:seven_day:1 x
12:00:01.000 POST /v1/messages/count_tokens -> 200 body=10B mode=ok x
12:00:02.000 GET /api/oauth/usage -> 200 body=0B mode=ok kind=account route=usage read=plain auth=oat served=yes";
    assert_eq!(turns(stub), 1, "count_tokens is no turn");
    assert_eq!(gets("", stub), 1, "a read that reached the stub");
    assert_eq!(gets(FETCH, stub), 1, "the same read in both logs is one");
    assert_eq!(gets(&format!("{FETCH}\n{FETCH}"), ""), 2);
    assert_eq!(gets("[DEBUG] fetchProfile: GET /api/oauth/profile", ""), 0);
    assert_eq!(turns(""), 0);
}

#[test]
fn the_headers_carry_each_figure_and_wall_the_limits_window() {
    let b = blocks(WallAt::Week, 1_000);
    let on = |block: &str, k: &str| {
        b.get(block)
            .and_then(|o| o.get(&format!("anthropic-ratelimit-unified-{k}")))
            .cloned()
    };
    assert_eq!(on("on200", "5h-utilization"), Some(J::s("0.62")));
    assert_eq!(on("on200", "7d-utilization"), Some(J::s("0.71")));
    assert_eq!(on("on200", "7d_oi-utilization"), Some(J::s("0.40")));
    assert_eq!(on("on429", "7d-status"), Some(J::s("rejected")));
    assert_eq!(on("on429", "7d-utilization"), Some(J::s("1.00")));
    assert_eq!(on("on429", "7d-reset"), Some(J::s("@RESET@")));
    assert_eq!(on("on429", "5h-status"), Some(J::s("allowed")));
    assert_eq!(
        blocks(WallAt::Session, 1_000)
            .get("on429")
            .and_then(|o| o.get("anthropic-ratelimit-unified-5h-status"))
            .cloned(),
        Some(J::s("rejected"))
    );
    assert_eq!(WallAt::Week.may_draw("seven_day"), [71, 100]);
    assert_eq!(WallAt::Week.may_draw("five_hour"), [62]);
}

#[test]
fn figures_name_each_window_and_refuse_one_the_stub_never_gave() {
    let w = |name: &str, used_pct| PanelWindow {
        name: name.into(),
        used_pct,
        reset_text: None,
    };
    let both = [w("five_hour", 100), w("seven_day", 71)];
    let (said, wrong) = figures(WallAt::Session, "refused", &both);
    assert_eq!(
        said,
        format!("five_hour:100,seven_day:71,{FABLE_WEEK_WINDOW}:-")
    );
    assert!(wrong.is_empty(), "{wrong:?}");
    // A wrong figure, and a window the wall's headers carry not drawn.
    let (_, wrong) = figures(WallAt::Session, "refused", &[w("seven_day", 70)]);
    assert_eq!(wrong.len(), 2, "{wrong:?}");
    assert!(wrong.iter().any(|f| f.contains("five_hour not drawn")));
    let (_, wrong) = figures(WallAt::Session, "refused", &[]);
    assert_eq!(wrong.len(), 2, "{wrong:?}");
    // A settled read had the account's body: Fable's week must be drawn too.
    let (_, wrong) = figures(WallAt::Session, "settled", &both);
    assert_eq!(
        wrong,
        [format!("{FABLE_WEEK_WINDOW} not drawn on a settled read")]
    );
    let mut all = both.to_vec();
    all.push(w(FABLE_WEEK_WINDOW, 40));
    assert!(figures(WallAt::Session, "settled", &all).1.is_empty());
}

#[test]
fn the_measured_run_passes_its_judges_and_an_unmeasured_tab_does_not() {
    // The 2.1.325 run's evidence, as drawn: both walls through `judge` and `verdict`.
    let session = judge(WallAt::Session, &opened());
    assert!(session.fails.is_empty(), "{:?}", session.fails);
    assert_eq!((session.gets, session.turns), (1, 0));
    assert!(session.standing_wall.is_some());
    assert_eq!(
        session.figures,
        format!("five_hour:100,seven_day:71,{FABLE_WEEK_WINDOW}:-")
    );
    let week = judge(WallAt::Week, &week_dialog());
    assert!(week.fails.is_empty(), "{:?}", week.fails);
    let v = verdict(&[session, week]);
    assert!(v.fails.is_empty(), "{:?}", v.fails);
    assert_eq!(field_wants(&v, "2.1.325").as_deref(), Some("2.1.325"));
    assert_eq!(
        pass_line("2.1.325", &v, Some("2.1.325")),
        "claude-wall-panel: PASS build=2.1.325 panel=opens read=refused gets=1 turns=0 dialog=dialog:untyped field=2.1.325"
    );
    // Negative control: 2.1.308's tab drew 42 and 61 where this run's stub gives 100 and 71.
    let mut e = opened();
    e.panel = Some(frame("claude-2.1.308-usage-header-readings-fullscreen.txt"));
    let j = judge(WallAt::Session, &e);
    assert_eq!(j.fails.len(), 2, "{:?}", j.fails);
    assert!(j.fails.iter().all(|f| f.contains("where the stub gave")));
}

/// One change to a wall's evidence that a judge must refuse.
type Breach = Box<dyn Fn(&mut Evidence)>;

#[test]
fn each_judge_fails_its_own_breach() {
    let base = opened();
    assert!(judge(WallAt::Session, &base).fails.is_empty());
    let refused = aterm_phase::anchor!("usage.could_not_refresh");
    let breaches: Vec<(&str, Breach)> = vec![
        (
            "/v1/messages",
            Box::new(|e| {
                e.stub_log = "t POST /v1/messages -> 429 body=1B mode=x y".into();
            }),
        ),
        (
            "not one",
            Box::new(|e| e.debug_log = format!("{FETCH}\n{FETCH}")),
        ),
        ("not one", Box::new(|e| e.debug_log.clear())),
        (
            "changed the transcript",
            Box::new(|e| {
                e.transcript_after = format!(
                    "{}\n{}",
                    wall_row(WALL_TS),
                    served_row("2026-10-02T17:01:00.000Z")
                );
            }),
        ),
        (
            "names no standing wall",
            Box::new(|e| {
                e.transcript_before = user_row(WALL_TS);
                e.transcript_after = user_row(WALL_TS);
            }),
        ),
        (
            "continuation or dialog changed",
            Box::new(|e| e.after = measured("week-wall")),
        ),
        (
            "continuation or dialog changed",
            Box::new(|e| {
                e.after = frame(
                    "claude-2.1.304-wall-session-auto-continue-cancelled-statusline-login-words.txt",
                );
            }),
        ),
        ("Escape not pressed", Box::new(|e| e.escaped = false)),
        (
            "still open after Escape",
            Box::new(|e| e.after = e.panel.clone().unwrap()),
        ),
        // A tab still loading is no measurement.
        (
            "never ended its load (read unsettled)",
            Box::new(move |e| {
                let rows = e.panel.take().unwrap();
                e.panel = Some(
                    rows.into_iter()
                        .map(|r| r.replace(refused, aterm_phase::anchor!("usage.loading")))
                        .collect(),
                );
            }),
        ),
        // A tab that drew neither window the wall's headers carry.
        (
            "seven_day not drawn",
            Box::new(|e| {
                let rows = e.panel.take().unwrap();
                e.panel = Some(
                    rows.into_iter()
                        .filter(|r| {
                            let t = r.trim();
                            !t.starts_with("Current week")
                                && !t.contains("71% used")
                                && !t.starts_with("Resets Nov")
                        })
                        .collect(),
                );
            }),
        ),
        (
            "refused (exit 1)",
            Box::new(|e| {
                e.unanswered
                    .push("`ctl key enter` refused (exit 1): no such tab".into());
            }),
        ),
        (
            "kept changing",
            Box::new(|e| {
                e.unanswered.push(format!(
                    "the screen kept changing for {GUARD:?} without the Usage tab's final read"
                ));
            }),
        ),
        (
            "left /usage in the composer",
            Box::new(|e| e.after = composing(&e.after, USAGE_COMMAND)),
        ),
    ];
    for (word, breach) in breaches {
        let mut e = base.clone();
        breach(&mut e);
        let j = judge(WallAt::Session, &e);
        assert!(
            j.fails.iter().any(|f| f.contains(word)),
            "{word}: {:?}",
            j.fails
        );
        assert_eq!(field_wants(&verdict(&[j]), "2.1.325"), None, "{word}");
    }
}

#[test]
fn a_dialog_is_never_typed_into_and_a_shut_panel_records_none() {
    let at_dialog = judge(WallAt::Week, &week_dialog());
    assert!(at_dialog.fails.is_empty(), "{:?}", at_dialog.fails);
    // A shut panel: Enter taken, `/usage` gone from the composer, its one usage read sent, and
    // the screen a minute unchanged with no Usage tab.
    let mut shut = opened();
    shut.panel = Some(measured("session-wall"));
    shut.escaped = false;
    shut.after = measured("session-wall");
    let shut_j = judge(WallAt::Session, &shut);
    assert!(shut_j.fails.is_empty(), "{:?}", shut_j.fails);
    let v = verdict(&[shut_j.clone(), at_dialog.clone()]);
    assert_eq!(
        (v.panel, v.read.as_str(), v.dialog.as_str()),
        ("shut", "-", "dialog:untyped")
    );
    assert!(v.fails.is_empty(), "{:?}", v.fails);
    assert_eq!(field_wants(&v, "2.1.325"), None);
    // A shut panel with no usage read is a `/usage` that never ran (a keystroke that never landed,
    // a command queued behind the continuation): no measurement, and the field is not rewritten.
    let mut unran = shut.clone();
    unran.debug_log.clear();
    let j = judge(WallAt::Session, &unran);
    assert!(
        j.fails.iter().any(|f| f.contains("made 0 GET")),
        "{:?}",
        j.fails
    );
    // `/usage` still in the composer under the screen read after Enter: never submitted.
    let mut held = shut.clone();
    held.panel = Some(composing(&measured("session-wall"), USAGE_COMMAND));
    let j = judge(WallAt::Session, &held);
    assert!(
        j.fails
            .iter()
            .any(|f| f.contains("still stood in the composer")),
        "{:?}",
        j.fails
    );
    // A run that typed nowhere measured nothing.
    assert!(!verdict(&[at_dialog]).fails.is_empty());
    // Untyped with no dialog standing is a step that did not happen.
    let lost = Evidence {
        typed: Err("no composer row".into()),
        panel: None,
        escaped: false,
        debug_log: String::new(),
        ..opened()
    };
    assert!(!judge(WallAt::Week, &lost).fails.is_empty());
}

#[test]
fn the_panel_opens_only_where_it_opened_at_every_wall_typed_at() {
    let open = judge(WallAt::Session, &opened());
    let mut shut = opened();
    shut.panel = Some(measured("session-wall"));
    shut.escaped = false;
    shut.after = measured("session-wall");
    let shut = judge(WallAt::Session, &shut);
    for walls in [
        vec![open.clone(), shut.clone()],
        vec![shut.clone(), open.clone()],
    ] {
        let v = verdict(&walls);
        assert_eq!((v.panel, v.read.as_str()), ("shut", "refused"), "{walls:?}");
        assert_eq!(field_wants(&v, "2.1.325"), None);
    }
    let v = verdict(&[open.clone(), open]);
    assert_eq!(
        (v.panel, v.read.as_str(), v.gets.as_str()),
        ("opens", "refused", "1")
    );
}

#[test]
fn the_field_is_read_and_written_in_the_connector_alone() {
    let path = aterm_tempfile::manifest_dir().join("../aterm-agent/src/ai/connector.rs");
    let source = std::fs::read_to_string(&path).expect("the connector");
    let held = field_in(&source).expect("the field");
    assert_eq!(
        held.as_deref(),
        aterm_agent::ai::connector::CLAUDE_ACCOUNTS.wall_panel_measured_on,
        "the checkout's field is the one this build compiled"
    );
    let set = record_field(&source, Some("2.1.999")).expect("written");
    assert_eq!(field_in(&set).unwrap().as_deref(), Some("2.1.999"));
    let diff: Vec<(&str, &str)> = source
        .lines()
        .zip(set.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(diff.len(), usize::from(held.as_deref() != Some("2.1.999")));
    assert_eq!(source.lines().count(), set.lines().count());
    let back = record_field(&set, held.as_deref()).expect("back");
    assert_eq!(back, source, "every other byte as it was");
    assert!(record_field(&source, Some("2.1.x\"); evil(")).is_err());
    assert!(
        field_in("pub const OTHER: Accounts = Accounts {\n    wall_panel_measured_on: None,\n};\n")
            .is_err()
    );
    assert!(
        field_in(&format!(
            "{CLAUDE_CONST}\n    wall_panel_measured_on: Some(\"x\"),\n}};\n"
        ))
        .is_err()
    );
    // Another constant's field is never written.
    let two = format!(
        "pub const OTHER: Accounts = Accounts {{\n    wall_panel_measured_on: None,\n}};\n{CLAUDE_CONST}\n    wall_panel_measured_on: None,\n}};\n"
    );
    let set = record_field(&two, Some("2.1.1")).unwrap();
    assert!(
        set.starts_with(
            "pub const OTHER: Accounts = Accounts {\n    wall_panel_measured_on: None,"
        )
    );
    assert_eq!(field_in(&set).unwrap().as_deref(), Some("2.1.1"));
}

#[test]
fn a_fixture_names_its_build_and_spells_no_random_folder() {
    let rows = vec![
        " ▝▝   ▝▝   /private/tmp/at.Xk2Q/proj".to_string(),
        "cd /tmp/at.Xk2Q/proj".to_string(),
        "▎ logging to /private/tmp/.ubd/at.lab-501/w-1/runs/cwp9/scratch/claude-debug.log"
            .to_string(),
        String::new(),
        "  ".to_string(),
    ];
    let text = scrub(
        &fixture("2.1.325", "2026-11-16", "a wall", &rows),
        &[
            ("/tmp/.ubd/at.lab-501/w-1/runs/cwp9", RUN_SPELLED),
            ("/tmp/at.Xk2Q", PROJ_SPELLED),
        ],
    );
    let mut lines = text.lines();
    let head = lines.next().unwrap();
    assert!(
        head.starts_with("# claude-code 2.1.325 \u{b7} MEASURED 2026-11-16"),
        "{head}"
    );
    assert!(
        !text.contains("at.Xk2Q") && !text.contains("cwp9"),
        "{text}"
    );
    assert_eq!(
        lines.collect::<Vec<_>>(),
        [
            " ▝▝   ▝▝   /private/tmp/at.lab0/proj",
            "cd /private/tmp/at.lab0/proj",
            "▎ logging to /private/tmp/at.lab0/run/scratch/claude-debug.log",
        ]
    );
    assert_eq!(scrub("x", &[("", PROJ_SPELLED)]), "x");
}

#[test]
fn the_measured_frames_read_as_the_run_judged_them() {
    let (wall, usage, escaped, week) = (
        measured("session-wall"),
        measured("session-usage"),
        measured("session-escaped"),
        measured("week-wall"),
    );
    let Panel::Opens { read, windows } = panel_of(&usage) else {
        panic!("the Usage tab over the five-hour wall");
    };
    assert_eq!(read, "refused", "the lab's usage body never arrives");
    assert!(final_panel(&usage));
    assert!(!final_panel(&wall));
    // A screen comparison reads the panel and the wall, not a redrawn figure.
    assert_eq!(reading(&wall), reading(&escaped));
    assert_ne!(reading(&wall), reading(&usage));
    let ticked: Vec<String> = usage
        .iter()
        .map(|r| r.replace("Total duration (wall): 16s", "Total duration (wall): 17s"))
        .collect();
    assert_ne!(ticked, usage);
    assert_eq!(reading(&ticked), reading(&usage));
    let (said, wrong) = figures(WallAt::Session, read, &windows);
    assert_eq!(
        said,
        format!("five_hour:100,seven_day:71,{FABLE_WEEK_WINDOW}:-")
    );
    assert!(wrong.is_empty(), "{wrong:?}");
    for shut in [&wall, &escaped, &week] {
        assert!(matches!(panel_of(shut), Panel::Shut(_)));
    }
    assert_eq!(dialog_of(&wall), None);
    assert_eq!(dialog_of(&week), Some("dialog"));
    // Escape left the wall and the armed continuation as they were; the weekly wall's stopped
    // continuation and dialog read otherwise (the negative control).
    assert_eq!(standing(&wall), standing(&escaped));
    assert!(
        standing(&wall).contains("Continuing automatically"),
        "{}",
        standing(&wall)
    );
    assert_ne!(standing(&wall), standing(&week));
}
