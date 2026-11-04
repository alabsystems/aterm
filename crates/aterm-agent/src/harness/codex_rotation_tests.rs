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

fn weekly_session() -> Session<'static> {
    let mut session = session(
        Cli::Codex,
        Supervisor::Own,
        Mode::Early,
        Path::new("/opt/codex"),
    );
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

#[test]
fn selected_quota_guard_requires_pool_commit_both_identities_and_current_policy() {
    use crate::harness::codex_renewal::{self as renewal, Intent, Phase, Purpose, Receipt};
    for changed in 0..6 {
        let (world, reader) = fixture("quota-selected-guard");
        let mut env = crate::harness::cli::Env::from_process().unwrap();
        env.home = Some(world.home.clone());
        env.state = world.state.clone();
        env.config = Some(world.cfg.join("aterm.toml"));
        env.now = NOW;
        let proposal = codex_rotation::prepare(
            &world.seat(),
            &weekly_session(),
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
        if matches!(changed, 0 | 4 | 5) {
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
            &weekly_session(),
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
        receipt.phase = phase.clone();
        receipt.launch_attempted = attempted;
        if phase == Phase::Consumed {
            receipt.accepted_turn = Some(receipt.intent.wall.clone());
            receipt.launch_attempted = true;
        }
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt)).unwrap();
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
    }
}

#[test]
fn quota_proposal_binds_fresh_identities_and_defers_the_pool_commit() {
    let (world, reader) = fixture("quota-proposal");
    let s = weekly_session();
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
    let s = weekly_session();
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
fn native_quota_retry_cannot_override_a_later_selection_or_unusable_target() {
    for change in 0..4 {
        let (world, reader) = fixture("quota-retry-revoked");
        let s = weekly_session();
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
            .transact_logins(NOW, &mut None, |state, _| {
                match change {
                    0 => state.pinned = true,
                    1 => state.active = Some("main".into()),
                    2 => {
                        return Ok(accounts::note_auth(
                            state,
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
        let s = weekly_session();
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
        let s = weekly_session();
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
    let request = weekly_session();
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
