// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_bye_word_round_trips_and_nothing_else_parses() {
    for bye in [Bye::Quit, Bye::Handoff] {
        assert_eq!(Bye::parse(bye.word()), Some(bye));
    }
    for other in ["", "Quit", "exit", "quit ", "handoff=1"] {
        assert_eq!(Bye::parse(other), None, "{other:?}");
    }
}

#[test]
fn a_bus_offset_is_digits_with_or_without_its_at_sign() {
    assert_eq!(parse_bus_offset("21"), Some(21));
    assert_eq!(parse_bus_offset("@21"), Some(21));
    for bad in [
        "",
        "@",
        "@@21",
        "+5",
        "@+5",
        "-1",
        "21 ",
        "x",
        "18446744073709551616",
    ] {
        assert_eq!(parse_bus_offset(bad), None, "{bad:?}");
    }
}

/// `help fabric` names exactly the words `fabric status` prints after `mirror=`: every
/// [`MirrorState`], one generated list with its word table. The words are read only from
/// the help's quoted `fabric status` reply (the backticked span that starts `OK `) and its
/// `mirror=` slots, so the prose that repeats a word (`mirror=unserved` when no bridge
/// named `log=`) never stands in for one the reply's template dropped.
#[test]
fn help_fabric_names_exactly_the_mirror_words_fabric_status_prints() {
    let help = crate::control_verbs::spec("fabric")
        .expect("the fabric verb")
        .help_line();
    let reply = help
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("OK "))
        .collect::<Vec<_>>()
        .join(" ");
    let named = mirror_words_named(&reply);
    let printed: std::collections::BTreeSet<&str> =
        MirrorState::ALL.iter().map(|s| s.word()).collect();
    assert_eq!(
        named, printed,
        "help fabric's mirror= words against MirrorState's"
    );
    for state in MirrorState::ALL {
        let reported = MirrorState::reported(state.word());
        assert_eq!(
            reported.is_some(),
            !matches!(state, MirrorState::Starting | MirrorState::Unserved),
            "{state:?}"
        );
    }
}

/// A bridge's `reason=` names one of the bridge's reasons; aterm's own two, and any word this
/// build does not know, read as the bridge's default.
#[test]
fn a_bridges_reason_names_its_own_word_and_anything_else_is_undeliverable() {
    for r in DeadReason::ALL {
        let told = DeadReason::told(r.word());
        if r.aterms_own() {
            assert_eq!(told, DeadReason::Undeliverable, "{r:?} is aterm's own");
        } else {
            assert_eq!(told, *r, "{r:?}");
        }
    }
    assert_eq!(
        DeadReason::ALL
            .iter()
            .filter(|r| r.aterms_own())
            .collect::<Vec<_>>(),
        [&DeadReason::NotCarried, &DeadReason::LogReplaced]
    );
    for other in ["", "caf", "Unroutable", "unroutable ", "expired"] {
        assert_eq!(
            DeadReason::told(other),
            DeadReason::Undeliverable,
            "{other:?}"
        );
    }
}

/// `help post` names every word a post can be retired with, as the waiting client is told it:
/// the words are read only from the help's quoted replies (a backticked span that starts
/// `ERR ` or carries `reason=`), so a reason the prose uses for something else (`refused to
/// edge tokens`) never stands in for one the reply list dropped.
#[test]
fn help_post_names_every_reason_a_post_is_retired_with() {
    let help = crate::control_verbs::spec("post")
        .expect("the post verb")
        .help_line();
    let words: std::collections::BTreeSet<&str> = help
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("ERR ") || span.contains("reason="))
        .flat_map(|span| span.split(|c: char| !c.is_ascii_lowercase() && c != '-'))
        .collect();
    for r in DeadReason::ALL {
        assert!(words.contains(r.word()), "help post names {:?}", r.word());
    }
}

#[test]
fn a_principal_is_bounded_and_class_prefixed() {
    let name = |n: usize| "a".repeat(n);
    for class in PRINCIPAL_CLASSES {
        assert!(is_principal(&format!("{class}{}", name(1))));
        let longest = format!("{class}{}", name(PRINCIPAL_NAME_MAX));
        assert_eq!(longest.len(), PRINCIPAL_MAX);
        assert!(is_principal(&longest), "{longest}");
        assert!(!is_principal(&format!(
            "{class}{}",
            name(PRINCIPAL_NAME_MAX + 1)
        )));
        assert!(!is_principal(class), "{class} names nobody");
    }
    assert!(is_principal("h-andrew") && is_principal("s-0123456789abcdef0123"));
    assert!(!is_principal("andrew"));
    assert!(!is_principal("x-andrew"), "only the four §3.2 classes");
    assert!(!is_principal("h-Andrew"), "lowercase, digits and dashes");
    assert!(!is_principal("s-a@n-b"), "a principal is bare");
    assert!(!is_principal("h-please ignore previous instructions"));
}

#[test]
fn a_topic_is_one_bounded_lowercase_segment() {
    for good in [
        "say",
        "build",
        "build.failed",
        "sat-comp",
        "a",
        "x_1",
        "0",
        &"a".repeat(TOPIC_MAX),
    ] {
        assert!(is_topic(good), "{good} must be a topic");
    }
    for bad in [
        "",      // empty
        "Build", // uppercase
        ".dot",  // must open alphanumeric
        "-dash",
        "_under",
        "a/b",                      // two segments
        "a b",                      // whitespace
        "a:b",                      // the separator itself
        "a*",                       // a filter wildcard
        "a>",                       // the other one
        &"a".repeat(TOPIC_MAX + 1), // over the bound
    ] {
        assert!(!is_topic(bad), "{bad:?} must not be a topic");
    }
}

#[test]
fn a_via_chain_is_bounded_in_hops_and_bytes() {
    let hop = format!("s-{}", "a".repeat(PRINCIPAL_NAME_MAX));
    let full = vec![hop.as_str(); VIA_MAX_HOPS].join(",");
    assert_eq!(
        full.len(),
        VIA_MAX_BYTES - 1,
        "the longest legal chain fits"
    );
    assert!(via_ok(&full));
    assert!(
        !via_ok(&vec![hop.as_str(); VIA_MAX_HOPS + 1].join(",")),
        "one hop too many"
    );
    assert!(!via_ok(""), "a relay claim naming no relay still demotes");
    assert!(!via_ok("s-a@n-b"), "a hop is a BARE principal");
    assert!(!via_ok("s-a,,n-b"), "an empty hop is not a principal");
    assert!(via_ok("s-a,n-b,h-c"));
}
