// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The `update*=` words a decoded `instance` line says, as its reader gets them: each value
/// pct-decoded once.
fn fields_of(tokens: &str) -> Vec<(String, String)> {
    tokens
        .split(' ')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), aterm_codec::pct::decode(v)))
        .collect()
}

fn full(standing: UpdateStanding) -> UpdateFact {
    UpdateFact {
        standing: StandingWord::Known(standing),
        checked_at: Some(1_800_000_000),
        to: Some("0.199.0+g5bb7817df4".to_owned()),
        until: Some(1_800_010_800),
        why: Some("crash-loop"),
        tries: Some(2),
        checks: Some(false),
        said_at: None,
    }
}

/// UPDATE STANDING WIRE: every standing's word reads back as itself and nothing else does; each
/// standing's fact crosses `tokens` and `of_fields` whole, a build named by number included; an
/// aterm that sends no `update=` says nothing; and a field outside its grammar reads as nothing
/// alone: an escape is no version, a path no reason, a word no time, while the rest of the fact
/// is kept. A newer aterm's grammatical word is kept as said; anything else is no standing.
/// NEGATIVE CONTROL: an `of_fields` that keeps `update_to` unchecked fails "an escape is no
/// version"; one that keeps `update_why` unchecked fails "a path is no reason".
#[test]
fn an_updaters_standing_crosses_the_wire_and_nothing_outside_its_grammar_does() {
    for standing in UpdateStanding::ALL {
        assert_eq!(UpdateStanding::from_word(standing.word()), Some(standing));
        assert_eq!(
            StandingWord::of(standing.word()),
            Some(StandingWord::Known(standing))
        );
        let fact = full(standing);
        assert_eq!(
            UpdateFact::of_fields(&fields_of(&fact.tokens())),
            Some(fact),
            "{standing:?} round-trips"
        );
        let bare = UpdateFact::of(standing);
        assert_eq!(
            UpdateFact::of_fields(&fields_of(&bare.tokens())),
            Some(bare)
        );
    }
    let mut numbered = full(UpdateStanding::Behind);
    numbered.to = Some(build_name(1234, None));
    assert_eq!(numbered.to.as_deref(), Some("build 1234"));
    assert!(numbered.tokens().contains(" update_to=build%201234 "));
    assert_eq!(
        UpdateFact::of_fields(&fields_of(&numbered.tokens())),
        Some(numbered)
    );

    assert_eq!(
        UpdateFact::of_fields(&fields_of(
            "pid=1 host=m version=0.162.0 sessions=0 window=1"
        )),
        None,
        "an older aterm says nothing"
    );

    let said = |tokens: &str| UpdateFact::of_fields(&fields_of(tokens)).expect("a standing");
    let escaped = said("update=held update_to=%1B%5B2J update_why=crash-loop update_tries=2");
    assert_eq!(escaped.to, None, "an escape is no version");
    assert_eq!(
        (escaped.why, escaped.tries),
        (Some("crash-loop"), Some(2)),
        "the rest is kept"
    );
    let pathed = said("update=failing update_why=/Users//someone/Library update_to=a@b.example");
    assert_eq!(pathed.why, None, "a path is no reason");
    assert_eq!(pathed.to, None, "an address is no version");
    let soon = said("update=current update_at=soon update_until=+5 update_checks=on");
    assert_eq!(
        (soon.checked_at, soon.until),
        (None, None),
        "a word is no time"
    );
    assert_eq!(soon.checks, Some(true), "the rest is kept");
    assert_eq!(
        said("update=standing-by").standing,
        StandingWord::Unknown("standing-by".to_owned()),
        "a newer aterm's word is kept"
    );
    for word in [
        "Held",
        "held!",
        "-held",
        "a-very-long-standing-word-indeed",
        "%1B",
    ] {
        assert_eq!(
            UpdateFact::of_fields(&fields_of(&format!("update={word}"))),
            None,
            "{word:?} is no standing"
        );
    }
}

/// The fact in words: a hold says its build, how long it holds, why and which try; the checked
/// suffix and the automatic-off suffix follow any standing; an unknown standing is said by its
/// word; and only an up-to-date updater with checks not said off is routine.
#[test]
fn the_facts_line_says_what_holds_how_long_and_when_it_last_checked() {
    let now = 1_800_000_000 + 10 * 60;
    let held = UpdateFact {
        said_at: Some(now),
        ..full(UpdateStanding::Held)
    };
    assert_eq!(
        held.line(),
        "0.199.0+g5bb7817df4 is held for 3 h more (crash-loop, try 2) \u{b7} checked 10 min ago \
         \u{b7} automatic checks off"
    );
    let mut lapsed = held.clone();
    lapsed.until = Some(now - 1);
    lapsed.checked_at = None;
    lapsed.checks = Some(true);
    assert_eq!(
        lapsed.line(),
        "0.199.0+g5bb7817df4 is held (crash-loop, try 2)"
    );
    let unknown = UpdateFact {
        standing: StandingWord::Unknown("standing-by".to_owned()),
        ..UpdateFact::of(UpdateStanding::Current)
    };
    assert_eq!(unknown.line(), "standing-by");
    let mut current = UpdateFact::of(UpdateStanding::Current);
    assert!(current.routine());
    current.checks = Some(false);
    assert!(!current.routine(), "checks off is said");
    assert!(!UpdateFact::of(UpdateStanding::Behind).routine());
    for standing in UpdateStanding::ALL {
        let line = full(standing).line();
        assert!(
            !line.is_empty() && !line.contains('\n'),
            "{standing:?}: {line:?}"
        );
    }
}

/// A FACT IS TOLD ON ITS OWN MACHINE'S CLOCK: the age of a peer's last check and the time left on
/// its hold are told by the clock its `instance` line was said on (that line's `at_ms=`), which
/// the reader takes with the fact, so a peer whose clock is hours off this Mac's still reads as
/// held for 3 h more and checked 10 min ago. A fact read with no clock tells neither, rather than
/// an age or a time left on a clock it was not said on.
/// NEGATIVE CONTROL: an `of_fields` that leaves `said_at` unread fails "on its own clock"; a `line`
/// that tells the time left with no clock fails "no clock, no time left".
#[test]
fn a_fact_is_told_on_the_clock_it_was_said_on() {
    let said = |line: &str| UpdateFact::of_fields(&fields_of(line)).expect("a standing");
    let held = "update=held update_at=1793129400 update_to=0.199.0 update_until=1793140800 \
                update_why=crash-loop update_tries=2 update_checks=on";
    let behind_hours = said(&format!("at_ms=1793130000999 {held}"));
    assert_eq!(behind_hours.said_at, Some(1_793_130_000));
    assert_eq!(
        behind_hours.line(),
        "0.199.0 is held for 3 h more (crash-loop, try 2) \u{b7} checked 10 min ago",
        "on its own clock"
    );
    assert_eq!(
        said(held).line(),
        "0.199.0 is held (crash-loop, try 2)",
        "no clock, no time left"
    );
    assert_eq!(said(&format!("at_ms=soon {held}")).said_at, None);
}

/// The plain-word helpers the updater's own lines and the fleet readers share.
#[test]
fn time_words_and_build_names_read_as_a_person_says_them() {
    assert_eq!(left_words(30), "under a minute");
    assert_eq!(left_words(90 * 60), "90 min");
    assert_eq!(left_words(5 * 3600 + 1), "6 h");
    assert_eq!(ago_words(100, 90), "just now");
    assert_eq!(ago_words(0, 59), "just now");
    assert_eq!(ago_words(0, 12 * 60 + 5), "12 min ago");
    assert_eq!(ago_words(0, 3 * 3600), "3 h ago");
    assert_eq!(ago_words(0, 86_400 + 5), "1 day ago");
    assert_eq!(ago_words(0, 3 * 86_400), "3 days ago");
    assert_eq!(build_name(9, Some("0.9.0")), "0.9.0");
    assert_eq!(build_name(9, Some("")), "build 9");
    assert_eq!(build_name(9, Some("/Users//x")), "build 9");
    assert!(is_build_name("build 9") && is_build_name("0.9.0"));
    for name in ["build ", "build x", "build  9", "a b", ""] {
        assert!(!is_build_name(name), "{name:?}");
    }
    assert_eq!(why_word("crash-loop"), Some("crash-loop"));
    assert_eq!(why_word("crash loop"), None);
}
