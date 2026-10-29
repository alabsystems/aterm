// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Inert candidate outcomes: discovery cannot certify that every default belongs elsewhere.

use super::{Error, ProbeOrigin as Origin, ProbeOutcome, Probed, select_probed_outcome};

#[derive(Clone, Copy)]
enum Answer {
    Timeout,
    Another,
    Absent,
    Refused,
    Found(u64, &'static str),
}

type CacheCase<'a> = (&'a str, &'a [(Origin, Answer)], bool);

fn answer(answer: Answer) -> crate::Result<ProbeOutcome> {
    match answer {
        Answer::Timeout => Err(Error::new("request deadline exceeded")),
        Answer::Another => Ok(ProbeOutcome::AnotherVault("another signer".into())),
        Answer::Absent => Ok(ProbeOutcome::Absent),
        Answer::Refused => Err(Error::new(super::LOCAL_NETWORK_REFUSED)),
        Answer::Found(seq, url) => Ok(ProbeOutcome::Found(Probed {
            url: url.into(),
            seq,
            fingerprint: "already verified by probe".into(),
            envelope: Vec::new(),
        })),
    }
}

/// The production selector sees every candidate's origin, including one that never answered.
fn select(replies: &[(Origin, Answer)]) -> crate::Result<ProbeOutcome> {
    select_probed_outcome(
        replies
            .iter()
            .map(|(origin, reply)| (*origin, answer(*reply))),
    )
}

/// Only AnotherVault is kept as the network's 24-hour negative discovery record.
/// The existing init_secrets look-cache test holds that consumer's behavior.
fn cacheable(outcome: &crate::Result<ProbeOutcome>) -> bool {
    matches!(outcome, Ok(ProbeOutcome::AnotherVault(_)))
}

#[test]
fn only_a_settled_authoritative_set_can_certify_another_vault() {
    use Answer::{Absent, Another, Timeout};
    use Origin::{Default, Discovered};

    let cases: &[CacheCase<'_>] = &[
        (
            "default timeout, discovered foreign",
            &[(Default, Timeout), (Discovered, Another)],
            false,
        ),
        (
            "default timeout, discovered 404",
            &[(Default, Timeout), (Discovered, Absent)],
            false,
        ),
        (
            "first of two defaults times out",
            &[(Default, Timeout), (Default, Another)],
            false,
        ),
        (
            "second of two defaults times out",
            &[(Default, Another), (Default, Timeout)],
            false,
        ),
        (
            "discovered 404 cannot mask the authoritative answer",
            &[(Default, Another), (Discovered, Absent)],
            true,
        ),
        (
            "a default 404 does not certify another vault",
            &[(Default, Another), (Default, Absent)],
            false,
        ),
        (
            "legacy single default belongs elsewhere",
            &[(Default, Another)],
            true,
        ),
        (
            "every default belongs elsewhere",
            &[(Default, Another), (Default, Another)],
            true,
        ),
        (
            "discovery alone cannot certify the defaults",
            &[(Discovered, Another)],
            false,
        ),
        (
            "legacy single default 404 is not kept",
            &[(Default, Absent)],
            false,
        ),
    ];
    let mut failures = Vec::new();
    for &(label, replies, expected) in cases {
        let outcome = select(replies);
        if cacheable(&outcome) != expected {
            failures.push(format!(
                "{label}: cacheable must be {expected}, got {outcome:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_verified_candidate_competes_and_ties_keep_the_input_order() {
    use Answer::{Another, Found, Timeout};
    use Origin::{Default, Discovered};

    for replies in [
        [
            (Default, Found(4, "default")),
            (Discovered, Found(9, "discovered")),
            (Default, Another),
        ],
        [
            (Default, Timeout),
            (Discovered, Found(9, "discovered")),
            (Default, Found(4, "default")),
        ],
    ] {
        let Ok(ProbeOutcome::Found(found)) = select(&replies) else {
            panic!("a verified candidate must win");
        };
        assert_eq!((found.seq, found.url.as_str()), (9, "discovered"));
    }
    for replies in [
        [
            (Default, Found(9, "first")),
            (Discovered, Found(9, "second")),
        ],
        [
            (Discovered, Found(9, "first")),
            (Default, Found(9, "second")),
        ],
    ] {
        let Ok(ProbeOutcome::Found(found)) = select(&replies) else {
            panic!("a verified candidate must win");
        };
        assert_eq!(found.url, "first", "origin does not break a verified tie");
    }
}

#[test]
fn local_network_refusal_is_preserved_unless_a_verified_vault_answered() {
    use Answer::{Absent, Another, Found, Refused};
    use Origin::{Default, Discovered};

    for replies in [
        [(Default, Refused), (Discovered, Another)],
        [(Default, Another), (Discovered, Refused)],
        [(Default, Refused), (Discovered, Absent)],
    ] {
        let outcome = select(&replies).expect_err("a refusal is not cached as absence");
        assert!(super::refused_by_local_network(&outcome));
    }
    let found = select(&[(Default, Refused), (Discovered, Found(9, "verified"))]);
    assert!(matches!(found, Ok(ProbeOutcome::Found(_))));
}
