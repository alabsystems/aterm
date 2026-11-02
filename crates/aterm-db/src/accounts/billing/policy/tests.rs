// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::resources::Terms;

fn trust() -> Trust {
    Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    }
}

fn resource(principal: u8, unit: &str) -> Resource {
    Resource {
        allocation: Proposal::prepare(
            trust(),
            Terms {
                resource: [principal; 32],
                window: [12; 32],
                unit: unit.into(),
                limit: 3,
                shares: BTreeMap::from([([4; 48], 3)]),
            },
        )
        .unwrap(),
        provider: "example".into(),
        adapter: "example-v1".into(),
    }
}

#[test]
fn policy_roundtrip_and_root_pin_cover_every_resource_term() {
    let policy = Policy::new(vec![resource(11, "requests")]).unwrap();
    let reference = policy.reference([11; 32]).unwrap();
    let parsed = Policy::parse(policy.text().as_bytes()).unwrap();
    assert_eq!(parsed.text(), policy.text());
    assert_eq!(parsed.reference([11; 32]), Some(reference));
    assert!(parsed.select(reference, trust(), "example").is_ok());
    assert!(parsed.select(reference, trust(), "different").is_err());
    let mut other = trust();
    other.network = [9; 16];
    assert!(parsed.select(reference, other, "example").is_err());
    for change in 0..3 {
        let mut changed = resource(11, "requests");
        match change {
            0 => changed.adapter = "example-v2".into(),
            1 => changed.provider = "other".into(),
            2 => {
                let mut terms = changed.allocation.terms().clone();
                terms.window = [13; 32];
                changed.allocation = Proposal::prepare(trust(), terms).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            Policy::new(vec![changed])
                .unwrap()
                .select(reference, trust(), "example")
                .is_err()
        );
    }
    let extended = Policy::new(vec![resource(14, "requests"), resource(11, "requests")]).unwrap();
    assert_eq!(extended.reference([11; 32]), Some(reference));
}

#[test]
fn policy_refuses_ambiguous_unknown_mismatched_and_monetary_terms() {
    let policy = Policy::new(vec![resource(11, "requests")]).unwrap();
    for text in [
        policy.text().replace("version = 1", "version = 2"),
        format!("{}\nunknown = 1\n", policy.text()),
        format!(
            "{}\n[[resource]]{}",
            policy.text(),
            policy.text().split_once("[[resource]]").unwrap().1
        ),
        policy
            .text()
            .replace("provider = \"example\"", "provider = \"invalid provider\""),
        policy.text().replace(
            &aterm_codec::hex::encode_lower(&[11; 32]),
            &aterm_codec::hex::encode_lower(&[15; 32]),
        ),
        policy.text().replace("pin = \"", "pin = \"00"),
    ] {
        assert!(Policy::parse(text.as_bytes()).is_err());
    }
    assert!(Policy::new(vec![resource(11, "requests"), resource(11, "requests")]).is_err());
    assert!(Policy::new(vec![resource(11, "dollars")]).is_err());
    assert!(Policy::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
    assert!(Policy::parse(&[255]).is_err());
}

#[test]
fn billing_references_roundtrip_and_validate_in_account_proposals() {
    let reference = Reference {
        principal: [1; 32],
        policy: [2; 32],
    };
    assert_eq!(Reference::parse(&reference.value()).unwrap(), reference);
    let mut account =
        json!({"provider":"example", "method":"api-key", "billing": reference.value()});
    assert!(crate::accounts::validate(&account).is_ok());
    for invalid in [
        Value::Null,
        json!({}),
        json!({"principal":"bad", "policy":"bad"}),
        Reference {
            principal: [0; 32],
            ..reference
        }
        .value(),
        json!({"principal":aterm_codec::hex::encode_lower(&[1; 32]), "policy":aterm_codec::hex::encode_lower(&[2; 32]), "secret":"forbidden"}),
    ] {
        assert!(Reference::parse(&invalid).is_err());
        if !invalid.is_null() {
            account
                .as_object_mut()
                .unwrap()
                .insert("billing".into(), invalid);
            assert!(crate::accounts::validate(&account).is_err());
        }
    }
}
