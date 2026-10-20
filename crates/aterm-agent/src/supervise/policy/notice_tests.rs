// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every notice holds the owner's four-part shape (module docs): checked by
//! structure — sentences, placeholders, the message engine's own word rules,
//! the band's title rule — never by the words themselves.

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

/// Every clause the policy fills `{why}` with: [`WHYS`], and the words
/// [`super::super::turn_end::RenewStep::ByHand`] names a held relaunch by.
fn whys() -> impl Iterator<Item = &'static str> {
    WHYS.iter()
        .chain(super::super::turn_end::BY_HAND_WHYS)
        .copied()
}

/// The longest clause the policy fills `{why}` with.
fn longest_why() -> &'static str {
    whys().max_by_key(|w| w.chars().count()).expect("a why")
}

/// `n` filled at its longest: the longest label the roster admits, and each
/// clause the policy fills `{why}` with.
fn filled_every(n: &NoticeText) -> Vec<Filled> {
    whys()
        .map(|why| n.fill(Some(&longest_label()), Some(why)))
        .collect()
}

fn filled(n: &NoticeText) -> Filled {
    n.fill(Some(&longest_label()), Some(longest_why()))
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
        for part in n.parts() {
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
    for n in ALL {
        for part in n.parts() {
            let mut rest = part;
            while let Some(at) = rest.find('{') {
                let end = rest[at..].find('}').expect("a closed placeholder") + at;
                let name = &rest[at..=end];
                assert!(
                    name == LABEL || name == WHY,
                    "{}: unknown placeholder {name}",
                    n.id
                );
                rest = &rest[end + 1..];
            }
        }
        let f = filled(n);
        for part in [&f.happened, &f.did, &f.todo, &f.expect] {
            assert!(!part.contains('{'), "{}: {part:?} left unfilled", n.id);
        }
        // A step that renews names the label it renews.
        assert_eq!(
            n.renews(),
            n.todo.contains(LABEL) && n.todo.contains("renew")
        );
    }
}

/// What happened is the engine's reason, and a label is the owner's own word:
/// an adversarial label (`retry-soon`) never reaches it.
#[test]
fn what_happened_holds_no_placeholder() {
    for n in ALL {
        assert!(!n.happened.contains('{'), "{}: {:?}", n.id, n.happened);
        let f = n.fill(Some("retry-soon-waiting"), Some(longest_why()));
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
        assert_eq!(Step::new(n.expect).fault(), None, "{}", n.id);
        assert!(
            !n.expect.starts_with("Run") && !n.expect.starts_with("Type"),
            "{}: a second step in the expectation",
            n.id
        );
    }
}

/// A label the room holds is named whole, and the renewal's step runs it; one
/// past the room is clipped in the sentences, never in a command: the step
/// falls back to the renewal of every due login, which an ended one is.
#[test]
fn a_label_is_named_whole_where_it_fits_and_never_cut_in_a_command() {
    let short = RENEWAL_WRONG_ACCOUNT.fill(Some("main"), None);
    assert!(short.todo.ends_with(" main"), "{short:?}");
    assert!(short.did.contains("main"));
    let long = longest_label();
    let f = RENEWAL_WRONG_ACCOUNT.fill(Some(&long), None);
    assert_eq!(f.todo, RENEW_DUE);
    assert!(!f.did.contains(&long));
    for n in ALL {
        let f = n.fill(Some(&"b".repeat(LABEL_ROOM)), Some(longest_why()));
        if n.todo.contains(LABEL) {
            assert!(f.todo.ends_with(&"b".repeat(LABEL_ROOM)), "{}", n.id);
        }
    }
}
