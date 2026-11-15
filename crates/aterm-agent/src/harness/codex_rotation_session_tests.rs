// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lost login's move judged by its session-scoped selection writer: the records alone say the
//! source ended ordinarily and the destination is this Mac's to use, and no pool record, canary
//! or `active` selection is read or written.

use super::*;
use crate::harness::codex_app_login::{OtherRefusal, RefusalReason};
use crate::harness::{cli::Env, codex_renewal as renewal};
use renewal::{Intent, Phase, Purpose, Receipt};

pub(super) const THREAD: &str = "00000000-0000-0000-0000-000000000001";
pub(super) const TURN: &str = "00000000-0000-0000-0000-000000000002";

/// A pool of two whose default row `main` (the session's) lost its login, beside `work`, a
/// signed-in row of another account: both login files hold real synthetic credentials.
pub(super) struct Lost {
    pub world: World,
    pub env: Env,
    /// `main`'s login directory, the machine's default login.
    pub source: PathBuf,
    /// `work`'s login directory.
    pub target: PathBuf,
}

/// [`Lost`], named `name`, with nothing yet recorded of the loss.
pub(super) fn lost(name: &str) -> Lost {
    let world = World::new(name);
    let target = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let source = world.home.join(Cli::Codex.accounts().default_home);
    native_auth(&source.join("auth.json"), "source");
    native_auth(&target.join("auth.json"), "target");
    let mut env = Env::from_process().unwrap();
    env.home = Some(world.home.clone());
    env.state = world.state.clone();
    env.config = Some(world.cfg.join("aterm.toml"));
    env.now = NOW;
    Lost {
        world,
        env,
        source,
        target,
    }
}

impl Lost {
    /// The issuer's refusal of the token `main`'s login file holds, for `reason`, at `at`.
    pub(super) fn refuse(&self, reason: RefusalReason, at: i64) {
        crate::harness::codex_app_login::record_refusal_for_test(&self.source, reason, at).unwrap();
    }

    /// `row` read `word` on record, as a provider read stages it.
    pub(super) fn stage(&self, row: &str, word: accounts::Auth) {
        let world = &self.world;
        world
            .seat()
            .store(Cli::Codex)
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(stage_current_auth(
                    world,
                    Cli::Codex,
                    state,
                    logins,
                    row,
                    word,
                    NOW,
                ))
            })
            .unwrap();
    }

    /// `main` signed out on record after a settled refusal of its token for `reason`.
    pub(super) fn ended(&self, reason: RefusalReason) {
        self.refuse(reason, NOW - 600);
        self.stage("main", accounts::Auth::SignedOut);
    }

    /// Edits the pool's own state.
    pub(super) fn edit(&self, edit: impl FnOnce(&mut accounts::State)) {
        self.world
            .seat()
            .store(Cli::Codex)
            .transact(NOW, &mut None, |state| {
                edit(state);
                Ok(Vec::new())
            })
            .unwrap();
    }

    /// A Ready receipt moving the session on `main` to `work`.
    pub(super) fn receipt(&self) -> Receipt {
        let store = self.world.seat().store(Cli::Codex);
        let raw = Roster::load_for(
            Cli::Codex,
            &self.world.cfg.join(Cli::Codex.accounts().roster_file),
        )
        .unwrap();
        let (logins, _) = store.load_logins(NOW).unwrap();
        let roster = accounts::bound(&raw, &logins);
        Receipt {
            destination: Some(Identity::from_native("target", "user")),
            id: "c".repeat(32),
            intent: Intent {
                purpose: Purpose::LoginRotation,
                thread: THREAD.into(),
                wall: TURN.into(),
                identity: Identity::from_native("source", "user"),
            },
            sid: "s-one".into(),
            old_pid: 10,
            old_birth: 1,
            claim_revision: 1,
            human_seq: 0,
            old_auth: self.source.join("auth.json"),
            new_auth: self.target.join("auth.json"),
            generation: logins
                .get("work")
                .map_or(0, |value| value.renewal_generation),
            row_stamp: renewal::row_stamp(roster.row("work").unwrap()),
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

    /// What the prepared and forward writers answer of `receipt`.
    fn judge(&self, receipt: &Receipt) -> [Result<Option<()>, &'static str>; 2] {
        [
            codex_rotation::with_prepared_writer(&self.env, receipt, |_, _| Ok(())),
            renewal::with_selected(&self.env, receipt, |_| Ok(())),
        ]
    }

    /// The pool's records a session's move must leave as they were.
    fn pool(&self) -> (Option<String>, Vec<i64>, bool, usize) {
        let state = self.world.load(Cli::Codex);
        let logins = self
            .world
            .seat()
            .store(Cli::Codex)
            .load_logins(NOW)
            .unwrap()
            .0;
        (
            state.active,
            state.rotations,
            state.canary.is_some(),
            logins.native_rotations.len(),
        )
    }
}

/// A SESSION'S MOVE OFF A LOGIN THAT ENDED ORDINARILY IS ADMITTED WHILE THE POOL STAYS ON ITS ROW:
/// with `main` signed out after a settled `refresh_token_expired`, the prepared and forward
/// writers admit `work` though the pool's `active` row is `main` and no pool record binds the
/// receipt, and the rollback writer admits restoring the source. The usage-limit handoff's writer
/// refuses the receipt as another scope's. Nothing in the pool's records changes on any path,
/// retire included: `active`, its rotations, its canary and its deduplication records.
#[test]
fn a_session_move_is_admitted_beside_the_pools_own_selection_and_changes_none_of_it() {
    let lost = lost("session-admitted");
    lost.ended(RefusalReason::Expired);
    let before = lost.pool();
    assert_eq!(before.0.as_deref(), Some("main"));
    let receipt = lost.receipt();
    assert_eq!(lost.judge(&receipt), [Ok(Some(())), Ok(Some(()))]);
    assert_eq!(
        codex_rotation::with_selected_writer(&lost.env, &receipt, |_, _| Ok(())),
        Err("receipt-scope"),
        "the pool's writer judges no session move"
    );
    let mut renewal = receipt.clone();
    renewal.intent.purpose = Purpose::Login;
    assert_eq!(
        codex_rotation::with_prepared_writer(&lost.env, &renewal, |_, _| Ok(())),
        Err("not-moving-receipt")
    );

    let mut launched = receipt.clone();
    launched.phase = Phase::Committing;
    launched.launch_attempted = true;
    renewal::with_receipt_writer(&lost.env, |_| renewal::write_receipt(&lost.env, &launched))
        .unwrap()
        .unwrap();
    assert_eq!(
        codex_rotation::with_rollback_writer(&lost.env, &launched, |_| Ok(())),
        Ok(Some(()))
    );
    let mut retired = launched.clone();
    retired.phase = Phase::Retired;
    retired.launch_attempted = false;
    renewal::with_receipt_writer(&lost.env, |writer| {
        renewal::write_receipt(&lost.env, &retired)?;
        writer.retire_pool_record(&retired)
    })
    .map(|removed| assert_eq!(removed, Some(false), "no pool record to retire"))
    .unwrap();
    assert_eq!(lost.pool(), before);
}

/// A PIN STOPS A SESSION'S MOVE IN EVERY PHASE, AS IT STOPS A POOL MOVE: a pin on the source (set
/// after its sign-out, a person's new decision) or on the destination refuses the prepared,
/// forward and rollback writers alike. CONTROL: unpinned, all three admit.
#[test]
fn any_pin_stops_a_session_move_in_every_phase() {
    for pinned in [Some("main"), Some("work"), None] {
        let lost = lost("session-pinned");
        lost.ended(RefusalReason::Expired);
        if let Some(row) = pinned {
            lost.edit(|state| {
                state.active = Some(row.into());
                state.pinned = true;
            });
        }
        let receipt = lost.receipt();
        let mut launched = receipt.clone();
        launched.phase = Phase::Committing;
        launched.launch_attempted = true;
        renewal::with_receipt_writer(&lost.env, |_| renewal::write_receipt(&lost.env, &launched))
            .unwrap()
            .unwrap();
        let rollback = codex_rotation::with_rollback_writer(&lost.env, &launched, |_| Ok(()));
        let [prepared, forward] = lost.judge(&receipt);
        if pinned.is_some() {
            for answer in [prepared, forward, rollback] {
                assert_eq!(answer, Err("target-unavailable"), "{pinned:?}");
            }
        } else {
            for answer in [prepared, forward, rollback] {
                assert_eq!(answer, Ok(Some(())), "CONTROL");
            }
        }
    }
}

/// ONLY A SOURCE STILL SIGNED OUT AFTER A SETTLED ORDINARY END IS MOVED: a source read back in,
/// one whose refusal was for any reason but `refresh_token_expired` (reused, invalidated,
/// `invalid_grant`, an unknown code such as a deactivation's, an invalid token), one whose
/// refusal is not settled yet, one whose token aterm never presented, and one whose journal says
/// nothing readable are each refused by the prepared and forward writers.
#[test]
fn only_a_settled_ordinary_end_of_a_signed_out_source_admits_a_session_move() {
    let refused = |lost: &Lost, why: &'static str| {
        assert_eq!(lost.judge(&lost.receipt()), [Err(why), Err(why)]);
    };
    let back = lost("session-signed-back");
    back.ended(RefusalReason::Expired);
    back.stage("main", accounts::Auth::Ok);
    refused(&back, "source-not-signed-out");

    for reason in [
        RefusalReason::Reused,
        RefusalReason::Invalidated,
        RefusalReason::InvalidGrant,
        RefusalReason::UNKNOWN,
        RefusalReason::of_code("account_deactivated"),
        RefusalReason::Other(OtherRefusal::InvalidToken),
    ] {
        let lost = lost("session-other-reason");
        lost.ended(reason);
        refused(&lost, "source-not-ended");
    }

    let pending = lost("session-pending");
    pending.refuse(RefusalReason::Expired, NOW - 10);
    pending.stage("main", accounts::Auth::SignedOut);
    refused(&pending, "source-not-ended");

    let unpresented = lost("session-unpresented");
    unpresented.stage("main", accounts::Auth::SignedOut);
    refused(&unpresented, "source-not-ended");

    let unknown = lost("session-unknown");
    unknown.stage("main", accounts::Auth::SignedOut);
    std::fs::write(unknown.source.join("auth.json.refreshed"), "not a journal").unwrap();
    refused(&unknown, "source-not-ended");

    let control = lost("session-control");
    control.ended(RefusalReason::Expired);
    assert_eq!(
        control.judge(&control.receipt()),
        [Ok(Some(())), Ok(Some(()))]
    );
}

/// A DESTINATION THIS MAC MAY NOT USE IS NO SESSION MOVE'S, AS A POOL MOVE'S BEGIN REFUSES IT: a
/// destination another Mac's live lease holds, or one held for preparation only, after the move
/// was prepared, is refused by the prepared and forward writers. (A preparation that failed is
/// an operation's own runtime record, never on disk for a writer to read.)
#[test]
fn a_destination_leased_elsewhere_or_held_for_preparation_stops_a_session_move() {
    let leased = lost("session-leased");
    leased.ended(RefusalReason::Expired);
    let receipt = leased.receipt();
    assert_eq!(leased.judge(&receipt), [Ok(Some(())), Ok(Some(()))]);
    leased.edit(|state| state.note_lease("work", "m-elsewhere", NOW + 3600, NOW));
    assert_eq!(
        leased.judge(&receipt),
        [Err("target-unavailable"), Err("target-unavailable")]
    );

    let prepared = lost("session-preparation-only");
    prepared.ended(RefusalReason::Expired);
    let receipt = prepared.receipt();
    prepared.edit(|state| {
        state.prepared_accounts.insert("work".into());
    });
    assert_eq!(
        prepared.judge(&receipt),
        [Err("target-unavailable"), Err("target-unavailable")]
    );
}
