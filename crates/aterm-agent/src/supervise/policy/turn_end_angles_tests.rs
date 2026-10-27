// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// v6 §14.5 (slice 5a): Cruise asks for a new kind of work at each done claim.

/// Done claims, each after real work and each in words of its own, so no reply is said twice.
const CLAIMS: [&str; 7] = [
    "Fixed the lexer and pushed it. All done.",
    "Fixed the parser and pushed it. All done.",
    "Fixed the printer and pushed it. All done.",
    "Fixed the loader and pushed it. All done.",
    "Fixed the docs and pushed them. All done.",
    "Fixed the cache and pushed it. All done.",
    "Fixed the config and pushed it. All done.",
];

const WORK: &str = "Stage 3 is in; the lexer is next.";

/// Standing rules of exactly `units` units, in words, so the fit cuts them at a word.
fn rules_of(units: usize) -> String {
    let mut rules = String::new();
    for word in [
        "never", "push", "to", "main", "and", "keep", "each", "commit", "small",
    ]
    .iter()
    .cycle()
    {
        let more = usize::from(!rules.is_empty()) + word.len();
        if rules.len() + more > units {
            break;
        }
        if !rules.is_empty() {
            rules.push(' ');
        }
        rules.push_str(word);
    }
    while rules.len() < units {
        rules.push('.');
    }
    rules
}

fn ruled(rules: Option<&str>, r: TurnEndReading) -> TurnEndReading {
    TurnEndReading {
        rules: rules.map(str::to_string),
        ..r
    }
}

/// A Cruise run's first turn, Cruise's continuation typed after it.
fn cruising(rules: Option<&str>) -> (TurnEndState, Instant) {
    let mut st = TurnEndState::default();
    let now = t0();
    let a = act(
        &mut st,
        &ruled(rules, idle("Stage 1 landed.", Some(3 * MIN))),
        now,
    );
    assert_eq!(a.rule_id(), Some(RULE_CONTINUE), "{a:?}");
    (st, now + 3 * MIN)
}

/// The done check `claim` gets, typed past the short streak's back-off, and when the next turn
/// ends.
fn claim_checked(
    st: &mut TurnEndState,
    claim: &str,
    rules: Option<&str>,
    now: Instant,
) -> (String, Instant) {
    let (a, then) = until_acted(st, &ruled(rules, idle(claim, Some(3 * MIN))), now);
    let TurnEndAction::Type {
        text,
        rule_id: RULE_DONE_CHECK,
    } = a
    else {
        panic!("{claim}: {a:?}");
    };
    (text, then + 3 * MIN)
}

/// Six done claims in one Cruise run type six distinct acts under `continue-done-check@v1`, each
/// [`DONE_ANGLES`]' next angle whole, and the seventh asks the first again; with a 400-unit rules
/// suffix each still fits [`VERBATIM_UNITS`], its words whole and only the rules cut. NEGATIVE
/// CONTROL: with `acted`'s count bump taken out, all seven acts are the first angle's and the test
/// fails at its first comparison.
#[test]
fn six_done_claims_ask_six_kinds_of_work_then_the_first_again() {
    let typed = |rules: Option<&str>| {
        let (mut st, mut now) = cruising(rules);
        let mut typed = Vec::new();
        for claim in CLAIMS {
            let (text, then) = claim_checked(&mut st, claim, rules, now);
            typed.push(text);
            now = then;
        }
        typed
    };
    let bare = typed(None);
    let words: Vec<String> = (0..CLAIMS.len()).map(done_check_words).collect();
    assert_eq!(bare, words, "each claim's check, in turn");
    let distinct: std::collections::BTreeSet<&String> = bare[..DONE_ANGLES.len()].iter().collect();
    assert_eq!(distinct.len(), DONE_ANGLES.len(), "{bare:#?}");
    assert_eq!(
        bare[DONE_ANGLES.len()],
        bare[0],
        "the seventh is the first again"
    );
    for (n, angle) in DONE_ANGLES.iter().enumerate() {
        assert!(
            bare[n].contains(angle) && bare[n].ends_with(DONE_TAIL),
            "{}",
            bare[n]
        );
    }
    let rules = rules_of(400);
    assert_eq!(rules.encode_utf16().count(), 400);
    for (n, text) in typed(Some(&rules)).iter().enumerate() {
        let units = text.encode_utf16().count();
        assert!(units <= VERBATIM_UNITS, "{n}: {units} units: {text}");
        let whole = format!(
            "{} (standing rules: never push to main",
            done_check_words(n)
        );
        assert!(
            text.starts_with(&whole),
            "{n}: the angle whole, then the rules: {text}"
        );
    }
}

/// Every angle's act fits [`VERBATIM_UNITS`] with every suffix a claim can carry (its open work,
/// the repository's, the reversible clause and NEEDS YOU's acknowledgement) and a 400-unit rules
/// suffix: the angle and the tail whole, the reversible clause and the acknowledgement whole.
#[test]
fn every_angles_act_fits_the_verbatim_bound_with_every_suffix() {
    let rules = rules_of(400);
    let claim = "All done.\nStill open: the lexer.\nRemaining work: the e2e suite.\nNEEDS YOU: \
                 approve the force-push to main";
    for n in 0..DONE_ANGLES.len() {
        let mut st = TurnEndState::default();
        act(&mut st, &idle("Stage 1 landed.", Some(3 * MIN)), t0());
        st.seed_checks(u8::try_from(n).expect("an angle"));
        let r = over_repo(
            ruled(Some(&rules), idle(claim, Some(3 * MIN))),
            uncommitted_repo(),
        );
        let (a, _) = past_backoff(&mut st, &r, t0() + 3 * MIN);
        let TurnEndAction::Type { text, rule_id } = &a else {
            panic!("{n}: {a:?}");
        };
        assert_eq!(*rule_id, RULE_DONE_CHECK);
        let units = text.encode_utf16().count();
        assert!(units <= VERBATIM_UNITS, "{n}: {units} units: {text}");
        assert!(text.starts_with(&done_check_words(n)), "{n}: {text}");
        for whole in [DONE_CHECK_REVERSIBLE, NEEDS_ACK] {
            assert!(text.contains(whole), "{n}: {whole:?} in {text}");
        }
    }
}

/// Someone's turn (a person's prompt between claims) starts the angles again; the agent's own
/// work between its claims does not, so a worker that works between its claims still meets every
/// angle. NEGATIVE CONTROL: with the someone's-turn reset taken out of the point's fold, the claim
/// after the person's turn asks the third angle and the test fails.
#[test]
fn someones_turn_starts_the_angles_again_and_the_agents_own_work_does_not() {
    let (mut st, mut now) = cruising(None);
    let (first, then) = claim_checked(&mut st, CLAIMS[0], None, now);
    assert_eq!(first, done_check_words(0));
    now = then;
    let a = act(&mut st, &idle(WORK, Some(3 * MIN)), now);
    assert_eq!(
        a.rule_id(),
        Some(RULE_CONTINUE),
        "the check answered with work: {a:?}"
    );
    now += 3 * MIN;
    let (second, then) = claim_checked(&mut st, CLAIMS[1], None, now);
    assert_eq!(
        second,
        done_check_words(1),
        "the agent's own work kept the count"
    );
    now = then;
    assert_eq!(st.checks(), 2);
    let a = at(&mut st, &idle(CLAIMS[2], Some(3 * MIN)), now);
    assert!(
        matches!(a, TurnEndAction::WaitUntil { .. }),
        "the claim backed off: {a:?}"
    );
    now += MIN;
    let theirs = TurnEndReading {
        person: Some(Duration::from_secs(8)),
        ..idle(
            "Hi! What would you like me to work on?",
            Some(Duration::from_secs(5)),
        )
    };
    // Their hands on the keys hold the act; Cruise leaves the exchange to nobody (owner ruling
    // 2026-10-26).
    let a = act(&mut st, &theirs, now);
    assert!(matches!(a, TurnEndAction::WaitUntil { .. }), "{a:?}");
    assert_eq!(st.checks(), 0, "someone's turn starts the angles again");
    now += 3 * MIN;
    let (after, _) = claim_checked(&mut st, CLAIMS[3], None, now);
    assert_eq!(after, done_check_words(0));
}

/// A word other than Cruise ends the run's count: back under Cruise, the next claim asks the first
/// angle. NEGATIVE CONTROL: with `observe`'s reset out of Cruise taken out, the claim back under
/// Cruise asks the second angle and the test fails.
#[test]
fn a_switch_out_of_cruise_starts_the_angles_again() {
    for other in [Autopilot::Land, Autopilot::Off] {
        let (mut st, now) = cruising(None);
        let (first, mut now) = claim_checked(&mut st, CLAIMS[0], None, now);
        assert_eq!(first, done_check_words(0));
        assert_eq!(st.checks(), 1);
        act(&mut st, &under(other, idle(WORK, Some(3 * MIN))), now);
        assert_eq!(st.checks(), 0, "{other:?}");
        now += 3 * MIN;
        let a = act(
            &mut st,
            &idle("Stage 4 is in; the docs are next.", Some(3 * MIN)),
            now,
        );
        assert_eq!(a.rule_id(), Some(RULE_CONTINUE), "{other:?}: {a:?}");
        now += 3 * MIN;
        let (back, _) = claim_checked(&mut st, CLAIMS[1], None, now);
        assert_eq!(back, done_check_words(0), "{other:?}");
    }
}

/// A loop after this one asks the next angle: [`TurnEndState::seed_checks`] (the count its note
/// carried) is where the angles go on. Past the wrap the count goes back to one whole turn of the
/// six, never 0.
#[test]
fn a_seeded_count_asks_the_angle_after_it() {
    for (seeded, after) in [(0_u8, 1_u8), (1, 2), (5, 6), (6, 7), (251, 6)] {
        let (mut st, now) = cruising(None);
        st.seed_checks(seeded);
        let (text, _) = claim_checked(&mut st, CLAIMS[0], None, now);
        assert_eq!(text, done_check_words(usize::from(seeded)), "{seeded}");
        assert_eq!(st.checks(), after, "{seeded}");
    }
}

/// THE COUNT GOES ON IN TURN AT AND PAST ITS WRAP. A seeded count at or past [`CHECKS_WRAP`] (no
/// loop writes one; a ledger's note is read as any `u8`) asks its own angle and counts on; and the
/// check typed at the wrap leaves a count a cut retypes that check by, angle 6, not angle 1.
/// NEGATIVE CONTROLS: with the seed stored as carried, a seeded 252's next claim asks angle 1
/// where angle 2 is due (and under the bump before this test, `(checks + 1) % 252`, 255's check
/// panicked: `attempt to add with overflow`); with the wrap going back to 0, the cut sixth is
/// retyped as angle 1.
#[test]
fn a_count_at_or_past_the_wrap_goes_on_in_turn() {
    for seeded in [252_u8, 253, 255] {
        let (mut st, now) = cruising(None);
        st.seed_checks(seeded);
        let (text, now) = claim_checked(&mut st, CLAIMS[0], None, now);
        assert_eq!(text, done_check_words(usize::from(seeded)), "{seeded}");
        let (next, _) = claim_checked(&mut st, CLAIMS[1], None, now + 3 * MIN);
        assert_eq!(next, done_check_words(usize::from(seeded) + 1), "{seeded}");
    }
    let (mut st, now) = cruising(None);
    st.seed_checks(CHECKS_WRAP - 1);
    let (sixth, _) = claim_checked(&mut st, CLAIMS[0], None, now);
    assert_eq!(sixth, done_check_words(DONE_ANGLES.len() - 1));
    let carried = st.checks();
    assert_ne!(carried, 0, "a count above 0 names the angle typed last");
    let mut cut = TurnEndState::default();
    cut.seed_checks(carried);
    cut.seed_compact_check(RULE_DONE_CHECK);
    let a = act(
        &mut cut,
        &idle("Conversation compacted.", Some(Duration::from_secs(40))),
        t0(),
    );
    assert_eq!(
        a,
        TurnEndAction::Type {
            text: done_check_act(DONE_ANGLES.len() - 1, None, true),
            rule_id: RULE_COMPACT,
        },
        "the cut check at the wrap is retyped at its own angle"
    );
}

/// A check a full context cut before this loop is typed again, after the compaction, at the angle
/// the loop before typed last: the count its note carried less one, and angle 1 where it carried
/// none. NEGATIVE CONTROL: the retype at the carried count itself asks the angle after the cut one.
#[test]
fn a_cut_check_is_typed_again_at_the_angle_the_cut_took() {
    let compacted = idle("Conversation compacted.", Some(Duration::from_secs(40)));
    for (carried, angle) in [(0_u8, 0_usize), (1, 0), (2, 1), (6, 5), (7, 0)] {
        let mut st = TurnEndState::default();
        st.seed_checks(carried);
        st.seed_compact_check(RULE_DONE_CHECK);
        let a = act(&mut st, &compacted, t0());
        let TurnEndAction::Type {
            text,
            rule_id: RULE_COMPACT,
        } = &a
        else {
            panic!("{carried}: {a:?}");
        };
        assert_eq!(*text, done_check_act(angle, None, true), "{carried}");
        assert_eq!(st.checks(), carried, "a retype asks nothing new: {carried}");
    }
}

/// Every angle's check, bare, with every suffix and the rules, and v1's fixed check with its
/// suffixes, is the done check to [`is_done_check`]. NEGATIVE CONTROL: v1 read under a base that
/// is not its prefix is not a done check, so v1 is read only through the base it shares.
#[test]
fn every_angle_and_v1s_check_is_read_as_the_done_check() {
    let open = open_work_suffix(&["the lexer"]).expect("open");
    let repo = own_repo_work_suffix(&uncommitted_repo());
    let suffixes = [
        open.as_str(),
        repo.as_str(),
        DONE_CHECK_REVERSIBLE,
        NEEDS_ACK,
    ];
    let mut texts = Vec::new();
    for n in 0..DONE_ANGLES.len() {
        let words = done_check_words(n);
        texts.push(act_words(&words, &suffixes, Some("never push to main")));
        texts.push(done_check_act(n, Some("never push to main"), true));
        texts.push(words);
    }
    texts.push(DONE_CHECK_FIXED_BEFORE.to_string());
    texts.push(act_words(
        DONE_CHECK_FIXED_BEFORE,
        &suffixes,
        Some("never push to main"),
    ));
    for text in &texts {
        assert!(is_done_check(text), "{text}");
    }
    assert!(DONE_CHECK_FIXED_BEFORE.starts_with(DONE_CHECK));
    assert!(!RETIRED_DONE_CHECKS.contains(&DONE_CHECK_FIXED_BEFORE));
    let moved = DONE_CHECK_FIXED_BEFORE.replacen("done is never the end.", "never stop.", 1);
    assert_ne!(moved, DONE_CHECK_FIXED_BEFORE);
    assert!(!is_done_check(&moved), "{moved}");
}

/// A repository holding only another session's work: a stash the agent did not make, which the
/// probe counts as the repository's ([`crate::harness::repo_work::RepoWork::stashes`]).
fn foreign_stash() -> crate::harness::repo_work::RepoWork {
    crate::harness::repo_work::RepoWork {
        repo: "/private/tmp/at.rw40/proj/cruise-angles".to_string(),
        stashes: 1,
        state: Some(0x0757),
        ..Default::default()
    }
}

/// EVERY ANGLE'S ACT, AS COMPOSED, ASKS ONLY FOR THE AGENT'S OWN WORK. Over a repository whose
/// only finding is another session's stash, and over one the probe could not read, with and
/// without open work and the rules, each of the six acts names the finding, asks the agent to land
/// what of it it made and to leave the rest exactly as it is, and never to commit, merge and push
/// all of it; none says a word that ends someone else's work. Land's nudge over the same finding
/// still asks for all of it. NEGATIVE CONTROL: the done check composed with Land's repository
/// ask (main's words: "commit, merge and push it to the remote main first") fails at angle 1.
#[test]
fn every_angles_composed_act_asks_only_for_the_agents_own_work() {
    let theirs = foreign_stash();
    assert!(theirs.found());
    for repo in [
        crate::harness::repo_work::RepoRead::Found(theirs.clone()),
        crate::harness::repo_work::RepoRead::Unread,
    ] {
        for claim in ["All done.", "All done.\nStill open: the lexer."] {
            for rules in [None, Some("never push to main")] {
                for n in 0..DONE_ANGLES.len() {
                    let mut st = TurnEndState::default();
                    act(&mut st, &idle("Stage 1 landed.", Some(3 * MIN)), t0());
                    st.seed_checks(u8::try_from(n).expect("an angle"));
                    let r = TurnEndReading {
                        repo: repo.clone(),
                        ..ruled(rules, idle(claim, Some(3 * MIN)))
                    };
                    let (a, _) = past_backoff(&mut st, &r, t0() + 3 * MIN);
                    let TurnEndAction::Type {
                        text,
                        rule_id: RULE_DONE_CHECK,
                    } = &a
                    else {
                        panic!("{n}: {a:?}");
                    };
                    let what = format!("angle {} over {repo:?}, {claim:?}", n + 1);
                    assert!(text.starts_with(&done_check_words(n)), "{what}: {text}");
                    assert_eq!(own_work_fault(text), None, "{what}: {text}");
                }
            }
        }
    }
    let mut st = TurnEndState::default();
    act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), t0());
    let r = land(over_repo(idle(DEFENSE, Some(3 * MIN)), theirs.clone()));
    let (a, _) = past_backoff(&mut st, &r, t0() + 3 * MIN);
    assert_eq!(
        a,
        TurnEndAction::Type {
            text: act_words(LAND_NUDGE, &[&repo_work_suffix(&theirs)], None),
            rule_id: RULE_LAND_NUDGE,
        },
        "Land's nudge is unchanged"
    );
    let main_words = act_words(&done_check_words(0), &[&repo_work_suffix(&theirs)], None);
    assert_eq!(
        own_work_fault(&main_words),
        Some("tells the agent to commit, merge and push all of it")
    );
}

/// What [`every_angles_composed_act_asks_only_for_the_agents_own_work`] holds a composed act to,
/// or `None` where it holds: the repository's finding is named, only the agent's own work is asked
/// for, and no word ends someone else's work.
fn own_work_fault(text: &str) -> Option<&'static str> {
    if text.contains("commit, merge and push it") || text.contains("land what it holds") {
        return Some("tells the agent to commit, merge and push all of it");
    }
    if !text.contains("Your repository") {
        return Some("names no repository");
    }
    if !text.contains(
        "land what of it you made first, and leave what you did not make exactly as it is",
    ) {
        return Some("asks for more than the agent's own work");
    }
    destroys(text).map(|_| "says a word that ends someone else's work")
}

/// What `no_angle_destroys` holds an angle or the tail to: none of the words that end someone
/// else's work, and angle 6 names only what the agent made.
fn destroys(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    ["delete", "remove", "reset", "retire", "drop", "prune"]
        .into_iter()
        .find(|word| lower.contains(word))
}

/// `no_angle_destroys`: no angle and not the tail asks to delete, remove, reset, retire, drop or
/// prune anything, and angle 6 asks only for what the agent made. NEGATIVE CONTROL: the first
/// draft's angle 6 ("land or retire every worktree … this repository holds") fails both.
#[test]
fn no_angle_destroys() {
    for text in DONE_ANGLES.iter().chain([&DONE_TAIL, &DONE_CHECK]) {
        assert_eq!(destroys(text), None, "{text}");
    }
    let sixth = DONE_ANGLES[DONE_ANGLES.len() - 1];
    assert!(sixth.contains("you made"), "{sixth}");
    let draft = "This time, land or retire every worktree, branch and stash this repository holds \
                 that no remote main has.";
    assert_eq!(destroys(draft), Some("retire"));
    assert!(!draft.contains("you made"));
}
