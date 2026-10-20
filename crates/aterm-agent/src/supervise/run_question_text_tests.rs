// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The question-policy texts (`long-term`, `short-term`) in the loop: the real `Session` over a
// scripted server answers Claude Code 2.1.283's question dialog by writing the policy text into
// its free-text row, reading it back, and pressing Enter on it, every key and the `send` fenced.
// The screens are the 2.1.283 captures with this build's texts (`fixtures::rewrap_free_text`).
// Included into `run_tests.rs`'s `mod engine::text`, whose helpers and Tier-1 bind these use.

use crate::supervise::policy::question::{LONG_TERM_TEXT, SHORT_TERM_TEXT};
use crate::supervise::policy::{QuestionPriority, RULE_ANSWER_LONG_TERM, RULE_ANSWER_SHORT_TERM};

const LONG: QuestionPriority = QuestionPriority::LongTerm;
const SHORT: QuestionPriority = QuestionPriority::ShortTerm;

/// A current host ([`questioned`]) whose `send` names its generation fence
/// (`FENCED_SEND_HELP`) and whose reads carry the person count
/// (`"human_seq"`, `0`): every capability a typed answer rests on.
fn texting(screens: Vec<Vec<String>>) -> Mock {
    let mut m = questioned(screens);
    m.help_send = Some(FENCED_SEND_HELP.to_string());
    m.human_seq = Some(0);
    m
}

/// The stop-at-review loop answering questions under `p`.
fn answering_by(p: QuestionPriority) -> SuperviseOpts {
    let mut opts = answering(30);
    opts.policy.question_priority = p;
    opts
}

/// The window's loop (the session's word read) under the global
/// `question_priority` `p`, and a person's quiet window of 10 s (`[harness]
/// human_grace_s`, as [`answering`]'s): the quiet every read the loop keys
/// on must show.
fn hosted_by(p: QuestionPriority) -> SuperviseOpts {
    let mut opts = hosted_answering(true);
    opts.policy.question_priority = p;
    opts.policy.human_grace_s = 10;
    opts
}

/// `capture` with `text` in its free-text row as 2.1.283 draws it at 120
/// columns, under the transcript the capture has above the dialog.
fn with_text(capture: &str, text: &str) -> Vec<String> {
    let rows = q(capture);
    let top = rows
        .iter()
        .rposition(|r| !r.is_empty() && r.chars().all(|c| c == '─'))
        .and_then(|divider| {
            rows[..divider]
                .iter()
                .rposition(|r| !r.is_empty() && r.chars().all(|c| c == '─'))
        })
        .expect("the dialog's top rule");
    let mut out = rows[..top].to_vec();
    out.extend(aterm_phase::prompt::fixtures::rewrap_free_text(
        capture, text, 120,
    ));
    out
}

/// The text `p` writes.
fn text_of(p: QuestionPriority) -> &'static str {
    p.text().expect("a text priority")
}

/// The `send` requests made, in order.
fn sends(m: &Mock) -> Vec<&String> {
    m.requests.iter().filter(|r| verb_of(r) == "send").collect()
}

/// The lone single-select question (c1): the focus on option 1 as the
/// vendor opens it, moved to option 2, on the pristine free-text row
/// (c1-03), then `text` typed there.
fn single_screens(text: &str) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    use aterm_phase::prompt::fixtures as f;
    let free = q(f::QUESTION_283_FREE_TEXT_FOCUSED);
    (
        focused_on(&free, "1. Red"),
        focused_on(&free, "2. Blue"),
        free,
        with_text(f::QUESTION_283_TYPED_LONG_TERM, text),
    )
}

/// A lone single-select under `long-term`: two seen moves down, the text written into the
/// pristine free-text row by one fenced `send`, read back exactly, and Enter pressed only on
/// the next read. The ledger names `answer-long-term@v1` for the text and the Enter, the
/// dialog is told once as `CHOSE …`, and the window hears `story chose long-term`.
#[test]
fn a_lone_single_select_is_answered_by_the_long_term_text() {
    let (opt1, opt2, free, typed) = single_screens(LONG_TERM_TEXT);
    let (ldir, ledger) = ledger_file("text-single");
    let mut m = texting(vec![
        opt1,
        opt2,
        free.clone(),
        typed.clone(),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(1);
    let opts = hosted_by(LONG);
    let mut out: Vec<u8> = Vec::new();
    {
        let mut s = session(&mut m, None);
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _ = s.run_hosted(&opts, stop, &mut out);
    }
    let lines: Vec<String> = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        m.writes(),
        [
            "down".to_string(),
            "down".to_string(),
            format!("text:{LONG_TERM_TEXT}"),
            "enter".to_string()
        ],
        "{:#?}",
        m.requests
    );
    let send = sends(&m)[0].clone();
    let free_row = free
        .iter()
        .find(|r| r.starts_with("❯ 3. Type something."))
        .expect("the free-text row");
    assert!(
        send.starts_with("send if-human=0 if-gen=1.")
            && send.contains(&format!(" if={} -- ", guarded(free_row))),
        "{send}"
    );
    for p in m.presses() {
        assert!(p.starts_with("key if-human=0 if-gen=1."), "{p}");
    }
    let told: Vec<&String> = lines.iter().filter(|l| l.starts_with("CHOSE ")).collect();
    assert_eq!(told.len(), 1, "{lines:#?}");
    assert!(
        told[0].contains(&format!("rule={RULE_ANSWER_LONG_TERM} policy=long-term "))
            && told[0].ends_with("Which colour do you prefer? → decided by the worker (long-term)"),
        "{}",
        told[0]
    );
    assert!(
        m.requests
            .iter()
            .any(|r| r.ends_with("story chose long-term")),
        "{:#?}",
        m.requests
    );
    let rows = ledger_rows(&ledger);
    let approved: Vec<&String> = rows
        .iter()
        .filter(|r| {
            r.contains(&format!("\"rule_id\":\"{RULE_ANSWER_LONG_TERM}\""))
                && r.contains("\"decision\":\"approved\"")
        })
        .collect();
    assert_eq!(approved.len(), 2, "the text and the Enter: {rows:#?}");
    assert!(
        approved[0].contains("typed the long-term policy text into the free-text row"),
        "{}",
        approved[0]
    );
    let _ = std::fs::remove_dir_all(&ldir);
}

/// The lone multi-select question (c3): the focus on option 1 as the vendor
/// opens it, moved down to options 2 and 3, on the pristine free-text row
/// (c3-04); `text` typed there, the row ticked, the focus kept (c3-05);
/// `down` to `Submit`, the text reflowed (c3-06); the review (c3-07).
fn multi_screens(text: &str) -> Vec<Vec<String>> {
    use aterm_phase::prompt::fixtures as f;
    let free = q(f::QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED);
    let typed = with_text(f::QUESTION_283_MULTISELECT_TYPED_LONG_TERM, text);
    vec![
        focused_on(&free, "1. [ ] Cheese"),
        focused_on(&free, "2. [ ] Olives"),
        focused_on(&free, "3. [ ] Basil"),
        free,
        typed,
        with_text(f::QUESTION_283_MULTISELECT_TYPED_BUTTON, text),
        q(f::QUESTION_283_MULTISELECT_REVIEW),
        idle_screen(),
    ]
}

/// The policy text's model (`Text = 1`) for a dialog of `tabs` question
/// tabs, with or without the review, single- or multi-select.
fn text_model(tabs: i64, review: i64, multi: i64) -> aterm_spec::derive::Model {
    aterm_spec::interp::with_consts(
        &aterm_models_harness::supervisor_question_text_model(),
        &[("Multi", multi), ("Tabs", tabs), ("Review", review)],
    )
}

/// Tier-1 for the policy text (`SupervisorQuestionText`, `Text = 1`): the real loop over the
/// scripted server, every read projected onto the model, every `key` its
/// `HarnessMove`/`HarnessEnter` and every `send` its `HarnessType`, each enabled where sent,
/// the send carrying the generation fence, row guard, person fence and the priority's text,
/// and all six laws checked after every request. Shapes:
///
/// 1. A lone single-select under each text priority: `↓ ↓`, the text, Enter on it.
/// 2. A lone multi-select with its review: `↓ ↓ ↓`, the text, `↓` onto `Submit`, Enter there,
///    the review's Enter.
/// 3. Negative control: a loop seeded with the long-term text as its own, over a field a
///    person typed it into, presses Enter, and the bind catches it.
#[test]
fn tier1_the_policy_text_goes_only_where_the_model_allows() {
    // 1. Single-select, both priorities.
    for p in [LONG, SHORT] {
        let (opt1, opt2, free, typed) = single_screens(text_of(p));
        let mut m = texting(vec![opt1, opt2, free, typed, idle_screen()]);
        let model = text_model(1, 0, 0);
        let (st, keys, _) = q_bound_with(&mut m, &model, 0, Some(p), &answering_by(p), |_| {});
        assert_eq!(
            keys,
            ["down", "down", "text", "enter"],
            "{p:?}: {:#?}",
            m.requests
        );
        assert_eq!(st["tab"], 2, "{p:?}: the dialog closed: {st:?}");
    }

    // 2. The lone multi-select.
    let mut m = texting(multi_screens(LONG_TERM_TEXT));
    let model = text_model(1, 1, 1);
    let (st, keys, _) = q_bound_with(&mut m, &model, 0, Some(LONG), &answering_by(LONG), |_| {});
    assert_eq!(
        keys,
        ["down", "down", "down", "text", "down", "enter", "enter"],
        "{:#?}",
        m.requests
    );
    assert_eq!(st["tab"], 2, "the dialog closed: {st:?}");

    // 3. Negative control: a blind loop over a person's own policy text.
    let person = with_text(
        aterm_phase::prompt::fixtures::QUESTION_283_TYPED_LONG_TERM,
        LONG_TERM_TEXT,
    );
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut m = texting(vec![person.clone(), idle_screen()]);
        // The person typed it, then was quiet.
        m.human = vec!["15000"];
        let key = {
            let p = aterm_phase::parse_prompt_v2(&person).expect("the dialog");
            approval_loop::question_key(p.question_dialog.as_ref().expect("read whole"), &person)
        };
        q_bound_with(
            &mut m,
            &text_model(1, 0, 0),
            0,
            Some(LONG),
            &answering_by(LONG),
            move |s| {
                s.question.typed.insert(key, LONG);
                s.question.mode = Some(LONG);
            },
        )
    }));
    let why = caught.expect_err("the bind let a blind Enter through");
    let why = why
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| why.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        why.contains("where the model forbids HarnessEnter"),
        "{why}"
    );
}

/// The window's loop over `m` until the mock's session ends: its printed
/// lines. `ledger`: the approval ledger's path.
fn hosted_run(m: &mut Mock, opts: &SuperviseOpts, ledger: Option<&Path>) -> Vec<String> {
    let mut out: Vec<u8> = Vec::new();
    {
        let mut s = session(m, None);
        s.set_approval_env(owner_env());
        s.set_approval_ledger(ledger.map(Path::to_path_buf));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _ = s.run_hosted(opts, stop, &mut out);
    }
    String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

/// The stop-at-review loop over `m`: why it handed boxes over, its ledger's
/// `escalated` rows' reasons ([`Book::escalated`]).
fn supervised(m: &mut Mock, opts: &SuperviseOpts, tag: &str) -> Vec<String> {
    let book = Book::new(tag);
    {
        let mut s = session(m, None);
        s.set_approval_env(owner_env());
        book.keep(&mut s);
        s.supervise(opts).expect("supervise");
    }
    book.escalated()
}

/// The badge the loop raised, if any (`meta set attention owner=supervisor`).
fn badge(m: &Mock) -> Option<String> {
    m.requests
        .iter()
        .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
        .map(str::to_string)
}

/// A 2.1.302 frame's dialog (its top rule to its footer) as a fresh session draws it in a
/// pane `pane` rows tall: from row 7, under the launch card's rows (blank here), blank rows
/// under it to the pane's last.
fn in_fresh_pane(rows: &[String], pane: usize) -> Vec<String> {
    let last = rows
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .expect("the footer");
    let mut out = vec![String::new(); 7];
    out.extend_from_slice(&rows[..=last]);
    assert!(out.len() <= pane, "the dialog fits the pane");
    out.resize(pane, String::new());
    out
}

/// The review of round 21 (measured on Claude Code 2.1.302 at 41x67 in the real-Claude lab,
/// its local stub; built here by [`in_fresh_pane`] from the 41x60 captures drawn from row 7):
/// a fresh multi-select drawn from row 7 of a pane `7 + 11 + N + 40` rows tall, the long-term
/// text `N` rows once the focus leaves it. The text is typed (it keeps its rows at 41
/// columns), `down` moves the focus to `❯    Submit` on row 27, and the 40-row tail the loop
/// reads begins AT that row, the cursor on it. That read is the dialog with its head off the
/// screen, so it is read again whole, where `Submit` is focused under the loop's own text, and
/// Enter goes on it: never a question named by its footer alone with its head on the screen,
/// which stood as the read, made `down` come back "not the question it was" and handed the box
/// to the manager. The scripted server serves the next screen to each read, so every screen
/// whose tail misses the live rows is scripted twice: its tail, then the grid whole.
#[test]
fn a_tail_cut_at_the_focused_submit_is_read_whole_and_submitted() {
    use aterm_phase::prompt::fixtures as f;
    let (cols, pristine) = f::QUESTION_MULTI_PRISTINE_2_1_302[1];
    let (kept_cols, kept) = f::QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302[1];
    assert_eq!((cols, kept_cols), (41, 41));
    let pane = 67;
    let pristine = q(pristine);
    let typed = in_fresh_pane(&q(f::QUESTION_MULTI_TYPED_41X60_2_1_302), pane);
    let button = in_fresh_pane(&q(kept), pane);
    let submit = button
        .iter()
        .position(|r| r == "❯    Submit")
        .expect("the focused button");
    assert_eq!(
        submit,
        pane - 40,
        "PRECONDITION: the tail begins at `Submit`"
    );
    let tail = aterm_phase::parse_prompt_v2(&button[submit..]).expect("a box");
    assert_eq!(tail.kind, aterm_phase::PromptKind::Question);
    let mut screens = Vec::new();
    for option in ["1. [ ] Unit", "2. [ ] Lint", "3. [ ] Fuzz", "4. [ ] Type"] {
        let rows = in_fresh_pane(&focused_on(&pristine, option), pane);
        screens.push(rows.clone());
        screens.push(rows);
    }
    for rows in [typed, button] {
        screens.push(rows.clone());
        screens.push(rows);
    }
    screens.push(idle_screen());
    let mut m = texting(screens);
    m.pane_cols = Some(41);
    m.cursor_on_caret = true;
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering_by(LONG)).expect("supervise");
    drop(s);
    assert_eq!(badge(&m), None, "handed over: {:#?}", m.requests);
    assert_eq!(
        m.writes(),
        [
            "down".to_string(),
            "down".to_string(),
            "down".to_string(),
            format!("text:{LONG_TERM_TEXT}"),
            "down".to_string(),
            "enter".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    let enter = m.presses()[4];
    assert!(
        enter.contains(&guarded("❯    Submit")) && enter.ends_with(" enter"),
        "the Enter is on the button: {enter}"
    );
}

/// ATERM-KI-1015-1's F2 residual, at the loop: a fresh 2.1.302 multi-select at 41 columns drawn
/// from row 7 of a pane whose 40-row tail begins AT `5. Chat about this`, where a person's keys
/// moved the focus (the loop never puts it there). That tail is the dialog with its head off
/// the screen, so it is read again whole, where the focus is on the chat row: the loop moves it
/// up off that row to the free-text row, types the long-term text, moves down to `Submit` and
/// presses Enter there. Nothing is handed over. Every screen is scripted twice (its tail, then
/// the grid whole): each tail begins at the chat row or inside the typed text.
#[test]
fn a_tail_cut_at_a_chat_row_a_person_focused_is_read_whole_and_answered() {
    use aterm_phase::prompt::fixtures as f;
    let (cols, pristine) = f::QUESTION_MULTI_PRISTINE_2_1_302[1];
    let (kept_cols, kept) = f::QUESTION_MULTI_REFLOW_LONG_KEPT_2_1_302[1];
    assert_eq!((cols, kept_cols), (41, 41));
    let pristine = q(pristine);
    let chat_at = in_fresh_pane(&pristine, 40)
        .iter()
        .position(|r| r.trim() == "5. Chat about this")
        .expect("the chat row");
    let pane = chat_at + 40;
    let chat = in_fresh_pane(&focused_on(&pristine, "5. Chat about this"), pane);
    assert_eq!(
        chat[chat_at], "❯ 5. Chat about this",
        "PRECONDITION: focused"
    );
    let tail = aterm_phase::parse_prompt_v2(&chat[chat_at..]).expect("a box");
    assert!(
        tail.kind == aterm_phase::PromptKind::Question && tail.head_off_screen,
        "PRECONDITION: the tail at the chat row is the dialog with its head off: {tail:?}"
    );
    let mut screens = Vec::new();
    for rows in [
        chat,
        in_fresh_pane(&focused_on(&pristine, "Submit"), pane),
        in_fresh_pane(&focused_on(&pristine, "4. [ ] Type"), pane),
        in_fresh_pane(&q(f::QUESTION_MULTI_TYPED_41X60_2_1_302), pane),
        in_fresh_pane(&q(kept), pane),
    ] {
        screens.push(rows.clone());
        screens.push(rows);
    }
    screens.push(idle_screen());
    let mut m = texting(screens);
    m.pane_cols = Some(41);
    m.cursor_on_caret = true;
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering_by(LONG)).expect("supervise");
    drop(s);
    assert_eq!(badge(&m), None, "handed over: {:#?}", m.requests);
    assert_eq!(
        m.writes(),
        [
            "up".to_string(),
            "up".to_string(),
            format!("text:{LONG_TERM_TEXT}"),
            "down".to_string(),
            "enter".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    let enter = m.presses()[3];
    assert!(
        enter.contains(&guarded("❯    Submit")) && enter.ends_with(" enter"),
        "the Enter is on the button: {enter}"
    );
}

/// `frame`, a multi-select with its free-text row ticked and the focus on
/// `❯    Submit`, as 2.1.300 draws a text that overflows its row once the
/// focus left it (ATERM-KI-1015-1 F2): the checkbox without its space
/// (`[✔]Decide`) and every wrap row one cell left, at column 8. SYNTHETIC:
/// measured at 38 to 51 columns, drawn here on c3-06's 120; the reader asks
/// no width. `wraps_too`: false moves the first row only.
fn against_its_checkbox(frame: &[String], wraps_too: bool) -> Vec<String> {
    let free = frame
        .iter()
        .position(|r| r.starts_with("  4. [✔] "))
        .expect("the ticked free-text row, unfocused");
    let button = frame
        .iter()
        .position(|r| r.trim_end() == "❯    Submit")
        .expect("the focused button");
    frame
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if i == free {
                r.replacen("[✔] ", "[✔]", 1)
            } else if wraps_too && (free..button).contains(&i) {
                r.strip_prefix(' ').expect("a wrap row").to_string()
            } else {
                r.clone()
            }
        })
        .collect()
}

/// ATERM-KI-1015-1 F2, the move across the column change: the decision read
/// has the focus in the field, its text at column 9 (`❯ 4. [✔] Decide`); the
/// `↓` lands on a frame whose text 2.1.300 drew against its checkbox, at
/// column 8 (`  4. [✔]Decide`). The Enter goes on `Submit` only because the
/// field as the landed read joins it equals the field as the decision read
/// joined it (`field_of`: text, pristine, tick — never a column), so the
/// dialog is submitted as at column 9: `[down×3, text, down, enter,
/// enter]`. On the base reader the landed frame read `Unknown`: no Enter on
/// `Submit`, and the dialog, still up, was handed over with the text in.
/// NEGATIVE CONTROLS, each landed frame left up: a word of the text not the
/// loop's ("the free-text row changed since the decision read"), and only
/// its first row moved, its wraps left at 9 ("could not read whole"), get no
/// Enter after the `↓` and are handed over.
#[test]
fn a_multi_select_text_that_moves_against_its_checkbox_is_still_submitted_by_its_button() {
    let screens = multi_screens(LONG_TERM_TEXT);
    let typed_at_9 = format!("❯ 4. [✔] {}", &LONG_TERM_TEXT[..16]);
    assert!(
        screens[4].iter().any(|r| r.starts_with(&typed_at_9)),
        "the decision read's text at column 9: {:#?}",
        screens[4]
    );
    let landed = against_its_checkbox(&screens[5], true);
    assert!(
        landed.iter().any(|r| r.starts_with("  4. [✔]Decide")),
        "{landed:#?}"
    );
    // `stands`: the landed frame is the last served, as a frame the loop
    // presses nothing on stays up.
    let run = |landed: Vec<String>, stands: bool| {
        let mut screens = screens.clone();
        screens[5] = landed;
        if stands {
            screens.truncate(6);
        }
        let mut m = texting(screens);
        let lines = supervised(&mut m, &answering_by(LONG), "text-against-checkbox");
        (m, lines)
    };
    let (m, _) = run(landed.clone(), false);
    assert_eq!(
        m.writes(),
        [
            "down".to_string(),
            "down".to_string(),
            "down".to_string(),
            format!("text:{LONG_TERM_TEXT}"),
            "down".to_string(),
            "enter".to_string(),
            "enter".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    let enters: Vec<&String> = m
        .presses()
        .into_iter()
        .filter(|p| p.ends_with(" enter"))
        .collect();
    assert!(
        enters[0].contains(&guarded("❯    Submit")),
        "the first Enter is on the button: {enters:#?}"
    );

    // Negative controls: a field that changed, a frame never read whole.
    let person = landed
        .iter()
        .map(|r| r.replacen("[✔]Decide yourself", "[✔]Decide myself", 1))
        .collect();
    let skewed = against_its_checkbox(&screens[5], false);
    for (frame, why) in [
        (person, "the free-text row changed since the decision read"),
        (skewed, "a question aterm could not read whole"),
    ] {
        let (m, lines) = run(frame, true);
        assert_eq!(
            m.writes(),
            [
                "down".to_string(),
                "down".to_string(),
                "down".to_string(),
                format!("text:{LONG_TERM_TEXT}"),
                "down".to_string(),
            ],
            "{why}: {:#?}",
            m.requests
        );
        assert!(lines.iter().any(|l| l.contains(why)), "{why}: {lines:#?}");
    }
}

/// Two questions and the review: each tab's text written once, the multi-select tab moved onto
/// its button, the review's Enter; the dialog told once under `long-term`.
#[test]
fn two_questions_and_their_review_are_answered_by_text_then_submitted() {
    use aterm_phase::prompt::fixtures as f;
    let first_typed = with_text(f::QUESTION_283_TABS_FIRST_TYPED_LONG_TERM, LONG_TERM_TEXT);
    let second_typed = with_text(f::QUESTION_283_TABS_SECOND_TYPED_LONG_TERM, LONG_TERM_TEXT);
    let mut m = texting(vec![
        q(f::QUESTION_283_TABS_FIRST),
        q(f::QUESTION_283_TABS_FIRST_DOWN),
        q(f::QUESTION_283_TABS_FIRST_FREE_TEXT_FOCUSED),
        first_typed,
        q(f::QUESTION_283_TABS_SECOND),
        q(f::QUESTION_283_TABS_SECOND_DOWN),
        q(f::QUESTION_283_TABS_SECOND_DOWN_TWICE),
        q(f::QUESTION_283_TABS_SECOND_FREE_TEXT_FOCUSED),
        second_typed,
        with_text(f::QUESTION_283_TABS_SECOND_TYPED_BUTTON, LONG_TERM_TEXT),
        q(f::QUESTION_283_TABS_REVIEW),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(1);
    let lines = hosted_run(&mut m, &hosted_by(LONG), None);
    let text = format!("text:{LONG_TERM_TEXT}");
    assert_eq!(
        m.writes(),
        [
            "down", "down", &text, "enter", "down", "down", "down", &text, "down", "enter", "enter"
        ],
        "{:#?}",
        m.requests
    );
    let told: Vec<&String> = lines.iter().filter(|l| l.starts_with("CHOSE ")).collect();
    assert_eq!(told.len(), 1, "{lines:#?}");
    assert!(
        told[0].contains("policy=long-term ")
            && told[0].contains("Which colour do you prefer? → decided by the worker (long-term)")
            && told[0].contains("Which toppings do you want? → decided by the worker (long-term)"),
        "{}",
        told[0]
    );
}

/// The session's word `short-term` answers over the window's `recommended`. Negative control:
/// with the word unset, the recommended option is pressed and nothing written.
#[test]
fn the_sessions_short_term_word_answers_over_a_global_recommended() {
    for word in [Some("short-term"), None] {
        let (opt1, opt2, free, typed) = single_screens(SHORT_TERM_TEXT);
        let mut m = texting(if word.is_some() {
            vec![opt1, opt2, free, typed, busy_screen(), idle_screen()]
        } else {
            vec![opt1, busy_screen(), idle_screen()]
        });
        m.vanish_after = Some(1);
        m.questions = word;
        let lines = hosted_run(&mut m, &hosted_by(QuestionPriority::Recommended), None);
        let told: Vec<&String> = lines.iter().filter(|l| l.starts_with("CHOSE ")).collect();
        if word.is_some() {
            assert_eq!(
                m.writes(),
                [
                    "down".to_string(),
                    "down".to_string(),
                    format!("text:{SHORT_TERM_TEXT}"),
                    "enter".to_string()
                ],
                "{:#?}",
                m.requests
            );
            assert!(
                told.len() == 1
                    && told[0]
                        .contains(&format!("rule={RULE_ANSWER_SHORT_TERM} policy=short-term ")),
                "{lines:#?}"
            );
        } else {
            assert_eq!(m.writes(), ["enter"], "{:#?}", m.requests);
            assert!(sends(&m).is_empty());
            assert!(
                told.len() == 1 && told[0].contains("policy=recommended "),
                "{lines:#?}"
            );
        }
    }
}

/// A person's text in the field (`go with blue`): nothing written or keyed; the badge says a
/// person has begun answering.
#[test]
fn a_persons_text_in_the_field_is_theirs_under_a_text_priority() {
    use aterm_phase::prompt::fixtures as f;
    let mut m = texting(vec![q(f::QUESTION_283_TYPED_PERSON)]);
    m.vanish_after = Some(0);
    let _ = hosted_run(&mut m, &hosted_by(LONG), None);
    assert!(m.writes().is_empty(), "{:#?}", m.requests);
    let badge = badge(&m).expect("escalated");
    assert!(badge.contains("a person has begun answering"), "{badge}");
}

/// A text that reads back short twice: one `send`, no Enter, handed over.
#[test]
fn a_text_that_reads_back_short_twice_is_handed_over_never_written_again() {
    use aterm_phase::prompt::fixtures as f;
    let free = q(f::QUESTION_283_FREE_TEXT_FOCUSED);
    let prefix = with_text(
        f::QUESTION_283_TYPED_LONG_TERM,
        "Decide yourself: choose the option that best serves long term",
    );
    let mut m = texting(vec![free, prefix.clone(), prefix]);
    let lines = supervised(&mut m, &answering_by(LONG), "text-prefix");
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(
        m.requests.iter().any(|r| r.starts_with("await idle 2000 ")),
        "one wait for the screen to hold still: {:#?}",
        m.requests
    );
    assert!(
        lines.iter().any(|l| l.contains(
            "the policy text read back short of what was written: a \
             text is never written twice"
        )),
        "{lines:#?}"
    );
}

/// A text that disappears after reading back exactly: the Enter, fenced on the read-back's
/// generation, is skipped `reason=changed`, and the dialog is handed over with no second
/// `send`.
#[test]
fn a_text_that_disappears_after_it_read_back_is_handed_over_never_rewritten() {
    let (_, _, free, typed) = single_screens(LONG_TERM_TEXT);
    let (ldir, ledger) = ledger_file("text-gone");
    let mut m = texting(vec![free.clone(), typed]);
    // The field emptied between the read-back and the Enter.
    m.swap_at_key = Some(free);
    {
        let mut s = session(&mut m, None);
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
        s.supervise(&answering_by(LONG)).expect("supervise");
    }
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    let presses = m.presses();
    assert!(
        presses.len() == 1 && presses[0].ends_with(" enter"),
        "{:#?}",
        m.requests
    );
    let rows = ledger_rows(&ledger);
    assert!(
        rows.iter().any(|r| r.contains("\"decision\":\"skipped\"")
            && r.contains("the screen moved past the judged read")),
        "the Enter skipped by its fence: {rows:#?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("\"decision\":\"escalated\"")
            && r.contains("the policy text the supervisor wrote is gone from its free-text row")),
        "handed over: {rows:#?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);
    // Before the text, though no key moved the focus first: the send is the
    // tab's first key (the dialog drawn still a second, the person's stamp
    // asked again after it).
    let send = m
        .requests
        .iter()
        .position(|r| r.starts_with("send "))
        .expect("the send");
    assert_eq!(
        &m.requests[send - 2..send],
        ["await idle 1000 timeout 3000", "status"],
        "{:#?}",
        m.requests
    );
}

/// A question whose dialog went with no answer of the loop's (a person's Esc after its text)
/// and is asked again is a new dialog: its pristine field is written afresh, never handed over
/// as the text gone.
#[test]
fn a_question_asked_again_after_its_dialog_went_is_written_afresh() {
    let (_, _, free, typed) = single_screens(LONG_TERM_TEXT);
    let mut m = texting(vec![
        free.clone(),
        typed.clone(),
        free,
        typed,
        busy_screen(),
        idle_screen(),
    ]);
    // A person's Esc between the read-back and the loop's Enter.
    m.swap_at_key = Some(idle_screen());
    m.vanish_after = Some(1);
    let _ = hosted_run(&mut m, &hosted_by(LONG), None);
    assert_eq!(badge(&m), None, "never handed over: {:#?}", m.requests);
    assert_eq!(sends(&m).len(), 2, "{:#?}", m.requests);
}

/// ATERM-KI-1016-1 (Questions): the loop answered a question by its own text and its Enter
/// went, and the worker asked the same question again in a new dialog with no read between
/// them (no busy read, no composer: the gap the reads did not sample). The re-ask's pristine
/// field read as the loop's text "gone" from the dialog it had answered, and was handed over.
/// The loop's Enter on its own text answered that question, and no person's key has come
/// since (the person count the Enter was fenced on still stands), so the pristine field is a
/// new dialog's: it is written afresh, the first dialog told once. NEGATIVE CONTROLS: a
/// person's key between (the count moved) still hands it over as "gone"; and
/// [`a_text_that_disappears_after_it_read_back_is_handed_over_never_rewritten`] (the Enter
/// skipped by its fence: the text gone from the same dialog) is unchanged.
#[test]
fn a_question_asked_again_with_no_read_between_after_the_loops_enter_is_written_afresh() {
    let (_, _, free, typed) = single_screens(LONG_TERM_TEXT);
    let run = |person_between: bool| {
        let mut m = texting(vec![
            free.clone(),
            typed.clone(),
            free.clone(),
            typed.clone(),
            busy_screen(),
            idle_screen(),
        ]);
        m.vanish_after = Some(1);
        if person_between {
            // The re-ask's first read counts one person's key more.
            for i in 2..6 {
                m.human_seq_on.insert(i, Some(1));
            }
        }
        let lines = hosted_run(&mut m, &hosted_by(LONG), None);
        (m, lines)
    };
    let (m, lines) = run(false);
    assert_eq!(badge(&m), None, "never handed over: {:#?}", m.requests);
    assert_eq!(
        m.writes(),
        [
            format!("text:{LONG_TERM_TEXT}"),
            "enter".to_string(),
            format!("text:{LONG_TERM_TEXT}"),
            "enter".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    let told = lines.iter().filter(|l| l.starts_with("CHOSE ")).count();
    assert_eq!(told, 2, "each dialog told once: {lines:#?}");
    let (m, _) = run(true);
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    assert!(
        badge(&m).is_some_and(|b| b.contains("is gone")),
        "a person's key between: {:?}",
        badge(&m)
    );
    // A lone multi-select, answered by its button and its review, asked
    // again straight after the review's Enter.
    let mut once = multi_screens(LONG_TERM_TEXT);
    let idle = once.pop().expect("the idle screen");
    let mut screens = once.clone();
    screens.extend(once);
    screens.push(idle);
    let mut m = texting(screens);
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering_by(LONG)).expect("supervise");
    drop(s);
    assert_eq!(badge(&m), None, "never handed over: {:#?}", m.requests);
    let answer = [
        "down".to_string(),
        "down".to_string(),
        "down".to_string(),
        format!("text:{LONG_TERM_TEXT}"),
        "down".to_string(),
        "enter".to_string(),
        "enter".to_string(),
    ];
    assert_eq!(
        m.writes(),
        [answer.clone(), answer].concat(),
        "{:#?}",
        m.requests
    );
}

/// `typed` with its key-hint footer row blanked: the loop's text still in a
/// dialog still up, which the reader reads neither as a dialog nor at its
/// composer — a reader hole for one frame.
fn unread_dialog(typed: &[String]) -> Vec<String> {
    let unread: Vec<String> = typed
        .iter()
        .map(|r| {
            if r.contains("Enter to select") {
                String::new()
            } else {
                r.clone()
            }
        })
        .collect();
    assert_ne!(worker_phase(&unread), Phase::Prompt, "{unread:#?}");
    {
        use aterm_phase::ScreenReader as _;
        assert!(aterm_phase::ClaudeReader.composer(&unread).is_none());
    }
    unread
}

/// The premise of the two tests below: a settled look took a missed frame
/// before the dialog was told. That frame reads the worker idle, so the look
/// says `EVENT idle` with its last line (not the closing idle screen's `⏺
/// Done.`); a press's own reads say nothing. A press can use the missed
/// frames up, and then no look takes one and nothing is under test.
fn a_look_took_a_missed_frame(lines: &[String]) {
    let missed = lines
        .iter()
        .position(|l| l.starts_with("EVENT idle ") && !l.ends_with("⏺ Done."));
    let told = lines.iter().position(|l| l.starts_with("CHOSE "));
    assert!(
        missed.is_some() && missed < told,
        "no look took a missed frame before the dialog was told: {lines:#?}"
    );
}

/// ATERM-KI-1015-1 F4's forget, held to its evidence: a dialog ends at a look
/// that reads the worker at its COMPOSER with no dialog up, never at one that
/// reads no dialog only. Here the loop's Enter is skipped (`reason=changed`)
/// and a whole settled look takes a frame of the same dialog the reader
/// misses; the dialog then reads again with the loop's text in it. Kept, the
/// memory knows the text for the loop's own: the Enter is pressed again and
/// the dialog told once (`CHOSE … policy=long-term`). Forgotten at the missed
/// frame, the text read as a person's and the dialog was handed over ("a
/// question a person has begun answering (text in its free-text row the
/// supervisor did not write)"). `continue_policy` is off, so the missed frame
/// gets no carry-on and only the memory is under test, and the test holds
/// its premise ([`a_look_took_a_missed_frame`]). NEGATIVE CONTROLS:
/// [`a_question_asked_again_after_its_dialog_went_is_written_afresh`] (the
/// composer read still forgets) and scenario 7 (a text gone from the same
/// dialog is still handed over).
#[test]
fn a_dialog_the_reader_misses_for_one_look_keeps_the_loops_text() {
    let (_, _, free, typed) = single_screens(LONG_TERM_TEXT);
    let unread = unread_dialog(&typed);
    let mut m = texting(vec![
        free,
        typed.clone(),
        unread.clone(),
        unread.clone(),
        unread.clone(),
        unread.clone(),
        typed,
        busy_screen(),
        idle_screen(),
    ]);
    // The loop's Enter skipped: its text in, no answer yet.
    m.swap_at_key = Some(unread);
    m.vanish_after = Some(1);
    let mut opts = hosted_by(LONG);
    opts.policy.continue_policy = false;
    let lines = hosted_run(&mut m, &opts, None);
    assert_eq!(badge(&m), None, "{:#?}", m.requests);
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("CHOSE ") && l.contains("policy=long-term")),
        "{lines:#?}"
    );
    a_look_took_a_missed_frame(&lines);
}

/// The same missed frame after the loop's Enter went (the dialog answered,
/// its memory holding that answer): kept, the dialog is told once, at the
/// worker's busy read, with no second Enter and nothing handed over.
/// Forgotten at the missed frame, it was told early there and the text that
/// read again was a person's: handed over. The premise held as above.
#[test]
fn a_dialog_the_reader_misses_after_the_loops_enter_is_told_once() {
    let (_, _, free, typed) = single_screens(LONG_TERM_TEXT);
    let unread = unread_dialog(&typed);
    let mut m = texting(vec![
        free,
        typed.clone(),
        unread.clone(),
        unread.clone(),
        unread.clone(),
        unread,
        typed,
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(1);
    let mut opts = hosted_by(LONG);
    opts.policy.continue_policy = false;
    let lines = hosted_run(&mut m, &opts, None);
    assert_eq!(badge(&m), None, "{:#?}", m.requests);
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    let enters = m.presses().iter().filter(|p| p.ends_with(" enter")).count();
    assert_eq!(enters, 1, "{:#?}", m.requests);
    let chose: Vec<&String> = lines.iter().filter(|l| l.starts_with("CHOSE ")).collect();
    assert!(
        chose.len() == 1 && chose[0].contains("policy=long-term"),
        "{lines:#?}"
    );
    a_look_took_a_missed_frame(&lines);
}

/// A text the field never shows (after the worker's reaction and after the screen held still):
/// one `send`, no Enter, handed over.
#[test]
fn a_text_the_dialog_never_took_is_handed_over_never_written_twice() {
    let (_, _, free, _) = single_screens(LONG_TERM_TEXT);
    let mut m = texting(vec![free]);
    let lines = supervised(&mut m, &answering_by(LONG), "text-not-taken");
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(
        lines.iter().any(|l| l.contains(
            "the dialog did not take the policy text: a text is never \
             written twice"
        )),
        "{lines:#?}"
    );
    // Not handed over at the first empty read: the worker's reaction, and
    // then once more after the screen held still.
    let after: Vec<&String> = m
        .requests
        .iter()
        .skip_while(|r| !r.starts_with("send "))
        .collect();
    assert!(
        after
            .iter()
            .any(|r| r.starts_with("await seq ") && r.ends_with(" timeout 10000"))
            && after.iter().any(|r| r.starts_with("await idle 2000 ")),
        "{after:#?}"
    );
}

/// A window that cannot fence a typed answer (no `send if-gen=`, no person count, or no person
/// stamp) gets the recommended option, ledgered under the word's rule with the fallback said
/// (`(recommended: this window cannot fence a typed answer: no …)`). Negative control: a
/// window naming every fence writes the text.
#[test]
fn a_window_that_cannot_fence_a_typed_answer_gets_the_recommended_option_said() {
    use aterm_phase::prompt::fixtures as f;
    let (opt1, ..) = single_screens(LONG_TERM_TEXT);
    for (tag, send_help, count, stamped, missing) in [
        ("no-send-gen", None, Some(0), true, "no send if-gen="),
        (
            "no-person-count",
            Some(FENCED_SEND_HELP),
            None,
            true,
            "no the person count (human_seq)",
        ),
        (
            "no-person-stamp",
            Some(FENCED_SEND_HELP),
            Some(0),
            false,
            "no the person stamp (human_ms)",
        ),
    ] {
        let (ldir, ledger) = ledger_file(tag);
        let (jdir, journal) = journal_file(tag);
        // A host that stamps no person gets the stand-in quiet first: one
        // read more.
        let mut screens = vec![opt1.clone(), idle_screen()];
        if !stamped {
            screens.insert(0, opt1.clone());
        }
        let mut m = questioned(screens);
        m.help_send = send_help.map(str::to_string);
        m.human_seq = count;
        if !stamped {
            m.human = vec![];
        }
        let mut s = session(&mut m, None);
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..answering_by(LONG)
        };
        s.supervise(&opts).expect("supervise");
        drop(s);
        assert_eq!(m.writes(), ["enter"], "{tag}: {:#?}", m.requests);
        assert!(
            m.presses()[0].contains(&guarded("❯ 1. Red (Recommended)")),
            "{tag}: {:#?}",
            m.presses()
        );
        let why = format!("this window cannot fence a typed answer: {missing}");
        let rows = ledger_rows(&ledger);
        assert!(
            rows.iter().any(
                |r| r.contains(&format!("\"rule_id\":\"{RULE_ANSWER_LONG_TERM}\""))
                    && r.contains("\"decision\":\"approved\"")
                    && r.contains(&format!("(recommended: {why})"))
            ),
            "{tag}: {rows:#?}"
        );
        let (records, _) = journal_records(&journal);
        assert!(
            records
                .iter()
                .any(|r| r.kind == "unproven"
                    && r.summary.contains(&format!("policy long-term: {why}"))),
            "{tag}: the fallback is journaled: {records:#?}"
        );
        let _ = std::fs::remove_dir_all(&ldir);
        let _ = std::fs::remove_dir_all(&jdir);
    }
    // Negative control.
    let mut m = texting(vec![
        q(f::QUESTION_283_FREE_TEXT_FOCUSED),
        with_text(f::QUESTION_283_TYPED_LONG_TERM, LONG_TERM_TEXT),
    ]);
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    let _ = s.supervise(&answering_by(LONG));
    drop(s);
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
}

/// Tier-1 for the person fence on the text: a person keys between the loop's read and its
/// `send`, which is skipped `reason=person` (the model's `PersonKey`, after which no
/// `HarnessType` or key is enabled); the loop reads again and writes the text once on the new
/// count. Negative control: without the fence (`PersonFence = 0`) the model enables the text
/// the server refused.
#[test]
fn a_persons_key_between_the_read_and_the_send_skips_the_policy_text() {
    let (opt1, opt2, free, typed) = single_screens(LONG_TERM_TEXT);
    let mut m = texting(vec![opt1, opt2, free.clone(), free, typed, idle_screen()]);
    m.human = vec!["15000"];
    m.person_at_send = 1;
    let model = text_model(1, 0, 0);
    let (st, keys, raced) =
        q_bound_with(&mut m, &model, 0, Some(LONG), &answering_by(LONG), |_| {});
    assert_eq!(
        keys,
        ["down", "down", "text", "enter"],
        "the skipped send is not counted as written: {:#?}",
        m.requests
    );
    assert_eq!(st["tab"], 2, "the dialog closed: {st:?}");
    assert_eq!(m.person_skipped.len(), 1, "{:#?}", m.requests);
    let sent = sends(&m);
    assert_eq!(sent.len(), 2, "{sent:#?}");
    assert!(
        sent[0].starts_with("send if-human=0 ") && sent[1].starts_with("send if-human=1 "),
        "{sent:#?}"
    );
    let raced = raced.expect("the person's key is in the bind");
    assert_eq!(raced["touched"], 1, "{raced:?}");
    let unfenced = aterm_spec::interp::with_consts(&model, &[("PersonFence", 0)]);
    assert!(
        unfenced.action_enabled("HarnessType", &raced),
        "without the fence the model allows the text the server refused: {raced:?}"
    );
}

/// A word changed after the loop's first key on a dialog hands it over, no Enter. Negative
/// control: a change before the first key simply applies.
#[test]
fn a_word_changed_after_the_first_key_hands_the_dialog_over() {
    let (opt1, opt2, free, typed) = single_screens(LONG_TERM_TEXT);
    let mut m = texting(vec![opt1.clone(), opt2, free, typed]);
    m.vanish_after = Some(0);
    m.questions = Some("long-term");
    // Meta read 1 is the start-up reconcile, 2 the text's decision, 3 the
    // Enter's (on the text's read-back).
    m.questions_at = vec![(3, Some("recommended"))];
    let _ = hosted_run(&mut m, &hosted_by(QuestionPriority::Recommended), None);
    assert!(
        !m.presses().iter().any(|p| p.ends_with(" enter")),
        "{:#?}",
        m.requests
    );
    let badge = badge(&m).expect("handed over");
    assert!(
        badge.contains(
            "the question policy was long-term at the supervisor's first key on this dialog and \
             is recommended now"
        ),
        "{badge}"
    );

    // Negative control: no key before the word changed.
    let mut m = texting(vec![opt1.clone(), opt1, idle_screen()]);
    m.vanish_after = Some(0);
    m.questions = Some("long-term");
    // The first decision's first-key wait finds a person: nothing keyed.
    m.human_ms_reads = VecDeque::from([None, Some(300)]);
    m.questions_at = vec![(3, Some("recommended"))];
    let _ = hosted_run(&mut m, &hosted_by(QuestionPriority::Recommended), None);
    assert!(sends(&m).is_empty(), "{:#?}", m.requests);
    assert_eq!(m.writes(), ["enter"], "{:#?}", m.requests);
}

/// An unknown stored word (a later build's) hands the dialog over, the badge naming the word.
#[test]
fn an_unknown_session_word_hands_the_dialog_over_under_a_text_global() {
    let (opt1, ..) = single_screens(LONG_TERM_TEXT);
    let mut m = texting(vec![opt1]);
    m.vanish_after = Some(0);
    m.questions = Some("longest-term");
    let _ = hosted_run(&mut m, &hosted_by(LONG), None);
    assert!(m.writes().is_empty(), "{:#?}", m.requests);
    let badge = badge(&m).expect("handed over");
    assert!(
        badge.contains(
            "this session's `questions` word longest-term is not one this supervisor knows"
        ),
        "{badge}"
    );
}

/// A preview under `long-term` has no free-text row: its recommended option is pressed, told
/// under the word's rule with the fallback in its subject and `policy=recommended`.
#[test]
fn a_preview_under_long_term_is_answered_with_its_recommended_option_and_told_so() {
    use aterm_phase::prompt::fixtures as f;
    let mut m = texting(vec![
        q(f::QUESTION_283_PREVIEW),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(1);
    let lines = hosted_run(&mut m, &hosted_by(LONG), None);
    assert_eq!(m.writes(), ["enter"], "{:#?}", m.requests);
    let told: Vec<&String> = lines.iter().filter(|l| l.starts_with("CHOSE ")).collect();
    assert_eq!(told.len(), 1, "{lines:#?}");
    assert!(
        told[0].contains(&format!("rule={RULE_ANSWER_LONG_TERM} policy=recommended "))
            && told[0].ends_with("(recommended: a preview question has no free-text row)"),
        "{}",
        told[0]
    );
    assert!(
        m.requests
            .iter()
            .any(|r| r.ends_with("story chose recommended")),
        "{:#?}",
        m.requests
    );
}

/// A fresh supervisor (memory lost) over a field holding the policy text no `send` of its own
/// wrote takes it for a person's: handed over, no key.
#[test]
fn a_fresh_supervisor_over_a_typed_field_hands_it_over_without_a_key() {
    let (_, _, _, typed) = single_screens(LONG_TERM_TEXT);
    let mut m = texting(vec![typed]);
    m.vanish_after = Some(0);
    let _ = hosted_run(&mut m, &hosted_by(LONG), None);
    assert!(m.writes().is_empty(), "{:#?}", m.requests);
    let badge = badge(&m).expect("handed over");
    assert!(
        badge.contains("a person has begun answering") && badge.contains("did not write"),
        "{badge}"
    );
}

/// A person's input after the text, then their quiet, the field intact: the loop presses
/// Enter on its own text, but the dialog's end is the person's: no `CHOSE`, no `story chose`,
/// no `chose (` note. Their input reaches the loop through the read-back's stamp, the read-back
/// and its `status` together, or the `status` alone. Negative control:
/// `a_lone_single_select_is_answered_by_the_long_term_text`.
#[test]
fn a_persons_input_after_the_text_leaves_the_enter_but_not_the_told_choice() {
    let (opt1, opt2, free, typed) = single_screens(LONG_TERM_TEXT);
    let (now, past) = (stamp_ms(TYPED_NOW), stamp_ms(QUIET_PAST_GRACE));
    // (case, each read's `"human_ms"`, `status human_ms=` from the n-th
    // read on: the 4th is the read-back).
    for (case, reads, status) in [
        (
            "the read alone",
            vec!["null", "null", "null", TYPED_NOW, QUIET_PAST_GRACE],
            vec![],
        ),
        (
            "the read and its status",
            vec!["null", "null", "null", TYPED_NOW, QUIET_PAST_GRACE],
            vec![(4, now), (5, past)],
        ),
        (
            "the status alone",
            vec!["null", "null", "null", "null", QUIET_PAST_GRACE],
            vec![(4, now), (5, past)],
        ),
    ] {
        let held = !status.is_empty();
        let mut screens = vec![opt1.clone(), opt2.clone(), free.clone(), typed.clone()];
        if !held {
            screens.push(typed.clone());
        }
        screens.extend([busy_screen(), idle_screen()]);
        let mut m = texting(screens);
        m.human = reads;
        if held {
            m.status_human_from = status.into_iter().collect();
            m.key_gates = vec![(3, 2)];
            m.stall_sleep = Some(Duration::from_millis(5));
        } else {
            m.vanish_after = Some(1);
        }
        let (jdir, journal) = journal_file("text-person-after");
        let mut opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..hosted_by(LONG)
        };
        opts.policy.human_grace_s = PERSON_GRACE_S;
        let lines = {
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut out: Vec<u8> = Vec::new();
            {
                let mut ctl = StopAtBusy {
                    mock: &mut m,
                    stop: held.then(|| stop.clone()),
                };
                let mut s = session(&mut ctl, None);
                s.set_approval_env(owner_env());
                s.set_clock(TEST_NOW, PDT);
                s.set_zone_lookup(test_zones);
                s.set_person_hold_wait(Duration::ZERO);
                let _ = s.run_hosted(&opts, stop, &mut out);
            }
            String::from_utf8(out)
                .expect("utf-8")
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        if held {
            assert!(
                records.iter().any(|r| r.line.starts_with("HELD ")),
                "{case}: held for the person: {records:#?}"
            );
        } else {
            // The wait names the decision's rule.
            assert!(
                records
                    .iter()
                    .any(|r| r.kind == "waiting" && r.phase == RULE_ANSWER_LONG_TERM),
                "{case}: {records:#?}"
            );
        }
        assert_eq!(
            m.writes(),
            [
                "down".to_string(),
                "down".to_string(),
                format!("text:{LONG_TERM_TEXT}"),
                "enter".to_string()
            ],
            "{case}: {:#?}",
            m.requests
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("CHOSE ")),
            "{case}: {lines:#?}"
        );
        assert!(
            !m.requests.iter().any(|r| r.contains("story chose")),
            "{case}: {:#?}",
            m.requests
        );
    }
}

/// The loop's own text read unticked between the exact read-back and the move off the field:
/// that move, fenced on the read-back, is skipped, and the dialog is handed over, no Enter.
#[test]
fn the_loops_own_text_unticked_on_a_multi_select_is_handed_over() {
    let mut screens = multi_screens(LONG_TERM_TEXT);
    let unticked: Vec<String> = screens[4]
        .iter()
        .map(|r| r.replace("[✔]", "[ ]").replace("☒ Toppings", "☐ Toppings"))
        .collect();
    screens.truncate(5);
    let mut m = texting(screens);
    // Unticked after the three moves onto the field, at the move off it.
    m.swap_at_key = Some(unticked);
    m.swap_after_keys = 3;
    let lines = supervised(&mut m, &answering_by(LONG), "text-unticked");
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    assert!(
        !m.presses().iter().any(|p| p.ends_with(" enter")),
        "{:#?}",
        m.requests
    );
    assert!(
        lines.iter().any(|l| l.contains(
            "the long-term policy text's row reads unticked: its answer \
             would be none"
        )),
        "{lines:#?}"
    );
}

/// A checked option on a multi-select under a text priority is a person's (a text answer
/// toggles none): handed over.
#[test]
fn a_checked_option_hands_a_multi_select_over_under_a_text_priority() {
    let screens = multi_screens(LONG_TERM_TEXT);
    let checked: Vec<String> = screens[3]
        .iter()
        .map(|r| r.replace("1. [ ] Cheese", "1. [✔] Cheese"))
        .collect();
    let mut m = texting(vec![checked]);
    m.vanish_after = Some(0);
    let _ = hosted_run(&mut m, &hosted_by(LONG), None);
    assert!(m.writes().is_empty(), "{:#?}", m.requests);
    let badge = badge(&m).expect("handed over");
    assert!(
        badge.contains("option 1 is checked, and the long-term policy answers this question"),
        "{badge}"
    );
}

/// The memory is recorded when the text may have been written: after a partial write or a lost
/// answer the question is remembered as holding the loop's text, so the next decision hands it
/// over (the text gone, or never taken) instead of writing it again. A text the server did not
/// write (`ERR busy sink`) is not remembered and is written once the sink clears. Sends: one,
/// one, one, two. The decision after the partial write comes after an outage, which is what
/// makes the dialog still showing a new point.
#[test]
fn a_text_in_doubt_is_remembered_and_never_written_again_one_not_written_is_not() {
    let (opt1, _, free, typed) = single_screens(LONG_TERM_TEXT);
    // The field empty on the next decision, the focus elsewhere.
    let moved = focused_on(&free, "2. Blue");
    let gone = "the policy text the supervisor wrote is gone";
    let not_taken = "the dialog did not take the policy text: a text is never written twice";
    for (tag, reply, after, sends_want, why) in [
        (
            "partial",
            err("write failed partial accepted=12"),
            vec![moved.clone(), moved.clone(), opt1.clone()],
            1,
            Some(gone),
        ),
        (
            "lost",
            closed(),
            vec![moved.clone(), moved.clone(), opt1.clone()],
            1,
            Some(gone),
        ),
        (
            "lost-unchanged",
            closed(),
            vec![free.clone()],
            1,
            Some(not_taken),
        ),
        (
            "busy",
            err("busy sink"),
            vec![free.clone(), typed.clone(), idle_screen()],
            2,
            None,
        ),
    ] {
        let mut screens = vec![free.clone()];
        screens.extend(after);
        let mut m = texting(screens);
        m.send_replies.push_back(reply);
        m.vanish_after = Some(1);
        if tag == "partial" {
            // The first wait after the hand-over (the reconcile's `meta`
            // uncounted): its connection drops.
            m.by_index.insert(11, closed());
        }
        let _ = hosted_run(&mut m, &hosted_by(LONG), None);
        if tag == "partial" {
            assert!(
                m.requests[12].starts_with("await gone "),
                "{tag}: the outage fell on the wait after the hand-over: {:#?}",
                m.requests
            );
        }
        assert_eq!(sends(&m).len(), sends_want, "{tag}: {:#?}", m.requests);
        if let Some(why) = why {
            assert!(
                m.requests.iter().any(|r| {
                    r.starts_with("meta set attention owner=supervisor ") && r.contains(why)
                }),
                "{tag}: handed over — {why}: {:#?}",
                m.requests
            );
        }
    }
}

/// The Enter goes on the field as decided: on a host that stamps no person (so long-term
/// answers with the recommended option), a person's text appearing in the field while the
/// focus moves means no Enter, handed over. Negative control: the field left as it was, the
/// same moves end in the Enter.
#[test]
fn an_enter_never_goes_after_the_field_changed_under_the_moves() {
    use aterm_phase::prompt::fixtures as f;
    let free = q(f::QUESTION_283_FREE_TEXT_FOCUSED);
    let person = q(f::QUESTION_283_TYPED_PERSON);
    for (changed, enters) in [(true, 0), (false, 1)] {
        let on_two = focused_on(if changed { &person } else { &free }, "2. Blue");
        let on_one = focused_on(if changed { &person } else { &free }, "1. Red");
        // The first read waits out the stand-in quiet; the second decides.
        let mut m = questioned(vec![
            free.clone(),
            free.clone(),
            on_two,
            on_one,
            idle_screen(),
        ]);
        m.human = vec![];
        let lines = supervised(
            &mut m,
            &answering_by(LONG),
            &format!("field-changed-{changed}"),
        );
        assert_eq!(
            m.presses().iter().filter(|p| p.ends_with(" enter")).count(),
            enters,
            "changed={changed}: {:#?}",
            m.requests
        );
        assert!(
            sends(&m).is_empty(),
            "changed={changed}: no text on a host that cannot fence one"
        );
        if changed {
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains("the free-text row changed since the decision read")),
                "{lines:#?}"
            );
        }
    }
}

/// On a multi-select the read-back wants the row ticked (it ticks itself as
/// the text lands): a text that reads back exactly but unticked would not be
/// submitted, so it is handed over at once, never taken for typed.
#[test]
fn a_multi_select_text_that_reads_back_unticked_is_handed_over_at_once() {
    let mut screens = multi_screens(LONG_TERM_TEXT);
    let unticked: Vec<String> = screens[4]
        .iter()
        .map(|r| r.replace("[✔]", "[ ]").replace("☒ Toppings", "☐ Toppings"))
        .collect();
    screens.truncate(4);
    screens.push(unticked);
    let mut m = texting(screens);
    let lines = supervised(&mut m, &answering_by(LONG), "text-read-back-unticked");
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    assert!(
        lines.iter().any(|l| l.contains(
            "the policy text did not read back as written (its \
             multi-select row reads unticked): a text is never written twice"
        )),
        "{lines:#?}"
    );
}

/// The re-ask pause comes before the text: the same question asked a third time after two
/// answers (the text and its Enter are one answer) waits the pause before any key, so nothing
/// of a third answer is written while the session ends in the pause. Never handed over.
/// Single- and multi-select.
#[test]
fn a_question_asked_a_third_time_under_a_text_word_waits_its_pause_before_its_text() {
    let (opt1, opt2, free, typed) = single_screens(LONG_TERM_TEXT);
    let single = vec![opt1.clone(), opt2, free, typed, busy_screen()];
    let mut multi = multi_screens(LONG_TERM_TEXT);
    multi.pop();
    multi.push(busy_screen());
    let multi_first = multi[0].clone();
    let text = format!("text:{LONG_TERM_TEXT}");
    for (tag, dialog, third, once) in [
        (
            "single",
            single,
            opt1,
            vec!["down", "down", text.as_str(), "enter"],
        ),
        (
            "multi",
            multi,
            multi_first,
            vec![
                "down",
                "down",
                "down",
                text.as_str(),
                "down",
                "enter",
                "enter",
            ],
        ),
    ] {
        let mut screens = dialog.clone();
        screens.extend(dialog);
        screens.push(third);
        let mut m = texting(screens);
        m.vanish_after = Some(0);
        let (jdir, journal) = journal_file(&format!("text-reask-{tag}"));
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..hosted_by(LONG)
        };
        let _ = hosted_run(&mut m, &opts, None);
        let twice: Vec<&str> = once.iter().chain(once.iter()).copied().collect();
        assert_eq!(m.writes(), twice, "{tag}: {:#?}", m.requests);
        let (records, _) = journal_records(&journal);
        assert!(
            records.iter().any(|r| r.line.starts_with("WAITING ")
                && r.line.contains(&format!("rule={RULE_ANSWER_LONG_TERM} "))
                && r.line
                    .contains("the same question came back after 2 answers")
                && r.line.contains("re-asking it): answered again in 250 ms")),
            "{tag}: {records:#?}"
        );
        assert_eq!(badge(&m), None, "{tag}: never handed over");
        let _ = std::fs::remove_dir_all(&jdir);
    }
}

/// A recommended fallback begun is finished: a multi-select whose text is estimated not to fit
/// is answered with its recommended option (Cheese toggled), and when the pane then has room
/// the fit is not estimated again under the loop's own toggle: the tab stays on its
/// recommended path and is never handed over for the check the loop made itself (a check it
/// did not make is a person's: `a_checked_option_hands_a_multi_select_over_under_a_text_priority`).
#[test]
fn a_fallback_begun_on_a_tight_pane_is_finished_when_the_pane_grows() {
    use aterm_phase::prompt::fixtures as f;
    let free = q(f::QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED);
    let tight = |rows: &[String]| -> Vec<String> {
        let top = rows
            .iter()
            .position(|r| !r.is_empty() && r.chars().all(|c| c == '─'))
            .expect("the top rule");
        let last = rows
            .iter()
            .rposition(|r| !r.trim().is_empty())
            .expect("the footer");
        rows[top..=last].to_vec()
    };
    let roomy: Vec<String> = free
        .iter()
        .map(|r| r.replace("1. [ ] Cheese", "1. [✔] Cheese"))
        .collect();
    let mut m = texting(vec![
        tight(&focused_on(&free, "1. [ ] Cheese")),
        focused_on(&roomy, "1. [✔] Cheese"),
        focused_on(&roomy, "2. [ ] Olives"),
        focused_on(&roomy, "3. [ ] Basil"),
        roomy.clone(),
        focused_on(&roomy, "Submit"),
        q(f::QUESTION_283_MULTISELECT_REVIEW),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(1);
    let lines = hosted_run(&mut m, &hosted_by(LONG), None);
    assert_eq!(
        m.writes(),
        ["enter", "down", "down", "down", "down", "enter", "enter"],
        "{:#?}",
        m.requests
    );
    assert!(sends(&m).is_empty(), "{:#?}", m.requests);
    assert!(badge(&m).is_none(), "never handed over: {:#?}", m.requests);
    let told: Vec<&String> = lines.iter().filter(|l| l.starts_with("CHOSE ")).collect();
    assert!(
        told.len() == 1
            && told[0].contains("policy=recommended ")
            && told[0].contains("(recommended: the answer would not fit the pane"),
        "{lines:#?}"
    );
}

/// `rows` (a capture's dialog) with `extra` more description rows under its
/// option 1, at the foot of a pane `pane` rows tall: a dialog taller than the
/// 40-row tail the loop reads.
fn tall_dialog(capture: &str, extra: usize, pane: usize) -> Vec<String> {
    let rows = q(capture);
    let top = rows
        .iter()
        .position(|r| !r.is_empty() && r.chars().all(|c| c == '─'))
        .expect("the top rule");
    let last = rows
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .expect("the footer");
    let mut dialog = rows[top..=last].to_vec();
    let under = dialog
        .iter()
        .position(|r| r == "     A warm colour")
        .expect("option 1's description");
    for k in 0..extra {
        dialog.insert(under + 1 + k, "     and warmer still".to_string());
    }
    let mut out = vec![String::new(); pane.saturating_sub(dialog.len())];
    out.extend(dialog);
    out
}

/// The fit is measured against the pane, not the 40-row tail the loop reads
/// (`text --json tail=40`; `dims.rows` is the pane): a single-select 38 rows
/// tall at the foot of a 60-row pane has room, so under `long-term` its text
/// is written and entered. Negative control: the same dialog in a 40-row pane
/// has no room for the text, so its recommended option, said.
#[test]
fn the_fit_is_measured_against_the_pane_not_the_tail_read() {
    use aterm_phase::prompt::fixtures::{self as f, rewrap_free_text};
    for (pane, want_text) in [(60, true), (40, false)] {
        let free = tall_dialog(f::QUESTION_283_FREE_TEXT_FOCUSED, 24, pane);
        let tail = &free[free.len() - 40..];
        let read = aterm_phase::parse_prompt_v2(tail).expect("the dialog in the tail");
        assert!(
            !read.head_off_screen,
            "PRECONDITION: the tail holds its head"
        );
        assert_eq!(
            read.span.1 - read.span.0 + 2,
            38,
            "PRECONDITION: a dialog 38 rows tall: {tail:#?}"
        );
        // The text typed, the dialog grown upward in the same pane (c3-04 to
        // c3-05): its rows above scroll off the top.
        let top = free
            .iter()
            .position(|r| !r.is_empty() && r.chars().all(|c| c == '─'))
            .expect("the top rule");
        let mut typed = free[..top].to_vec();
        typed.extend(rewrap_free_text(&free.join("\n"), LONG_TERM_TEXT, 120));
        typed.drain(..typed.len() - pane);
        let (screens, writes) = if want_text {
            (
                vec![free, typed, busy_screen(), idle_screen()],
                vec![format!("text:{LONG_TERM_TEXT}"), "enter".to_string()],
            )
        } else {
            (
                vec![
                    free.clone(),
                    focused_on(&free, "2. Blue"),
                    focused_on(&free, "1. Red"),
                    busy_screen(),
                    idle_screen(),
                ],
                vec!["up".to_string(), "up".to_string(), "enter".to_string()],
            )
        };
        let mut m = texting(screens);
        m.vanish_after = Some(1);
        // The cursor on the focused row, where Claude Code parks it, inside the
        // tail. The scripted default (row 10) sits above a 60-row pane's tail,
        // and `tail_misses_the_live_rows` would re-read such a screen whole,
        // serving the next scripted screen.
        m.cursor_on_caret = true;
        let lines = hosted_run(&mut m, &hosted_by(LONG), None);
        assert_eq!(m.writes(), writes, "pane {pane}: {:#?}", m.requests);
        let told: Vec<&String> = lines.iter().filter(|l| l.starts_with("CHOSE ")).collect();
        assert_eq!(told.len(), 1, "pane {pane}: {lines:#?}");
        assert_eq!(
            told[0].contains("(recommended: the answer would not fit the pane"),
            !want_text,
            "pane {pane}: {}",
            told[0]
        );
    }
}

/// A controller's option over text in the field (`aterm drive answer`): an Enter on a
/// single-select's option submits no text, so it is pressed over a person's text; a
/// multi-select's button would send the field's text with the options, so that answer is
/// refused before any key.
#[test]
fn a_controllers_option_over_text_in_the_field_is_pressed_and_a_multi_selects_is_refused_first() {
    use aterm_phase::prompt::fixtures as f;
    let typed = q(f::QUESTION_FREE_TEXT_TYPED);
    let mut m = questioned(vec![
        typed,
        q(f::QUESTION_FREE_TEXT_TYPED_AWAY),
        idle_screen(),
    ]);
    let (line, code) = controller_answer(&mut m, "3", None);
    assert_eq!(
        (line.as_str(), code),
        ("ANSWERED @s-w enters=1 answer=High contrast next=none\n", 0),
        "{:#?}",
        m.requests
    );
    assert_eq!(m.writes(), ["up", "enter"], "{:#?}", m.requests);

    let person = q(f::QUESTION_283_TYPED_PERSON);
    let mut m = questioned(vec![
        person.clone(),
        focused_on(&person, "2. Blue"),
        focused_on(&person, "1. Red"),
        idle_screen(),
    ]);
    let (line, code) = controller_answer(&mut m, "1", None);
    assert_eq!(
        (line.as_str(), code),
        (
            "ANSWERED @s-w enters=1 answer=Red (Recommended) next=none\n",
            0
        ),
        "{:#?}",
        m.requests
    );
    assert_eq!(m.writes(), ["up", "up", "enter"], "{:#?}", m.requests);

    let mut m = questioned(vec![q(f::QUESTION_283_MULTISELECT_TYPED_PERSON)]);
    let (line, code) = controller_answer(&mut m, "3", None);
    assert!(
        line.starts_with("REFUSED @s-w ") && line.contains("free-text row"),
        "{line}"
    );
    assert_eq!(code, 2, "{line}");
    assert!(m.writes().is_empty(), "{:#?}", m.requests);
}

/// `rows`' dialog alone — its top rule to its last row drawn — as the whole
/// pane: no room for a text to grow in.
fn dialog_only(rows: &[String]) -> Vec<String> {
    let top = rows
        .iter()
        .position(|r| !r.is_empty() && r.chars().all(|c| c == '─'))
        .expect("the top rule");
    let last = rows
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .expect("the footer");
    rows[top..=last].to_vec()
}

/// Tier-1 for a tab that falls back (`SupervisorQuestionText`, `Text = 1`, the model's
/// `FitLostOnIt` / `FitLostOffIt`): the real loop under `long-term`, bound as
/// `tier1_the_policy_text_goes_only_where_the_model_allows`, over a lone single-select too
/// short for the text (`↑ ↑`, Enter on `Red (Recommended)`, nothing written) and a dialog of a
/// typed tab then a multi-select tab too short for it (its recommended Cheese toggled, `↓`
/// four times onto `Submit`, Enter, the review's Enter).
#[test]
fn tier1_a_tab_that_falls_back_goes_only_where_the_model_allows() {
    use aterm_phase::prompt::fixtures as f;
    // 1. The lone single-select, too short.
    let (opt1, opt2, free, _) = single_screens(LONG_TERM_TEXT);
    let mut m = texting(vec![
        dialog_only(&free),
        dialog_only(&opt2),
        dialog_only(&opt1),
        idle_screen(),
    ]);
    let (st, keys, _) = q_bound_with(
        &mut m,
        &text_model(1, 0, 0),
        0,
        Some(LONG),
        &answering_by(LONG),
        |_| {},
    );
    assert_eq!(keys, ["up", "up", "enter"], "{:#?}", m.requests);
    assert_eq!(st["tab"], 2, "the dialog closed: {st:?}");
    assert!(sends(&m).is_empty(), "{:#?}", m.requests);

    // 2. A typed tab, then a tab that falls back.
    let first_typed = with_text(f::QUESTION_283_TABS_FIRST_TYPED_LONG_TERM, LONG_TERM_TEXT);
    let second = q(f::QUESTION_283_TABS_SECOND);
    let cheese: Vec<String> = second
        .iter()
        .map(|r| r.replace("1. [ ] Cheese", "1. [✔] Cheese"))
        .collect();
    let mut m = texting(vec![
        q(f::QUESTION_283_TABS_FIRST),
        q(f::QUESTION_283_TABS_FIRST_DOWN),
        q(f::QUESTION_283_TABS_FIRST_FREE_TEXT_FOCUSED),
        first_typed,
        dialog_only(&second),
        dialog_only(&cheese),
        dialog_only(&focused_on(&cheese, "2. [ ] Olives")),
        dialog_only(&focused_on(&cheese, "3. [ ] Basil")),
        dialog_only(&focused_on(&cheese, "4. [ ] Type something")),
        dialog_only(&focused_on(&cheese, "Submit")),
        q(f::QUESTION_283_TABS_REVIEW),
        idle_screen(),
    ]);
    let (st, keys, _) = q_bound_with(
        &mut m,
        &text_model(3, 1, 0),
        0,
        Some(LONG),
        &answering_by(LONG),
        |_| {},
    );
    assert_eq!(
        keys,
        [
            "down", "down", "text", "enter", "enter", "down", "down", "down", "down", "enter",
            "enter"
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(st["tab"], 4, "the dialog closed: {st:?}");
    assert_eq!(
        sends(&m).len(),
        1,
        "one text, on the typed tab: {:#?}",
        m.requests
    );
}

// ---- Codex's question: the text as a note under `None of the above` --------

/// A Codex 0.163.0 question capture's rows as a `tail=40` read serves them:
/// its last 40. Whole, the 50-row capture's blank gap sends each read to a
/// second, whole read, which a script answers with its NEXT screen.
fn cx(text: &str) -> Vec<String> {
    let rows = aterm_phase::prompt::fixtures::screen(text);
    rows[rows.len().saturating_sub(40)..].to_vec()
}

/// The hosted loop over `m` under `opts`, its ledger at `ledger`: the lines
/// it printed.
fn codex_hosted(m: &mut Mock, opts: &SuperviseOpts, ledger: &Path) -> Vec<String> {
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(m, None);
    s.set_approval_env(owner_env());
    s.set_approval_ledger(Some(ledger.to_path_buf()));
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = s.run_hosted(opts, stop, &mut out);
    String::from_utf8_lossy(&out)
        .lines()
        .map(str::to_string)
        .collect()
}

/// The `CHOSE` lines of a loop's output.
fn chose_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.starts_with("CHOSE "))
        .cloned()
        .collect()
}

/// MUSE CODE'S QUESTION UNDER `long-term` THROUGH THE WINDOW'S LOOP (the
/// screens measured on 1.4.10): two arrows from `Red (Recommended)` onto
/// `None of the above`, Tab, the long-term text written into the empty note
/// with one fenced `send`, Enter only on the next read, the note holding
/// exactly that text — then, on the REVIEW the note draws, Enter on `⟩ Submit
/// answers`: `[down, down, tab, text, enter, enter]`, every key `key
/// if-human=0 if-gen=…`. Told once as the latched word's choice: `CHOSE …
/// policy=long-term <question> → None of the above (the long-term policy
/// text as its note)`. NEGATIVE CONTROL: a review holding other words than
/// the loop's note is handed over — its Enter never pressed — and nothing is
/// told.
#[test]
fn a_muse_question_is_answered_by_a_note_and_its_review_sent() {
    use aterm_phase::muse::fixtures as u;
    let screens = |review: &str| {
        vec![
            cx(u::QUESTION),
            cx(u::QUESTION_DOWN),
            cx(u::QUESTION_NONE),
            cx(u::QUESTION_NOTE_OPEN),
            cx(u::QUESTION_NOTE_LONG),
            cx(review),
            cx(u::QUESTION_NOTE_LONG_ANSWERED),
        ]
    };
    let muse_texting = |screens: Vec<Vec<String>>| {
        let mut m = texting(screens);
        m.program = "muse";
        m.vanish_after = Some(1);
        m
    };
    let (ldir, ledger) = ledger_file("muse-note");
    let mut m = muse_texting(screens(u::QUESTION_NOTE_LONG_REVIEW));
    let lines = codex_hosted(&mut m, &hosted_by(LONG), &ledger);
    assert_eq!(
        m.writes(),
        [
            "down".to_string(),
            "down".to_string(),
            "tab".to_string(),
            format!("text:{LONG_TERM_TEXT}"),
            "enter".to_string(),
            "enter".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    for p in m.presses() {
        assert!(p.starts_with("key if-human=0 if-gen=1."), "{p}");
    }
    let presses = m.presses();
    assert!(
        presses
            .last()
            .is_some_and(|p| p.ends_with(&format!(" if={} enter", guarded("  ⟩ Submit answers")))),
        "{presses:#?}"
    );
    let told = chose_lines(&lines);
    assert_eq!(told.len(), 1, "{lines:#?}");
    assert!(
        told[0].contains(&format!(" rule={RULE_ANSWER_LONG_TERM} policy=long-term "))
            && told[0].ends_with(
                "Which color should the probe use? → None of the above (the long-term policy text \
                 as its note)"
            ),
        "{}",
        told[0]
    );
    let _ = std::fs::remove_dir_all(&ldir);

    // The control: a review of other words is a person's.
    let (ldir, ledger) = ledger_file("muse-note-other");
    let mut m = muse_texting(screens(u::QUESTION_NOTE_REVIEW));
    let lines = codex_hosted(&mut m, &hosted_by(LONG), &ledger);
    assert_eq!(
        m.writes(),
        [
            "down".to_string(),
            "down".to_string(),
            "tab".to_string(),
            format!("text:{LONG_TERM_TEXT}"),
            "enter".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    assert!(chose_lines(&lines).is_empty(), "{lines:#?}");
    let _ = std::fs::remove_dir_all(&ldir);
}
