// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::accounts::broker::{Grant, Policy as BrokerPolicy, Publication};

/// A published policy of two grants, the first with a refresh waiting to be published.
pub(crate) struct World {
    world: super::broker_tests::World,
    other: Grant,
}

fn world() -> World {
    let world = super::broker_tests::setup();
    let (f, _, _, old) = &world;
    let other = Grant {
        id: [71; 16],
        producer: [72; 32],
        binding: crate::resources::RequestBinding {
            account: [73; 32],
            ..old.binding.clone()
        },
        ..old.clone()
    };
    super::broker_tests::publish(f, vec![old.clone(), other.clone()]);
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![old.binding.revision],
    );
    World { world, other }
}

pub(crate) fn publication_transition(
    world: &World,
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(world, before, action, false)
}

fn transition_with(
    World {
        world: (f, broker, _, old),
        other,
    }: &World,
    before: &aterm_spec::interp::State,
    action: &str,
    bypass_review: bool,
) -> aterm_spec::interp::State {
    let refresh = broker.refresh_proposal_until(old.id, deadline()).unwrap();
    let base = if action == "StaleBase" {
        [99; 32]
    } else {
        refresh.base()
    };
    let mut rows = refresh.policy().grants().cloned().collect::<Vec<_>>();
    if action == "Producer" {
        rows.iter_mut().find(|g| g.id == old.id).unwrap().producer = [98; 32];
    }
    if action == "OtherGrant" {
        rows.retain(|g| g.id != other.id);
    }
    let target = BrokerPolicy::new(rows).unwrap();
    let mut admin = client::admin_open(&f.lab.paths[0]).unwrap();
    // An unrelated public-draft edit must not be included in publication.
    admin
        .doc
        .set_entry(&Audience::Everyone, "draft-only", b"never publish")
        .unwrap();
    if action == "MovedHead" {
        f.publish(|doc| {
            doc.set_entry(&Audience::Everyone, "concurrent", b"retain me")
                .unwrap();
        });
    }
    if action == "ForeignActor" {
        let mut foreign = old.clone();
        foreign.actor = [97; 48];
        super::broker_tests::publish(f, vec![foreign, other.clone()]);
    }
    let mut stored = 0;
    if bypass_review {
        let mut doc = admin.published_document().clone();
        doc.set_entry(
            &Audience::Everyone,
            crate::accounts::broker::ENTRY,
            target.text().as_bytes(),
        )
        .unwrap();
        client::admin_publish_conditional_until(&f.lab.paths[0], &admin, &doc, deadline()).unwrap();
        stored = 1;
    } else {
        let result = broker.publish_refresh_until(old.id, base, &target, &admin, deadline());
        if matches!(action, "Apply" | "Repeat") {
            assert!(matches!(result.unwrap(), Publication::Published(_)));
            stored = 1;
            if action == "Repeat" {
                let next = client::admin_open(&f.lab.paths[0]).unwrap();
                let sequence = next.seq();
                let result = broker
                    .publish_refresh_until(old.id, base, &target, &next, deadline())
                    .unwrap();
                assert!(
                    matches!(result, Publication::AlreadyCurrent { sequence: n } if n == sequence)
                );
                assert_eq!(client::admin_open(&f.lab.paths[0]).unwrap().seq(), sequence);
            }
        } else {
            assert!(result.is_err(), "{action} accepted");
        }
    }
    let current = client::admin_open(&f.lab.paths[0]).unwrap();
    assert!(
        current
            .published_entry(&Audience::Everyone, "draft-only")
            .is_none()
    );
    if action == "MovedHead" {
        assert!(
            current
                .published_entry(&Audience::Everyone, "concurrent")
                .is_some()
        );
    }
    let policy = BrokerPolicy::parse(
        current
            .published_entry(&Audience::Everyone, crate::accounts::broker::ENTRY)
            .unwrap()
            .value
            .as_slice(),
    )
    .unwrap();
    let identity = policy
        .grants()
        .find(|g| g.id == old.id)
        .is_some_and(|g| g.producer == old.producer);
    let kept = policy.grants().any(|g| g == other);
    let used = f
        .source(31, 0)
        .prepare_until(deadline())
        .unwrap()
        .resources()
        .snapshot()
        .unwrap()
        .used
        .values()
        .sum::<u64>();
    let mut next = before.clone();
    let scenario = [
        "Apply",
        "Repeat",
        "Producer",
        "OtherGrant",
        "StaleBase",
        "MovedHead",
        "ForeignActor",
    ]
    .iter()
    .position(|name| *name == action)
    .unwrap()
        + 1;
    next.insert("scenario", scenario as i64);
    next.insert("stored", stored);
    next.insert("identity", i64::from(identity));
    next.insert("kept", i64::from(kept));
    next.insert("used", used as i64);
    next
}

/// The model's steps on four worlds: the three publications refused before any write, then `Apply`
/// on the same world, whose success shows they left the base and the head usable; `Repeat`,
/// `MovedHead` and `ForeignActor` each on its own, since each changes the world first.
/// GAP: a refused publication's effect on a later one on the same world could stand in for that
/// later step's result. `Repeat` still publishes first on a pristine world.
#[test]
fn publication_conforms_and_unreviewed_root_write_changes_identity() {
    let model = aterm_models_db::broker_publication_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    let worlds: [&[&str]; 4] = [
        &["Producer", "OtherGrant", "StaleBase", "Apply"],
        &["Repeat"],
        &["MovedHead"],
        &["ForeignActor"],
    ];
    // A new action falls out of the walk here, not silently.
    let mut named: Vec<_> = worlds.iter().copied().flatten().copied().collect();
    let mut actions: Vec<_> = model.actions.iter().map(|action| action.name).collect();
    named.sort_unstable();
    actions.sort_unstable();
    assert_eq!(named, actions);
    aterm_spec::check::each_on(aterm_spec::fs_lane::PLACES, &worlds, |steps| {
        let world = world();
        for action in *steps {
            let next = super::publication_transition(&world, &initial, action);
            assert!(
                model.successors(action, &initial).contains(&next),
                "{action}: {next:?}"
            );
        }
    });
    let bad = transition_with(&world(), &initial, "Producer", true);
    assert!(!model.check_invariant("ReviewedOnly", &bad));
    assert!(!model.check_invariant("IdentityStable", &bad));
}

#[test]
fn publication_command_stores_the_reviewed_file_and_recognizes_its_retry() {
    let (f, broker, _, old) = super::broker_tests::setup();
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![old.binding.revision],
    );
    let refresh = broker.refresh_proposal_until(old.id, deadline()).unwrap();
    let file = f.lab.paths[0].dir.join("reviewed-policy.toml");
    std::fs::write(&file, refresh.policy().text()).unwrap();
    let id = aterm_codec::hex::encode_lower(&old.id);
    let base = aterm_codec::hex::encode_lower(&refresh.base());
    let options = std::collections::BTreeMap::from([
        ("--grant", id.as_str()),
        ("--base", base.as_str()),
        ("--file", file.to_str().unwrap()),
    ]);
    let stored = crate::accounts::broker::command::test_command_with_paths(
        "accounts-broker-publish",
        options.clone(),
        f.lab.paths[0].clone(),
    )
    .unwrap();
    assert_eq!(stored["status"], aterm_json::json!("stored"));
    assert_eq!(
        stored["provider_request"],
        aterm_json::json!("not-attempted")
    );
    let retry = crate::accounts::broker::command::test_command_with_paths(
        "accounts-broker-publish",
        options,
        f.lab.paths[0].clone(),
    )
    .unwrap();
    assert_eq!(retry["status"], aterm_json::json!("already-current"));
    assert_eq!(retry["sequence"], stored["sequence"]);
    assert_eq!(retry["base_checked"], aterm_json::json!(false));
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![refresh.after().binding.revision],
    );
    assert!(
        crate::accounts::broker::command::test_command_with_paths(
            "accounts-broker-publish",
            std::collections::BTreeMap::from([
                ("--grant", id.as_str()),
                ("--base", base.as_str()),
                ("--file", file.to_str().unwrap()),
            ]),
            f.lab.paths[0].clone(),
        )
        .is_err(),
        "a repeat must not describe a superseded billing binding as current"
    );
}
