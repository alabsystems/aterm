// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::json;

fn principal() -> Principal {
    Principal {
        session: Some("s-47051b9a207955a92b14".into()),
        program: Some("claude".into()),
        ..Principal::default()
    }
}

/// A statement spells as a value and reads back as itself; a reason of two lines, one past
/// its byte bound and a 17th piece of evidence are each refused.
#[test]
fn a_statement_round_trips_and_its_bounds_hold() {
    let said = Said {
        principal: principal(),
        reason: Some("claim TK-03".into()),
        evidence: vec![Evidence::note("the exit test passed")],
    };
    assert_eq!(Said::from_value(&said.to_value()), Ok(said.clone()));
    assert_eq!(said.who(), "claude s-47051b9a207955a92b14");
    for bad in ["two\nlines", " padded", ""] {
        assert!(reason(bad).is_err(), "{bad:?}");
    }
    assert!(reason(&"x".repeat(MAX_REASON)).is_ok());
    assert!(reason(&"x".repeat(MAX_REASON + 1)).is_err());
    let mut many = said.clone();
    many.evidence = vec![Evidence::note("n"); values::MAX_EVIDENCE + 1];
    assert!(many.check().is_err());
    assert!(Said::from_value(&json!({"principal": {"program": "claude"}, "why": "x"})).is_err());
    assert!(Said::from_value(&json!({"principal": {}})).is_err());
    assert!(
        Said::from_value(&json!({"principal": {"program": "claude"}, "evidence": []})).is_err()
    );
}

/// The principal states what aterm read of the writer's lineage and nothing more: the agent it
/// found, `person` only where the instance found none and nothing else acts in the session, and
/// otherwise the session with the writer's own word, never a guess of `person`.
#[cfg(unix)]
#[test]
fn the_principal_states_only_what_the_lineage_answered() {
    use crate::alab::command::session::Lineage;
    let lineage = |program: Option<&str>, person: bool| {
        Some(Lineage {
            session: "tqn5r6e0148".to_owned(),
            program: program.map(str::to_owned),
            person,
        })
    };
    let session = Some("tqn5r6e0148".to_owned());
    assert_eq!(
        principal_of(lineage(Some("claude"), false), "program"),
        Principal {
            session: session.clone(),
            program: Some("claude".to_owned()),
            ..Principal::default()
        }
    );
    assert_eq!(
        principal_of(lineage(None, true), "person"),
        Principal {
            session: session.clone(),
            program: Some("person".to_owned()),
            ..Principal::default()
        }
    );
    let unknown = principal_of(lineage(None, false), "program");
    assert_eq!(
        unknown,
        Principal {
            session,
            said: Some("program".to_owned()),
            ..Principal::default()
        },
        "an unknown actor is never stated as a person"
    );
    assert_eq!(
        Said::by(unknown.clone()).who(),
        "tqn5r6e0148 (said: program)",
        "its own word is marked as said"
    );
    assert!(crate::typed::values::principal(&unknown.to_value()).is_ok());
    assert_eq!(principal_of(None, "program"), Principal::said("program"));
}
