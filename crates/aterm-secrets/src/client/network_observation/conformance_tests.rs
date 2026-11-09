// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::{derive::Model, interp::State as ModelState, verify};

struct Fixture {
    server: Option<crate::server::Running>,
    _root: aterm_tempfile::TempDir,
    paths: Paths,
    config: Config,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let (paths, server, _) = super::tests::fixture(root.path());
        let config = configured(&paths).unwrap();
        assert!(files_intact(&paths, &load_state(&paths).unwrap()));
        Self {
            server: Some(server),
            _root: root,
            paths,
            config,
        }
    }

    fn received(&self, source: VaultNetworkSource, stamp: u64) -> (Body, Envelope) {
        let mut attempt = Attempt::start(&self.paths, &self.config, source).unwrap();
        attempt.sampled_at_ms = stamp;
        let fetched = transport::fetch(&self.config.source, None, Duration::from_secs(60)).unwrap();
        let body = attempt
            .received(&fetched)
            .expect("CONTROL: actual URL body");
        let Fetched::Body(bytes) = fetched else {
            panic!("CONTROL: HTTP200")
        };
        let (envelope, _, _) = envelope::verify_for_reader(
            &bytes,
            &self.config.name,
            &self.config.signer.unwrap(),
            &self.config.followed,
            None,
        )
        .unwrap();
        (body, envelope)
    }

    fn seeded(&self) -> (State, Body, Envelope) {
        let (body, envelope) = self.received(VaultNetworkSource::ConfiguredUrl, 500);
        let lock = StateLock::take(&self.paths).unwrap();
        let mut state = load_state(&self.paths).unwrap();
        assert!(record(
            &self.paths,
            &lock,
            &self.config,
            &self.config,
            &mut state,
            &envelope,
            Some(body)
        ));
        (state, body, envelope)
    }

    fn change_recipient(&self) {
        let other = Identity::from_secret_bytes([223; 32]).to_bech32();
        fsx::write_atomic(&self.paths.identity_file(), other.as_slice(), 0o600).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop();
        }
    }
}

fn encoded(receipt: &Receipt) -> String {
    let mut text = String::new();
    receipt.write(&mut text);
    text
}

fn check_model(model: &Model) {
    let coverage = verify::prove_and_catch_scalar(model, "Vault network materialization");
    eprintln!("{}: {coverage:?}", model.name);
    let faults = verify::sweep_faults(std::slice::from_ref(model));
    assert!(faults.is_empty(), "{}: {faults:?}", model.name);
    assert_eq!(
        aterm_spec::interp::fired_actions(model).len(),
        model.actions.len()
    );
}

fn admits(model: &Model, before: &ModelState, after: &ModelState, action: &str) -> bool {
    let (accepted, why) = verify::validate_transition_tiered(
        model,
        &[],
        before,
        after,
        Some(action),
        "Vault network materialization conformance",
    );
    eprintln!("{} {action}: {why}", model.name);
    accepted
}

const ADMISSION_ACTIONS: &[&str] = &[
    "ConfiguredBody",
    "PeerBody",
    "FreshSameVersion",
    "EarlierStartLaterCommit",
    "RawBody",
    "NoNewer",
    "Missing",
    "Path",
    "CachedSubstitute",
    "ChangedConfiguration",
    "ChangedRecipient",
    "UnboundSource",
    "WrongSequence",
    "WrongEnvelopeDigest",
    "FailedApply",
    "PendingApply",
    "NoOwnership",
    "Revoked",
    "CacheMissing",
    "FileMissing",
    "FileChanged",
    "MissingIdentity",
    "PlainSource",
    "UnknownMembership",
    "ChangedRequestedContext",
];

pub(crate) fn admission_transition(before: &ModelState, action: &str) -> ModelState {
    admission_transition_with(before, action, false)
}

fn admission_transition_with(
    before: &ModelState,
    action: &str,
    forge_cache_origin: bool,
) -> ModelState {
    let f = Fixture::new();
    let (mut state, previous_body, previous_envelope) = f.seeded();
    let previous = encoded(state.network_materialization.as_ref().unwrap());
    let source = if action == "PeerBody" {
        VaultNetworkSource::PeerUrl
    } else {
        VaultNetworkSource::ConfiguredUrl
    };
    let stamp = if action == "EarlierStartLaterCommit" {
        100
    } else {
        1_000
    };
    let (mut received_body, envelope) = f.received(source, stamp);
    let lock = StateLock::take(&f.paths).unwrap();
    if action == "EarlierStartLaterCommit" {
        received_body.attempt._sampled_at =
            previous_body.attempt._sampled_at - Duration::from_secs(1);
    }
    if matches!(action, "FreshSameVersion" | "EarlierStartLaterCommit") {
        assert_eq!(envelope.digest(), previous_envelope.digest());
    }
    let mut body = Some(received_body);
    let mut requested = f.config.clone();
    let mut config = f.config.clone();
    match action {
        "RawBody" => body = Received::from(Fetched::Body(envelope.raw().to_vec())).body,
        "NoNewer" => body = received_body.attempt.received(&Fetched::NoNewer(None)),
        "Missing" => body = received_body.attempt.received(&Fetched::Missing),
        "Path" => {
            let mut local = config.clone();
            local.source = Source::Path(f.paths.home.join("scratch-vault"));
            assert!(Attempt::start(&f.paths, &local, source).is_none());
            body = None;
        }
        "CachedSubstitute" => body.as_mut().unwrap().digest = [224; 32],
        "ChangedConfiguration" => {
            let mut changed = config.clone();
            changed.host.push_str("-changed");
            save_config(&f.paths, &changed).unwrap();
        }
        "ChangedRecipient" => f.change_recipient(),
        "UnboundSource" => state.source_identity = None,
        "WrongSequence" => state.seq += 1,
        "WrongEnvelopeDigest" => state.digest = Some([224; 32]),
        "FailedApply" => state.failed = true,
        "PendingApply" => {
            state.pending.insert("token".into(), "00".into());
        }
        "NoOwnership" => state.own = None,
        "Revoked" => state.membership = Stored::Revoked,
        "CacheMissing" => std::fs::remove_file(f.paths.cache_file()).unwrap(),
        "FileMissing" => {
            std::fs::remove_file(materialize::target_path(&f.paths.home, "token").unwrap()).unwrap()
        }
        "FileChanged" => fsx::write_atomic(
            &materialize::target_path(&f.paths.home, "token").unwrap(),
            b"locally-changed",
            0o600,
        )
        .unwrap(),
        "MissingIdentity" => std::fs::remove_file(f.paths.identity_file()).unwrap(),
        "PlainSource" => {
            config.source = Source::Path(f.paths.home.join("scratch-vault"));
            config.signer = None;
            save_config(&f.paths, &config).unwrap();
        }
        "UnknownMembership" => state.membership = Stored::None,
        "ChangedRequestedContext" => requested.followed.push([224; 32]),
        _ => {}
    }
    if forge_cache_origin {
        // Executable unsafe policy: invent request provenance for raw cached
        // bytes by reusing a real earlier attempt and replacing its sample.
        assert!(body.is_none());
        let mut forged = previous_body;
        forged.attempt.sampled_at_ms = stamp;
        body = Some(forged);
    }
    let recorded = record(
        &f.paths, &lock, &requested, &config, &mut state, &envelope, body,
    );
    let receipt = state.network_materialization.as_ref().unwrap();
    let original = receipt.observation
        == VaultNetworkMaterialization {
            source,
            sequence: envelope.seq,
            sampled_at_ms: stamp,
        }
        && receipt.configuration == configuration_binding(&config)
        && receipt.source_identity == source_identity(&config)
        && receipt.recipient == received_body.attempt.recipient
        && receipt.digest == envelope.digest()
        && receipt.records_digest == state.records_digest;
    let retained = if encoded(receipt) == previous { 1 } else { 2 };
    let mut next = before.clone();
    next.insert(
        "scenario",
        (ADMISSION_ACTIONS
            .iter()
            .position(|name| *name == action)
            .unwrap()
            + 1) as i64,
    );
    next.insert("recorded", i64::from(recorded));
    next.insert("retained", retained);
    next.insert("original", i64::from(recorded && original));
    next
}

#[test]
fn producer_admission_conforms_and_forging_cache_origin_is_detected() {
    let model = aterm_models_db::vault_network_materialization_admission_model();
    check_model(&model);
    let initial = model.init_state();
    for action in &model.actions {
        let next = admission_transition(&initial, action.name);
        assert!(
            admits(&model, &initial, &next, action.name),
            "{}: {next:?}",
            action.name
        );
    }
    let unsafe_next = admission_transition_with(&initial, "RawBody", true);
    assert_eq!(unsafe_next["recorded"], 1);
    assert!(!admits(&model, &initial, &unsafe_next, "RawBody"));
    assert!(!model.check_invariant("ExactAdmission", &unsafe_next));
}

const PROJECTION_ACTIONS: &[&str] = &[
    "Matching",
    "NoReceipt",
    "ChangedSource",
    "ChangedName",
    "ChangedPin",
    "ChangedFollowed",
    "ChangedHost",
    "ChangedRecipient",
    "ChangedSequence",
    "ChangedEnvelope",
    "ChangedRecords",
    "ChangedSourceIdentity",
    "StateReload",
    "ReadAgain",
    "MissingIdentity",
    "MalformedReceipt",
    "LegacyState",
    "Unready",
    "StaleSelection",
    "ReappliedSelection",
];

pub(crate) fn projection_transition(before: &ModelState, action: &str) -> ModelState {
    projection_transition_with(before, action, false)
}

fn projection_transition_with(
    before: &ModelState,
    action: &str,
    sequence_only: bool,
) -> ModelState {
    if matches!(action, "StaleSelection" | "ReappliedSelection") {
        let (lab, paths) =
            super::super::log_entries_tests::stale_host_materialization(false, false);
        let config = configured(&paths).unwrap();
        let before_replay = load_state(&paths).unwrap();
        let receipt = before_replay.network_materialization.as_ref().unwrap();
        let expected = receipt.observation;
        let historical = encoded(receipt);
        assert_eq!(project(&paths, &config, &before_replay), Some(expected));
        if action == "ReappliedSelection" {
            let replay = apply_until(
                &paths,
                config.clone(),
                before_replay,
                ApplyInput::Cached,
                None,
                None,
                true,
            )
            .unwrap();
            assert!(matches!(replay, SyncOutcome::Applied { .. }));
        }
        let state = load_state(&paths).unwrap();
        assert_eq!(
            encoded(state.network_materialization.as_ref().unwrap()),
            historical
        );
        let observation = if sequence_only {
            project(&paths, &config, &state)
        } else {
            match observe_materialization(&paths, &config, || Ok(())).unwrap() {
                MaterializationObservation::Ready(held) => held.snapshot.network_materialization(),
                MaterializationObservation::Blocked(_) => None,
            }
        };
        lab.server.stop();
        let mut next = before.clone();
        next.insert("scenario", if action == "StaleSelection" { 19 } else { 20 });
        next.insert("shown", i64::from(observation.is_some()));
        next.insert("original", i64::from(observation == Some(expected)));
        return next;
    }
    let f = Fixture::new();
    let (mut state, _, _) = f.seeded();
    let expected = state.network_materialization.as_ref().unwrap().observation;
    let mut config = f.config.clone();
    match action {
        "NoReceipt" => state.network_materialization = None,
        "ChangedSource" => config.source = Source::Url("http://127.0.0.1:1/v1/other".into()),
        "ChangedName" => config.name.push_str("-other"),
        "ChangedPin" => config.signer = Some([224; 32]),
        "ChangedFollowed" => config.followed.push([224; 32]),
        "ChangedHost" => config.host.push_str("-other"),
        "ChangedRecipient" => f.change_recipient(),
        "ChangedSequence" => state.seq += 1,
        "ChangedEnvelope" => state.digest = Some([224; 32]),
        "ChangedRecords" => {
            state.records_digest = if state.records_digest.is_some() {
                None
            } else {
                Some([224; 32])
            }
        }
        "ChangedSourceIdentity" => {
            state
                .network_materialization
                .as_mut()
                .unwrap()
                .source_identity = [224; 32]
        }
        "StateReload" | "ReadAgain" => {
            if action == "ReadAgain" {
                state.heard = state.heard.saturating_add(10_000);
            }
            save_state(&f.paths, &state).unwrap();
            state = load_state(&f.paths).unwrap();
        }
        "MissingIdentity" => std::fs::remove_file(f.paths.identity_file()).unwrap(),
        "MalformedReceipt" => {
            let mut text = String::new();
            state
                .network_materialization
                .as_ref()
                .unwrap()
                .write(&mut text);
            let mut table: aterm_toml::Table = aterm_toml::from_str(&text).unwrap();
            let mut receipt_table = table.remove("network_materialization").unwrap();
            receipt_table
                .as_table_mut()
                .unwrap()
                .insert("sampled_at_ms".into(), aterm_toml::Value::Integer(0));
            table.insert("network_materialization".into(), receipt_table);
            state.network_materialization = Receipt::parse(&table);
        }
        "LegacyState" => state.network_materialization = Receipt::parse(&aterm_toml::Table::new()),
        "Unready" => {
            state.failed = true;
            save_state(&f.paths, &state).unwrap();
        }
        _ => {}
    }
    let observation = if sequence_only {
        // Executable unsafe policy: a matching sequence loses source, signer,
        // recipient, envelope and records context checks.
        state
            .network_materialization
            .as_ref()
            .filter(|receipt| receipt.sequence == state.seq)
            .map(|receipt| receipt.observation)
    } else if action == "Unready" {
        assert!(
            project(&f.paths, &config, &state).is_some(),
            "CONTROL: context still matches"
        );
        match observe_materialization(&f.paths, &config, || Ok(())).unwrap() {
            MaterializationObservation::Ready(held) => held.snapshot.network_materialization(),
            MaterializationObservation::Blocked(_) => None,
        }
    } else {
        project(&f.paths, &config, &state)
    };
    let mut next = before.clone();
    next.insert(
        "scenario",
        (PROJECTION_ACTIONS
            .iter()
            .position(|name| *name == action)
            .unwrap()
            + 1) as i64,
    );
    next.insert("shown", i64::from(observation.is_some()));
    next.insert("original", i64::from(observation == Some(expected)));
    next
}

#[test]
fn projection_conforms_and_sequence_only_or_unready_projection_is_detected() {
    let model = aterm_models_db::vault_network_materialization_projection_model();
    check_model(&model);
    let initial = model.init_state();
    for action in &model.actions {
        let next = projection_transition(&initial, action.name);
        assert!(
            admits(&model, &initial, &next, action.name),
            "{}: {next:?}",
            action.name
        );
    }
    for action in [
        "ChangedSource",
        "ChangedRecipient",
        "ChangedEnvelope",
        "ChangedRecords",
        "Unready",
        "StaleSelection",
    ] {
        let unsafe_next = projection_transition_with(&initial, action, true);
        assert_eq!(unsafe_next["shown"], 1);
        assert!(!admits(&model, &initial, &unsafe_next, action));
        assert!(!model.check_invariant("ExactContext", &unsafe_next));
    }
}
