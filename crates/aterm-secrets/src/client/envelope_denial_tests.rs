// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn envelope_with_rosters(everyone: &[u8], host: &[u8]) -> (Identity, Envelope) {
    envelope_with_optional_rosters(Some(everyone), Some(host))
}

fn envelope_with_optional_rosters(
    everyone: Option<&[u8]>,
    host: Option<&[u8]>,
) -> (Identity, Envelope) {
    let identity = Identity::generate().unwrap();
    let bytes = sealed_rosters(&identity, everyone, host);
    (identity, envelope::parse(&bytes).unwrap())
}

fn sealed_rosters(identity: &Identity, everyone: Option<&[u8]>, host: Option<&[u8]>) -> Vec<u8> {
    let admin = Identity::generate().unwrap();
    let mut doc = Document::new_vault("admin", &admin.recipient().to_bech32());
    doc.add_machine("m7", &identity.recipient().to_bech32())
        .unwrap();
    if let Some(everyone) = everyone {
        doc.set_entry(&Audience::Everyone, crate::SWARM_ROSTER_ENTRY, everyone)
            .unwrap();
    }
    if let Some(host) = host {
        doc.set_entry(
            &Audience::Hosts(vec!["m7".to_owned()]),
            crate::SWARM_ROSTER_ENTRY,
            host,
        )
        .unwrap();
    }
    seal("dual-roster", 1, &[31; 32], &[32; 32], false, &doc, &[]).unwrap()
}

#[test]
fn an_envelope_host_roster_cannot_hide_everyone_denials() {
    let everyone = crate::swarm_roster::SwarmRoster {
        deny_labels: vec!["m99".to_owned()],
        deny_nodes: vec![aterm_codec::hex::encode_lower(&[99; 32])],
        ..Default::default()
    };
    let host = crate::swarm_roster::SwarmRoster::default();
    let (identity, envelope) =
        envelope_with_rosters(everyone.to_toml().as_bytes(), host.to_toml().as_bytes());
    // Ordinary entry precedence stays host-specific; denial evidence has to retain both.
    let ordinary = log_entries::roster_with_log(None, &envelope, &identity)
        .unwrap()
        .0
        .into_roster()
        .unwrap()
        .unwrap();
    assert!(!ordinary.is_label_denied("m99"));
    let denied = log_entries::denial_roster(None, Some(&envelope), &identity)
        .unwrap()
        .unwrap();
    assert!(denied.is_label_denied("m99"));
    assert!(denied.is_node_denied(&aterm_codec::hex::encode_lower(&[99; 32])));
}

#[test]
fn a_malformed_everyone_roster_cannot_hide_behind_an_envelope_host_roster() {
    let host = crate::swarm_roster::SwarmRoster::default();
    let (identity, envelope) = envelope_with_rosters(b"not a roster [", host.to_toml().as_bytes());
    assert!(
        log_entries::roster_with_log(None, &envelope, &identity)
            .unwrap()
            .0
            .into_roster()
            .is_ok()
    );
    assert!(log_entries::denial_roster(None, Some(&envelope), &identity).is_err());
}

#[test]
fn an_envelope_only_members_admission_observes_a_hidden_everyone_denial() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let identity = machine_identity(&paths).unwrap();
    let device = crate::commons::bootstrap::Device::open(&paths)
        .unwrap()
        .unwrap();
    let request = device.request([1; 16], [2; 32]).unwrap();
    let everyone = crate::swarm_roster::SwarmRoster {
        deny_labels: vec!["m7".to_owned()],
        deny_nodes: vec![aterm_codec::hex::encode_lower(request.binding().writer())],
        ..Default::default()
    };
    let host = crate::swarm_roster::SwarmRoster::default();
    let config = Config {
        source: Source::Url("http://127.0.0.1:1/v1/dual-roster".to_owned()),
        mirrors: Vec::new(),
        name: "dual-roster".to_owned(),
        signer: Some(envelope::signer_public(&[31; 32]).unwrap()),
        followed: Vec::new(),
        host: "m7".to_owned(),
    };
    save_config(&paths, &config).unwrap();
    sync_with(
        &paths,
        sealed_rosters(
            &identity,
            Some(everyone.to_toml().as_bytes()),
            Some(host.to_toml().as_bytes()),
        ),
    )
    .unwrap();
    let healthy = load_state(&paths).unwrap();
    assert_eq!(healthy.membership, Stored::In);
    for membership in [Stored::In, Stored::Pending, Stored::Revoked] {
        let mut state = healthy.clone();
        state.membership = membership;
        save_state(&paths, &state).unwrap();
        assert!(steward::local_recipient_denied_in_roster(&paths));
        let lock = StateLock::observe(&paths).unwrap().unwrap();
        assert!(
            admit::roster_keeps_out(&paths, None, request.binding(), [0; 32], Some(&lock))
                .unwrap()
                .is_some()
        );
    }
}

pub(crate) fn envelope_denials_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Everyone" => {
            next.insert("everyone", state["everyone"] + 1);
        }
        "Host" => {
            next.insert("host", state["host"] + 1);
        }
        "Read" => {
            let contents = |value| match value {
                0 => None,
                1 => Some(crate::swarm_roster::SwarmRoster::default().to_toml()),
                2 => Some(
                    crate::swarm_roster::SwarmRoster {
                        deny_labels: vec!["m99".to_owned()],
                        ..Default::default()
                    }
                    .to_toml(),
                ),
                3 => Some("not a roster [".to_owned()),
                _ => panic!("unknown envelope roster state"),
            };
            let everyone = contents(state["everyone"]);
            let host = contents(state["host"]);
            let (identity, envelope) = envelope_with_optional_rosters(
                everyone.as_ref().map(|s| s.as_bytes()),
                host.as_ref().map(|s| s.as_bytes()),
            );
            let result = log_entries::denial_roster(None, Some(&envelope), &identity);
            next.insert("phase", 1);
            next.insert("failed", i64::from(result.is_err()));
            next.insert(
                "denied",
                i64::from(
                    result
                        .ok()
                        .flatten()
                        .is_some_and(|r| r.is_label_denied("m99")),
                ),
            );
        }
        _ => panic!("unknown envelope denial action {action}"),
    }
    next
}

#[test]
fn envelope_denials_model_conforms_to_native_audience_union() {
    let model = aterm_models_swarm::envelope_denials_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = envelope_denials_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
            }
        }
    }
    let mut input = model.init_state();
    input.insert("everyone", 2);
    input.insert("host", 1);
    let mut shadowed = envelope_denials_transition(&input, "Read");
    shadowed.insert("denied", 0);
    assert!(!model.check_invariant("EveryoneDenial", &shadowed));
    assert!(!model.successors("Read", &input).contains(&shadowed));
}
