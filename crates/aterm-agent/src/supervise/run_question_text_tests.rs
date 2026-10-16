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

/// The CLI loop answering questions under `p` (`--question-priority`).
fn answering_by(p: QuestionPriority, notes: Option<PathBuf>) -> SuperviseOpts {
    let mut opts = answering(30, notes);
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
    let (ndir, notes) = notes_file("text-single-chose");
    let opts = SuperviseOpts {
        notes: Some(notes.clone()),
        ..hosted_by(LONG)
    };
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
    let noted = read_notes(&ndir, &notes);
    assert!(
        noted.iter().any(|l| l.contains(&format!(
            "chose ({RULE_ANSWER_LONG_TERM}, long-term): Which colour do you prefer? → decided by \
             the worker (long-term)"
        ))),
        "{noted:#?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);
}

/// A still screen over [`Mock`] (which moves its screen on every read): the screen moves only
/// when a `key` or `send` was written since the last read, an `await seq` runs out its whole
/// timeout, and `await idle <ms>` sleeps `ms` first, as the real server does on a quiescent
/// surface. From the first Enter on it is the mock's own again.
struct Still<'m> {
    mock: &'m mut Mock,
    /// A `key` or `send` was written since the last read.
    moved: bool,
    last: Option<CtlReply>,
    entered: bool,
    /// Every request with when it went, from the first.
    at: Vec<(String, Duration)>,
    started: Instant,
}

impl<'m> Still<'m> {
    fn new(mock: &'m mut Mock) -> Self {
        Self {
            mock,
            moved: true,
            last: None,
            entered: false,
            at: Vec::new(),
            started: Instant::now(),
        }
    }
}

impl Ctl for Still<'_> {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let line = args.join(" ");
        self.at.push((line.clone(), self.started.elapsed()));
        let verb = verb_of(&line).to_string();
        if self.entered {
            return self.mock.call(args);
        }
        let ms_after = |word: &str| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words
                .iter()
                .position(|w| *w == word)
                .and_then(|k| words.get(k + 1))
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_millis)
        };
        match verb.as_str() {
            "text" if !self.moved => {
                if let Some(r) = &self.last {
                    return Ok(r.clone());
                }
            }
            "await" if args.get(1) == Some(&"seq") && !self.moved => {
                std::thread::sleep(ms_after("timeout").unwrap_or_default());
                return Ok(timeout());
            }
            "await" if args.get(1) == Some(&"idle") => {
                std::thread::sleep(ms_after("idle").unwrap_or_default());
            }
            _ => {}
        }
        let r = self.mock.call(args)?;
        match verb.as_str() {
            "text" => {
                self.moved = false;
                self.last = Some(r.clone());
            }
            "key" | "send" => {
                self.moved = true;
                self.entered = line.ends_with(" enter");
            }
            _ => {}
        }
        Ok(r)
    }
}

/// The read-back decides the Enter: the text read back exactly after the screen held still for
/// the read's settle (`await idle 500`) is the read the Enter is decided on, with no second read
/// or box settle (the typed text opens no dialog, and 500 ms still is longer than
/// [`BOX_SETTLE`]). The Enter costs `status`, `meta` and the fenced key. Negative control: a
/// read-back whose settle ran out is decided on a fresh look.
#[test]
fn the_enter_on_a_read_back_text_costs_status_meta_and_the_key() {
    let (opt1, opt2, free, typed) = single_screens(LONG_TERM_TEXT);
    let run = |idle_never: bool| {
        let mut m = texting(vec![
            opt1.clone(),
            opt2.clone(),
            free.clone(),
            typed.clone(),
            busy_screen(),
            idle_screen(),
        ]);
        m.vanish_after = Some(1);
        m.idle_never = idle_never;
        let mut still = Still::new(&mut m);
        {
            let mut s = Session::new(&mut still, None);
            s.set_approval_env(owner_env());
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let _ = s.run_hosted(&hosted_by(LONG), stop, &mut Vec::new());
        }
        let at = std::mem::take(&mut still.at);
        drop(still);
        (m, at)
    };
    let (m, at) = run(false);
    assert_eq!(
        m.writes(),
        [
            "down".to_string(),
            "down".to_string(),
            format!("text:{LONG_TERM_TEXT}"),
            "enter".to_string()
        ],
        "{at:#?}"
    );
    let send = at
        .iter()
        .position(|(r, _)| r.starts_with("send "))
        .expect("the send");
    let back = send
        + at[send..]
            .iter()
            .position(|(r, _)| r.starts_with("text "))
            .expect("the read-back");
    let enter = at
        .iter()
        .position(|(r, _)| r.starts_with("key ") && r.ends_with(" enter"))
        .expect("the enter");
    let between: Vec<&str> = at[back + 1..enter]
        .iter()
        .map(|(r, _)| r.as_str())
        .collect();
    assert_eq!(between, ["status", "meta"], "{at:#?}");
    let took = at[enter].1.saturating_sub(at[back].1);
    eprintln!(
        "read-back #{back} → enter #{enter}: {} requests, {} ms",
        enter - back - 1,
        took.as_millis()
    );
    // The Enter is fenced on the read-back's own generation and row.
    let (enter_line, _) = &at[enter];
    let typed_row = typed
        .iter()
        .find(|r| r.starts_with("❯ 3. "))
        .expect("the field's row");
    assert!(
        enter_line.starts_with("key if-human=0 if-gen=1.")
            && enter_line.ends_with(&format!(" if={} enter", guarded(typed_row))),
        "{enter_line}"
    );

    // Negative control: a screen that never holds still gets a fresh look.
    let (m, at) = run(true);
    assert_eq!(
        m.writes().last().map(String::as_str),
        Some("enter"),
        "{at:#?}"
    );
    let send = at
        .iter()
        .position(|(r, _)| r.starts_with("send "))
        .expect("the send");
    let back = send
        + at[send..]
            .iter()
            .position(|(r, _)| r.starts_with("text "))
            .expect("the read-back");
    let enter = at
        .iter()
        .position(|(r, _)| r.starts_with("key ") && r.ends_with(" enter"))
        .expect("the enter");
    assert!(
        at[back + 1..enter]
            .iter()
            .any(|(r, _)| r.starts_with("text ")),
        "an unsettled read-back is read again before the Enter: {at:#?}"
    );
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
        &aterm_spec::derive::supervisor_question_text_model(),
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
        let (st, keys, _) =
            q_bound_with(&mut m, &model, 0, Some(p), &answering_by(p, None), |_| {});
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
    let (st, keys, _) = q_bound_with(
        &mut m,
        &model,
        0,
        Some(LONG),
        &answering_by(LONG, None),
        |_| {},
    );
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
            &answering_by(LONG, None),
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

/// The CLI loop over `m` with `notes`: the notes' lines.
fn supervised(m: &mut Mock, opts: &SuperviseOpts, dir: &Path, notes: &Path) -> Vec<String> {
    {
        let mut s = session(m, None);
        s.set_approval_env(owner_env());
        s.supervise(opts).expect("supervise");
    }
    read_notes(dir, notes)
}

/// The badge the loop raised, if any (`meta set attention owner=supervisor`).
fn badge(m: &Mock) -> Option<String> {
    m.requests
        .iter()
        .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
        .map(str::to_string)
}

/// A lone multi-select: the text, then `↓` off the field onto `Submit` and Enter there, then
/// the review's Enter; never an Enter in the field (it unticks the row), never a Tab.
#[test]
fn a_lone_multi_select_is_typed_moved_off_and_submitted_by_its_button() {
    let mut m = texting(multi_screens(LONG_TERM_TEXT));
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.supervise(&answering_by(LONG, None)).expect("supervise");
    drop(s);
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
    assert!(
        enters[1].contains(&guarded("❯ 1. Submit answers")),
        "{enters:#?}"
    );
    // The `↓` off the field is guarded on the field's first row as drawn.
    let down = m.presses()[3];
    assert!(
        down.contains("if=^❯\\x204\\.\\x20\\[✔\\]\\x20Decide\\x20yourself:")
            && down.ends_with(" down"),
        "{down}"
    );
    assert!(
        !m.presses().iter().any(|p| p.ends_with(" tab")),
        "{:#?}",
        m.presses()
    );
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
    let (dir, notes) = notes_file("text-prefix");
    let mut m = texting(vec![free, prefix.clone(), prefix]);
    let lines = supervised(
        &mut m,
        &answering_by(LONG, Some(notes.clone())),
        &dir,
        &notes,
    );
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(
        m.requests.iter().any(|r| r.starts_with("await idle 2000 ")),
        "one wait for the screen to hold still: {:#?}",
        m.requests
    );
    assert!(
        lines.iter().any(|l| l.contains(
            "handed to the manager (the policy text read back short of what was written: a \
             text is never written twice)"
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
    let (dir, notes) = notes_file("text-gone");
    let (ldir, ledger) = ledger_file("text-gone");
    let mut m = texting(vec![free.clone(), typed]);
    // The field emptied between the read-back and the Enter.
    m.swap_at_key = Some(free);
    let lines = {
        let mut s = session(&mut m, None);
        s.set_approval_env(owner_env());
        s.set_approval_ledger(Some(ledger.clone()));
        s.supervise(&answering_by(LONG, Some(notes.clone())))
            .expect("supervise");
        drop(s);
        read_notes(&dir, &notes)
    };
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
    let _ = std::fs::remove_dir_all(&ldir);
    assert!(
        lines.iter().any(|l| l.contains(
            "handed to the manager (the policy text the supervisor wrote is gone from its \
             free-text row)"
        )),
        "{lines:#?}"
    );
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

/// A text the field never shows (after the worker's reaction and after the screen held still):
/// one `send`, no Enter, handed over.
#[test]
fn a_text_the_dialog_never_took_is_handed_over_never_written_twice() {
    let (_, _, free, _) = single_screens(LONG_TERM_TEXT);
    let (dir, notes) = notes_file("text-not-taken");
    let mut m = texting(vec![free]);
    let lines = supervised(
        &mut m,
        &answering_by(LONG, Some(notes.clone())),
        &dir,
        &notes,
    );
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(
        lines.iter().any(|l| l.contains(
            "handed to the manager (the dialog did not take the policy text: a text is never \
             written twice)"
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
            ..answering_by(LONG, None)
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
    let _ = s.supervise(&answering_by(LONG, None));
    drop(s);
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
}

/// A person the server sees at the send (its person count moved past the read's): `send
/// if-human=0` is skipped `reason=person`, and the loop yields, waits out their quiet, and
/// writes the text once on the new count (`if-human=1`), then presses Enter.
#[test]
fn a_person_the_server_sees_at_the_send_gets_the_question_first_then_one_text() {
    let (opt1, opt2, free, typed) = single_screens(LONG_TERM_TEXT);
    let mut m = texting(vec![
        opt1,
        opt2,
        free.clone(),
        free.clone(),
        free,
        typed,
        idle_screen(),
    ]);
    m.human = vec!["null", "null", "null", "200", "15000"];
    m.person_at_send = 1;
    let (ldir, ledger) = ledger_file("text-person-at-send");
    let mut s = session(&mut m, None);
    s.set_approval_env(owner_env());
    s.set_approval_ledger(Some(ledger.clone()));
    s.supervise(&answering_by(LONG, None)).expect("supervise");
    drop(s);
    let text = format!("text:{LONG_TERM_TEXT}");
    assert_eq!(
        m.writes(),
        ["down", "down", &text, &text, "enter"],
        "the first send skipped, the second written: {:#?}",
        m.requests
    );
    let sent = sends(&m);
    assert!(
        sent[0].starts_with("send if-human=0 ") && sent[1].starts_with("send if-human=1 "),
        "each send names the count of the read it was decided on: {sent:#?}"
    );
    assert_eq!(m.person_skipped.len(), 1, "{:#?}", m.requests);
    let rows = ledger_rows(&ledger);
    assert!(
        rows.iter().any(|r| r.contains("\"decision\":\"skipped\"")
            && r.contains("a person gave the session input while the answer was keyed")),
        "{rows:#?}"
    );
    assert!(badge(&m).is_none(), "never handed over: {:#?}", m.requests);
    let _ = std::fs::remove_dir_all(&ldir);
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
    let (st, keys, raced) = q_bound_with(
        &mut m,
        &model,
        0,
        Some(LONG),
        &answering_by(LONG, None),
        |_| {},
    );
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
        let (ndir, notes) = notes_file("text-person-after-notes");
        let mut opts = SuperviseOpts {
            journal: Some(journal.clone()),
            notes: Some(notes.clone()),
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
        let noted = read_notes(&ndir, &notes);
        assert!(
            !noted.iter().any(|l| l.contains("chose (")),
            "{case}: {noted:#?}"
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
    let (dir, notes) = notes_file("text-unticked");
    let mut m = texting(screens);
    // Unticked after the three moves onto the field, at the move off it.
    m.swap_at_key = Some(unticked);
    m.swap_after_keys = 3;
    let lines = supervised(
        &mut m,
        &answering_by(LONG, Some(notes.clone())),
        &dir,
        &notes,
    );
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    assert!(
        !m.presses().iter().any(|p| p.ends_with(" enter")),
        "{:#?}",
        m.requests
    );
    assert!(
        lines.iter().any(|l| l.contains(
            "handed to the manager (the long-term policy text's row reads unticked: its answer \
             would be none)"
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

/// A pane too short for the text (its growth would cut the dialog's head off): that tab is
/// answered with its recommended option (`↑ ↑`, Enter on `Red (Recommended)`), and told so.
#[test]
fn a_tab_too_short_for_the_text_falls_back_to_its_recommended_option() {
    let (opt1, opt2, free, _) = single_screens(LONG_TERM_TEXT);
    let dialog = |rows: &[String]| -> Vec<String> {
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
    let mut m = texting(vec![
        dialog(&free),
        dialog(&opt2),
        dialog(&opt1),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(1);
    let lines = hosted_run(&mut m, &hosted_by(LONG), None);
    assert_eq!(m.writes(), ["up", "up", "enter"], "{:#?}", m.requests);
    let told: Vec<&String> = lines.iter().filter(|l| l.starts_with("CHOSE ")).collect();
    assert!(
        told.len() == 1
            && told[0]
                .ends_with("→ Red (Recommended) (recommended: the answer would not fit the pane)"),
        "{lines:#?}"
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
        let (dir, notes) = notes_file(&format!("field-changed-{changed}"));
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
            &answering_by(LONG, Some(notes.clone())),
            &dir,
            &notes,
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
                lines.iter().any(|l| l.contains(
                    "handed to the manager (the free-text row changed since the decision read)"
                )),
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
    let (dir, notes) = notes_file("text-read-back-unticked");
    let mut m = texting(screens);
    let lines = supervised(
        &mut m,
        &answering_by(LONG, Some(notes.clone())),
        &dir,
        &notes,
    );
    assert_eq!(sends(&m).len(), 1, "{:#?}", m.requests);
    assert!(
        lines.iter().any(|l| l.contains(
            "handed to the manager (the policy text did not read back as written (its \
             multi-select row reads unticked): a text is never written twice)"
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
        &answering_by(LONG, None),
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
        &answering_by(LONG, None),
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

/// The measured question with its `›` cursor on answer `n` (`  › N.`), the
/// rest at the answers' column.
fn codex_focus(rows: &[String], n: u8) -> Vec<String> {
    let want = format!("{n}. ");
    rows.iter()
        .map(|r| match r.strip_prefix("  › ") {
            Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => format!("    {rest}"),
            _ => match r.strip_prefix("    ") {
                Some(rest) if rest.starts_with(&want) => format!("  › {rest}"),
                _ => r.clone(),
            },
        })
        .collect()
}

/// `rows` (the notes field open) with the field holding `text` on one row.
fn codex_noted(rows: &[String], text: &str) -> Vec<String> {
    let mut out = rows.to_vec();
    let at = out
        .iter()
        .rposition(|r| r.as_str() == "  › Add notes")
        .expect("the empty notes field");
    out[at] = format!("  › {text}");
    out
}

/// A scripted Codex host for the note: every fence a typed answer rests on.
fn codex_texting(screens: Vec<Vec<String>>) -> Mock {
    let mut m = texting(screens);
    m.program = "codex";
    m
}

/// The hosted loop over `m` under `opts`, its ledger at `ledger`.
fn codex_hosted(m: &mut Mock, opts: &SuperviseOpts, ledger: &Path) {
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(m, None);
    s.set_approval_env(owner_env());
    s.set_approval_ledger(Some(ledger.to_path_buf()));
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = s.run_hosted(opts, stop, &mut out);
}

/// CODEX'S QUESTION UNDER `long-term` (gap #11, measured on 0.163.0): the
/// loop moves the focus from the recommended answer onto `None of the
/// above` with two ARROWS (never its digit, which would answer at once),
/// presses Tab there, writes the long-term text into the empty notes field
/// with one fenced `send` guarded on `  › Add notes`, and presses Enter only
/// on the next read, the field holding exactly that text: `[down, down, tab,
/// text, enter]`, every key `key if-human=0 if-gen=…`, ledgered under
/// `answer-long-term@v1`. NEGATIVE CONTROLS: under `recommended` the same
/// question is answered by its recommended digit alone; and where the read
/// after the text shows the field holding other words, no Enter is pressed
/// and the question is handed over.
#[test]
fn a_codex_question_is_answered_by_a_note_under_none_of_the_above() {
    use aterm_phase::codex::fixtures as f;
    let dialog = cx(f::QUESTION_0_163);
    let open = cx(f::QUESTION_NOTES_OPEN_0_163);
    let screens = |last: Vec<String>| {
        vec![
            dialog.clone(),
            codex_focus(&dialog, 2),
            cx(f::QUESTION_NONE_FOCUSED_0_163),
            open.clone(),
            last,
            cx(f::QUESTION_NOTES_SUBMITTED_0_163),
        ]
    };
    let (ldir, ledger) = ledger_file("codex-note");
    let mut m = codex_texting(screens(codex_noted(&open, LONG_TERM_TEXT)));
    m.vanish_after = Some(1);
    codex_hosted(&mut m, &hosted_by(LONG), &ledger);
    assert_eq!(
        m.writes(),
        [
            "down".to_string(),
            "down".to_string(),
            "tab".to_string(),
            format!("text:{LONG_TERM_TEXT}"),
            "enter".to_string()
        ],
        "{:#?}",
        m.requests
    );
    for p in m.presses() {
        assert!(p.starts_with("key if-human=0 if-gen=1."), "{p}");
    }
    let send = sends(&m)[0].clone();
    assert!(
        send.starts_with("send if-human=0 if-gen=1.")
            && send.contains(&format!(" if={} -- ", guarded("  › Add notes"))),
        "{send}"
    );
    let rows = ledger_rows(&ledger);
    assert!(
        rows.iter().any(|r| {
            r.contains(&format!("\"rule_id\":\"{RULE_ANSWER_LONG_TERM}\""))
                && r.contains("\"decision\":\"approved\"")
        }),
        "{rows:#?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);

    // The control: `recommended` presses the recommended answer's digit.
    let (ldir, ledger) = ledger_file("codex-note-recommended");
    let mut m = codex_texting(vec![dialog.clone(), cx(f::QUESTION_NOTES_SUBMITTED_0_163)]);
    m.vanish_after = Some(1);
    codex_hosted(&mut m, &hosted_by(QuestionPriority::Recommended), &ledger);
    assert_eq!(m.writes(), ["1".to_string()], "{:#?}", m.requests);
    let _ = std::fs::remove_dir_all(&ldir);

    // The control: the field reads other words after the text: no Enter.
    let (ldir, ledger) = ledger_file("codex-note-other");
    let mut m = codex_texting(screens(codex_noted(&open, "a person's own words")));
    m.vanish_after = Some(1);
    codex_hosted(&mut m, &hosted_by(LONG), &ledger);
    let writes = m.writes();
    assert_eq!(
        writes,
        [
            "down".to_string(),
            "down".to_string(),
            "tab".to_string(),
            format!("text:{LONG_TERM_TEXT}"),
        ],
        "{:#?}",
        m.requests
    );
    let _ = std::fs::remove_dir_all(&ldir);
}

/// CODEX'S QUESTION WHOSE NOTES FIELD NEVER READS OPEN FALLS BACK TO ITS
/// RECOMMENDED ANSWER (gap #11): the loop's Tab on `None of the above` is
/// waited on, and when no read shows the field open, the question's fallback
/// is latched (`QuestionMemory::fell_back`, `NOTES_UNREAD`) and it is
/// decided again — its recommended answer's digit, ledgered under the word's
/// rule with the fallback said (`(recommended: its notes field did not read
/// open after Tab)`). Never a second Tab, never the text. NEGATIVE CONTROL:
/// the same script whose Tab does open the field writes the text and no
/// digit.
#[test]
fn a_codex_question_whose_notes_never_open_falls_back_to_its_recommended_answer() {
    use crate::supervise::policy::question::NOTES_UNREAD;
    use aterm_phase::codex::fixtures as f;
    let dialog = cx(f::QUESTION_0_163);
    let focused = cx(f::QUESTION_NONE_FOCUSED_0_163);
    let submitted = cx(f::QUESTION_DIGIT_SUBMITS_0_163);
    let (ldir, ledger) = ledger_file("codex-note-unread");
    // `None of the above` stands, its field closed, until a fourth key (the
    // two arrows and the Tab are three) reaches the dialog.
    let mut m = codex_texting(vec![
        dialog.clone(),
        codex_focus(&dialog, 2),
        focused.clone(),
        submitted.clone(),
    ]);
    m.key_gates = vec![(2, 3)];
    m.vanish_after = Some(1);
    codex_hosted(&mut m, &hosted_by(LONG), &ledger);
    assert_eq!(
        m.writes(),
        [
            "down".to_string(),
            "down".to_string(),
            "tab".to_string(),
            "1".to_string()
        ],
        "{:#?}",
        m.requests
    );
    let rows = ledger_rows(&ledger);
    assert!(
        rows.iter().any(|r| {
            r.contains(&format!("\"rule_id\":\"{RULE_ANSWER_LONG_TERM}\""))
                && r.contains("\"decision\":\"approved\"")
                && r.contains(&format!("(recommended: {NOTES_UNREAD})"))
        }),
        "{rows:#?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);

    // The control: the Tab opens the field, and the text goes in.
    let (ldir, ledger) = ledger_file("codex-note-read");
    let open = cx(f::QUESTION_NOTES_OPEN_0_163);
    let mut m = codex_texting(vec![
        dialog.clone(),
        codex_focus(&dialog, 2),
        focused,
        open.clone(),
        codex_noted(&open, LONG_TERM_TEXT),
        cx(f::QUESTION_NOTES_SUBMITTED_0_163),
    ]);
    m.vanish_after = Some(1);
    codex_hosted(&mut m, &hosted_by(LONG), &ledger);
    let writes = m.writes();
    assert!(
        writes.contains(&format!("text:{LONG_TERM_TEXT}")) && !writes.contains(&"1".to_string()),
        "the control: {writes:?}"
    );
    let _ = std::fs::remove_dir_all(&ldir);
}
