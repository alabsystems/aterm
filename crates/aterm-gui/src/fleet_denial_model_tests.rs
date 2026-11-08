// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::client::{SwarmDenialSnapshot as Snapshot, SwarmDenialUnavailable as Why};
use aterm_spec::interp::State;

const DENIED: &str = "one-denied-node";

fn observation(input: i64) -> aterm_secrets::Result<Snapshot> {
    Ok(match input {
        0 => Snapshot::Available(Vec::new()),
        1 => Snapshot::Available(vec![DENIED.to_owned()]),
        2 => Snapshot::Unavailable(Why::WriterBusy),
        3 => Snapshot::Unavailable(Why::ConfigurationChanged),
        4 => Snapshot::Unavailable(Why::Revoked),
        5 => Snapshot::Unavailable(Why::MissingStateLock),
        6 => Snapshot::Unavailable(Why::EntryUnavailable),
        7 => Snapshot::Unavailable(Why::MissingIdentity),
        8 => Snapshot::Unavailable(Why::RecipientHeldOut),
        9 => return Err(aterm_secrets::Error::new("read-a")),
        10 => return Err(aterm_secrets::Error::new("read-b")),
        _ => panic!("unknown observation"),
    })
}

fn cause_code(cause: Option<&DeniedCause>) -> i64 {
    match cause {
        None => 0,
        Some(DeniedCause::Unavailable(why)) => match why {
            Why::WriterBusy => 1,
            Why::ConfigurationChanged => 2,
            Why::Revoked => 3,
            Why::MissingStateLock => 4,
            Why::EntryUnavailable => 5,
            Why::MissingIdentity => 6,
            Why::RecipientHeldOut => 7,
        },
        Some(DeniedCause::Read(why)) if why == "read-a" => 8,
        Some(DeniedCause::Read(why)) if why == "read-b" => 9,
        Some(DeniedCause::Read(_)) => panic!("unknown read failure"),
    }
}

/// Project the production cache's result, without reproducing its transition policy.
pub(crate) fn project(state: &State, action: &str) -> State {
    let mut next = state.clone();
    match action {
        "Known" => {
            next.insert("known", state["known"] + 1);
        }
        "Cause" => {
            next.insert("cause", state["cause"] + 1);
        }
        "Input" => {
            next.insert("input", state["input"] + 1);
        }
        "Observe" => {
            let mut cache = DeniedCache::new();
            if state["known"] > 0 {
                let _ = update_denied_nodes_cache(&mut cache, observation(state["known"] - 1));
            }
            if state["cause"] > 0 {
                let _ = update_denied_nodes_cache(&mut cache, observation(state["cause"] + 1));
            }
            let (denied, said) = update_denied_nodes_cache(&mut cache, observation(state["input"]));
            let known = match denied.as_deref() {
                None => 0,
                Some([]) => 1,
                Some([node]) if node == DENIED => 2,
                _ => panic!("unknown deny list"),
            };
            next.insert("before", state["known"]);
            next.insert("previous", state["cause"]);
            next.insert("known", known);
            next.insert("cause", cause_code(cache.unavailable_said.as_ref()));
            next.insert("announced", i64::from(said.is_some()));
            next.insert("phase", 1);
        }
        _ => panic!("unknown cache action"),
    }
    next
}

#[test]
fn denied_cache_conforms_and_rejects_opening_or_repeated_reports() {
    let model = aterm_models_swarm::fleet_denied_cache_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    let mut observed = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = project(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
                if action.name == "Observe" {
                    observed += 1;
                }
            }
        }
    }
    assert_eq!(
        observed, 330,
        "every prior verdict, cause and observation is exercised"
    );

    let mut cold = model.init_state();
    cold.insert("input", 2);
    let mut opened = project(&cold, "Observe");
    opened.insert("known", 1);
    assert!(!model.successors("Observe", &cold).contains(&opened));
    assert!(!model.check_invariant("KeepsVerdict", &opened));

    let mut repeated = cold;
    repeated.insert("cause", 1);
    let mut noisy = project(&repeated, "Observe");
    noisy.insert("announced", 1);
    assert!(!model.successors("Observe", &repeated).contains(&noisy));
    assert!(!model.check_invariant("ReportsTransitions", &noisy));
}

#[cfg(unix)]
#[test]
fn a_real_busy_vault_keeps_the_verified_peer_denial_and_reports_recovery() {
    use aterm_secrets::client::{self, Paths};

    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let source = root.path().join("fixture.vault");
    client::create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
    let blocked = "9a".repeat(16);
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        client::SWARM_ROSTER_ENTRY,
        format!("deny_nodes = [\"{blocked}\"]\n").as_bytes(),
    )
    .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    let peer = aterm_net::fleet::NodeProof {
        node: blocked,
        key: aterm_net::fleet::NodeKey::from_bytes([0x9a; 32]),
    };
    let mut cache = DeniedCache::new();
    let (ready, _) = update_denied_nodes_cache(&mut cache, client::swarm_denial_snapshot(&paths));
    assert!(denied_under(ready.as_deref(), None).refuses(Some(&peer)));

    let writer = client::test_hold_state_lock(&paths).unwrap();
    let (retained, said) =
        update_denied_nodes_cache(&mut cache, client::swarm_denial_snapshot(&paths));
    assert!(said.is_some(), "a new unavailable observation is recorded");
    assert_eq!(retained, ready);
    assert!(denied_under(retained.as_deref(), None).refuses(Some(&peer)));
    assert!(
        update_denied_nodes_cache(&mut cache, client::swarm_denial_snapshot(&paths))
            .1
            .is_none()
    );
    let (cold, _) = update_denied_nodes_cache(
        &mut DeniedCache::new(),
        client::swarm_denial_snapshot(&paths),
    );
    assert!(denied_under(cold.as_deref(), None).unread);
    drop(writer);

    let (recovered, said) =
        update_denied_nodes_cache(&mut cache, client::swarm_denial_snapshot(&paths));
    assert_eq!(recovered, ready);
    assert!(
        said.is_some(),
        "a return to a verified read is recorded once"
    );
    assert!(denied_under(recovered.as_deref(), None).refuses(Some(&peer)));
}
