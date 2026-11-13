// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_phase::prompt::fixtures as f;

fn read(text: &str) -> (aterm_phase::PromptV2, Vec<String>) {
    let rows = f::screen(text);
    let p = aterm_phase::parse_prompt_v2(&rows).expect("a question dialog");
    (p, rows)
}

fn planned(text: &str, answer: &str) -> Result<Plan, String> {
    let (p, rows) = read(text);
    plan(&p, &rows, answer, "@s-w")
}

#[test]
fn a_single_select_answer_names_one_option_or_is_refused() {
    let one = |n| {
        Ok(Plan::Keys {
            targets: vec![AnswerTarget::Option(n)],
            said: ["Dark (Recommended)", "Light", "High contrast"][usize::from(n) - 1].to_string(),
        })
    };
    assert_eq!(planned(f::QUESTION_ONE, "2"), one(2));
    assert_eq!(planned(f::QUESTION_ONE, " high CONTRAST "), one(3));
    assert_eq!(planned(f::QUESTION_ONE, "Dark (Recommended)"), one(1));
    assert!(matches!(
        planned(f::QUESTION_ONE, "recommended"),
        Ok(Plan::Keys { ref targets, .. }) if targets == &[AnswerTarget::Option(1)]
    ));
    assert_eq!(planned(f::QUESTION_ONE, "human"), Ok(Plan::Human));
    for bad in ["whatever you think is best", "4", "9", "Type something."] {
        planned(f::QUESTION_ONE, bad).expect_err(bad);
    }
}

#[test]
fn a_priority_word_that_labels_an_option_answers_that_option() {
    let single = f::QUESTION_ONE.replace("  2. Light", "  2. Long-term");
    for answer in ["long-term", "Long-term", " LONG-TERM "] {
        assert_eq!(
            planned(&single, answer),
            Ok(Plan::Keys {
                targets: vec![AnswerTarget::Option(2)],
                said: "Long-term".to_string(),
            }),
            "{answer:?}"
        );
    }
    let err = planned(&single, "short-term").expect_err("no such option");
    assert!(err.contains("meta set questions short-term"), "{err}");
    let multi =
        f::QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED.replace("[ ] Olives", "[ ] Short-term");
    assert_eq!(
        planned(&multi, "short-term"),
        Ok(Plan::Keys {
            targets: vec![AnswerTarget::Option(2), AnswerTarget::Button],
            said: "Short-term".to_string(),
        })
    );
    planned(&multi, "long-term").expect_err("no such option");
}

#[test]
fn recommended_on_a_multi_select_plans_the_whole_tab() {
    let said = "Which glyph sets should be bundled? → Next (Box drawing (Recommended), \
                Powerline (Recommended))";
    assert_eq!(
        planned(f::QUESTION_MULTISELECT, "recommended"),
        Ok(Plan::Keys {
            targets: vec![
                AnswerTarget::Option(1),
                AnswerTarget::Option(3),
                AnswerTarget::Button
            ],
            said: said.to_string(),
        })
    );
    assert_eq!(
        planned(f::QUESTION_MULTISELECT, "recommended").map(|p| match p {
            Plan::Keys { targets, .. } => targets,
            Plan::Human | Plan::Digit(_) | Plan::Focus(_) => Vec::new(),
        }),
        planned(f::QUESTION_MULTISELECT, "1, 3").map(|p| match p {
            Plan::Keys { targets, .. } => targets,
            Plan::Human | Plan::Digit(_) | Plan::Focus(_) => Vec::new(),
        }),
        "the explicit answer's keys"
    );
    assert_eq!(
        planned(f::QUESTION_MULTISELECT_CHECKED, "recommended"),
        Ok(Plan::Keys {
            targets: vec![AnswerTarget::Button],
            said: said.to_string(),
        })
    );
    let person = f::QUESTION_MULTISELECT.replace("[ ] Emoji", "[✔] Emoji");
    planned(&person, "recommended").expect_err("a person's check");
}

#[test]
fn a_multi_select_toggles_what_differs_then_presses_the_button() {
    assert_eq!(
        planned(f::QUESTION_MULTISELECT, "1, Powerline (Recommended)"),
        Ok(Plan::Keys {
            targets: vec![
                AnswerTarget::Option(1),
                AnswerTarget::Option(3),
                AnswerTarget::Button
            ],
            said: "Box drawing (Recommended), Powerline (Recommended)".to_string(),
        })
    );
    assert!(planned(f::QUESTION_MULTISELECT, "1, nonsense").is_err());
    assert!(planned(f::QUESTION_MULTISELECT, " , ").is_err());
    assert_eq!(
        planned(f::QUESTION_REVIEW, "Submit"),
        Ok(Plan::Keys {
            targets: vec![AnswerTarget::ReviewSubmit],
            said: "submit".to_string(),
        })
    );
    planned(f::QUESTION_REVIEW, "1").expect_err("not submit");
}

#[test]
fn the_box_token_names_one_screen() {
    let token = |t: &str| question_box_token(&read(t).0.question_dialog.expect("read whole"));
    for fixture in [
        f::QUESTION_ONE,
        f::QUESTION_TABS_INCIDENT,
        f::QUESTION_TABS_SECOND,
        f::QUESTION_TABS_THIRD,
        f::QUESTION_TABS_FOURTH,
        f::QUESTION_REVIEW,
        f::QUESTION_MULTISELECT,
    ] {
        let t = token(fixture);
        assert_eq!(t, token(fixture));
        assert_eq!(t.len(), 8);
        assert!(
            t.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "{t:?}"
        );
    }
    let relabelled_option = f::QUESTION_ONE.replace("Red (Recommended)", "Crimson");
    assert_eq!(
        token(&relabelled_option),
        token(f::QUESTION_ONE),
        "option labels do not move a question_dialog's box token"
    );
}

#[test]
fn a_muse_question_is_planned_as_the_focus_moved_to_its_answer() {
    use aterm_phase::muse::fixtures as u;
    let read = |text: &str| {
        let rows = f::screen(text);
        let p = aterm_phase::Program::Muse
            .reader()
            .prompt(&rows)
            .expect("the question");
        (p, rows)
    };
    let (p, rows) = read(u::QUESTION);
    let guard = row_guard("  Which color should the probe use?");
    let focus = |steps: i32, label: &str, said: &str| {
        Ok(Plan::Focus(Focus {
            steps,
            label: label.to_string(),
            guard: guard.clone(),
            said: said.to_string(),
        }))
    };
    assert_eq!(
        plan(&p, &rows, "recommended", "@s-w"),
        focus(
            0,
            "Red (Recommended)",
            "Request user input Color → Red (Recommended)"
        )
    );
    for answer in ["2", "blue"] {
        assert_eq!(
            plan(&p, &rows, answer, "@s-w"),
            focus(1, "Blue", "Blue"),
            "{answer}"
        );
    }
    let (down, down_rows) = read(u::QUESTION_DOWN);
    assert_eq!(
        plan(&down, &down_rows, "1", "@s-w"),
        focus(-1, "Red (Recommended)", "Red (Recommended)")
    );
    for answer in ["3", "none of the above"] {
        let err = plan(&p, &rows, answer, "@s-w").expect_err(answer);
        assert!(err.contains("person's own words"), "{answer}: {err}");
    }
    assert_eq!(plan(&p, &rows, "human", "@s-w"), Ok(Plan::Human));
    let (narrow, narrow_rows) = read(u::QUESTION_WRAPPED);
    assert_eq!(
        plan(&narrow, &narrow_rows, "blue", "@s-w"),
        Ok(Plan::Focus(Focus {
            steps: 1,
            label: "Blue".to_string(),
            guard: row_guard("  Which shade should the probe use for the long label test?"),
            said: "Blue".to_string(),
        }))
    );
}

#[test]
fn a_roles_question_has_a_box_token_and_another_box_none() {
    use aterm_phase::muse::fixtures as u;
    let muse = |text: &str| {
        aterm_phase::Program::Muse
            .reader()
            .prompt(&f::screen(text))
            .expect("a Muse Code box")
    };
    let q = muse(u::QUESTION);
    let token = prompt_box_token(&q).expect("a question's token");
    assert_eq!(token.len(), 8);
    assert!(token.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
    assert_eq!(prompt_box_token(&muse(u::QUESTION)), Some(token.clone()));
    let mut refocused = q;
    for opt in &mut refocused.options {
        opt.focused = !opt.focused;
    }
    assert_eq!(
        prompt_box_token(&refocused),
        Some(token),
        "focus changes do not move a role question's box token"
    );
    let (claude, _) = read(f::QUESTION_ONE);
    assert_eq!(
        prompt_box_token(&claude),
        claude.question_dialog.as_ref().map(question_box_token)
    );
    assert_eq!(prompt_box_token(&muse(u::BOX_SHELL)), None);
}
