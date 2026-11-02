// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// THE GRAMMAR (§3 of the slice's design): `LANDED` alone, whitespace, one closing full stop,
/// markdown's matching wrappers and ASCII case aside, lands; anything that says more is the
/// agent's list.
#[test]
fn only_landed_lands() {
    for landed in [
        "LANDED",
        "  LANDED  ",
        "\n\nLANDED\n",
        "LANDED.",
        "landed",
        "Landed.",
        "**LANDED**",
        "**LANDED**.",
        "**LANDED.**",
        "__LANDED__",
        "*LANDED*",
        "_LANDED_",
        "`LANDED`",
        "``LANDED``",
        "```\nLANDED\n```",
        "**`LANDED`**",
        "NEEDS YOU: none\nLANDED",
        "LANDED\nNEEDS YOU: none",
    ] {
        assert_eq!(
            land_check_answer(landed, None, false),
            CheckAnswer::Landed,
            "{landed:?}"
        );
    }
    for left in [
        "LANDED — all on main",
        "LANDED, except the docs PR",
        "LANDED!",
        "\"LANDED\"",
        "\u{201c}LANDED\u{201d}",
        "- LANDED",
        "# LANDED",
        "> LANDED",
        "LANDED..",
        "**LANDED*",
        "`LANDED``",
        "```text\nLANDED\n```",
        "Nothing left: everything is on origin/main.",
        "Not LANDED",
        "LANDED\nThe docs PR is still open.",
        "The docs PR is still open.",
        "",
        "NEEDS YOU: none",
    ] {
        assert!(
            matches!(land_check_answer(left, None, false), CheckAnswer::Left(_)),
            "{left:?}: {:?}",
            land_check_answer(left, None, false)
        );
    }
    // A cut reply is never the word: the part cut is something else.
    assert_eq!(
        land_check_answer("LANDED", None, true),
        CheckAnswer::Left("LANDED".to_string())
    );
    // The list is the reply without its needs lines.
    assert_eq!(
        land_check_answer("The docs PR is still open.\nNEEDS YOU: none", None, false),
        CheckAnswer::Left("The docs PR is still open.".to_string())
    );
}

/// A NEED IS A NEED, whatever else the answer says: alone (or with `LANDED`) Land rests on it,
/// with more it is the agent's list too; a need earlier in the same turn counts.
#[test]
fn a_need_in_the_answer_is_a_need() {
    for alone in [
        "NEEDS YOU: rotate the deploy key",
        "NEEDS YOU: rotate the deploy key\nLANDED",
        "LANDED\n\nNEEDS YOU: rotate the deploy key.",
    ] {
        assert_eq!(
            land_check_answer(alone, None, false),
            CheckAnswer::Needs { rest_landed: true },
            "{alone:?}"
        );
    }
    assert_eq!(
        land_check_answer(
            "NEEDS YOU: rotate the deploy key\nThe docs PR is still open.",
            None,
            false
        ),
        CheckAnswer::Needs { rest_landed: false }
    );
    assert_eq!(
        land_check_answer("LANDED", Some("NEEDS YOU: the signing key"), false),
        CheckAnswer::Needs { rest_landed: true }
    );
    assert_eq!(
        land_check_answer("NEEDS YOU: rotate the deploy key", None, true),
        CheckAnswer::Needs { rest_landed: false },
        "a cut answer never rests"
    );
}

/// THE ANSWER'S LAST WORD ON NEEDS WINS (the post-landing review, 2026-11-01): a last message that
/// says none stands retracts the needs the turn's earlier messages named, so `LANDED` after it
/// lands; a need the last message names, or one it leaves unretracted, is still a need.
/// Negative control: `land_check_answer` reading `turn_said` without `needs::standing_before`.
#[test]
fn the_answers_last_word_on_needs_wins() {
    let interim = "./check.sh fails on main in tests/test_clock.sh. NEEDS YOU: decide whether to \
                   fix or delete tests/test_clock.sh.";
    assert_eq!(
        land_check_answer("NEEDS YOU: nothing\nLANDED", Some(interim), false),
        CheckAnswer::Landed
    );
    assert_eq!(
        land_check_answer(
            "NEEDS YOU: nothing\nThe docs PR is still open.",
            Some(interim),
            false
        ),
        CheckAnswer::Left("The docs PR is still open.".into())
    );
    assert_eq!(
        land_check_answer("LANDED", Some(interim), false),
        CheckAnswer::Needs { rest_landed: true },
        "unretracted, the earlier need stands"
    );
    assert_eq!(
        land_check_answer(
            "NEEDS YOU: rotate the deploy key\nLANDED",
            Some("NEEDS YOU: nothing"),
            false
        ),
        CheckAnswer::Needs { rest_landed: true },
        "a need after a none stands"
    );
}

/// The grammar's negative controls: each mutant of [`landed_word`] the design names takes a reply
/// the table refuses, or refuses one it takes.
#[test]
fn the_grammars_mutants_misread_the_table() {
    let contains = |b: &str| b.contains(LANDED);
    let untrimmed = |b: &str| b == LANDED;
    let case_kept = |b: &str| {
        let t = b.trim().trim_end_matches('.');
        let t = t.trim_matches(['*', '_', '`']);
        t == LANDED
    };
    let unpaired = |b: &str| {
        b.trim()
            .trim_matches(['*', '_', '`', '.'])
            .eq_ignore_ascii_case(LANDED)
    };
    assert!(contains("LANDED, except the docs PR") && !landed_word("LANDED, except the docs PR"));
    assert!(!untrimmed("  LANDED\n") && landed_word("  LANDED\n"));
    assert!(!case_kept("landed") && landed_word("landed"));
    assert!(unpaired("**LANDED*") && !landed_word("**LANDED*"));
    assert!(unpaired("LANDED..") && !landed_word("LANDED.."));
}

/// The plain check is the frozen text whole, and every form keeps its head, so every reader of a
/// typed check knows it ([`LAND_CHECK_HEAD`]).
#[test]
fn every_form_opens_with_the_head_and_holds_the_body() {
    assert_eq!(land_check_text(None, false, None), LAND_CHECK);
    for (hint, ack, rules) in [
        (None, false, None),
        (Some("the docs PR is still open"), false, None),
        (
            Some("the docs PR is still open"),
            true,
            Some("never force-push"),
        ),
        (None, true, Some("never force-push")),
    ] {
        let text = land_check_text(hint, ack, rules);
        assert!(text.starts_with(LAND_CHECK_HEAD), "{text}");
        assert!(text.contains(LAND_CHECK_BODY), "{text}");
        assert_eq!(text.contains(needs::NEEDS_ACK), ack, "{text}");
        if let Some(h) = hint {
            assert!(
                text.contains(&format!("{LAND_CHECK_HINT_OPEN}{h}{LAND_CHECK_HINT_CLOSE}")),
                "{text}"
            );
        }
    }
    // The check names no `Nothing left:`: a second defense is not its answer.
    assert!(!LAND_CHECK.contains(needs::NOTHING_LEFT));
    assert!(LAND_CHECK.contains(super::super::turn_end::NO_REPOSITORY));
}

/// THE HINT YIELDS TO THE OWNER'S RULES: every form with the ack and the longest rules fits
/// [`VERBATIM_UNITS`], and where the hinted form would cut the rules the plain one is typed with
/// them whole. Negative control: a composer that kept the hint first cuts the rules.
#[test]
fn the_hint_yields_to_the_owners_rules() {
    let clause = "x".repeat(HINT_UNITS);
    let fits_plain =
        "r".repeat(VERBATIM_UNITS - units(LAND_CHECK) - units(needs::NEEDS_ACK) - 1 - 19);
    let hinted_room = VERBATIM_UNITS
        - units(LAND_CHECK)
        - units(LAND_CHECK_HINT_OPEN)
        - units(LAND_CHECK_HINT_CLOSE)
        - HINT_UNITS
        - 19;
    for rules in [
        "never force-push".to_string(),
        "r".repeat(hinted_room),
        "r".repeat(hinted_room + 1),
        fits_plain.clone(),
        "r".repeat(2000),
    ] {
        for ack in [false, true] {
            let text = land_check_text(Some(&clause), ack, Some(&rules));
            assert!(units(&text) <= VERBATIM_UNITS, "{} units", units(&text));
            let whole = format!("{RULES_OPEN}{rules})");
            let plain_whole = units(LAND_CHECK)
                + if ack { 1 + units(needs::NEEDS_ACK) } else { 0 }
                + units(&whole)
                <= VERBATIM_UNITS;
            if plain_whole {
                assert!(
                    text.ends_with(&whole),
                    "the rules whole: {} units",
                    rules.len()
                );
            }
            if text.contains(LAND_CHECK_HINT_OPEN) {
                assert!(text.ends_with(&whole), "a hint never cuts the rules");
            }
        }
    }
    // The mutant: the hint kept first, the rules fitted after it.
    let hint_first = |hint: &str, rules: &str| {
        let hinted = format!(
            "{LAND_CHECK_HEAD}{LAND_CHECK_HINT_OPEN}{hint}{LAND_CHECK_HINT_CLOSE}{LAND_CHECK_BODY}"
        );
        with_rules(&hinted, Some(rules))
    };
    let rules = "r".repeat(hinted_room + 1);
    assert!(!hint_first(&clause, &rules).ends_with(&format!("{RULES_OPEN}{rules})")));
    assert!(
        land_check_text(Some(&clause), false, Some(&rules))
            .ends_with(&format!("{RULES_OPEN}{rules})"))
    );
}

/// THE HINT QUOTES THE FIRST OPEN CLAUSE WITHIN 120 UNITS: the strict reading's first piece, in
/// report order, its match kept whole where the clause is cut, one closing mark trimmed; a
/// defense the reading finds clean gets none.
#[test]
fn the_hint_quotes_the_first_open_clause_within_120_units() {
    let tail = "Nothing left: the parser is on origin/main, and the docs PR is still open; \
                the lexer is parked on a branch for later.";
    let found = land_open_found(Some(tail));
    assert!(found.len() >= 2, "{found:?}");
    let hint = hint_clause(tail).expect("a clause");
    assert!(units(&hint) <= HINT_UNITS, "{hint}");
    assert!(
        hint.contains(&found[0].said[found[0].matched.clone()]),
        "{hint}: {found:?}"
    );
    assert!(!hint.ends_with(['.', ',', ';', ':']), "{hint}");
    // Negative control: the second piece is not what is quoted.
    let second = &found[1].said[found[1].matched.clone()];
    assert!(
        !hint.contains(second) || found[0].said == found[1].said,
        "{hint}: {second}"
    );
    // A long sentence is cut around its match, which stays whole.
    let long = format!(
        "Nothing left: {} and the release tag is still not pushed because {}.",
        "the parser, the lexer, the formatter and the docs are all merged on origin/main",
        "the remote refused it twice over a flaky network and the retry is still running"
    );
    let found = land_open_found(Some(&long));
    let hint = hint_clause(&long).expect("a clause");
    assert!(units(&hint) <= HINT_UNITS, "{} units: {hint}", units(&hint));
    let matched = &found[0].said[found[0].matched.clone()];
    assert!(hint.contains(matched), "{hint}: {matched:?}");
    // Mutant: a cut that keeps the start loses the match.
    let start_kept: String = found[0].said.chars().take(HINT_UNITS).collect();
    assert!(
        found[0].matched.start >= HINT_UNITS
            || !start_kept.contains(matched)
            || hint.contains(matched),
        "{start_kept}"
    );
    // Clean: none.
    let clean = "Nothing left: every branch is merged into the remote main branch and pushed.";
    assert!(land_open_found(Some(clean)).is_empty());
    assert_eq!(hint_clause(clean), None);
    // A closing full stop is trimmed (mutant: the quote reads `open.”.`).
    let dotted = "Nothing left: the docs PR is still open.";
    let hint = hint_clause(dotted).expect("a clause");
    assert!(!hint.ends_with('.'), "{hint}");
    let text = land_check_text(Some(&hint), false, None);
    assert!(!text.contains(".\u{201d}."), "{text}");
}

/// THE LIST QUOTED: its lines without markdown's marks, joined, cut at a word's edge, then the
/// ask to resolve what the agent can (slice 4b).
#[test]
fn the_list_is_quoted_plain_and_short() {
    assert_eq!(
        listed_suffix("- The docs PR is still open.\n- **CI** on `main` is red.").as_deref(),
        Some(
            format!(
                "You said this is left: \u{201c}The docs PR is still open.; CI on main is \
                 red.\u{201d}. {LISTED_RESOLVE}"
            )
            .as_str()
        )
    );
    assert_eq!(listed_suffix("  \n"), None);
    // A row the screen wrapped carries on its sentence (mutant: `first; step`).
    assert_eq!(
        listed_words("Not landed: ./check.sh exits 2 at its first\nstep, zzlint.\nNEEDS YOU: x"),
        "Not landed: ./check.sh exits 2 at its first step, zzlint.; NEEDS YOU: x"
    );
    let long = "word ".repeat(100);
    let quoted = listed_suffix(&long).expect("quoted");
    let inner = quoted
        .strip_prefix(LISTED_OPEN)
        .and_then(|q| q.strip_suffix(format!("{LISTED_CLOSE} {LISTED_RESOLVE}").as_str()))
        .expect("framed");
    assert!(
        inner.len() <= LISTED_CAP && inner.ends_with("word…"),
        "{inner}"
    );
}

/// THE AUDITED DEFENSE IS THE DEFENSE LINE AND AFTER (the landing's receipt quotes it): the work
/// before the defense is dropped, the lines after it kept, and the whole cut to [`AUDITED_CAP`]
/// bytes at a character's edge. NEGATIVE CONTROL: a tail cut from its start (the defense behind
/// more than the cap of work) would keep no defense at all.
#[test]
fn the_audited_defense_is_the_defense_line_and_after() {
    let work = "Ran the suite and fixed the flaky parser test. ".repeat(8);
    let tail = format!("{work}\nNothing left: pushed to origin/main.\n\n- its tests ran green\n");
    assert!(work.len() > AUDITED_CAP);
    let kept = audited(&tail);
    assert_eq!(
        kept,
        "Nothing left: pushed to origin/main.\n- its tests ran green"
    );
    let from_start = &tail[..AUDITED_CAP];
    assert!(
        !from_start.contains("Nothing left"),
        "the control: the start alone holds no defense"
    );
    let long = format!("Nothing left: {}", "é".repeat(AUDITED_CAP));
    let cut = audited(&long);
    assert!(cut.len() <= AUDITED_CAP && long.starts_with(&cut), "{cut}");
    assert_eq!(audited("no defense here"), "no defense here");
}
