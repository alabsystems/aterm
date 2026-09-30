// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-1 for `NativeUpdateSuccessorWarmBeforeClaim` (aterm-spec's
//! `native_update_successor_warm_before_claim_model`), phase P1 of
//! docs/DESIGN-warm-successor-2026-09-29.md: the REAL snapshot and intake,
//! driven as a successor runs them, projected onto the model and checked
//! transition by transition with `interp::admits` (the model must admit each
//! real step, by the named action) and every invariant on every state.
//!
//! What is measured, not asserted by the harness:
//!
//! * `captured` — after the real `HandoffEnv::capture` no handoff name is left
//!   in the process environment, and the snapshot holds what the launch
//!   environment carried;
//! * `threads_spawned` — a real thread, alive for the rest of the walk (the
//!   P2 warm prologue's shape);
//! * `env_mut_after_spawn` — a CANARY in the process environment under every
//!   handoff name, planted after the spawn: any read-and-clear, set or unset of
//!   one by the code under test moves a canary (and a read of one would feed
//!   the intake a canary, which authenticates nothing);
//! * `claim_ok` — the real claim returned and its descriptors landed in the
//!   snapshot; `owned_taken` — the single-use manifest was consumed and every
//!   session adopted.
//!
//! The launched lane (macOS) runs the real `claim_incoming` against a real
//! `Rendezvous` whose other end is a thread of this process standing in for the
//! outgoing parent, then the real `ClaimedIntake::install`, `take_incoming_from`
//! and the channel admissions. The fork lane (unix) runs the real prearm and
//! intake and checks the proof against the parent's expectation — the SAME
//! authentication as before the snapshot, to the byte. The negative control
//! replays the retired transport (`ClaimedHandoff::publish`'s three `set_var`s
//! after a thread exists) and the model admits it only as the `Buggy`
//! `PublishToEnv`.
//!
//! P2 (the warm prologue before the dial) adds three binds: the REAL order
//! decision (`warm_order`) is the model's `SpawnWarm` guard over the socket
//! shapes, relative included; the REAL miss decision (`warm_miss_px`, over
//! the real `launch_font_for_zoom`/`successor_font_px` and the real hint
//! parser) is its `Reshape`/`RealPresent` guards; and the real intake, run with
//! a stale and a hostile hint in the snapshot, proves exactly the parent's
//! expectation — the hint never reaches the adoption proof (`HintNotInProof`),
//! with a hint-folding replay as the caught negative control.
//!
//! Not driven, and waived by name in `handoff_env`: the warm prologue itself
//! (it builds a GPU device — AGENTS.md rule 5), the window's reveal and
//! presents (a window, likewise), the relative shape's `chdir` (driven by
//! `control_socket_identity`'s own suites) and Prove/Commit (bound by
//! `NativeUpdateSeamlessHandoffOwnership`'s Tier-1).

use super::tests::{ENV_LOCK, RestoreVar, StagedHandoff, pipe_pair, stage_outgoing_handoff};
use super::*;
use crate::handoff_env::{HANDOFF_ENV_KEYS, HandoffEnv};
use aterm_spec::derive::{Model, native_update_successor_warm_before_claim_model};
use aterm_spec::interp::{self, State};
use std::sync::PoisonError;

#[cfg(target_os = "macos")]
const WARMING: i64 = 1;
#[cfg(target_os = "macos")]
const DIALLED: i64 = 2;
const CLAIMED: i64 = 3;
#[cfg(target_os = "macos")]
const ADOPTED: i64 = 4;
#[cfg(target_os = "macos")]
const EXITED: i64 = 7;

/// What the harness observed of the real successor, in the model's terms.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Observed {
    phase: i64,
    captured: bool,
    threads_spawned: bool,
    env_mut_after_spawn: bool,
    owned_taken: bool,
    claim_ok: bool,
    /// P2: the warm prologue's main-thread section returned.
    warm_done: bool,
    /// P2: this launch's control socket is an explicit relative path.
    relative_socket: bool,
    /// P2: the backend was built at a size the carry contradicts.
    stale_build: bool,
}

/// Project an observation onto the model's state. The window, the warm
/// present and the hint stay 0: P1 has none of them (headless, and no code).
pub(crate) fn project(model: &Model, seen: Observed) -> State {
    let mut state = model.init_state();
    state.insert("phase", seen.phase);
    state.insert("captured", i64::from(seen.captured));
    state.insert("threads_spawned", i64::from(seen.threads_spawned));
    state.insert("env_mut_after_spawn", i64::from(seen.env_mut_after_spawn));
    state.insert("owned_taken", i64::from(seen.owned_taken));
    state.insert("claim_ok", i64::from(seen.claim_ok));
    state.insert("warm_done", i64::from(seen.warm_done));
    state.insert("relative_socket", i64::from(seen.relative_socket));
    state.insert("build_size_ok", i64::from(!seen.stale_build));
    state
}

/// The model must admit `prev -> next` by exactly `action`, and every
/// invariant must hold at `next`.
#[cfg(target_os = "macos")]
fn step(model: &Model, prev: Observed, next: Observed, action: &str) {
    let (from, to) = (project(model, prev), project(model, next));
    assert_eq!(
        interp::admits(model, &from, &to),
        Some(action),
        "the model admits the real step {from:?} -> {to:?} as {action}"
    );
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &to),
            "{} holds after the real {action}: {to:?}",
            invariant.name
        );
    }
}

/// No handoff name is left in the process environment.
fn env_is_clear() -> bool {
    HANDOFF_ENV_KEYS
        .iter()
        .all(|key| std::env::var_os(key).is_none())
}

/// A canary under every handoff name, planted once a thread exists.
fn plant_canaries() -> Vec<(&'static str, String)> {
    HANDOFF_ENV_KEYS
        .iter()
        .map(|key| {
            let canary = format!("canary-{key}");
            aterm_log::env::set(key, &canary);
            (*key, canary)
        })
        .collect()
}

/// Whether any canary moved: the process environment was mutated under a
/// handoff name after the spawn.
fn canaries_moved(planted: &[(&'static str, String)]) -> bool {
    planted
        .iter()
        .any(|(key, canary)| std::env::var_os(key).as_deref() != Some(canary.as_ref()))
}

/// Save every name the walk touches, restored on drop (declare after the lock).
fn restore_all() -> Vec<RestoreVar> {
    ["XDG_RUNTIME_DIR", "HOME"]
        .iter()
        .chain(HANDOFF_ENV_KEYS)
        .map(|key| RestoreVar::new(key))
        .collect()
}

/// A real thread, alive until the returned sender drops or sends.
fn spawn_warm_thread() -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let warm = std::thread::spawn(move || {
        let _ = stopped.recv();
    });
    (stop, warm)
}

fn this_build() -> String {
    encode_target_identity(
        crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
        crate::build_info::GIT_COMMIT,
    )
}

/// THE LAUNCHED LANE, end to end over the snapshot: capture, a thread, the real
/// dial and grant, the claim installed into the snapshot, the intake and both
/// channels admitted — and not one process-environment access after the spawn.
#[cfg(target_os = "macos")]
#[test]
fn the_launched_lane_claims_into_the_snapshot_and_adopts_without_touching_the_environment() {
    use std::os::fd::BorrowedFd;
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_all();
    let model = native_update_successor_warm_before_claim_model();
    let staged = stage_outgoing_handoff("p1-launched", 2, None);
    let entries = decode_fds_bounded(&staged.fds_wire)
        .expect("the fixture's wire")
        .entries;
    let rendezvous =
        crate::handoff_rendezvous::Rendezvous::bind(&staged.nonce).expect("bind the rendezvous");
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    // SAFETY: `getpid` has no preconditions.
    let own = unsafe { libc::getpid() };
    let parent = AttestedParent::attest_live_for_test(own).expect("this process attests");

    // THE LAUNCH ENVIRONMENT a LaunchServices successor is started with: the
    // out-of-band shape (no descriptor numbers), as `launch_successor` writes it.
    for key in HANDOFF_ENV_KEYS {
        aterm_log::env::unset(key);
    }
    aterm_log::env::set(ENV_MANIFEST, &staged.manifest_path);
    aterm_log::env::set(ENV_NONCE, &staged.nonce);
    aterm_log::env::set(
        ENV_LAYOUT,
        staged.manifest_path.with_extension("layout.toml"),
    );
    aterm_log::env::set(ENV_TARGET, this_build());
    aterm_log::env::set(ENV_RENDEZVOUS, rendezvous.path());
    aterm_log::env::set(ENV_CLAIM, rendezvous.claim());
    aterm_log::env::set(crate::handoff_rendezvous::ENV_PROOF_TERM, "device");
    aterm_log::env::set(crate::control_socket_identity::ENV_IDENTITY, "");

    // Capture.
    let launched = Observed::default();
    let mut env = HandoffEnv::capture();
    let captured = Observed {
        phase: WARMING,
        captured: env_is_clear() && env.present(ENV_MANIFEST) && env.present(ENV_RENDEZVOUS),
        ..launched
    };
    step(&model, launched, captured, "Capture");

    // SpawnWarm: a real thread; canaries from here on.
    let (stop, warm) = spawn_warm_thread();
    let planted = plant_canaries();
    let warming = Observed {
        threads_spawned: warm.thread().id() != std::thread::current().id(),
        ..captured
    };
    step(&model, captured, warming, "SpawnWarm");
    // WarmDone: the prologue's main-thread section returns without joining
    // the thread it spawned — the stand-in stays parked through the dial.
    let warming = {
        let done = Observed {
            warm_done: true,
            ..warming
        };
        step(&model, warming, done, "WarmDone");
        done
    };

    // Dial: the real `claim_incoming` over the snapshot, against the real
    // listener, whose outgoing end is a thread of this process.
    assert_eq!(
        crate::handoff_rendezvous::take_device_proof_term(&mut env),
        Some(true),
        "the proof term is read from the snapshot"
    );
    let nonce = env.string(ENV_NONCE).expect("the nonce is in the snapshot");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let claimed = std::thread::scope(|scope| {
        let outgoing = scope.spawn(|| {
            let peer = rendezvous
                .accept_claim(Some(own), deadline, &|| false)
                .expect("the successor presents its claim");
            // SAFETY: descriptors this test owns for the length of the call.
            let sessions = entries
                .iter()
                .map(|(id, fd, pid)| (*id, *pid, unsafe { BorrowedFd::borrow_raw(*fd) }))
                .collect::<Vec<_>>();
            // SAFETY: as above.
            let (ready, commit) = unsafe {
                (
                    BorrowedFd::borrow_raw(ready_write),
                    BorrowedFd::borrow_raw(commit_read),
                )
            };
            // Handed back and dropped once the claim returned. (The real
            // outgoing process drops it the instant the grant is sent —
            // `app_update_handoff`, "From here the successor holds copies of
            // everything"; that ordering is pinned by handoff_rendezvous's
            // `a_grant_whose_connection_closes_at_once_is_still_claimed`.)
            let granted = peer.transfer(&staged.nonce, &sessions, ready, commit, deadline);
            (peer, granted)
        });
        let claimed = crate::handoff_rendezvous::claim_incoming(
            &mut env,
            &nonce,
            crate::handoff_rendezvous::ClaimDeadlines {
                dial: deadline,
                grant: deadline,
            },
            Some(parent),
        );
        let (peer, granted) = outgoing.join().expect("the outgoing half");
        assert!(granted.is_ok(), "the grant: {granted:?}");
        drop(peer);
        claimed.expect("the claim is granted")
    });
    let dialled = Observed {
        phase: DIALLED,
        env_mut_after_spawn: canaries_moved(&planted),
        ..warming
    };
    assert!(
        !env.present(ENV_RENDEZVOUS) && !env.present(ENV_CLAIM),
        "the dial consumed the rendezvous from the snapshot"
    );
    step(&model, warming, dialled, "Dial");
    assert!(
        !warm.is_finished(),
        "the dial never waited on the warm thread (JoinWorkerBeforeDial)"
    );

    // ClaimOk: the grant lands in the snapshot where the fork lane's arrive.
    assert_eq!(claimed.session_count(), 2);
    claimed.into_intake().install(&mut env);
    let claim_ok = Observed {
        phase: CLAIMED,
        claim_ok: [ENV_FDS, ENV_READY_FD, ENV_COMMIT_FD]
            .iter()
            .all(|key| env.present(key)),
        env_mut_after_spawn: canaries_moved(&planted),
        ..dialled
    };
    step(&model, dialled, claim_ok, "ClaimOk");

    // TakeOwned: the unchanged intake, over the snapshot.
    assert!(incoming_offered_in(&env));
    assert!(
        staged.manifest_path.exists(),
        "nothing owned before the intake"
    );
    let incoming = take_incoming_from(&mut env);
    let owned = Observed {
        owned_taken: incoming.adopted.len() == 2 && !staged.manifest_path.exists(),
        env_mut_after_spawn: canaries_moved(&planted),
        ..claim_ok
    };
    step(&model, claim_ok, owned, "TakeOwned");

    // Adopt: the target, both channels and the socket witness, admitted.
    let target = take_target_identity_from(&mut env).expect("this build is the target");
    let adopted_fds = incoming
        .adopted
        .iter()
        .map(|adopted| adopted.master.raw())
        .collect::<Vec<_>>();
    let ready = take_ready_fd_from(
        &mut env,
        incoming.nonce.clone(),
        incoming.layout_digest,
        incoming.screen_digest,
        Some(target),
        &adopted_fds,
    )
    .expect("the readiness channel is admitted");
    let commit = take_commit_fd_from(
        &mut env,
        incoming.nonce.clone(),
        &adopted_fds,
        Some(ready.raw_fd()),
        Some(parent),
    );
    assert!(commit.is_some(), "the Commit channel is admitted");
    assert!(
        crate::control_socket_identity::consume_incoming_from(&mut env) == Ok(None),
        "the explicit empty witness"
    );
    let adopted = Observed {
        phase: ADOPTED,
        env_mut_after_spawn: canaries_moved(&planted),
        ..owned
    };
    step(&model, owned, adopted, "Adopt");
    assert!(
        env.exec_env().is_empty(),
        "every handoff name was consumed from the snapshot"
    );
    assert!(
        !canaries_moved(&planted),
        "and the process environment was never touched"
    );

    drop((ready, commit, incoming));
    let _ = stop.send(());
    warm.join().expect("the warm thread");
    drop(rendezvous);
    for (_, fd, _) in entries {
        aterm_pty::close_fd(fd);
    }
    for fd in [ready_read, ready_write, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    StagedHandoff::teardown(staged);
}

/// A FAILED CLAIM LEAVES NOTHING: nobody listens at the rendezvous, the real
/// dial fails, and the manifest — the outgoing process's — is untouched, with
/// nothing owned and nothing shown.
#[cfg(target_os = "macos")]
#[test]
fn a_failed_claim_owns_nothing_and_touches_no_environment() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_all();
    let model = native_update_successor_warm_before_claim_model();
    let staged = stage_outgoing_handoff("p1-refused", 1, None);
    // A path in the private dir with no listener behind it: bound, then dropped
    // (which unlinks it), as an outgoing process that gave up leaves it.
    let path = crate::handoff_rendezvous::Rendezvous::bind(&staged.nonce)
        .expect("bind the rendezvous")
        .path()
        .to_path_buf();
    // SAFETY: `getpid` has no preconditions.
    let own = unsafe { libc::getpid() };
    let parent = AttestedParent::attest_live_for_test(own).expect("this process attests");
    for key in HANDOFF_ENV_KEYS {
        aterm_log::env::unset(key);
    }
    aterm_log::env::set(ENV_MANIFEST, &staged.manifest_path);
    aterm_log::env::set(ENV_NONCE, &staged.nonce);
    aterm_log::env::set(
        ENV_LAYOUT,
        staged.manifest_path.with_extension("layout.toml"),
    );
    aterm_log::env::set(ENV_RENDEZVOUS, &path);
    aterm_log::env::set(ENV_CLAIM, "0".repeat(64));

    let launched = Observed::default();
    let mut env = HandoffEnv::capture();
    let captured = Observed {
        phase: WARMING,
        captured: env_is_clear(),
        ..launched
    };
    step(&model, launched, captured, "Capture");
    let (stop, warm) = spawn_warm_thread();
    let planted = plant_canaries();
    let warming = Observed {
        threads_spawned: true,
        ..captured
    };
    step(&model, captured, warming, "SpawnWarm");
    let warming = {
        let done = Observed {
            warm_done: true,
            ..warming
        };
        step(&model, warming, done, "WarmDone");
        done
    };

    let soon = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let refused = crate::handoff_rendezvous::claim_incoming(
        &mut env,
        &staged.nonce,
        crate::handoff_rendezvous::ClaimDeadlines {
            dial: soon,
            grant: soon,
        },
        Some(parent),
    );
    assert!(refused.is_err(), "nobody is listening");
    let dialled = Observed {
        phase: DIALLED,
        env_mut_after_spawn: canaries_moved(&planted),
        ..warming
    };
    step(&model, warming, dialled, "Dial");
    let exited = Observed {
        phase: EXITED,
        owned_taken: !staged.manifest_path.exists(),
        env_mut_after_spawn: canaries_moved(&planted),
        ..dialled
    };
    step(&model, dialled, exited, "ClaimFail");
    assert!(
        env.present(ENV_MANIFEST) && staged.manifest_path.exists(),
        "the refused successor consumed nothing of the outgoing process's"
    );

    let _ = stop.send(());
    warm.join().expect("the warm thread");
    staged.teardown();
}

/// THE FORK LANE, and the SAME AUTHENTICATION: the launch environment as the
/// fork lane's parent writes it, captured, prearmed and taken entirely from
/// the snapshot after a thread exists — and the proof the successor computes
/// is the parent's expectation, byte for byte, as before the snapshot.
#[cfg(unix)]
#[test]
fn the_fork_lane_intake_from_the_snapshot_proves_what_the_parent_expects() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_all();
    let staged = stage_outgoing_handoff("p1-fork", 3, None);
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    for key in HANDOFF_ENV_KEYS {
        aterm_log::env::unset(key);
    }
    staged.publish_env(ready_write, commit_read, Some(this_build()));

    let mut env = HandoffEnv::capture();
    assert!(env_is_clear(), "capture leaves no handoff name behind");
    let prearmed = prearm_incoming_fds_from(&mut env);
    assert!(!prearmed.rejects_boot() && prearmed.parent_pid().is_some());
    assert!(
        !env.present(ENV_PARENT_PID),
        "the parent's identity is typed state"
    );
    // The boot apply's re-exec would carry exactly what is still held, then
    // the attestation.
    let exec = prearmed.final_exec_env_with(&env);
    assert!(exec.iter().any(|(key, _)| key == ENV_FDS));
    assert!(exec.iter().any(|(key, _)| key == ENV_PARENT_PID));

    let (stop, warm) = spawn_warm_thread();
    let planted = plant_canaries();

    let incoming = take_incoming_from(&mut env);
    assert_eq!(incoming.adopted.len(), 3, "every session is adopted");
    let target = take_target_identity_from(&mut env).expect("this build is the target");
    let adopted_fds = incoming
        .adopted
        .iter()
        .map(|adopted| adopted.master.raw())
        .collect::<Vec<_>>();
    let ready = take_ready_fd_from(
        &mut env,
        incoming.nonce.clone(),
        incoming.layout_digest,
        incoming.screen_digest,
        Some(target),
        &adopted_fds,
    )
    .expect("the readiness channel is admitted");
    let commit = take_commit_fd_from(
        &mut env,
        incoming.nonce.clone(),
        &adopted_fds,
        Some(ready.raw_fd()),
        prearmed.parent_pid(),
    );
    assert!(commit.is_some(), "the Commit channel is admitted");
    let identities = incoming
        .adopted
        .iter()
        .map(|item| (item.local_id, item.master.raw(), item.pid))
        .collect::<Vec<_>>();
    assert_eq!(
        ready.proof(&identities),
        Some(staged.expected),
        "THE SAME AUTHENTICATION: the proof over the snapshot is the parent's expectation"
    );
    assert!(env.exec_env().is_empty(), "every name consumed");
    assert!(
        !canaries_moved(&planted),
        "no process-environment access after the spawn"
    );

    drop((ready, commit, incoming));
    let _ = stop.send(());
    warm.join().expect("the warm thread");
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    staged.teardown();
}

/// NEGATIVE CONTROL: the retired transport. `ClaimedHandoff::publish` wrote the
/// claimed descriptors into the PROCESS environment — three `set_var`s — and
/// with a thread alive (P2's order) that is the data race the snapshot removes.
/// Replayed here exactly, the canaries see it, and the model admits the step
/// only as its `Buggy` mutant `PublishToEnv`, whose state breaks
/// `NoEnvMutationAfterSpawn`. A projection blind to env writes would pass this
/// replay as a legal step, and this test would fail.
#[cfg(unix)]
#[test]
fn the_retired_env_transport_is_the_caught_mutant() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_all();
    let model = native_update_successor_warm_before_claim_model();
    let buggy = interp::with_buggy(&model, 1);
    for key in HANDOFF_ENV_KEYS {
        aterm_log::env::unset(key);
    }
    let _env_snapshot = HandoffEnv::capture();
    let (stop, warm) = spawn_warm_thread();
    let planted = plant_canaries();
    // After Capture, SpawnWarm, Dial and the claim (the harness's walk above).
    let claimed = Observed {
        phase: CLAIMED,
        captured: true,
        threads_spawned: true,
        claim_ok: true,
        env_mut_after_spawn: canaries_moved(&planted),
        ..Observed::default()
    };
    assert!(!claimed.env_mut_after_spawn);
    // `publish`, verbatim in effect.
    aterm_log::env::set(ENV_FDS, "0=9:4000");
    aterm_log::env::set(ENV_READY_FD, "10");
    aterm_log::env::set(ENV_COMMIT_FD, "11");
    let published = Observed {
        env_mut_after_spawn: canaries_moved(&planted),
        ..claimed
    };
    assert!(published.env_mut_after_spawn, "the canaries see the writes");
    let (from, to) = (project(&model, claimed), project(&model, published));
    assert_eq!(
        interp::admits(&model, &from, &to),
        None,
        "no shipping action mutates the environment after a spawn"
    );
    assert_eq!(
        interp::admits(&buggy, &from, &to),
        Some("PublishToEnv"),
        "the retired transport is the model's mutant"
    );
    assert!(!model.check_invariant("NoEnvMutationAfterSpawn", &to));
    let _ = stop.send(());
    warm.join().expect("the warm thread");
}

/// P2's ORDER, BOUND: the real `warm_order` over every socket shape a launched
/// successor can carry is the model's `SpawnWarm` guard — before the dial
/// exactly where the model lets a thread exist before the dial, and never on
/// the relative shape, whose intake `chdir`s. The negative control warms before
/// the dial on that shape anyway; the model admits it only as its `Buggy`
/// mutant, and the walk that follows breaks `NoCwdChangeAfterSpawn` at the
/// `chdir`.
#[test]
fn the_real_warm_order_is_the_models_spawn_guard() {
    let model = native_update_successor_warm_before_claim_model();
    let buggy = interp::with_buggy(&model, 1);
    for explicit in [
        None,
        Some("/tmp/at.x/a.sock"),
        Some("a.sock"),
        Some("./run/a.sock"),
    ] {
        let order = crate::warm_order(true, explicit);
        let relative = explicit.is_some_and(|path| !path.starts_with('/'));
        let captured = project(
            &model,
            Observed {
                phase: CAPTURED_PHASE,
                captured: true,
                relative_socket: relative,
                ..Observed::default()
            },
        );
        assert_eq!(
            model.action_enabled("SpawnWarm", &captured),
            order == crate::WarmOrder::BeforeDial,
            "{explicit:?}: the model's guard and the real decision ({order:?}) agree"
        );
        assert_eq!(
            order == crate::WarmOrder::AfterIntake,
            relative,
            "{explicit:?}: only the relative shape keeps the late order"
        );
        if relative {
            // NEGATIVE CONTROL: warm before the dial on the relative shape.
            let mut run = captured.clone();
            assert!(!model.action_enabled("WarmBeforeDialOnRelativeSocket", &run));
            assert!(buggy.fire("WarmBeforeDialOnRelativeSocket", &mut run));
            for action in ["Dial", "ClaimOk", "TakeOwned", "Adopt", "PrepareSocketDir"] {
                assert!(buggy.fire(action, &mut run), "{action}: {run:?}");
            }
            assert!(
                !model.check_invariant("NoCwdChangeAfterSpawn", &run),
                "the chdir with the warm's threads alive is the caught defect"
            );
        }
    }
    // A cold launch and the fork lane carry no rendezvous: they keep the late
    // order whatever the socket (the model is the launched lane's).
    for explicit in [None, Some("/tmp/at.x/a.sock"), Some("a.sock")] {
        assert_eq!(
            crate::warm_order(false, explicit),
            crate::WarmOrder::AfterIntake
        );
    }
}

/// The phase the model enters at `Capture`.
const CAPTURED_PHASE: i64 = 1;

/// A STALE HINT COSTS A WARM, NEVER A FRAME: the real hint parser, the real
/// build font (`launch_font_for_zoom`, what the prologue builds at), the real
/// launch font (`successor_font_px`, from the carry) and the real miss decision
/// (`warm_miss_px`) decide the model's `Reshape`/`RealPresent` guards — a hint
/// that matches builds at the carried size and presents; a stale or hostile one
/// is re-selected first.
#[cfg(target_os = "macos")]
#[test]
fn a_stale_hint_is_reselected_before_any_carried_present() {
    use crate::handoff_warm_hint::WarmHint;
    let model = native_update_successor_warm_before_claim_model();
    let carry = |zoom: Option<(u32, u32)>| crate::session_store::WindowCarry {
        rows: 30,
        cols: 100,
        outer_x: None,
        outer_y: None,
        status_bar_rows: 0,
        messages: Default::default(),
        next_message_id: Default::default(),
        update_verified_unix_ms: None,
        font_px_milli: zoom.map(|(px, _)| px),
        font_reset_px_milli: zoom.map(|(_, reset)| reset),
        update_health_said: Default::default(),
    };
    // (hint on the wire, the carry the park then writes, a miss expected)
    let cases = [
        ("w1;windows=1;sessions=1;cols=100;rows=30", None, false),
        (
            "w1;windows=1;sessions=1;cols=100;rows=30;px=28000;reset=24000",
            Some((28_000, 24_000)),
            false,
        ),
        // Zoomed during the hold: the hint said unzoomed.
        (
            "w1;windows=1;sessions=1;cols=100;rows=30",
            Some((28_000, 24_000)),
            true,
        ),
        // Unzoomed during the hold: the hint said zoomed.
        (
            "w1;windows=1;sessions=1;cols=100;rows=30;px=28000;reset=24000",
            None,
            true,
        ),
        // Hostile: a size outside the zoom's bounds is no zoom, so no miss.
        (
            "w1;windows=1;sessions=1;cols=100;rows=30;px=999999;reset=24000",
            None,
            false,
        ),
        // Unreadable: warms at the config's size, like no hint.
        ("w9;px=28000", Some((28_000, 24_000)), true),
    ];
    for (wire, carried, miss_expected) in cases {
        let zoom = WarmHint::parse(wire).map_or((None, None), |hint| hint.zoom_milli());
        let built = crate::app_config::launch_font_for_zoom(24.0, false, zoom.0, zoom.1);
        let launch = crate::app_config::successor_font_px(24.0, false, Some(&carry(carried)));
        let miss = crate::warm_miss_px(built.px, launch.px);
        assert_eq!(miss.is_some(), miss_expected, "{wire} against {carried:?}");
        if let Some(px) = miss {
            assert!(
                (px - launch.px).abs() < f32::EPSILON,
                "the join selects the carry's"
            );
        }
        let adopted = project(
            &model,
            Observed {
                phase: 4,
                captured: true,
                threads_spawned: true,
                owned_taken: true,
                claim_ok: true,
                warm_done: true,
                stale_build: miss.is_some(),
                ..Observed::default()
            },
        );
        assert_eq!(
            model.action_enabled("Reshape", &adopted),
            miss.is_some(),
            "{wire}: a miss is exactly what the join re-selects"
        );
        assert_eq!(
            model.action_enabled("RealPresent", &adopted),
            miss.is_none(),
            "{wire}: no carried present at a stale hint's size"
        );
        // After the join's re-selection the build size is the launch size.
        let reshaped = miss.unwrap_or(built.px);
        assert!(crate::warm_miss_px(reshaped, launch.px).is_none());
    }
}

/// THE HINT NEVER REACHES THE PROOF: the real intake, run over a snapshot that
/// carries no hint, a stale one and a hostile one, computes the adoption proof
/// the outgoing process expects, byte for byte, every time. NEGATIVE CONTROL: a
/// hint folded into the layout digest (the mutant `FoldHint`) proves something
/// else — so a check blind to the hint could not pass this.
#[cfg(target_os = "macos")]
#[test]
fn the_hint_never_reaches_the_adoption_proof() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_all();
    let hint_key = crate::handoff_warm_hint::ENV_WARM_HINT;
    for (label, hint) in [
        ("absent", None),
        (
            "stale",
            Some("w1;windows=3;sessions=9;cols=80;rows=24;px=40000;reset=12000"),
        ),
        (
            "hostile",
            Some("w1;windows=4294967295;sessions=0;cols=1;rows=1;px=0;reset=0"),
        ),
    ] {
        let staged = stage_outgoing_handoff(&format!("p2-hint-{label}"), 2, None);
        let (ready_read, ready_write) = pipe_pair("ready");
        let (commit_read, commit_write) = pipe_pair("commit");
        for key in HANDOFF_ENV_KEYS {
            aterm_log::env::unset(key);
        }
        staged.publish_env(ready_write, commit_read, Some(this_build()));
        if let Some(hint) = hint {
            aterm_log::env::set(hint_key, hint);
        }
        let mut env = HandoffEnv::capture();
        assert!(env_is_clear(), "{label}: the hint left the process too");
        let prearmed = prearm_incoming_fds_from(&mut env);
        assert!(!prearmed.rejects_boot(), "{label}");
        // What main_entry does before the dial: the hint leaves the snapshot.
        let taken = crate::handoff_warm_hint::take_from(&mut env);
        assert_eq!(
            taken.is_ok_and(|hint| hint.is_some()),
            hint.is_some(),
            "{label}"
        );
        let incoming = take_incoming_from(&mut env);
        assert_eq!(incoming.adopted.len(), 2, "{label}");
        let target = take_target_identity_from(&mut env).expect("this build is the target");
        let adopted_fds = incoming
            .adopted
            .iter()
            .map(|adopted| adopted.master.raw())
            .collect::<Vec<_>>();
        let identities = incoming
            .adopted
            .iter()
            .map(|item| (item.local_id, item.master.raw(), item.pid))
            .collect::<Vec<_>>();
        // NEGATIVE CONTROL: the same proof with the hint folded into the
        // layout digest (the model's `FoldHint`) is not the parent's.
        if let (Some(hint), Some(layout), Some(screen)) =
            (hint, incoming.layout_digest, incoming.screen_digest)
        {
            let mut folded = layout;
            for (at, byte) in hint.bytes().enumerate() {
                folded[at % folded.len()] ^= byte;
            }
            assert_ne!(
                adoption_proof(
                    incoming.nonce.as_deref().expect("the nonce"),
                    target.build,
                    &target.commit,
                    &folded,
                    &screen,
                    &identities,
                ),
                Some(staged.expected),
                "{label}: a hint folded into the digest proves something else"
            );
        }
        let ready = take_ready_fd_from(
            &mut env,
            incoming.nonce.clone(),
            incoming.layout_digest,
            incoming.screen_digest,
            Some(target),
            &adopted_fds,
        )
        .expect("the readiness channel is admitted");
        assert_eq!(
            ready.proof(&identities),
            Some(staged.expected),
            "{label}: the proof is the parent's expectation whatever the hint said"
        );
        let commit = take_commit_fd_from(
            &mut env,
            incoming.nonce.clone(),
            &adopted_fds,
            Some(ready.raw_fd()),
            prearmed.parent_pid(),
        );
        assert!(commit.is_some(), "{label}");
        drop((ready, commit, incoming));
        for fd in [ready_read, commit_read, commit_write] {
            aterm_pty::close_fd(fd);
        }
        staged.teardown();
    }
    // In the model: the fold is admitted only as the mutant, and breaks
    // `HintNotInProof`.
    let model = native_update_successor_warm_before_claim_model();
    let adopted = Observed {
        phase: 4,
        captured: true,
        threads_spawned: true,
        owned_taken: true,
        claim_ok: true,
        warm_done: true,
        ..Observed::default()
    };
    let from = project(&model, adopted);
    let mut to = from.clone();
    to.insert("hint_in_digest", 1);
    assert_eq!(interp::admits(&model, &from, &to), None);
    assert_eq!(
        interp::admits(&interp::with_buggy(&model, 1), &from, &to),
        Some("FoldHint")
    );
    assert!(!model.check_invariant("HintNotInProof", &to));
}
