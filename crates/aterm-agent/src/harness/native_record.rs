// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Expected native state for cooperating local record writers. This is a read
//! baseline, not host authority, and is never serialized into the record.

use super::native_account_handoff::Pending;
use super::native_act::{Binding, Prepare};
use super::native_observe::SavedObservation;
use super::upgrade::Phase;
use super::upgrade_drive::St;
use std::sync::Mutex;

#[derive(Clone, PartialEq, Eq)]
struct State {
    coordinated: bool,
    native: Option<Binding>,
    prepare: String,
    completed: Option<Box<Prepare>>,
    observation: Option<SavedObservation>,
    rollback: bool,
    retired: bool,
    candidate: Option<Pending>,
    phase: Phase,
    line: String,
    back_line: String,
    account: Option<super::accounts_pool::ExecutionIntent>,
    back_account: Option<super::accounts_pool::ExecutionIntent>,
    pending_since: u64,
    salt: u64,
    from: String,
    to: String,
    source: String,
}

impl State {
    fn of(st: &St) -> Self {
        Self {
            coordinated: super::native_restart::coordinated(st),
            native: st.native.clone(),
            prepare: st.native_prepare.clone(),
            completed: st.native_completed_prepare.clone(),
            observation: st.native_observation.clone(),
            rollback: st.native_rollback,
            retired: st.native_retired_unissued,
            candidate: st.native_account_candidate.clone(),
            phase: st.phase.clone(),
            line: st.line.clone(),
            back_line: st.back_line.clone(),
            account: st.account_intent.clone(),
            back_account: st.back_account_intent.clone(),
            pending_since: st.pending_since,
            salt: st.salt,
            from: st.from.clone(),
            to: st.to.clone(),
            source: st.source.clone(),
        }
    }
}

/// Clones own independent read baselines. A later save on one clone must not
/// silently authorize an older clone to overwrite that save.
#[derive(Default)]
pub(super) struct Baseline {
    expected_native_state: Mutex<Memory>,
}

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    generation: u64,
    write_id: Option<String>,
    state: State,
}

#[derive(Clone, Default)]
struct Memory {
    acknowledged: Option<Stamp>,
    attempted: Option<Stamp>,
}

impl Clone for Baseline {
    fn clone(&self) -> Self {
        Self {
            expected_native_state: Mutex::new(Memory {
                acknowledged: self
                    .expected_native_state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .acknowledged
                    .clone(),
                // A clone may share the read baseline, never the right to retry
                // another writer's unacknowledged publication.
                attempted: None,
            }),
        }
    }
}

// A read baseline is writer bookkeeping, not part of St's application value.
impl PartialEq for Baseline {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Baseline {}

impl std::fmt::Debug for Baseline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeRecordBaseline(redacted)")
    }
}

pub(super) fn acknowledge(st: &St, written: &St) {
    let written = Stamp {
        generation: generation(written),
        write_id: write_id(written),
        state: State::of(written),
    };
    *st.native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Memory {
        acknowledged: Some(written),
        attempted: None,
    };
}

pub(super) fn generation(st: &St) -> u64 {
    st.native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .acknowledged
        .as_ref()
        .map_or(0, |stamp| stamp.generation)
}

pub(super) fn write_id(st: &St) -> Option<String> {
    st.native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .acknowledged
        .as_ref()
        .and_then(|stamp| stamp.write_id.clone())
}

pub(super) fn was_coordinated(st: &St) -> bool {
    let memory = st
        .native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    memory
        .acknowledged
        .iter()
        .chain(memory.attempted.iter())
        .any(|stamp| stamp.state.coordinated)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeRecord",
        action = "Reload",
        project = "aterm_agent::harness::native_account_handoff::tests::record_tests::transition"
    )
)]
pub(super) fn loaded(st: &St, generation: u64, write_id: Option<String>) {
    let state = State::of(st);
    *st.native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Memory {
        acknowledged: Some(Stamp {
            generation,
            write_id,
            state,
        }),
        attempted: None,
    };
}

/// Restore ordinary request metadata after the native path has positively
/// cleared its operation. Keep that path's baseline and coordination history;
/// replaying the pre-admission snapshot would discard both.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeWriteIdentity",
        action = "Retry",
        project = "aterm_agent::harness::native_account_handoff::tests::record_tests::identity::transition"
    )
)]
pub(super) fn restore_inactive(
    opts: &super::upgrade_drive::Opts,
    session: &str,
    current: &mut St,
    prior: Option<&St>,
) -> std::io::Result<()> {
    if current.native.is_some()
        || !current.native_prepare.is_empty()
        || current.native_retired_unissued
        || current.native_account_candidate.is_some()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "native operation remains unresolved",
        ));
    }
    let mut next = prior.cloned().unwrap_or_else(|| {
        let mut closed = current.clone();
        closed.phase = Phase::Done;
        closed.done_at = super::upgrade_drive::now_s();
        closed
    });
    next.agent = current.agent;
    next.native_coordination = super::native_restart::coordinated(current)
        || prior.is_some_and(super::native_restart::coordinated);
    next.native = current.native.clone();
    next.native_prepare.clone_from(&current.native_prepare);
    next.native_completed_prepare = current.native_completed_prepare.clone();
    next.native_observation = current.native_observation.clone();
    next.native_rollback = current.native_rollback;
    next.native_retired_unissued = current.native_retired_unissued;
    next.native_account_candidate = current.native_account_candidate.clone();
    // Transfer this writer's retry ownership, never clone it. On failure the
    // caller keeps its old application value and the exact pending publication.
    next.native_record = std::mem::take(&mut current.native_record);
    let result = super::upgrade_drive::restore_checked(opts, session, &next);
    if result.is_ok() {
        *current = next;
    } else {
        current.native_record = next.native_record;
    }
    result
}

#[cfg(all(test, unix))]
pub(super) fn expected_matches(st: &St, expected: &St) -> bool {
    let expected = State::of(expected);
    st.native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .acknowledged
        .as_ref()
        .is_some_and(|stamp| stamp.state == expected)
}

#[cfg(all(test, unix))]
pub(super) fn seed_attempt(st: &St, attempted: &St, generation: u64, write_id: Option<String>) {
    let state = State::of(attempted);
    st.native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .attempted = Some(Stamp {
        generation,
        write_id,
        state,
    });
}

#[cfg(all(test, unix))]
pub(super) fn attempted_id(st: &St) -> Option<String> {
    st.native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .attempted
        .as_ref()
        .and_then(|stamp| stamp.write_id.clone())
}

#[cfg(all(test, unix))]
pub(super) fn attempted_matches(st: &St, candidate: &St) -> Option<u64> {
    let state = State::of(candidate);
    st.native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .attempted
        .as_ref()
        .filter(|stamp| stamp.state == state)
        .map(|stamp| stamp.generation)
}

/// Called under the record's file lock, before replacing its bytes. An exact
/// replay is allowed after an uncertain rename; it still needs a fresh barrier.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeRecord",
        action = "Write",
        project = "aterm_agent::harness::native_account_handoff::tests::record_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeRecord",
        action = "Advance",
        reason = "An independent writer completes native preparation and admission"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeRecord",
        action = "FailBarrier",
        reason = "The filesystem refuses a directory durability barrier"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeRecord",
        action = "RepairBarrier",
        reason = "The filesystem resumes acknowledging directory durability barriers"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeRecord",
        action = "ABA",
        reason = "An independent actor observes positive cancellation and restores ordinary metadata"
    )
)]
pub(super) fn check(previous: Option<&St>, caller: &St, next: &St) -> std::io::Result<()> {
    let current = previous.map(|previous| Stamp {
        generation: generation(previous),
        write_id: write_id(previous),
        state: State::of(previous),
    });
    let requested = State::of(next);
    let memory = caller
        .native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let native_before = current
        .as_ref()
        .is_some_and(|stamp| stamp.state.coordinated);
    let retry = current.as_ref().is_some_and(|current| {
        current.write_id.is_some()
            && memory.attempted.as_ref() == Some(current)
            && current.state == requested
    });
    if native_before && memory.acknowledged != current && !retry {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "native record changed; reload its current state",
        ));
    }
    let next_generation = if retry {
        current.as_ref().unwrap().generation
    } else if native_before || requested.coordinated {
        current
            .as_ref()
            .map_or(0, |stamp| stamp.generation)
            .checked_add(1)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "native record generation exhausted",
                )
            })?
    } else {
        0
    };
    let next_id = if native_before || requested.coordinated {
        if let Some(attempt) = memory.attempted.as_ref().filter(|attempt| {
            attempt.generation == next_generation
                && attempt.state == requested
                && attempt.write_id.is_some()
        }) {
            attempt.write_id.clone()
        } else {
            Some(super::native_act::operation_id()?)
        }
    } else {
        None
    };
    let attempted = Stamp {
        generation: next_generation,
        write_id: next_id,
        state: requested,
    };
    *next
        .native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Memory {
        acknowledged: Some(attempted.clone()),
        attempted: None,
    };
    caller
        .native_record
        .expected_native_state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .attempted = Some(attempted);
    Ok(())
}

pub(super) fn validate_encoding(st: &St, text: &str) -> std::io::Result<()> {
    let decoded = St::from_json(text).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "native record cannot be decoded",
        )
    })?;
    if generation(st) != generation(&decoded)
        || write_id(st) != write_id(&decoded)
        || State::of(st) != State::of(&decoded)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "native record loses state during encoding",
        ));
    }
    Ok(())
}
