// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn attempt(counter: u64) -> Attempt {
    Attempt {
        sequence: PreloadSequence::parse(&format!("0123456789abcdef{counter:016x}")).unwrap(),
        operation: format!("{counter:032x}"),
        origin: Origin::Ordinary,
    }
}
fn retirement(attempt: &Attempt) -> Retirement {
    Retirement {
        operation: "c".repeat(32),
        sequence: attempt.sequence,
        preload: attempt.operation.clone(),
        registration: None,
    }
}

#[test]
fn preload_sequence_rejects_zero_counter_uppercase_and_changed_accepted_body() {
    assert!(PreloadSequence::parse(&"0".repeat(32)).is_none());
    assert!(PreloadSequence::parse(&"A".repeat(32)).is_none());
    let process = Process::observe(std::process::id()).unwrap();
    let first = attempt(1);
    assert_eq!(first.sequence.encoded(), "0123456789abcdef0000000000000001");
    let ledger = Ledger::new(process.clone(), first.clone());
    assert!(ledger.check_attempt(&process, &first).is_ok());
    let mut changed = first.clone();
    changed.operation = "f".repeat(32);
    assert!(matches!(
        ledger.check_attempt(&process, &changed),
        Err(Error::Stale)
    ));
    changed = first;
    changed.origin = Origin::Target {
        assignment: "a".repeat(32),
        context: "b".repeat(64),
    };
    assert!(matches!(
        ledger.check_attempt(&process, &changed),
        Err(Error::Stale)
    ));
}

#[test]
fn higher_attempt_waits_for_ready_and_old_unknown_cannot_close_newer_attempt() {
    let process = Process::observe(std::process::id()).unwrap();
    let first = attempt(1);
    let second = attempt(2);
    let mut ledger = Ledger::new(process.clone(), first.clone());
    assert!(matches!(
        ledger.accept(&process, second.clone()),
        Err(Error::Busy)
    ));
    ledger
        .loaded(&first, &"a".repeat(32), 1, &"b".repeat(64))
        .unwrap();
    assert!(matches!(
        ledger.accept(&process, second.clone()),
        Err(Error::Busy)
    ));
    ledger.complete(&first, &"a".repeat(32)).unwrap();
    ledger.accept(&process, second.clone()).unwrap();
    assert!(matches!(
        ledger.fence(&process, retirement(&first)),
        Err(Error::Stale)
    ));
    let mut foreign = retirement(&second);
    foreign.sequence = PreloadSequence::parse("1123456789abcdef0000000000000003").unwrap();
    assert!(matches!(ledger.fence(&process, foreign), Err(Error::Stale)));
    assert!(ledger.check_attempt(&process, &second).is_ok());
}

#[test]
fn exact_higher_unsent_fence_is_not_closed_until_actual_outer_retirement() {
    let process = Process::observe(std::process::id()).unwrap();
    let first = attempt(1);
    let higher = attempt(2);
    let mut ledger = Ledger::new(process.clone(), first.clone());
    ledger
        .loaded(&first, &"a".repeat(32), 1, &"b".repeat(64))
        .unwrap();
    let mut invalid = retirement(&higher);
    invalid.registration = Some("a".repeat(32));
    assert!(matches!(ledger.fence(&process, invalid), Err(Error::Stale)));
    let fence = retirement(&higher);
    ledger.fence(&process, fence.clone()).unwrap();
    assert!(!ledger.exact_closed(&process, &fence));
    assert!(matches!(
        ledger.complete(&first, &"a".repeat(32)),
        Err(Error::Stale)
    ));
    assert!(matches!(
        ledger.check_attempt(&process, &higher),
        Err(Error::Stale)
    ));
    ledger.close(&process, &fence).unwrap();
    assert!(ledger.exact_closed(&process, &fence));
    let mut changed = fence;
    changed.preload = "f".repeat(32);
    assert!(!ledger.exact_closed(&process, &changed));
}
#[cfg(unix)]
fn snapshot(generation: u64, registration: char) -> Preload {
    // Metadata fixture only; no owner/source/Scope constructor authority.
    use crate::harness::codex_session_slot::metadata_fixture;
    Preload {
        member: "1".repeat(32),
        operation: "a".repeat(32),
        registration: registration.to_string().repeat(32),
        source: Arc::new(metadata_fixture::source(
            metadata_fixture::binding("resample"),
            std::path::PathBuf::from("/synthetic-never-read/auth.json"),
            generation,
        )),
    }
}
#[cfg(unix)]
fn predecessor(value: &Preload) -> PreloadPredecessor {
    PreloadPredecessor {
        registration: value.registration.clone(),
        credential_generation: value.source.credential_generation(),
        digest: value.source.digest().into(),
    }
}

#[test]
#[cfg(unix)]
fn resample_replays_immutable_steps_and_preserves_initial_snapshot() {
    let process = Process::observe(std::process::id()).unwrap();
    let attempt = attempt(1);
    let mut ledger = Ledger::new(process, attempt.clone());
    let first = snapshot(1, 'a');
    ledger
        .loaded(&attempt, &first.registration, 1, first.source.digest())
        .unwrap();
    ledger.record_initial(&attempt, first.clone()).unwrap();
    let second = snapshot(2, 'b');
    let first_step = predecessor(&first);
    assert!(matches!(
        ledger.begin_resample(&attempt, &first_step),
        Ok(ResampleAdmission::Read)
    ));
    assert!(
        ledger
            .accept_ready(&attempt, &first.registration, ReadyRole::ReaderLoaded)
            .is_err()
    );
    ledger
        .finish_resample(&attempt, &first_step, second.clone())
        .unwrap();
    let third = snapshot(3, 'c');
    let second_step = predecessor(&second);
    ledger.begin_resample(&attempt, &second_step).unwrap();
    ledger
        .finish_resample(&attempt, &second_step, third)
        .unwrap();
    match ledger.resample_status(&attempt, &first_step).unwrap() {
        ResampleAdmission::Replay(value) => {
            assert_eq!(value.registration, second.registration);
            assert_eq!(value.source.credential_generation(), 2);
        }
        _ => panic!("exact step lost its recorded result"),
    }
    assert_eq!(
        ledger.initial_preload(&attempt).unwrap().registration,
        first.registration
    );
}

#[test]
#[cfg(unix)]
fn ready_committed_is_exact_correlation_and_changed_report_is_rejected() {
    let process = Process::observe(std::process::id()).unwrap();
    let attempt = attempt(1);
    let mut ledger = Ledger::new(process, attempt.clone());
    let value = snapshot(1, 'a');
    ledger
        .loaded(&attempt, &value.registration, 1, value.source.digest())
        .unwrap();
    ledger
        .accept_ready(&attempt, &value.registration, ReadyRole::ReaderLoaded)
        .unwrap();
    assert!(
        ledger
            .accept_ready(&attempt, &value.registration, ReadyRole::PolicyUnavailable)
            .is_err()
    );
    ledger.complete(&attempt, &value.registration).unwrap();
    let previous = predecessor(&value);
    assert!(matches!(
        ledger.resample_status(&attempt, &previous),
        Ok(ResampleAdmission::ReadyCommitted)
    ));
    let mut foreign = previous.clone();
    foreign.registration = "b".repeat(32);
    assert!(ledger.resample_status(&attempt, &foreign).is_err());
    foreign = previous;
    foreign.digest = "f".repeat(64);
    assert!(ledger.resample_status(&attempt, &foreign).is_err());
}

#[test]
#[cfg(unix)]
fn bounded_resample_receipts_never_reexecute_evicted_predecessors() {
    let process = Process::observe(std::process::id()).unwrap();
    let attempt = attempt(1);
    let mut ledger = Ledger::new(process, attempt.clone());
    let first = snapshot(1, 'a');
    ledger
        .loaded(&attempt, &first.registration, 1, first.source.digest())
        .unwrap();
    let old = predecessor(&first);
    let mut current = first;
    for generation in 2..14 {
        let previous = predecessor(&current);
        ledger.begin_resample(&attempt, &previous).unwrap();
        let mut next = snapshot(generation, 'b');
        next.registration = format!("{generation:032x}");
        ledger
            .finish_resample(&attempt, &previous, next.clone())
            .unwrap();
        current = next;
    }
    assert_eq!(ledger.resampled.len(), 8);
    assert!(matches!(
        ledger.begin_resample(&attempt, &old),
        Err(Error::Stale)
    ));
    assert!(matches!(
        ledger.begin_resample(&attempt, &predecessor(&current)),
        Ok(ResampleAdmission::Read)
    ));
}
