// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::{derive::Model, interp::State as ModelState, verify};

const OLD: u64 = 1_000;
const NEW: u64 = 2_000;
const ROLLBACK: u64 = 500;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    UnansweredSuccess,
    IgnoreContext,
    SortClock,
    ReadClock,
    MissingSuccess,
}

fn stamp_token(stamp: u64) -> i64 {
    match stamp {
        OLD => 1,
        NEW => 2,
        ROLLBACK => 3,
        _ => 4,
    }
}

fn outcome_token(outcome: VaultNetworkRefreshOutcome) -> i64 {
    match outcome {
        VaultNetworkRefreshOutcome::Failed => 1,
        VaultNetworkRefreshOutcome::Succeeded => 2,
    }
}

fn authority_bytes(paths: &Paths, state: &State) -> Vec<u8> {
    let mut authority = state.clone();
    authority.network_refresh = None;
    save_state(paths, &authority).unwrap();
    std::fs::read(paths.state_file()).unwrap()
}

fn seeded(config: &Config, recipient: Option<[u8; 32]>) -> State {
    let binding = network_observation::configuration_binding(config);
    let digest = hex(&[34; 32]);
    let history = format!(
        "[network_materialization]\nversion = 1\nsource = \"configured-url\"\n\
         configuration = {}\nsource_identity = {}\nrecipient = {}\nsequence = 7\n\
         digest = {}\nrecords_digest = {}\nsampled_at_ms = 250\n\
         [http204_response]\nversion = 1\nconfiguration = {}\nrecipient = {}\n\
         responded_at_ms = 300\npersisted_at_ms = 400\n",
        toml_str(&hex(&binding)),
        toml_str(&hex(&[36; 32])),
        toml_str(&hex(&[33; 32])),
        toml_str(&digest),
        toml_str(&hex(&[35; 32])),
        toml_str(&hex(&binding)),
        toml_str(&recipient.map(|key| hex(&key)).unwrap_or_default()),
    );
    let table: aterm_toml::Table = aterm_toml::from_str(&history).unwrap();
    let mut state = State {
        seq: 7,
        digest: Some([34; 32]),
        records_digest: Some([35; 32]),
        source_identity: Some([36; 32]),
        network_materialization: network_observation::Receipt::parse(&table),
        http204_response: http204_observation::Receipt::parse(&table),
        heard: 17,
        requested: 19,
        membership: Stored::In,
        proof: Some("fixture-proof".into()),
        own: Some(BTreeSet::from(["fixture-token".into()])),
        ..State::default()
    };
    state.manifest.files.insert("fixture-token".into(), digest);
    assert!(state.network_materialization.is_some());
    assert!(state.http204_response.is_some());
    let attempt = Attempt {
        configuration: binding,
        recipient,
    };
    assert!(record(
        &mut state,
        config,
        recipient,
        &attempt,
        VaultNetworkRefreshOutcome::Failed,
        OLD,
    ));
    state
}

fn observe(refresh: &mut Refresh, fetched: Fetched) {
    // These are the transport boundary's closed inputs. Actual HTTP and full
    // refresh integration are exercised by the sibling persistence tests.
    refresh.observed(&Ok(transport::ObservedFetch {
        fetched,
        http204: None,
    }));
}

fn fail(refresh: &mut Refresh) {
    refresh.observed(&Err(Error::new("fixture network failure")));
}

pub(crate) fn refresh_transition(before: &ModelState, action: &str) -> ModelState {
    refresh_transition_with(before, action, Fault::None)
}

fn refresh_transition_with(before: &ModelState, action: &str, fault: Fault) -> ModelState {
    if action == "Case" {
        let mut next = before.clone();
        next.insert("case", before["case"] + 1);
        return next;
    }
    assert!(matches!(action, "Complete" | "Project" | "Reload"));
    let case = before["case"];
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let mut config = Config {
        source: Source::Url("http://127.0.0.1:1/model-vault".into()),
        mirrors: Vec::new(),
        name: "model-vault".into(),
        signer: Some([31; 32]),
        followed: vec![[32; 32]],
        host: "model-reader".into(),
    };
    let mut recipient = if case == 16 { None } else { Some([33; 32]) };
    let mut state = seeded(&config, recipient);
    let authority = authority_bytes(&paths, &state);
    let attempt = Attempt {
        configuration: network_observation::configuration_binding(&config),
        recipient,
    };
    let mut refresh = Refresh::default();
    match case {
        1 | 12 | 16 => observe(&mut refresh, Fetched::Body(b"received body".to_vec())),
        2 => observe(&mut refresh, Fetched::NoNewer(None)),
        3 | 14 => {}
        5 => observe(&mut refresh, Fetched::Body(b"invalid body".to_vec())),
        6 => {
            fail(&mut refresh);
            refresh.peer_response();
        }
        15 => {
            fail(&mut refresh);
            refresh.delegated();
        }
        18 => observe(&mut refresh, Fetched::Missing),
        _ => fail(&mut refresh),
    }
    match case {
        7 => config.host.push_str("-changed"),
        8 => recipient = Some([44; 32]),
        9 => config.source = Source::Path(root.path().join("local-vault")),
        _ => {}
    }
    let completed_at_ms = if matches!(case, 12 | 17) {
        ROLLBACK
    } else {
        NEW
    };
    // A local repair may finish despite the failed network request (case 4).
    // A received body may fail verification/application (case 5).
    let mut outcome = refresh.outcome(case != 5);
    if fault == Fault::UnansweredSuccess && case == 2 {
        // Executable former inference: an ordinary no-news result is success.
        outcome = Some(VaultNetworkRefreshOutcome::Succeeded);
    }
    if fault == Fault::MissingSuccess && case == 18 {
        // Executable unsafe policy: a 404 followed by a cache fallback clears failure.
        outcome = Some(VaultNetworkRefreshOutcome::Succeeded);
    }
    let recorded = outcome.is_some_and(|outcome| {
        if fault == Fault::SortClock
            && state
                .network_refresh
                .as_ref()
                .is_some_and(|old| old.observation.completed_at_ms() > completed_at_ms)
        {
            // Executable unsafe policy: wall-clock order overrides completion order.
            false
        } else {
            record(
                &mut state,
                &config,
                recipient,
                &attempt,
                outcome,
                completed_at_ms,
            )
        }
    });
    if action == "Reload" {
        save_state(&paths, &state).unwrap();
        state = load_state(&paths).unwrap();
    }
    let mut next = before.clone();
    let retained = state.network_refresh.as_ref().unwrap().observation;
    next.insert("recorded", i64::from(recorded));
    next.insert("outcome", outcome_token(retained.outcome()));
    next.insert("stamp", stamp_token(retained.completed_at_ms()));
    next.insert(
        "preserved",
        i64::from(authority_bytes(&paths, &state) == authority),
    );
    if action == "Complete" {
        next.insert("phase", 1);
        return next;
    }
    match case {
        10 => config.source = Source::Url("http://127.0.0.1:1/other-vault".into()),
        11 => recipient = Some([44; 32]),
        _ => {}
    }
    let mut observation = if fault == Fault::IgnoreContext {
        // Executable unsafe policy: retained diagnostic presence replaces its context guard.
        state
            .network_refresh
            .as_ref()
            .map(|receipt| receipt.observation)
    } else {
        project(&config, recipient, &state)
    };
    if fault == Fault::ReadClock {
        observation = observation.map(|mut observation| {
            observation.completed_at_ms = 9_000;
            observation
        });
    }
    next.insert("phase", if action == "Reload" { 3 } else { 2 });
    next.insert("shown", i64::from(observation.is_some()));
    next.insert(
        "shown_outcome",
        observation.map_or(0, |entry| outcome_token(entry.outcome())),
    );
    next.insert(
        "shown_stamp",
        observation.map_or(0, |entry| stamp_token(entry.completed_at_ms())),
    );
    next
}

fn admits(model: &Model, before: &ModelState, after: &ModelState, action: &str) -> bool {
    let (accepted, why) = verify::validate_transition_tiered(
        model,
        &[],
        before,
        after,
        Some(action),
        "Vault network refresh conformance",
    );
    eprintln!("{} {action}: {why}", model.name);
    accepted
}

fn at_case(model: &Model, case: i64) -> ModelState {
    let mut state = model.init_state();
    for _ in 0..case {
        state = refresh_transition(&state, "Case");
    }
    state
}

#[test]
fn refresh_outcomes_conform_through_projection_and_state_codec() {
    let model = aterm_models_db::vault_network_refresh_model();
    let coverage = verify::prove_and_catch_scalar(&model, "Vault network refresh");
    eprintln!("{}: {coverage:?}", model.name);
    let faults = verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{}: {faults:?}", model.name);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len(),
    );
    let mut input = model.init_state();
    for case in 0..=18 {
        assert_eq!(input["case"], case);
        let mut state = input.clone();
        for action in ["Complete", "Project", "Reload"] {
            let next = refresh_transition(&state, action);
            assert!(
                admits(&model, &state, &next, action),
                "case {case}: {next:?}"
            );
            state = next;
        }
        if case < 18 {
            let next = refresh_transition(&input, "Case");
            assert!(admits(&model, &input, &next, "Case"));
            input = next;
        }
    }
}

#[test]
fn unanswered_contextless_sorted_or_restamped_refresh_policies_are_rejected() {
    let model = aterm_models_db::vault_network_refresh_model();
    for (case, action, fault, law) in [
        (
            2,
            "Complete",
            Fault::UnansweredSuccess,
            "CompletionAdmission",
        ),
        (10, "Project", Fault::IgnoreContext, "ContextProjection"),
        (11, "Project", Fault::IgnoreContext, "ContextProjection"),
        (12, "Complete", Fault::SortClock, "CompletionOrder"),
        (0, "Project", Fault::ReadClock, "OriginalCompletionTime"),
        (18, "Complete", Fault::MissingSuccess, "LatestOutcome"),
    ] {
        let mut before = at_case(&model, case);
        if action == "Project" {
            before = refresh_transition(&before, "Complete");
        }
        let after = refresh_transition_with(&before, action, fault);
        assert!(
            !admits(&model, &before, &after, action),
            "case {case}: {after:?}"
        );
        assert!(!model.check_invariant(law, &after), "case {case}: {law}");
    }
}
