// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every notice holds the owner's four-part shape (`NoticeText::parts`: what happened, what aterm
//! already did, the one thing to do now, what to expect): checked by structure — sentences,
//! placeholders, the message engine's own word rules, the band's title rule — never by the words
//! themselves.

use std::collections::BTreeSet;

use aterm_messages::need::{REASON_CAP, Reason, STEP_CAP, Step};

use super::*;

/// The longest label the roster admits, so a filled part is measured at its
/// longest (`crate::harness::accounts::admissible_label`).
fn longest_label() -> String {
    let label = "a".repeat(crate::harness::accounts::LABEL_MAX);
    assert!(crate::harness::accounts::admissible_label(&label));
    label
}

/// The longest clause the policy fills `n`'s `{why}` with
/// ([`NoticeText::whys`]), if it names one.
fn longest_why(n: &NoticeText) -> Option<&'static str> {
    n.whys.iter().copied().max_by_key(|w| w.chars().count())
}

/// `n` filled at its longest: the longest label the roster admits, and each
/// clause the policy fills its `{why}` with.
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

/// The longest words a usage limit's reset is said in
/// (`crate::supervise::limit::ResetClock::words`): a two-digit day and a
/// clock with minutes past noon.
fn longest_reset() -> String {
    crate::supervise::limit::ResetClock::of(Some("Sep 30 at 12:30pm"))
        .expect("a clock")
        .words()
}

/// The usage limit's notices raised where aterm continues the tab past the
/// limit's reset: each says when the limit resets, where that is known.
const LIMITS: [NoticeText; 14] = [
    LIMIT_SIGN_IN,
    LIMIT_RENEW,
    LIMIT_RENEW_HELD,
    LIMIT_LOGIN,
    LIMIT_LOGIN_HELD,
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

/// [`stay_why`] NAMES EVERY SETTING THAT HOLDS THE TAB, and only those (the
/// review of the login carry, r4h-2): each of the seven combinations of
/// `accounts`, `relaunch` and `resume_limits` off is a clause of
/// [`STAY_WHYS`] that names exactly their lines, each its own.
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

/// A notice's clauses are exactly those of a `{why}` it names: each one it
/// is raised with is measured, and a dev build fills it with one of its own
/// (the review of the login carry, 2026-10-20: every `{why}` was filled with
/// any notice's, and a renewal held by hand read as held by a draft).
#[test]
fn a_notice_names_a_why_exactly_where_it_has_clauses_for_it() {
    for n in ALL {
        let named = n.parts().iter().any(|p| p.contains(WHY));
        assert_eq!(named, !n.whys.is_empty(), "{}", n.id);
    }
}

/// One sentence: no line break, no sentence break inside it, opened by a
/// capital, a slash command or `aterm`.
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

/// The shape check itself: a `did` (opened by `aterm`) with a sentence break
/// or a line break inside is no one sentence — the control the check's
/// grouping needs, since every `did` opens with `aterm`.
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

/// `f`, a filling of `n`, is what the message engine admits: each part
/// within its cap and free of the words it refuses, the did one sentence.
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

/// A command the owner runs is the end of its step, so what they copy runs
/// as written (no period stuck to the label); a step names at most one.
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

/// What happened is the engine's reason, and a label is the owner's own word:
/// an adversarial label (`retry-soon`) never reaches it.
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

/// What aterm did names aterm, and what to expect never asks for anything
/// more: the one step is the step's.
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

/// A label the room holds is named whole, and the renewal's step runs it; one
/// past the room is clipped in the sentences, never in a command: a command
/// the cap cannot hold with its label whole is a line of Details, whole, and
/// the step says so — never a command with words in its label's place, never
/// another command (`--due` renewed nothing of a row a renewal signed in
/// another account for — the review of the login carry, 2026-10-20).
#[test]
fn a_label_is_named_whole_where_it_fits_and_never_cut_in_a_command() {
    let short = RENEWAL_WRONG_ACCOUNT.fill(Some("main"), None);
    assert!(short.todo.ends_with(" main"), "{short:?}");
    assert!(short.did.contains("main"));
    assert_eq!(short.command, None, "a command that fits is the step's");
    let long = longest_label();
    for n in [
        RENEWAL_WRONG_ACCOUNT,
        ACCOUNT_SIGNED_OUT,
        LIMIT_RENEW,
        LIMIT_LOGIN,
    ] {
        let f = n.fill(Some(&long), None);
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

/// A ROSTER ROW IS NAMED AS A NAME, never as a possessor (the review of the
/// owner's notices, 2026-10-20: `sign in with main's own account` read as if
/// `main` were a person): no filled part says `<label>'s`.
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

/// A SIGN-IN BY HAND OF A LOST ROSTER LOGIN NAMES THE ROW (the review of the
/// login carry, 2026-10-20): every notice whose step has the owner sign the
/// lost login in again themselves says, where the session's login is a
/// roster row, whose own account to sign in with — nothing at the hand-off of
/// their own `/login` does, and another account makes two rows of the roster
/// one.
#[test]
fn a_sign_in_by_hand_on_a_roster_login_names_the_row() {
    let lost = [
        LOGIN_CANCELLED,
        LOGIN_LEFT,
        LOGIN_NOT_RUN,
        LOGIN_NOT_RUN_DRAFT,
        RENEWED_BY_HAND,
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
    let other = LIMIT_SIGN_IN.fill(Some("pool-7"), None);
    assert!(!LIMIT_SIGN_IN.takes_label());
    assert!(
        [&other.did, &other.todo]
            .iter()
            .all(|p| !p.contains("pool-7")),
        "{other:?}"
    );
}

/// A USAGE LIMIT'S NOTICE SAYS WHEN THE TAB GOES ON BY ITSELF (the review
/// of the band, 2026-10-20: none named the reset, nor that aterm continues
/// the tab after it, which happens whatever the owner does): filled with
/// the reset, each limit notice's expectation names it.
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

/// THE BAND IS THE WINDOW'S (the review of the band, 2026-10-20): it shows
/// whichever tab is in front, so no step says `this tab` — read on tab 2,
/// `Press Esc in this tab` would interrupt whatever runs there.
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

/// A STEP THAT RUNS A COMMAND HAS ITS SHORT FORM, a sentence none: the band
/// shows a command whole or puts [`COMMAND_UNDER_DETAILS`] in its place,
/// never a cut of it (the review of the band, 2026-10-20: `Run aterm harness
/// accounts renew…` at 514 px).
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

/// A STEP THAT OUTLASTS ITS NOTICE SAYS HOW IT ENDS (the review of the login
/// carry, r3h-3): `accounts use <label>` pins the account for every session
/// until the pin is ended, so the notice that asks for it names, on a line
/// of Details ([`NoticeText::undo`]), the one command that ends the pin —
/// another command than the step's.
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
    assert_eq!(LIMIT_RENEW.undo, None, "the control");
}

/// Why `short` may not be the band's short form of `title`, or `None`: two
/// or three words, narrower than the title, no tab and no number in it (the
/// band's `Tab N` names the tab), and a title the engine admits.
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

/// A TITLE'S SHORT FORM SAYS WHAT HAPPENED IN TWO OR THREE WORDS (the review
/// of the band, 2026-10-20: at the default window every notice's title read
/// as a stub, `Claude's lo…`): the band paints it where the whole title does
/// not fit, so it holds [`short_title_fault`]'s shape, and it is one per
/// title — two notices share a short form exactly where they share their
/// title, so the band never says less of a difference the title makes.
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
    let title = LIMIT_SIGN_IN.title;
    assert!(short_title_fault("Limit in tab 1", "Claude usage limit reached in tab 1").is_some());
    assert!(short_title_fault(title, title).is_some());
    assert!(short_title_fault("Limit", title).is_some());
}

/// The arguments after `aterm harness` of the command `f`'s step runs, whole
/// (its line of Details where the step points there); `None` for a step that
/// runs none.
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

/// EVERY COMMAND A STEP RUNS IS ONE `aterm harness` TAKES: the owner copies
/// it from the band or from Details, so it parses as the verb's own grammar
/// does (`crate::harness::cli::parse`), at a label of every length.
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

/// A NOTICE'S RECORD CLOSES IN WORDS TRUE OF HOW IT ENDED (the review of the
/// login carry, r8c-7): a usage limit, where no login was lost, closes as the
/// switch to another account that ends it, never as a login back; a lost or
/// renewed login closes as its login back.
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
