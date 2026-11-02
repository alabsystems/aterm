// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// LAND'S CONTRADICTION (v6 §14.4's 4c, `docs/DESIGN-harness-autopilot-2026-10-15.md`): where a
// land act's reply defends the work with `Nothing left:` while git still found work, or while
// the agent's own list of what is left still stands, Land's nudge opens with the contradiction,
// the measured fact or the quoted list first, under the nudge's own rule. The slice's As built
// note names the mutant each test was seen failing under.

use crate::harness::repo_work::{RepoRead, RepoWork};
use crate::supervise::policy::land_check;

/// A false defense: clean words over work git still finds.
const DEFENSE: &str = "Nothing left: the parser is committed and pushed to origin/main.";

/// L2's defense: clean words, a leftover beside them (the forever lab's `open_defense`).
const OPEN_DEFENSE: &str = "Nothing left: the parser is committed and pushed to origin/main.\n\
                            The docs PR is still open.";

/// The agent's list, answering the check.
const LIST: &str = "The docs PR is still open.";

/// An answer to the check that carries a defense line beside the agent's list.
const CHECK_DEFENSE: &str = "Nothing left: all but one item.\nThe docs PR is still open.";

/// The words a contradiction must never say: each would clear git's probe or the reader without
/// landing anything (§14.4: "ignore it", "move it out", "say it is not open").
const ESCAPES: [&str; 3] = ["ignore", "move", "not open"];

/// land-c's repository: one untracked file.
fn untracked() -> RepoWork {
    RepoWork {
        repo: "/private/tmp/at.x/proj/land-c".to_string(),
        untracked: 1,
        paths: vec!["wip-notes.txt".to_string()],
        main_ref: "origin/main".to_string(),
        ..RepoWork::default()
    }
}

/// Land on, its instruction typed at `t0`; the state and when the instruction's turn ends.
fn instructed() -> (TurnEndState, Instant) {
    let mut st = TurnEndState::default();
    assert_eq!(
        act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), t0()),
        land_instruction()
    );
    (st, t0() + 3 * MIN)
}

/// A short answer `said`, no tools run, over `repo`.
fn short_over(said: &str, repo: RepoRead) -> TurnEndReading {
    TurnEndReading {
        ran_tools: false,
        repo,
        ..land(idle(said, Some(Duration::from_secs(10))))
    }
}

/// The text `a` types and its rule; a panic where it types nothing.
fn typed_of(a: &TurnEndAction) -> (&str, &str) {
    match a {
        TurnEndAction::Type { text, rule_id } => (text, rule_id),
        other => panic!("nothing typed: {other:?}"),
    }
}

/// The contradiction typed after Land's instruction, at `said`'s point over `repo`, its back-off
/// waited out: the act, the state and when.
fn contradicted_at(said: &str, repo: RepoRead) -> (TurnEndAction, TurnEndState, Instant) {
    let (mut st, now) = instructed();
    let r = short_over(said, repo);
    let (a, now) = past_backoff(&mut st, &r, now);
    st.acted(&a, &r, now);
    (a, st, now)
}

/// L2's defense after real work, over a clean repository: what Land's check audits.
fn worked_defense() -> TurnEndReading {
    TurnEndReading {
        repo: RepoRead::Clean,
        ..land(idle(OPEN_DEFENSE, Some(3 * MIN)))
    }
}

/// Land's instruction answered by L2's defense after real work, the check it owes answered by
/// `list` and the nudge quoting that list typed: the state and when.
fn listed(list: &str) -> (TurnEndState, Instant) {
    let (mut st, now) = instructed();
    let r = worked_defense();
    let (a, now) = past_backoff(&mut st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "the check: {a:?}");
    st.acted(&a, &r, now);
    let answer = short_over(list, RepoRead::Clean);
    let (b, now) = past_backoff(&mut st, &answer, now + MIN);
    let (text, rule) = typed_of(&b);
    assert_eq!(rule, RULE_LAND_NUDGE, "{b:?}");
    assert!(
        text.starts_with(LAND_NUDGE),
        "the check's own answer contradicts nothing: {text}"
    );
    st.acted(&b, &answer, now);
    (st, now)
}

/// A FALSE DEFENSE OVER WORK GIT FOUND LEADS WITH THE CONTRADICTION: the nudge after it, typed
/// under the nudge's own rule, opens "Still landing: your last reply says Nothing left, but" and
/// then git's finding, before Land's own words, and asks only that the work be landed.
/// Negative control: the old suffix order (LAND_NUDGE, then the repository's suffix) fails it.
#[test]
fn a_false_defense_over_work_git_found_leads_with_the_contradiction() {
    let (a, _, _) = contradicted_at(DEFENSE, RepoRead::Found(untracked()));
    let (text, rule) = typed_of(&a);
    assert_eq!(rule, RULE_LAND_NUDGE, "{text}");
    let first = text.split(". ").next().unwrap_or_default();
    assert!(
        first.starts_with(LAND_CONTRADICTED_HEAD),
        "the first sentence: {first}"
    );
    let lead = format!(
        "{LAND_CONTRADICTED_HEAD} {CONTRADICTED_GIT} ({}). {CONTRADICTED_GIT_ASK}",
        untracked().summary()
    );
    assert!(text.starts_with(&lead), "the fact leads: {text}");
    assert!(lead.contains("wip-notes.txt"), "{lead}");
    let fact = text.find(CONTRADICTED_GIT).expect("the fact");
    let close = text.find(NO_REPOSITORY).expect("Land's close");
    assert!(fact < close, "the fact before Land's own words: {text}");
    assert!(
        !text.contains("Your repository still holds work"),
        "the repository's suffix said again: {text}"
    );
    assert!(is_land_words(text), "{text}");
}

/// THE CONTRADICTION KEEPS LAND'S NO-REPOSITORY SENTENCE AND OFFERS NO WAY OUT: on either leg,
/// with or without the clauses that ride it, it says files no repository holds land where they
/// are (so a plain folder raises no need for a remote), says never to delete work, and never
/// says to ignore, move or call the work not open.
/// Negative controls: the close without `NO_REPOSITORY` fails it, and so does an ask that says
/// "or ignore it" or "move it out".
#[test]
fn the_contradiction_keeps_the_no_repository_sentence_and_offers_no_escape() {
    let found = RepoRead::Found(untracked());
    let forms = [
        land_contradicted(&found, None, &[], None),
        land_contradicted(
            &found,
            Some(LIST),
            &[DONE_CHECK_REVERSIBLE, NEEDS_ACK],
            None,
        ),
        land_contradicted(&RepoRead::Clean, Some(LIST), &[], None),
        land_contradicted(&RepoRead::Unread, Some(LIST), &[NEEDS_ACK], None),
    ];
    for text in forms {
        let text = text.expect("a leg holds");
        assert!(text.starts_with(LAND_CONTRADICTED_HEAD), "{text}");
        assert!(text.contains(NO_REPOSITORY), "{text}");
        assert!(text.contains("never delete work"), "{text}");
        let lower = text.to_lowercase();
        for escape in ESCAPES {
            assert!(!lower.contains(escape), "{escape:?} in {text}");
        }
    }
    assert_eq!(
        land_contradicted(&RepoRead::Clean, None, &[], None),
        None,
        "no leg, no contradiction"
    );
    assert_eq!(land_contradicted(&RepoRead::Unread, None, &[], None), None);
}

/// THE OPEN LEG'S CONTRADICTION QUOTES THE AGENT'S LIST WITHIN 120 CHARACTERS: the check
/// answered with a list and the nudge quoting it answered by the defense the check audited, no
/// work between, Land's next nudge opens with the contradiction and the list, its first item
/// leading; a long list is cut to [`CONTRADICTED_QUOTE_UNITS`] with `…`. The check's own answer,
/// though it carries `Nothing left:`, contradicts nothing of its own. (The defense is first said
/// after real work, so the reply to the nudge is its first no-work saying, not the same reply
/// again, which holds before any act.)
/// Negative control: the Open leg left to [`LAND_NUDGE`] fails the lead.
#[test]
fn the_open_legs_contradiction_quotes_the_list_within_120_characters() {
    let (mut st, now) = listed(LIST);
    let r = short_over(OPEN_DEFENSE, RepoRead::Clean);
    let (a, _) = past_backoff(&mut st, &r, now + MIN);
    let (text, rule) = typed_of(&a);
    assert_eq!(rule, RULE_LAND_NUDGE, "{text}");
    let lead = format!(
        "{LAND_CONTRADICTED_HEAD} {CONTRADICTED_LISTED}{LIST}\u{201d} {CONTRADICTED_LISTED_ASK}"
    );
    assert!(text.starts_with(&lead), "{text}");
    assert!(
        !text.contains(land_check::LISTED_OPEN),
        "quoted once: {text}"
    );
    let long = "The docs PR is still open and waits on a review from the docs owners, who are \
                away until Monday.\nThe version bump for the parser crate is not pushed yet.";
    let text = land_contradicted(&RepoRead::Clean, Some(long), &[], None).expect("the list");
    let quote = text
        .split_once(CONTRADICTED_LISTED)
        .and_then(|(_, q)| q.split_once('\u{201d}'))
        .map(|(q, _)| q)
        .expect("quoted");
    assert!(quote.starts_with("The docs PR is still open"), "{quote}");
    assert!(
        units(quote) <= CONTRADICTED_QUOTE_UNITS && quote.ends_with('\u{2026}'),
        "{} units: {quote}",
        units(quote)
    );
    // The check answered with words that carry a defense: its own list, no contradiction.
    let (mut st, now) = instructed();
    let r = worked_defense();
    let (a, now) = past_backoff(&mut st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "the check: {a:?}");
    st.acted(&a, &r, now);
    let answer = short_over(CHECK_DEFENSE, RepoRead::Clean);
    let (b, _) = past_backoff(&mut st, &answer, now + MIN);
    let (text, _) = typed_of(&b);
    assert!(
        text.starts_with(LAND_NUDGE),
        "the check's answer is no contradiction of itself: {text}"
    );
}

/// GIT'S FACT LEADS WHERE BOTH LEGS HOLD, AND THE LIST STILL RIDES WITH ITS ASK TO RESOLVE: the
/// agent's list stands and git finds work, so the contradiction opens with the measured fact and
/// the list follows as Land's nudge quotes it (§14.17 item 1: Land keeps resolving).
/// Negative control: the list's quote led where git found work fails the lead.
#[test]
fn gits_fact_leads_where_both_legs_hold_and_the_list_rides() {
    let text =
        land_contradicted(&RepoRead::Found(untracked()), Some(LIST), &[], None).expect("both legs");
    assert!(
        text.starts_with(&format!("{LAND_CONTRADICTED_HEAD} {CONTRADICTED_GIT}")),
        "{text}"
    );
    let quoted = land_check::listed_suffix(LIST).expect("quoted");
    assert!(text.contains(&quoted), "{text}");
    assert!(
        text.find(NO_REPOSITORY) < text.find(land_check::LISTED_OPEN),
        "the list after Land's own words, as its nudge carries it: {text}"
    );
}

/// EVERY READER OF A TYPED LAND ACT READS THE CONTRADICTION AS ONE (limit 4): the policy's
/// `is_land_words`, the ledger's `delivers_land`, the transcript's `is_land_act` and the stats'
/// `opens_with_land_words` (on a journal line as clipped), each by its head.
/// Negative controls: each reader's list without [`LAND_CONTRADICTED_HEAD`] misses it.
#[test]
fn every_reader_reads_the_contradiction_as_lands_act() {
    let found = RepoRead::Found(untracked());
    let texts = [
        land_contradicted(&found, None, &[], None),
        land_contradicted(&RepoRead::Clean, Some(LIST), &[], Some("never force-push")),
    ];
    for text in texts {
        let text = text.expect("a leg holds");
        assert!(is_land_words(&text), "is_land_words: {text}");
        assert!(
            crate::supervise::approvals::delivers_land(Some(&text)),
            "delivers_land: {text}"
        );
        assert!(
            crate::harness::upgrade::is_land_act(&text),
            "is_land_act: {text}"
        );
        let clipped: String = text
            .chars()
            .take(crate::supervise::run::LINE_CHARS)
            .collect();
        assert!(
            crate::harness::stats::opens_with_land_words(&clipped),
            "opens_with_land_words: {clipped}"
        );
    }
}

/// ONE ACT PER POINT, UNDER THE BACK-OFF (§14.17: never spam the agent): the contradiction waits
/// the short streak's back-off as the nudge it replaces does, and the same reply after it holds,
/// with nothing typed.
/// Negative control: none needed beyond the hold's own (`TurnEndState::repeats`): the
/// contradiction is the nudge's words, its decision the nudge's.
#[test]
fn the_contradiction_is_one_act_per_point_under_the_back_off() {
    let (mut st, start) = instructed();
    let r = short_over(DEFENSE, RepoRead::Found(untracked()));
    let (a, now) = past_backoff(&mut st, &r, start);
    assert!(now > start, "typed only once the back-off was waited out");
    assert!(typed_of(&a).0.starts_with(LAND_CONTRADICTED_HEAD), "{a:?}");
    st.acted(&a, &r, now);
    let again = short_over(DEFENSE, RepoRead::Found(untracked()));
    let (b, _) = past_backoff(&mut st, &again, now + MIN);
    assert_eq!(b, TurnEndAction::Nothing, "the same reply holds: {b:?}");
}

/// THE CONTRADICTION FITS THE VERBATIM BOUND WITH THE RULES: with the longest finding, a standing
/// list, the reversible clause, NEEDS YOU's acknowledgment and long standing rules it stays within
/// [`VERBATIM_UNITS`], its head, its lead's words, its ask and Land's close whole.
/// Negative control: the finding left uncut (`RepoWork::summary` whole, not [`repo_finding`])
/// runs past the bound.
#[test]
fn the_contradiction_fits_the_verbatim_bound_with_the_rules() {
    let big = RepoWork {
        repo: format!(
            "/Users//a/Developer/{}",
            "a-rather-long-checkout-name-".repeat(3)
        ),
        changed: 41,
        untracked: 17,
        stashes: 3,
        unpushed: 9,
        unlanded: 2,
        paths: (0..12)
            .map(|i| format!("crates/a-long-crate-name-{i}/src/a_rather_long_module_{i}.rs"))
            .collect(),
        main_ref: "origin/main".to_string(),
        ..RepoWork::default()
    };
    let list = "The docs PR is still open and waits on a review from the docs owners.\n\
                The version bump is not pushed.\nThe e2e suite was not run.";
    let rules = "keep each commit small and name the stage it lands ".repeat(8);
    let after = [DONE_CHECK_REVERSIBLE, NEEDS_ACK];
    for (repo, listed, ask) in [
        (
            RepoRead::Found(big.clone()),
            Some(list),
            CONTRADICTED_GIT_ASK,
        ),
        (RepoRead::Found(big.clone()), None, CONTRADICTED_GIT_ASK),
        (RepoRead::Unread, Some(list), CONTRADICTED_LISTED_ASK),
        (RepoRead::Clean, Some(list), CONTRADICTED_LISTED_ASK),
    ] {
        for rules in [None, Some(rules.as_str())] {
            let text = land_contradicted(&repo, listed, &after, rules).expect("a leg holds");
            let n = units(&text);
            assert!(n <= VERBATIM_UNITS, "{n} units: {text}");
            for whole in [
                LAND_CONTRADICTED_HEAD,
                ask,
                NO_REPOSITORY,
                DONE_CHECK_REVERSIBLE,
                NEEDS_ACK,
            ] {
                assert!(text.contains(whole), "{whole:?} in {text}");
            }
            if listed.is_some() && repo.found().is_some() {
                assert!(text.contains(land_check::LISTED_RESOLVE), "{text}");
            }
        }
    }
}

/// A NEW DEFENSE OVER A CLEAN REPOSITORY GETS THE LAND CHECK, NOT THE CONTRADICTION: the agent's
/// list stands and the nudge quoting it was answered by a defense the check has not audited; over
/// a clean read the check (13a, §14.17.13) outranks the Open leg's contradiction, once per
/// defense, and over a repository the probe could not read (no check there) the same defense gets
/// the contradiction quoting the list, then [`REPO_UNREAD`]. So the Open leg's contradiction is
/// typed only for an audited defense said again, or over an unread repository.
/// Negative control: [`contradicted`] asked before the check's arm in `land_words` types the
/// contradiction over the clean read.
#[test]
fn a_new_defense_over_a_clean_repository_gets_the_check_not_the_contradiction() {
    let fresh = "Nothing left: the parser, the lexer and the docs are merged and pushed.";
    let (mut st, now) = listed(LIST);
    let r = short_over(fresh, RepoRead::Clean);
    let (a, _) = past_backoff(&mut st, &r, now + MIN);
    let (text, rule) = typed_of(&a);
    assert_eq!(rule, RULE_LAND_CHECK, "the check outranks: {text}");
    assert!(!text.contains(LAND_CONTRADICTED_HEAD), "{text}");
    let (mut st, now) = listed(LIST);
    let r = short_over(fresh, RepoRead::Unread);
    let (a, _) = past_backoff(&mut st, &r, now + MIN);
    let (text, rule) = typed_of(&a);
    assert_eq!(rule, RULE_LAND_NUDGE, "{text}");
    assert!(
        text.starts_with(&format!(
            "{LAND_CONTRADICTED_HEAD} {CONTRADICTED_LISTED}{LIST}\u{201d}"
        )),
        "{text}"
    );
    assert!(
        text.find(NO_REPOSITORY) < text.find(REPO_UNREAD),
        "the unread read after Land's close: {text}"
    );
}
