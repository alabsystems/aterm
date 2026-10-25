// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Structural checks for `NoticeText::parts`.

use std::collections::BTreeSet;

use aterm_messages::need::{REASON_CAP, Reason, STEP_CAP, Step};

use super::*;

fn longest_label() -> String {
    let label = "a".repeat(crate::harness::accounts::LABEL_MAX);
    assert!(crate::harness::accounts::admissible_label(&label));
    label
}

fn longest_why(n: &NoticeText) -> Option<&'static str> {
    n.whys.iter().copied().max_by_key(|w| w.chars().count())
}

fn filled_every(n: &NoticeText) -> Vec<Filled> {
    let whys: Vec<Option<&str>> = if n.whys.is_empty() {
        vec![None]
    } else {
        n.whys.iter().copied().map(Some).collect()
    };
    let label = longest_label();
    let reset = longest_reset();
    whys.into_iter()
        .flat_map(|why| {
            [
                n.fill(Some(&label), why),
                n.fill(None, why),
                n.fill_reset(Some(&label), why, Some(&reset)),
            ]
        })
        .collect()
}

fn filled(n: &NoticeText) -> Filled {
    n.fill(Some(&longest_label()), longest_why(n))
}

fn longest_reset() -> String {
    crate::supervise::limit::ResetClock::of(Some("Sep 30 at 12:30pm"))
        .expect("a clock")
        .words()
}

const LIMITS: [NoticeText; 9] = [
    LIMIT_USE,
    LIMIT_DRAFT,
    LIMIT_DRAFT_OWN,
    LIMIT_DRAFT_MENU,
    LIMIT_UNMADE,
    LIMIT_STAY,
    LIMIT_STAY_HELD,
    LIMIT_ROSTER,
    LIMIT_UNREAD,
];

#[test]
fn the_stay_clause_names_every_setting_that_holds_the_tab() {
    let lines = [
        "accounts = \"wall\"",
        "relaunch = true",
        "resume_limits = true",
    ];
    let mut clauses = BTreeSet::new();
    for bits in 1..8_u8 {
        let off = [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0];
        let why = stay_why(off[0], off[1], off[2]).expect("a clause");
        assert!(STAY_WHYS.contains(&why), "{off:?}: {why}");
        for (line, held) in lines.iter().zip(off) {
            assert_eq!(why.contains(line), held, "{off:?}: {why}");
        }
        clauses.insert(why);
    }
    assert_eq!(clauses.len(), STAY_WHYS.len());
    assert_eq!(stay_why(false, false, false), None, "the control");
}

#[test]
fn a_notice_names_a_why_exactly_where_it_has_clauses_for_it() {
    for n in ALL {
        let named = n.parts().iter().any(|p| p.contains(WHY));
        assert_eq!(named, !n.whys.is_empty(), "{}", n.id);
    }
}

fn one_sentence(part: &str) -> bool {
    let body = part.strip_suffix('.').unwrap_or(part);
    !part.contains('\n')
        && !body.contains(". ")
        && (part
            .chars()
            .next()
            .is_some_and(|c| c.is_uppercase() || c == '/')
            || part.starts_with("aterm "))
}

#[test]
fn one_sentence_refuses_two_sentences_whatever_opens_them() {
    assert!(one_sentence("aterm ran /login in the tab."));
    assert!(!one_sentence("aterm ran /login. It opened the browser."));
    assert!(!one_sentence("aterm ran /login\nin the tab."));
    assert!(!one_sentence("ran /login in the tab."));
}

#[test]
fn every_notice_has_four_parts_each_one_sentence() {
    for n in ALL {
        for part in n
            .parts()
            .into_iter()
            .chain(n.todo_as)
            .chain(n.expect_reset)
            .chain(n.undo)
        {
            assert!(!part.trim().is_empty(), "{}: an empty part", n.id);
            assert!(one_sentence(part), "{}: {part:?} is not one sentence", n.id);
        }
        let parts = n.parts();
        let distinct: BTreeSet<&str> = parts.iter().copied().collect();
        assert_eq!(distinct.len(), 4, "{}: two parts say the same", n.id);
    }
}

#[test]
fn every_notice_is_one_the_message_engine_admits() {
    for n in ALL {
        for f in filled_every(n) {
            admitted(n, &f);
        }
    }
}

fn admitted(n: &NoticeText, f: &Filled) {
    let reason = Reason::new(&f.happened);
    assert_eq!(reason.fault(), None, "{}: {:?}", n.id, f.happened);
    assert!(
        f.happened.chars().count() <= REASON_CAP,
        "{}: what happened is cut on the band",
        n.id
    );
    let did = Reason::new(&f.did);
    assert_eq!(did.fault(), None, "{}: {:?}", n.id, f.did);
    assert!(f.did.chars().count() <= REASON_CAP, "{}: {:?}", n.id, f.did);
    let step = Step::new(&f.todo);
    assert_eq!(step.fault(), None, "{}: {:?}", n.id, f.todo);
    assert!(f.todo.chars().count() <= STEP_CAP, "{}: {:?}", n.id, f.todo);
    assert!(
        f.expect.chars().count() <= REASON_CAP,
        "{}: {:?}",
        n.id,
        f.expect
    );
    assert_eq!(
        aterm_messages::text::glass_title_fault(n.title),
        None,
        "{}: {:?}",
        n.id,
        n.title
    );
    assert!(one_sentence(&f.did), "{}: {:?}", n.id, f.did);
    if let Some(undo) = &f.undo {
        assert!(one_sentence(undo), "{}: {undo:?}", n.id);
        assert!(
            undo.chars().count() <= aterm_messages::DETAIL_LINE_CAP,
            "{}: {undo:?}",
            n.id
        );
    }
}

#[test]
fn a_step_that_names_a_command_ends_with_it() {
    for n in ALL {
        let f = filled(n);
        let commands = f.todo.matches("aterm harness").count();
        assert!(commands <= 1, "{}: two commands", n.id);
        if commands == 1 {
            let at = f.todo.find("aterm harness").expect("a command");
            let command = &f.todo[at..];
            assert!(
                !command.ends_with('.') && !command.contains(", "),
                "{}: {command:?} does not end the step",
                n.id
            );
        } else {
            assert!(f.todo.ends_with('.'), "{}: {:?}", n.id, f.todo);
        }
    }
}

#[test]
fn placeholders_are_known_and_every_one_is_filled() {
    let names = |part: &'static str| {
        let mut names = Vec::new();
        let mut rest = part;
        while let Some(at) = rest.find('{') {
            let end = rest[at..].find('}').expect("a closed placeholder") + at;
            names.push(&rest[at..=end]);
            rest = &rest[end + 1..];
        }
        names
    };
    for n in ALL {
        for part in n.parts().into_iter().chain(n.todo_as).chain(n.undo) {
            for name in names(part) {
                assert!(
                    name == LABEL || name == WHY,
                    "{}: unknown placeholder {name}",
                    n.id
                );
            }
        }
        if let Some(part) = n.expect_reset {
            assert!(names(part).contains(&RESET), "{}: {part:?}", n.id);
            assert!(
                names(part)
                    .iter()
                    .all(|name| [LABEL, WHY, RESET].contains(name)),
                "{}: {part:?}",
                n.id
            );
        }
        for f in [
            filled(n),
            n.fill_reset(
                Some(&longest_label()),
                longest_why(n),
                Some(&longest_reset()),
            ),
        ] {
            for part in [&f.happened, &f.did, &f.todo, &f.expect]
                .into_iter()
                .chain(f.undo.as_ref())
            {
                assert!(!part.contains('{'), "{}: {part:?} left unfilled", n.id);
            }
        }
    }
}

#[test]
fn what_happened_holds_no_placeholder() {
    for n in ALL {
        assert!(!n.happened.contains('{'), "{}: {:?}", n.id, n.happened);
        let f = n.fill(Some("retry-soon-waiting"), longest_why(n));
        assert_eq!(Reason::new(&f.happened).fault(), None, "{}", n.id);
    }
}

#[test]
fn ids_are_unique_words_and_codes_unique_letter_pairs() {
    let ids: BTreeSet<&str> = ALL.iter().map(|n| n.id).collect();
    assert_eq!(ids.len(), ALL.len(), "an id twice");
    for id in ids {
        assert!(
            !id.is_empty() && id.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
            "{id}"
        );
    }
    let codes: BTreeSet<&str> = ALL.iter().map(|n| n.code).collect();
    assert_eq!(codes.len(), ALL.len(), "a code twice");
    for code in codes {
        assert!(
            code.len() == 2 && code.bytes().all(|b| b.is_ascii_lowercase()),
            "{code}"
        );
    }
}

#[test]
fn what_aterm_did_is_aterms_and_the_expectation_asks_nothing() {
    for n in ALL {
        assert!(n.did.starts_with("aterm "), "{}: {:?}", n.id, n.did);
        for expect in std::iter::once(n.expect).chain(n.expect_reset) {
            assert_eq!(Step::new(expect).fault(), None, "{}", n.id);
            assert!(
                !expect.starts_with("Run") && !expect.starts_with("Type"),
                "{}: a second step in the expectation",
                n.id
            );
        }
    }
}

#[test]
fn a_label_is_named_whole_where_it_fits_and_never_cut_in_a_command() {
    let short = LIMIT_USE.fill(Some("main"), Some(WHY_USE_PIN));
    assert!(short.todo.ends_with(" main"), "{short:?}");
    assert!(short.did.contains("main"));
    assert_eq!(short.command, None, "a command that fits is the step's");
    let long = longest_label();
    let named: Vec<&NoticeText> = ALL
        .iter()
        .filter(|n| n.todo.contains(COMMAND) && n.todo.contains(LABEL))
        .collect();
    assert!(!named.is_empty(), "a step's command names its row");
    for n in named {
        let f = n.fill(Some(&long), longest_why(n));
        assert_eq!(f.todo, COMMAND_UNDER_DETAILS, "{}: {f:?}", n.id);
        let command = f.command.as_deref().expect("the command, whole");
        assert!(
            command.starts_with("aterm harness ") && command.ends_with(&format!(" {long}")),
            "{}: {command}",
            n.id
        );
        assert!(
            !command.contains("--"),
            "{}: no other logins: {command}",
            n.id
        );
        assert!(!f.did.contains(&long));
        assert_eq!(Step::new(&f.todo).fault(), None, "{}", n.id);
    }
    for n in ALL {
        let f = n.fill(Some(&"b".repeat(LABEL_ROOM)), longest_why(n));
        if n.todo.contains(LABEL) {
            assert!(f.todo.contains(&"b".repeat(LABEL_ROOM)), "{}", n.id);
            assert_eq!(f.command, None, "{}", n.id);
        }
        if n.todo_as.is_some() {
            let f = n.fill(Some(&long), longest_why(n));
            assert!(f.todo.contains(&"a".repeat(LABEL_ROOM)), "{}", n.id);
            assert_ne!(f.todo, COMMAND_UNDER_DETAILS, "{}", n.id);
            assert_eq!(f.command, None, "{}", n.id);
        }
    }
}

#[test]
fn a_label_is_never_a_possessor() {
    let possessive = |part: &str, label: &str| part.contains(&format!("{label}'s"));
    for n in ALL {
        let f = n.fill(Some("pool-7"), longest_why(n));
        for part in [&f.happened, &f.did, &f.todo, &f.expect] {
            assert!(!possessive(part, "pool-7"), "{}: {part:?}", n.id);
        }
    }
    let old =
        "Type /login in the tab and sign in with {label}'s own account.".replace(LABEL, "pool-7");
    assert!(possessive(&old, "pool-7"), "the control");
}

#[test]
fn a_sign_in_by_hand_on_a_roster_login_names_the_row() {
    let lost = [
        LOGIN_CANCELLED,
        LOGIN_LEFT,
        LOGIN_NOT_RUN,
        LOGIN_NOT_RUN_DRAFT,
    ];
    for n in &lost {
        let f = n.fill(Some("pool-7"), longest_why(n));
        assert!(
            [&f.did, &f.todo].iter().any(|p| p.contains("pool-7")),
            "{}: {f:?}",
            n.id
        );
        assert!(n.takes_label(), "{}", n.id);
        assert_eq!(n.fill(None, longest_why(n)).todo, n.todo, "{}", n.id);
    }
    let other = LOGIN_SIGN_IN.fill(Some("pool-7"), None);
    assert!(!LOGIN_SIGN_IN.takes_label());
    assert!(
        [&other.did, &other.todo]
            .iter()
            .all(|p| !p.contains("pool-7")),
        "{other:?}"
    );
}

#[test]
fn a_limit_notice_names_its_reset_where_it_is_known() {
    let reset = "on Oct 23 at 9am";
    for n in LIMITS {
        let label = n.takes_label().then_some("pool-7");
        let told = n.fill_reset(label, None, Some(reset));
        assert!(told.expect.contains(reset), "{}: {:?}", n.id, told.expect);
        assert_eq!(
            [&told.happened, &told.did, &told.todo],
            {
                let plain = n.fill(label, None);
                [&plain.happened, &plain.did, &plain.todo].map(Clone::clone)
            }
            .each_ref(),
            "{}: only the expectation says the reset",
            n.id
        );
        let unknown = n.fill_reset(label, None, None);
        assert_eq!(unknown.expect, n.fill(label, None).expect, "{}", n.id);
        assert!(!unknown.expect.contains(reset), "{}", n.id);
    }
    for n in ALL.iter().filter(|n| !LIMITS.iter().any(|l| l.id == n.id)) {
        assert_eq!(n.expect_reset, None, "{}", n.id);
        let f = n.fill_reset(Some("pool-7"), longest_why(n), Some(reset));
        assert!(!f.expect.contains(reset), "{}: {:?}", n.id, f.expect);
    }
}

#[test]
fn no_step_says_this_tab() {
    let says = |step: &str| step.to_lowercase().contains("this tab");
    for n in ALL {
        for step in std::iter::once(n.todo).chain(n.todo_as) {
            assert!(!says(step), "{}: {step:?}", n.id);
        }
    }
    assert!(
        says("Press Esc in this tab to close the sign-in."),
        "the control"
    );
}

#[test]
fn a_command_step_has_its_short_form() {
    assert_eq!(Step::new(COMMAND_UNDER_DETAILS).fault(), None);
    for n in ALL {
        for label in [None, Some("main"), Some(longest_label().as_str())] {
            let f = n.fill(label, longest_why(n));
            assert_eq!(
                f.short.is_some(),
                f.todo.contains("aterm harness"),
                "{}: {f:?}",
                n.id
            );
        }
    }
    assert_eq!(LOGIN_CANCELLED.fill(Some("main"), None).short, None);
}

#[test]
fn a_pin_names_how_it_ends() {
    let pins = |n: &NoticeText| n.todo.contains("accounts use ");
    assert!(ALL.iter().any(pins), "a notice asks for the pin");
    for n in ALL {
        let f = n.fill(Some("main"), longest_why(n));
        assert_eq!(f.undo.is_some(), pins(n), "{}", n.id);
        if let Some(undo) = &f.undo {
            assert_eq!(undo.matches(COMMAND).count(), 1, "{}: {undo:?}", n.id);
            let step = &f.todo[f.todo.find(COMMAND).expect("the step's command")..];
            assert!(!undo.contains(step), "{}: {undo:?}", n.id);
        }
    }
    assert_eq!(LIMIT_DRAFT.undo, None, "the control");
}

fn short_title_fault(short: &str, title: &str) -> Option<&'static str> {
    let words = aterm_messages::text::title_words(short);
    if !(2..=3).contains(&words) {
        Some("two or three words")
    } else if short.chars().count() >= title.chars().count() {
        Some("no narrower than the title")
    } else if short.chars().any(|c| c.is_ascii_digit()) || short.to_lowercase().contains("tab") {
        Some("a tab named")
    } else {
        aterm_messages::text::glass_title_fault(short)
    }
}

#[test]
fn a_short_title_says_what_happened_in_two_or_three_words() {
    for n in ALL {
        assert_eq!(
            short_title_fault(n.short_title, n.title),
            None,
            "{}: {:?}",
            n.id,
            n.short_title
        );
        for m in ALL {
            assert_eq!(
                n.short_title == m.short_title,
                n.title == m.title,
                "{} and {}",
                n.id,
                m.id
            );
        }
    }
    let title = LIMIT_USE.title;
    assert!(short_title_fault("Limit in tab 1", "Claude usage limit reached in tab 1").is_some());
    assert!(short_title_fault(title, title).is_some());
    assert!(short_title_fault("Limit", title).is_some());
}

fn step_args(f: &Filled) -> Option<Vec<String>> {
    let step = f.command.as_deref().unwrap_or(&f.todo);
    let at = step.find(COMMAND)?;
    Some(
        step[at + COMMAND.len()..]
            .split_whitespace()
            .map(str::to_string)
            .collect(),
    )
}

#[test]
fn every_command_a_step_runs_is_one_aterm_harness_takes() {
    use crate::harness::cli::parse;
    let mut commands = 0;
    for n in ALL {
        for label in ["main".to_string(), longest_label()] {
            let f = n.fill(Some(&label), longest_why(n));
            let Some(args) = step_args(&f) else {
                continue;
            };
            commands += 1;
            assert!(parse(&args).is_ok(), "{}: {args:?}", n.id);
        }
    }
    assert!(commands > 0, "a step runs a command");
    let unknown = ["accounts", "adds", "main"].map(str::to_string);
    assert!(parse(&unknown).is_err(), "the control");
}

#[test]
fn a_notice_closes_in_words_true_of_how_it_ended() {
    let limits: Vec<&str> = LIMITS
        .iter()
        .chain(&[LIMIT_ROSTER_KEPT, LIMIT_UNREAD_KEPT])
        .map(|n| n.id)
        .collect();
    for n in ALL {
        let limit = limits.contains(&n.id);
        assert_eq!(n.back, if limit { SWITCHED } else { BACK }, "{}", n.id);
        let closed = format!("{} in tab 12", n.back);
        assert_eq!(
            aterm_messages::text::glass_title_fault(&closed),
            None,
            "{}: {closed:?}",
            n.id
        );
    }
    assert_ne!(SWITCHED, BACK, "the control");
}

/// Whether a step asks a person to sign a roster row in by hand: `accounts renew` or
/// `accounts login`, which aterm's sign-in ladder runs by itself for every row with an email.
fn asks_a_sign_in(step: &str) -> bool {
    step.contains("accounts renew") || step.contains("accounts login")
}

#[test]
fn no_notice_asks_a_person_to_sign_a_roster_row_in() {
    for n in ALL {
        for label in [None, Some("pool-7")] {
            let f = n.fill(label, longest_why(n));
            for step in std::iter::once(&f.todo).chain(f.command.as_ref()) {
                assert!(!asks_a_sign_in(step), "{}: {step:?}", n.id);
            }
        }
    }
    assert!(
        asks_a_sign_in("Run aterm harness accounts renew pool-7"),
        "the control: the step the wrong account's notice asked before"
    );
}

#[test]
fn the_rosters_facts_name_the_row_what_aterm_did_and_read_what_it_tried() {
    for n in [ACCOUNT_SIGNED_OUT, RENEWAL_WRONG_ACCOUNT] {
        let f = n.fill(Some("pool-7"), None);
        assert!(
            f.todo.contains(COMMAND) && !asks_a_sign_in(&f.todo),
            "{}: the step reads what aterm tried: {f:?}",
            n.id
        );
        assert!(!f.todo.contains("pool-7"), "{}: the step is a read", n.id);
        assert!(f.did.contains("pool-7"), "{}: {f:?}", n.id);
        assert!(f.did.starts_with("aterm "), "{}: {f:?}", n.id);
        assert!(
            f.expect.starts_with("aterm "),
            "{}: what aterm does next",
            n.id
        );
    }
    let signed_out = ACCOUNT_SIGNED_OUT.fill(Some("pool-7"), None);
    assert!(
        signed_out.happened.contains("email"),
        "the recorded error says why no route exists: {signed_out:?}"
    );
}
