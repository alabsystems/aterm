// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::codex_app_login::IdentifiedUsage;
use crate::harness::codex_renewal::Identity;
use crate::harness::codex_rotation::{self, UsageReader};
use std::cell::RefCell;
use std::collections::BTreeMap;

struct Reader {
    calls: RefCell<Vec<PathBuf>>,
    answers: BTreeMap<PathBuf, IdentifiedUsage>,
}

impl UsageReader for Reader {
    fn read(
        &self,
        path: &Path,
        _: Option<&str>,
        _: i64,
        wait: Duration,
    ) -> Option<IdentifiedUsage> {
        assert!(!wait.is_zero());
        self.calls.borrow_mut().push(path.to_owned());
        self.answers.get(path).cloned()
    }
}

fn usage(account: &str, full: bool) -> IdentifiedUsage {
    IdentifiedUsage {
        identity: Identity::from_native(account, "user"),
        usage: crate::harness::codex_app_login::usage_from_backend(
            &aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": !full,
                    "primary_window": {"used_percent": 0, "limit_window_seconds": 18000,
                        "reset_at": NOW + 3600},
                    "secondary_window": {"used_percent": if full { 100 } else { 0 },
                        "limit_window_seconds": 604800, "reset_at": NOW + 86_400}
                }
            }),
            NOW,
        )
        .expect("real provider percentages"),
    }
}

fn weekly_session(world: &World) -> Session<'_> {
    let mut session = session(Cli::Codex, Supervisor::Own, Mode::Early, &world.exe);
    session.point = Point::Wall {
        window: "seven_day".into(),
        at: Some(NOW),
    };
    session
}

fn fixture(name: &str) -> (World, Reader) {
    let world = World::new(name);
    let work = world.two(Cli::Codex).join("auth.json");
    world.on_main(Cli::Codex, &[]);
    let source = world.home.join(".codex/auth.json");
    let reader = Reader {
        calls: RefCell::new(Vec::new()),
        answers: BTreeMap::from([
            (source, usage("source", true)),
            (work, usage("target", false)),
        ]),
    };
    (world, reader)
}

fn native_auth(path: &Path, account: &str) {
    let claims = aterm_json::json!({"https://api.openai.com/auth": {
        "chatgpt_account_id": account, "chatgpt_user_id": "user"
    }});
    let encoded = aterm_codec::base64::encode(claims.to_string().as_bytes())
        .unwrap()
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_owned();
    let token = format!("e30.{encoded}.fixture");
    std::fs::write(
        path,
        aterm_json::json!({"auth_mode":"chatgpt","tokens":{
            "id_token":token, "access_token":token,"refresh_token":"fixture"
        }})
        .to_string(),
    )
    .unwrap();
}

/// A PIN ON ANOTHER ROW HOLDS A HANDOFF BEFORE ANY PROVIDER READ: its writer takes no target under
/// a pin, so a session on `main` under a pin on `work` reads nothing and proposes nothing, however
/// often its control retries. A PIN ON THE SESSION'S OWN ROW ENDS AT ITS WALL: the session moves,
/// and the writer takes it. CONTROL: with no pin it moves.
#[test]
fn a_pin_on_another_row_holds_the_handoff_before_any_provider_read() {
    let pin = |world: &World, label: &str| {
        world
            .seat()
            .store(Cli::Codex)
            .transact(NOW, &mut None, |state| {
                state.active = Some(label.to_string());
                state.pinned = true;
                Ok(Vec::new())
            })
            .unwrap();
    };
    let source = Identity::from_native("source", "user");
    let (world, reader) = fixture("quota-pinned-elsewhere");
    pin(&world, "work");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    );
    assert!(matches!(proposal, Ok(None)), "{proposal:?}");
    assert!(reader.calls.borrow().is_empty(), "{:?}", reader.calls);
    assert!(world.load(Cli::Codex).pinned, "the owner's pin stands");

    let (world, reader) = fixture("quota-pinned-here");
    pin(&world, "main");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("its own wall ends the pin");
    assert_eq!(proposal.destination.label, "work");
    assert!(!world.load(Cli::Codex).pinned, "the wall ended the pin");

    let (world, reader) = fixture("quota-unpinned");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(proposal.is_some(), "CONTROL");
}

/// A LOW ROW IS NO HANDOFF'S TARGET, AS ITS WRITER REFUSES ONE: with `work` low the session proposes
/// nothing and reads no usage of `work`, rather than selecting a row the writer would refuse.
/// CONTROL: with `work` at room it is selected.
#[test]
fn a_low_row_is_no_handoffs_target() {
    let source = Identity::from_native("source", "user");
    let (world, reader) = fixture("quota-low-target");
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW, &mut None, |state| {
            Ok(accounts::note_sample(
                state,
                "work",
                Sample {
                    session_pct: Some(95),
                    session_reset: Some(NOW + 7200),
                    at: NOW,
                    ..Sample::default()
                },
                NOW,
            ))
        })
        .unwrap();
    let work = world.work_dir(Cli::Codex).join("auth.json");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(proposal.is_none(), "{proposal:?}");
    assert!(!reader.calls.borrow().contains(&work), "{:?}", reader.calls);
    let (world, reader) = fixture("quota-room-target");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(
        proposal.map(|proposal| proposal.destination.label),
        Some("work".to_string()),
        "CONTROL"
    );
}

#[test]
fn selected_quota_guard_requires_pool_commit_both_identities_and_current_policy() {
    use crate::harness::codex_renewal::{self as renewal, Intent, Phase, Purpose, Receipt};
    for changed in 0..7 {
        let (world, reader) = fixture("quota-selected-guard");
        if changed == 6 {
            let roster = accounts::Roster::load_for(
                Cli::Codex,
                &world.cfg.join(Cli::Codex.accounts().roster_file),
            )
            .unwrap();
            let work = PathBuf::from(roster.row("work").unwrap().dir.as_ref().unwrap());
            for home in [world.home.join(".codex"), work] {
                std::fs::create_dir_all(&home).unwrap();
                std::fs::write(
                    home.join("config.toml"),
                    "cli_auth_credentials_store='file'\n",
                )
                .unwrap();
            }
            world
                .seat()
                .store(Cli::Codex)
                .transact(NOW, &mut None, |state| {
                    Ok(accounts::note_auth(
                        state,
                        "work",
                        accounts::Auth::SignedOut,
                        NOW,
                    ))
                })
                .unwrap();
        }
        let mut env = crate::harness::cli::Env::from_process().unwrap();
        env.home = Some(world.home.clone());
        env.state = world.state.clone();
        env.config = Some(world.cfg.join("aterm.toml"));
        env.now = NOW;
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &weekly_session(&world),
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap();
        native_auth(&proposal.old_auth, "source");
        native_auth(&proposal.new_auth, "target");
        let next = proposal.successor().unwrap();
        let mut receipt = Receipt {
            destination: Some(next.identity.clone()),
            id: "a".repeat(32),
            intent: Intent {
                purpose: Purpose::WeeklyQuota,
                thread: "00000000-0000-0000-0000-000000000001".into(),
                wall: "00000000-0000-0000-0000-000000000002".into(),
                identity: proposal.source_identity.clone(),
            },
            sid: "s-one".into(),
            old_pid: 10,
            old_birth: 1,
            claim_revision: 1,
            human_seq: 0,
            old_auth: next.old_auth.clone(),
            new_auth: next.new_auth.clone(),
            generation: next.generation,
            row_stamp: renewal::row_stamp(&next.row),
            native_prepare: "sealed native recipe".into(),
            expires_ms: u64::MAX,
            phase: Phase::Ready,
            successor_pid: None,
            successor_birth: None,
            accepted_turn: None,
            launch_attempted: false,
            candidate: None,
            current_attempt: None,
            revoked: false,
        };
        assert_eq!(
            renewal::with_selected(&env, &receipt, |_| Ok(true)),
            Err("pool-commit-missing")
        );
        assert_eq!(
            codex_rotation::with_prepared_writer(&env, &receipt, |_, _| Ok(true)),
            Ok(Some(true))
        );
        assert!(world.load(Cli::Codex).rotations.is_empty());
        for invalid in 0..3 {
            let mut attempted = receipt.clone();
            match invalid {
                0 => attempted.phase = Phase::Committing,
                1 => attempted.launch_attempted = true,
                _ => attempted.accepted_turn = Some("accepted-turn".into()),
            }
            assert_eq!(
                codex_rotation::with_prepared_writer(&env, &attempted, |_, _| {
                    panic!("invalid preparation reached the receipt writer")
                }),
                Err::<Option<()>, _>("not-ready")
            );
        }
        proposal.commit(&receipt, NOW).unwrap();
        assert_eq!(
            renewal::with_selected(&env, &receipt, |_| Ok(true)),
            Ok(Some(true))
        );
        let mut rollback = receipt.clone();
        rollback.phase = Phase::Committing;
        rollback.launch_attempted = true;
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &rollback)).unwrap();
        assert_eq!(
            codex_rotation::with_rollback_writer(&env, &rollback, |_| Ok(true)),
            Ok(Some(true))
        );
        match changed {
            0 => native_auth(&proposal.new_auth, "other-target"),
            1 => native_auth(&proposal.old_auth, "other-source"),
            2 => {
                receipt.native_prepare.push_str(" changed");
            }
            3 => {
                world
                    .seat()
                    .store(Cli::Codex)
                    .transact_logins(NOW, &mut None, |state, _| {
                        state.pinned = true;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            4 => std::fs::remove_file(&proposal.new_auth).unwrap(),
            6 => {
                let store = world.seat().store(Cli::Codex);
                let records = store
                    .read_account_records(
                        Some(&world.cfg.join(Cli::Codex.accounts().roster_file)),
                        NOW,
                    )
                    .unwrap();
                assert_eq!(
                    records.state.account("work").auth,
                    accounts::Auth::SignedOut
                );
                let generation = records
                    .logins
                    .get("work")
                    .map(|binding| binding.renewal_generation);
                let raw = records.raw_roster.row("work").unwrap();
                let context = store
                    .capture_auth_context(records.roster.row("work").unwrap())
                    .unwrap();
                let events = store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        accounts::stage_auth(
                            state,
                            raw,
                            logins,
                            context,
                            accounts::Auth::SignedOut,
                            NOW,
                        );
                        Ok(Vec::new())
                    })
                    .unwrap();
                assert!(
                    events
                        .iter()
                        .all(|event| event.kind != accounts::Kind::Auth)
                );
                assert_eq!(
                    store
                        .load_logins(NOW)
                        .unwrap()
                        .0
                        .get("work")
                        .map(|binding| binding.renewal_generation),
                    generation
                );
                assert!(
                    proposal
                        .plan
                        .clone()
                        .commit_native(
                            NOW,
                            &receipt.id,
                            &codex_rotation::receipt_binding(&receipt).unwrap()
                        )
                        .line
                        .is_none()
                );
            }
            _ => {
                world
                    .seat()
                    .store(Cli::Codex)
                    .transact(NOW, &mut None, |state| {
                        Ok(accounts::note_wall(
                            state,
                            "work",
                            "seven_day",
                            Some(NOW + 86_400),
                            NOW,
                            NOW,
                        ))
                    })
                    .unwrap();
            }
        }
        assert!(renewal::with_selected(&env, &receipt, |_| Ok(true)).is_err());
        assert!(codex_rotation::with_prepared_writer(&env, &receipt, |_, _| Ok(true)).is_err());
        // Failed/replaced/missing destination credentials and destination quota
        // do not authorize Forward, but cannot block the sealed compensation.
        // Source identity, immutable receipt and owner pin changes still refuse it.
        rollback.native_prepare = receipt.native_prepare.clone();
        let restoration = codex_rotation::with_rollback_writer(&env, &rollback, |_| Ok(true));
        if matches!(changed, 0 | 4 | 5 | 6) {
            assert_eq!(restoration, Ok(Some(true)));
        } else {
            assert!(restoration.is_err());
        }
    }
}

#[test]
fn terminal_quota_receipts_retire_only_their_exact_pool_binding() {
    use crate::harness::codex_renewal::{self as renewal, Intent, Phase, Purpose, Receipt};
    for (phase, attempted) in [
        (Phase::Ready, false),
        (Phase::Committing, true),
        (Phase::Retired, false),
        (Phase::Retired, true),
        (Phase::Consumed, true),
    ] {
        let (world, reader) = fixture("quota-terminal-receipt");
        let mut env = crate::harness::cli::Env::from_process().unwrap();
        env.home = Some(world.home.clone());
        env.state = world.state.clone();
        env.config = Some(world.cfg.join("aterm.toml"));
        env.now = NOW;
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &weekly_session(&world),
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap();
        let next = proposal.successor().unwrap();
        let mut receipt = Receipt {
            destination: Some(next.identity.clone()),
            id: "a".repeat(32),
            intent: Intent {
                purpose: Purpose::WeeklyQuota,
                thread: "00000000-0000-0000-0000-000000000001".into(),
                wall: "00000000-0000-0000-0000-000000000002".into(),
                identity: proposal.source_identity.clone(),
            },
            sid: "s-one".into(),
            old_pid: 10,
            old_birth: 1,
            claim_revision: 1,
            human_seq: 0,
            old_auth: next.old_auth,
            new_auth: next.new_auth,
            generation: next.generation,
            row_stamp: renewal::row_stamp(&next.row),
            native_prepare: "sealed native recipe".into(),
            expires_ms: u64::MAX,
            phase: Phase::Ready,
            successor_pid: None,
            successor_birth: None,
            accepted_turn: None,
            launch_attempted: false,
            candidate: None,
            current_attempt: None,
            revoked: false,
        };
        proposal.commit(&receipt, NOW).unwrap();
        native_auth(&receipt.old_auth, "source");
        native_auth(&receipt.new_auth, "target");
        receipt.phase = phase.clone();
        receipt.launch_attempted = attempted;
        if phase == Phase::Consumed {
            receipt.accepted_turn = Some(receipt.intent.wall.clone());
            receipt.launch_attempted = true;
        }
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt)).unwrap();
        if phase != Phase::Consumed {
            assert!(renewal::record_completed_rotation(&env, &receipt).is_err());
        }
        let allowed = phase == Phase::Consumed || (phase == Phase::Retired && !attempted);
        let mut wrong = receipt.clone();
        wrong.native_prepare.push_str(" changed");
        assert!(renewal::retire_pool_record(&env, &wrong).is_err());
        if allowed {
            assert_eq!(renewal::retire_pool_record(&env, &receipt), Ok(Some(true)));
            assert_eq!(renewal::retire_pool_record(&env, &receipt), Ok(Some(false)));
            assert!(
                world
                    .seat()
                    .store(Cli::Codex)
                    .load_logins(NOW)
                    .unwrap()
                    .0
                    .native_rotations
                    .is_empty()
            );
            assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
            if phase == Phase::Consumed {
                assert!(
                    !world.load(Cli::Codex).proven,
                    "consumption is not completion"
                );
                assert!(renewal::record_completed_rotation(&env, &wrong).is_err());
                native_auth(&receipt.new_auth, "changed-target");
                assert!(renewal::record_completed_rotation(&env, &receipt).is_err());
                assert!(!world.load(Cli::Codex).proven);
                native_auth(&receipt.new_auth, "target");
                let active = world.load(Cli::Codex).active;
                let store = world.seat().store(Cli::Codex);
                store
                    .transact_logins(NOW, &mut None, |state, _| {
                        state.active = Some("main".into());
                        Ok(Vec::new())
                    })
                    .unwrap();
                assert_eq!(
                    codex_rotation::with_consumed_member_writer(&env, &receipt, |_, _| Ok(true)),
                    Ok(Some(true))
                );
                native_auth(&receipt.new_auth, "changed-target");
                assert!(
                    codex_rotation::with_consumed_member_writer(&env, &receipt, |_, _| Ok(true))
                        .is_err()
                );
                native_auth(&receipt.new_auth, "target");
                store
                    .transact_logins(NOW, &mut None, |state, _| {
                        state.active = active.clone();
                        Ok(Vec::new())
                    })
                    .unwrap();
                assert_eq!(
                    renewal::record_completed_rotation(&env, &receipt),
                    Ok(Some(()))
                );
                assert!(world.load(Cli::Codex).proven);
                assert!(world.load(Cli::Codex).canary.is_none());
                assert_eq!(
                    renewal::record_completed_rotation(&env, &receipt),
                    Ok(Some(()))
                );
            }
        } else {
            assert!(renewal::retire_pool_record(&env, &receipt).is_err());
            assert_eq!(
                world
                    .seat()
                    .store(Cli::Codex)
                    .load_logins(NOW)
                    .unwrap()
                    .0
                    .native_rotations
                    .len(),
                1
            );
        }
        if phase == Phase::Ready {
            let mut changed = receipt.clone();
            changed.native_prepare.push_str(" changed");
            assert!(renewal::cancel_unexecuted_rotation(&env, &changed, "work").is_err());
            let mut executed = receipt.clone();
            executed.launch_attempted = true;
            assert!(renewal::cancel_unexecuted_rotation(&env, &executed, "work").is_err());
            let retired = renewal::cancel_unexecuted_rotation(&env, &receipt, "work")
                .unwrap()
                .unwrap();
            assert_eq!(retired.phase, Phase::Retired);
            assert!(retired.revoked);
            assert_eq!(world.load(Cli::Codex).active.as_deref(), Some("main"));
            assert!(world.load(Cli::Codex).canary.is_none());
            assert!(
                world
                    .seat()
                    .store(Cli::Codex)
                    .load_logins(NOW)
                    .unwrap()
                    .0
                    .native_rotations
                    .is_empty()
            );
            assert_eq!(
                renewal::cancel_unexecuted_rotation(&env, &receipt, "work"),
                Ok(Some(retired.clone()))
            );
            assert_eq!(
                renewal::cancel_unexecuted_rotation(&env, &retired, "work"),
                Ok(Some(retired))
            );
        }
    }
}

#[test]
fn quota_proposal_binds_fresh_identities_and_defers_the_pool_commit() {
    let (world, reader) = fixture("quota-proposal");
    let s = weekly_session(&world);
    let expected = Identity::from_native("source", "user");
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &s,
        &expected,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("quota proposal");
    assert_eq!(proposal.source.label, "main");
    assert_eq!(proposal.destination.label, "work");
    assert_eq!(proposal.source_identity, expected);
    assert_eq!(
        proposal.destination_identity,
        Identity::from_native("target", "user")
    );
    assert_eq!(
        &*reader.calls.borrow(),
        &[proposal.old_auth.clone(), proposal.new_auth.clone()]
    );
    assert!(world.load(Cli::Codex).rotations.is_empty());
    assert!(proposal.plan.commit(NOW).line.is_some());
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
}

#[test]
fn native_quota_commit_retries_charge_once_and_refuse_a_different_binding() {
    let (world, reader) = fixture("quota-commit-retry");
    let s = weekly_session(&world);
    let expected = Identity::from_native("source", "user");
    let prepare = || {
        codex_rotation::prepare(
            &world.seat(),
            &s,
            &expected,
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap()
        .plan
    };
    let first = prepare();
    let retry = prepare();
    let conflicting = prepare();
    let id = "a".repeat(32);
    let binding = "b".repeat(64);
    assert!(first.commit_native(NOW, &id, &binding).line.is_some());
    assert!(
        conflicting
            .commit_native(NOW, &id, &"c".repeat(64))
            .line
            .is_none()
    );
    assert!(retry.commit_native(NOW, &id, &binding).line.is_some());
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
}

#[test]
fn concurrent_native_plans_follow_the_proven_target_without_another_rotation() {
    let (world, reader) = fixture("quota-concurrent-follow");
    let first_session = weekly_session(&world);
    let mut next_session = weekly_session(&world);
    next_session.sid = "next-native-session";
    let expected = Identity::from_native("source", "user");
    let prepare = |session: &Session<'_>| {
        codex_rotation::prepare(
            &world.seat(),
            session,
            &expected,
            &reader,
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap()
        .plan
    };
    let first = prepare(&first_session);
    let next = prepare(&next_session);
    let id = "a".repeat(32);
    let next_id = "c".repeat(32);
    let binding = "b".repeat(64);
    assert!(first.commit_native(NOW, &id, &binding).line.is_some());
    assert!(
        next.clone()
            .commit_native(NOW, &next_id, &binding)
            .line
            .is_none(),
        "a pending canary still owns admission"
    );
    let store = world.seat().store(Cli::Codex);
    store
        .transact(NOW, &mut None, |state| {
            Ok(accounts::prove(state, "work", NOW).into_iter().collect())
        })
        .unwrap();
    assert!(
        next.commit_native(NOW, &next_id, &binding).line.is_some(),
        "a successful canary lets the already-prepared session follow the same target"
    );
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
    assert_eq!(store.load_logins(NOW).unwrap().0.native_rotations.len(), 2);
}

#[test]
fn native_quota_retry_cannot_override_a_later_selection_or_unusable_target() {
    for change in 0..4 {
        let (world, reader) = fixture("quota-retry-revoked");
        let s = weekly_session(&world);
        let expected = Identity::from_native("source", "user");
        let prepare = || {
            codex_rotation::prepare(
                &world.seat(),
                &s,
                &expected,
                &reader,
                Duration::from_secs(60),
            )
            .unwrap()
            .unwrap()
            .plan
        };
        let first = prepare();
        let retry = prepare();
        let id = "a".repeat(32);
        let binding = "b".repeat(64);
        assert!(first.commit_native(NOW, &id, &binding).line.is_some());
        let store = world.seat().store(Cli::Codex);
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                match change {
                    0 => state.pinned = true,
                    1 => state.active = Some("main".into()),
                    2 => {
                        return Ok(stage_current_auth(
                            &world,
                            Cli::Codex,
                            state,
                            logins,
                            "work",
                            accounts::Auth::SignedOut,
                            NOW,
                        ));
                    }
                    _ => {
                        return Ok(accounts::note_wall(
                            state,
                            "work",
                            "seven_day",
                            Some(NOW + 24 * HOUR),
                            NOW,
                            NOW,
                        ));
                    }
                }
                Ok(Vec::new())
            })
            .unwrap();
        let after_change = world.load(Cli::Codex);
        assert!(retry.commit_native(NOW, &id, &binding).line.is_none());
        assert_eq!(world.load(Cli::Codex), after_change);
        assert_eq!(after_change.rotations, [NOW]);
    }
}

#[test]
fn quota_proposal_refuses_wrong_source_restored_quota_and_unusable_destinations() {
    for case in 0..4 {
        let (world, mut reader) = fixture("quota-proposal-refused");
        let source = world.home.join(".codex/auth.json");
        let work = world.work_dir(Cli::Codex).join("auth.json");
        match case {
            0 => {
                reader
                    .answers
                    .insert(source, usage("replaced-source", true));
            }
            1 => {
                reader.answers.insert(source, usage("source", false));
            }
            2 => {
                reader.answers.insert(work, usage("target", true));
            }
            _ => {
                reader.answers.insert(work, usage("source", false));
            }
        }
        let s = weekly_session(&world);
        let result = codex_rotation::prepare(
            &world.seat(),
            &s,
            &Identity::from_native("source", "user"),
            &reader,
            Duration::from_secs(60),
        );
        if case == 1 {
            assert_eq!(result.as_ref().err().copied(), Some("not-weekly-exhausted"));
        }
        assert!(result.is_err() || result.is_ok_and(|proposal| proposal.is_none()));
        assert!(world.load(Cli::Codex).rotations.is_empty());
    }
}

#[test]
fn disabled_rotation_and_an_expired_budget_make_no_provider_requests() {
    for disabled in [false, true] {
        let (world, reader) = fixture("quota-proposal-disabled");
        if disabled {
            std::fs::write(
                world.cfg.join("aterm.toml"),
                "[harness]\naccounts = 'off'\n",
            )
            .unwrap();
        }
        let s = weekly_session(&world);
        let result = codex_rotation::prepare(
            &world.seat(),
            &s,
            &Identity::from_native("source", "user"),
            &reader,
            if disabled {
                Duration::from_secs(60)
            } else {
                Duration::ZERO
            },
        );
        assert!(result.unwrap().is_none());
        assert!(reader.calls.borrow().is_empty());
    }
}

#[test]
#[cfg(unix)]
fn native_exec_proposal_uses_fresh_shared_account_reads_and_typed_dispatch() {
    use crate::harness::codex_exec_handoff::ExecCommand;
    let (world, reader) = fixture("exec-quota-proposal");
    let request = weekly_session(&world);
    let program = std::env::current_exe().unwrap();
    let image = crate::harness::native_act::Image::at(&program).unwrap();
    let source = Identity::from_native("source", "user");
    let request = Session {
        cli: request.cli,
        sid: request.sid,
        supervisor: request.supervisor,
        env: request.env,
        sample: request.sample,
        point: request.point,
        mode: request.mode,
        relaunch: (),
    };
    let proposal = codex_rotation::prepare_exec(
        &world.seat(),
        &request,
        &world.exe,
        &source,
        &reader,
        Duration::from_secs(60),
        |row| {
            Some(ExecCommand {
                argv0: "codex".into(),
                image: image.clone(),
                program: program.to_str().unwrap().into(),
                argv: vec!["exec".into(), "--json".into(), "original\ninput".into()],
                auth_path: row
                    .dir
                    .as_ref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| world.home.join(".codex"))
                    .join("auth.json")
                    .to_str()
                    .unwrap()
                    .into(),
                cwd: None,
            })
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(proposal.source_identity, source);
    assert_eq!(
        proposal.destination_identity,
        Identity::from_native("target", "user")
    );
    assert_eq!(
        &*reader.calls.borrow(),
        &[proposal.old_auth.clone(), proposal.new_auth.clone()]
    );
    assert!(world.load(Cli::Codex).rotations.is_empty());
    let selected = proposal
        .plan
        .commit_native(NOW, &"d".repeat(32), &"e".repeat(64))
        .line
        .expect("native account selection");
    assert_eq!(Path::new(&selected.auth_path), proposal.new_auth);
    assert_eq!(selected.argv, ["exec", "--json", "original\ninput"]);
    assert_eq!(world.load(Cli::Codex).rotations, [NOW]);
}

/// A provider's read with `session` and `week` percent spent.
fn usage_at(account: &str, session: u64, week: u64) -> IdentifiedUsage {
    IdentifiedUsage {
        identity: Identity::from_native(account, "user"),
        usage: crate::harness::codex_app_login::usage_from_backend(
            &aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": week < 100,
                    "primary_window": {"used_percent": session, "limit_window_seconds": 18000,
                        "reset_at": NOW + 3600},
                    "secondary_window": {"used_percent": week,
                        "limit_window_seconds": 604800, "reset_at": NOW + 86_400}
                }
            }),
            NOW,
        )
        .expect("real provider percentages"),
    }
}

/// A FRESH READ THAT FINDS A ROW LOW SELECTS NOTHING, AND THE READ IS ON RECORD: a writer would
/// refuse that row, and the report judges it by what was just read. CONTROL: with room it moves.
#[test]
fn a_fresh_read_that_finds_a_row_low_selects_nothing_and_stays_on_record() {
    let source = Identity::from_native("source", "user");
    let (world, mut reader) = fixture("quota-fresh-low");
    let work = world.work_dir(Cli::Codex).join("auth.json");
    native_auth(&work, "target");
    reader.answers.insert(work, usage_at("target", 95, 0));
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(proposal.is_none(), "{proposal:?}");
    let state = world.load(Cli::Codex);
    assert_eq!(
        state.accounts["work"]
            .sample
            .as_ref()
            .and_then(|s| s.session_pct),
        Some(95),
        "the read is on record"
    );
}

/// A WALL BESIDE A READ AT THAT LIMIT ENDS AT THE READ'S RESET, AND A LATER WALL BESIDE A READ SHORT
/// OF IT KEEPS THAT RESET, through the writer a session's plan and a handoff's prepare both reach. A
/// read short of a limit says nothing of when that limit's wall ends: the planning model's case 9.
#[test]
fn a_wall_takes_the_reset_of_a_read_at_that_limit() {
    let walls_after = |reads: &[(Sample, &str)]| {
        let world = World::new("wall-reset");
        world.two(Cli::Codex);
        world.on_main(Cli::Codex, &[]);
        let exe = crate::harness::codex_alab::lay_package(&world.root.join("store/tried"), true);
        for (sample, window) in reads {
            let mut session = session(Cli::Codex, Supervisor::Aterm, Mode::Early, &exe);
            session.sample = Some(sample.clone());
            session.point = Point::Wall {
                window: (*window).into(),
                at: Some(NOW),
            };
            let _ = session_plan(&world.seat(), &session, |_| {});
        }
        let state = world.load(Cli::Codex);
        let window = reads.last().expect("a read").1;
        state.accounts["main"].walls[window].until
    };
    let at_limit = Sample {
        week_pct: Some(100),
        week_reset: Some(NOW + 86_400),
        at: NOW,
        ..Sample::default()
    };
    let short = Sample {
        session_pct: Some(95),
        session_reset: Some(NOW + 3600),
        at: NOW,
        ..Sample::default()
    };
    assert_eq!(
        walls_after(&[(at_limit.clone(), "seven_day")]),
        Some(NOW + 86_400)
    );
    assert_eq!(walls_after(&[(short.clone(), "five_hour")]), None);
    assert_eq!(
        walls_after(&[(at_limit, "seven_day"), (short, "seven_day")]),
        Some(NOW + 86_400)
    );
}

/// A HANDOFF'S PREPARE, AFTER THE READ A LAUNCH MADE, KEEPS THE WEEKLY RESET THAT READ NAMED.
#[test]
fn a_prepare_keeps_the_reset_a_launch_read_named() {
    let source = Identity::from_native("source", "user");
    let (world, reader) = fixture("prepare-keeps-reset");
    let main = world.seat().store(Cli::Codex);
    main.transact(NOW, &mut None, |state| {
        let read = Sample {
            week_pct: Some(100),
            week_reset: Some(NOW + 86_400),
            at: NOW,
            ..Sample::default()
        };
        Ok(accounts::note_sample(state, "main", read, NOW))
    })
    .unwrap();
    codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &source,
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .expect("a proposal");
    let state = world.load(Cli::Codex);
    let until = state.accounts["main"]
        .sample
        .as_ref()
        .and_then(|s| s.week_reset);
    assert_eq!(until, Some(NOW + 86_400));
}

/// A LOGIN NO ROW HANDS OFF IS A REFUSAL THAT STANDS, HOWEVER OFTEN IT IS ASKED; a spent budget or
/// a lock is one to ask about again.
#[test]
fn a_session_no_row_hands_off_stands_and_a_busy_one_is_asked_again() {
    use codex_rotation::{NoHandoff, prepare_refusal_stands};
    for why in ["source-unbound", "source-missing", "source-unmanaged"] {
        assert_eq!(prepare_refusal_stands(why), Some(NoHandoff::NoRowOfThePool));
    }
    assert_eq!(
        prepare_refusal_stands("not-weekly-exhausted"),
        Some(NoHandoff::LimitNotReached)
    );
    assert_eq!(prepare_refusal_stands("roster-busy"), None);
}

/// A RECORD THE DEADLINE DOES NOT HANG ON FITS IN WHAT IS LEFT OF IT: a spent deadline waits for no
/// pause, and a wide one takes the whole ladder.
#[test]
fn a_fresh_read_waits_no_longer_for_its_record_than_the_deadline_leaves() {
    assert!(super::super::ladder_within(Duration::ZERO).is_empty());
    let some = Duration::from_millis(100);
    let within = super::super::ladder_within(some);
    assert!(!within.is_empty());
    assert!(within.iter().sum::<Duration>() <= some);
    assert_eq!(
        super::super::ladder_within(Duration::from_secs(3600)),
        accounts::LOCK_LADDER
    );
}
