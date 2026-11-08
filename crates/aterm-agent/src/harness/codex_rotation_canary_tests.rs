// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::codex_renewal::{self as renewal, Intent, Phase, Purpose, Receipt};

#[path = "codex_native_canary_model_tests.rs"]
mod model;

struct Fresh<'a>(&'a Reader);
impl UsageReader for Fresh<'_> {
    fn read(
        &self,
        path: &Path,
        _: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: crate::harness::provider_read::Use,
    ) -> Option<IdentifiedUsage> {
        let _read = use_token
            .begin_home(
                crate::harness::provider_read::Read::CodexEndpoint,
                path,
                now,
            )
            .ok()?;
        assert!(!wait.is_zero());
        let mut usage = self.0.answers.get(path)?.clone();
        usage.usage.sample.at = now;
        usage.usage.sample.session_read_at = Some(now);
        usage.usage.sample.week_read_at = Some(now);
        Some(usage)
    }
}

fn root(proposal: &codex_rotation::Proposal, id: &str, wall: &str) -> Receipt {
    let next = proposal.successor().unwrap();
    Receipt {
        destination: Some(next.identity),
        id: id.repeat(32),
        intent: Intent {
            purpose: Purpose::WeeklyQuota,
            thread: "00000000-0000-0000-0000-000000000001".into(),
            wall: wall.into(),
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
    }
}

fn reserved(
    name: &str,
) -> (
    World,
    Reader,
    crate::harness::cli::Env,
    Receipt,
    codex_rotation::Proposal,
) {
    let (world, reader) = fixture(name);
    let source = Identity::from_native("source", "user");
    let proposal = {
        let mut session = weekly_session(&world);
        session.sid = "s-one";
        codex_rotation::prepare(
            &world.seat(),
            &session,
            &source,
            &Fresh(&reader),
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap()
    };
    let first = root(&proposal, "a", "00000000-0000-0000-0000-000000000002");
    proposal.commit(&first, NOW).unwrap();
    native_auth(&first.old_auth, "source");
    native_auth(&first.new_auth, "target");
    let mut env = crate::harness::cli::Env::from_process().unwrap();
    env.home = Some(world.home.clone());
    env.state = world.state.clone();
    env.config = Some(world.cfg.join("aterm.toml"));
    env.now = NOW;
    renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &first))
        .unwrap()
        .unwrap();
    (world, reader, env, first, proposal)
}

#[test]
fn delayed_no_go_cannot_clear_a_newer_native_canary_in_the_same_session() {
    let (world, reader, mut env, first, original_plan) = reserved("native-canary-aba");
    let source = Identity::from_native("source", "user");
    let mut session = weekly_session(&world);
    session.sid = "s-one";
    renewal::with_receipt_writer(&env, |writer| {
        writer.cancel(&first, renewal::CancellationKind::OwnerStop, 0, 0)
    })
    .unwrap()
    .unwrap();

    // The old source is closed; delayed cleanup has not yet undone its pool
    // reservation. A later invocation uses the same session and target.
    let later = NOW + accounts::CANARY_HOLD_S + 1;
    let mut seat = world.seat();
    seat.now = later;
    session.point = Point::Wall {
        window: "seven_day".into(),
        at: Some(later),
    };
    let next = codex_rotation::prepare(
        &seat,
        &session,
        &source,
        &Fresh(&reader),
        Duration::from_secs(60),
    )
    .unwrap()
    .unwrap();
    let mut second = root(&next, "b", "00000000-0000-0000-0000-000000000003");
    next.commit(&second, later).unwrap();
    original_plan.commit(&first, later).unwrap();
    second.phase = Phase::Committing;
    second.launch_attempted = true;
    env.now = later;
    renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &second))
        .unwrap()
        .unwrap();
    let store = seat.store(Cli::Codex);
    let before = store.load(later).unwrap().0;
    let canary = before.canary.as_ref().unwrap();
    assert_eq!(canary.sid, first.sid);
    assert_eq!(canary.at, later);
    assert_eq!(canary.native_receipt.as_deref(), Some(second.id.as_str()));

    renewal::cancel_unexecuted_rotation(&env, &first, "work")
        .unwrap()
        .unwrap();
    let after = store.load(later).unwrap().0;
    assert_eq!(
        after.canary, before.canary,
        "old cleanup must retain the later canary"
    );
    assert_eq!(after.rotations, before.rotations);
    assert_eq!(after.active, before.active);
    let logins = store.load_logins(later).unwrap().0;
    assert!(!logins.native_rotations.contains_key(&first.id));
    assert!(logins.native_rotations.contains_key(&second.id));
}

#[test]
fn ordinary_no_move_report_cannot_clear_a_native_reservation() {
    let (world, _, _, receipt, _) = reserved("native-canary-ordinary");
    let store = world.seat().store(Cli::Codex);
    let before = store.load(NOW).unwrap().0;
    assert_eq!(
        before.canary.as_ref().unwrap().native_receipt.as_deref(),
        Some(receipt.id.as_str())
    );
    let events = store
        .transact(NOW, &mut None, |state| {
            Ok(accounts::strike(
                state,
                "work",
                &receipt.sid,
                &accounts::Strike::NotMoved("ordinary late report".into()),
                NOW,
            ))
        })
        .unwrap();
    assert!(events.is_empty());
    let after = store.load(NOW).unwrap().0;
    assert_eq!(after.canary, before.canary);
    assert_eq!(after.rotations, before.rotations);
    assert_eq!(after.active, before.active);
}

#[test]
fn unknown_native_canary_owner_preserves_the_existing_hold() {
    let (world, _, _, _, _) = reserved("native-canary-decode");
    let state = world.seat().store(Cli::Codex).load(NOW).unwrap().0;
    let mut expected = state.canary.clone().unwrap();
    expected.native_receipt = None;
    for value in [
        aterm_json::Value::Null,
        aterm_json::json!(false),
        aterm_json::json!("invalid"),
        aterm_json::json!("A".repeat(32)),
        aterm_json::json!("a".repeat(33)),
    ] {
        let mut data: aterm_json::Value = aterm_json::from_str(&state.to_json()).unwrap();
        data.as_object_mut()
            .unwrap()
            .get_mut("canary")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("native_receipt".into(), value);
        let parsed = accounts::State::from_json(&data.to_string(), NOW).unwrap();
        assert_eq!(parsed.canary, Some(expected.clone()));
        assert!(parsed.canary_holds(NOW));
    }
}

#[test]
fn native_cancellation_keeps_an_explicit_person_pin() {
    let (world, _, env, receipt, _) = reserved("native-canary-pin");
    let store = world.seat().store(Cli::Codex);
    store
        .transact(NOW, &mut None, |state| {
            state.pinned = true;
            Ok(Vec::new())
        })
        .unwrap();
    let before = store.load(NOW).unwrap().0;
    renewal::cancel_unexecuted_rotation(&env, &receipt, "work")
        .unwrap()
        .unwrap();
    let after = store.load(NOW).unwrap().0;
    assert!(after.pinned);
    assert_eq!(after.canary, before.canary);
    assert_eq!(after.rotations, before.rotations);
    assert_eq!(after.active, before.active);
}
