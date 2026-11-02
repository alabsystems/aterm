// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::cli::shared_renewal::{Authority, Grant};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

struct Approval {
    grant: Grant,
    mode: AtomicU8,
}
impl Authority for Approval {
    fn prepare(&self, _: &Row) -> Result<Grant, String> {
        let mut grant = self.grant.clone();
        match self.mode.load(Ordering::SeqCst) {
            1 => return Err("root revoked".into()),
            4 => grant.decision = [8; 32],
            5 => grant.configuration = [8; 32],
            6 => grant.generation += 1,
            7 => grant.identity = "other@example.test".into(),
            8 => grant.source.account = [8; 32],
            9 => grant.expected_principal = Some([8; 32]),
            _ => {}
        }
        Ok(grant)
    }
    fn current(&self, grant: &Grant, row: &Row) -> Result<(), String> {
        if self.mode.load(Ordering::SeqCst) == 10 || self.prepare(row)? != *grant {
            Err("root changed after preparation".into())
        } else {
            Ok(())
        }
    }
}

fn observations(case: u8) -> Vec<aterm_spec::interp::State> {
    let bed = Bed::new(Cli::Claude);
    let staging = bed.staging("account");
    fs::write(staging.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"account@example.test","accountUuid":"stable-provider"}}"#).unwrap();
    let principal = crate::harness::profile::principal_id("stable-provider").unwrap();
    let grant = Grant {
        source: accounts::shared::Source {
            network: [1; 32],
            account: [2; 32],
        },
        decision: [3; 32],
        generation: 1,
        configuration: [4; 32],
        identity: "account@example.test".into(),
        expected_principal: Some(principal),
        required_browser_profile: None,
    };
    bed.store
        .transact_logins(NOW - 2, &mut None, |_, logins| {
            logins.shared.begin(
                accounts::shared::Intent {
                    source: grant.source,
                    decision: grant.decision,
                    generation: grant.generation,
                    configuration: grant.configuration,
                    request: [5; 16],
                    label: if case == 14 { "shared" } else { "account" }.into(),
                    identity: if case == 14 {
                        "shared@example.test".into()
                    } else {
                        grant.identity.clone()
                    },
                    staging: staging.to_str().unwrap().into(),
                    before_generation: None,
                    begun_at: NOW - 2,
                },
                (case != 14).then_some(principal),
            )?;
            Ok(Vec::new())
        })
        .unwrap();
    let before = bed.store.load_logins(NOW).unwrap().0;
    accounts::write_renewal_checkpoint(
        "account",
        &grant.identity,
        staging.to_str().unwrap(),
        Some("max"),
        NOW - 1,
        &before,
    )
    .unwrap();
    let evidence = fs::read(staging.join(accounts::RENEWAL_CHECKPOINT_FILE)).unwrap();
    let credential = fs::read(staging.join(".claude.json")).unwrap();
    let files = account_files(&bed.store);
    let approval = Arc::new(Approval {
        grant,
        mode: AtomicU8::new(case),
    });
    let config = config_beside(&bed.path);
    if case == 11 {
        fs::write(&config, "[harness]\nlogins = 'off'\n").unwrap();
    }
    // This factory is local and needs no LAN, central server or provider request.
    let expected_home = bed.home.clone();
    let factory_approval = approval.clone();
    let factory =
        move |home: &Path, source, _: &Row| -> Result<shared_renewal::CheckedAuthority, String> {
            assert_eq!(home, expected_home);
            assert_eq!(source, factory_approval.grant.source);
            Ok(factory_approval.clone())
        };
    let snapshot = |phase: i64, published: i64| {
        let logins = bed.store.load_logins(NOW).unwrap().0;
        let outcome = logins.shared.latest(approval.grant.source).unwrap().outcome;
        let consistent = if published == 0 {
            logins == before
        } else {
            matches!(outcome, accounts::shared::Outcome::Published { generation }
                if logins.get("account").is_some_and(|binding| binding.renewal_generation == generation && binding.dir.as_deref() == staging.to_str()))
                && logins.shared.principal_source(principal) == Some(approval.grant.source)
        };
        aterm_spec::interp::State::from([
            ("case", phase),
            ("published", published),
            ("custody", i64::from(consistent)),
            ("unchanged", i64::from(files == account_files(&bed.store))),
            (
                "kept",
                i64::from(
                    fs::read(staging.join(accounts::RENEWAL_CHECKPOINT_FILE)).unwrap() == evidence
                        && fs::read(staging.join(".claude.json")).unwrap() == credential,
                ),
            ),
        ])
    };
    let recover = |mode| {
        let authority = if mode == 12 {
            shared_renewal::CheckpointAuthority::Unavailable
        } else {
            shared_renewal::CheckpointAuthority::background(
                Some(&bed.home),
                Some(&factory),
                &bed.path,
            )
        };
        recover_interrupted_staging_renewals_checked(
            Cli::Claude,
            &bed.store,
            &bed.path,
            &bed.guard(),
            NOW,
            (&[], &authority),
            || {},
        )
    };
    let mut result = vec![snapshot(0, 0)];
    let mut published = i64::from(recover(case));
    result.push(snapshot(i64::from(case), published));
    if case == 1 {
        approval.mode.store(0, Ordering::SeqCst);
        published += i64::from(recover(0));
        result.push(snapshot(2, published));
        published += i64::from(recover(0));
        result.push(snapshot(3, published));
    }
    result
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let case = match action {
        "Revoke" | "Restore" | "Repeat" => 1,
        "Decision" => 4,
        "Configuration" => 5,
        "Generation" => 6,
        "Identity" => 7,
        "Source" => 8,
        "Principal" => 9,
        "ChangedDuringCheck" => 10,
        "Disabled" => 11,
        "Unavailable" => 12,
        "Offline" => 13,
        "StagingAlias" => 14,
        _ => panic!("unknown checkpoint authority action {action}"),
    };
    let phase = match action {
        "Restore" => 2,
        "Repeat" => 3,
        _ => 1,
    };
    let observed = observations(case);
    assert_eq!(&observed[phase - 1], state);
    observed[phase].clone()
}

#[test]
fn shared_checkpoint_authority_conforms_and_detects_unauthorized_or_lost_recovery() {
    let model = aterm_models_harness::shared_checkpoint_authority_model();
    assert_eq!(model.mutant_dials(), (1..=6).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    let reachable = aterm_spec::interp::reachable_states(&model);
    assert_eq!(reachable.len(), 15);
    for state in reachable {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&observed),
                    "{} {state:?} -> {observed:?}",
                    action.name
                );
                let mut bypass = observed.clone();
                bypass.insert("published", 2);
                assert!(!model.successors(action.name, &state).contains(&bypass));
            }
        }
    }
}
