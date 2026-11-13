// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::native_act::ClaimOutcome;
use super::super::upgrade::Phase;
use super::*;

fn pending() -> (St, super::super::upgrade_drive::Unsent) {
    let mut st = St {
        phase: Phase::Announced { at_s: 7, asks: 1 },
        ..St::default()
    };
    let back = st.signalled(4242, "", 400, "tab", "exact-forward".into(), 8);
    st.native_prepare = super::retirement_debt_tests::pending().native_prepare;
    st.native_coordination = true;
    st.native = Some(Binding {
        id: "ab".repeat(16),
        capability: "cd".repeat(32),
    });
    (st, back)
}

#[test]
fn positive_safe_cancellation_restores_unsent_constructor_while_unknown_retains_original() {
    let (mut st, back) = pending();
    let recovered = recover_reply(
        &mut st,
        Reply::Decision(Decision::ProvenNotCommittedRollbackSafe),
    );
    assert_eq!(recovered, Ok(Recovery::Cancelled));
    assert_eq!(
        super::super::relaunch::outcome(&format!("wait:{}", refusal_word(recovered))),
        super::super::relaunch::Outcome::Busy
    );
    assert!(st.native_prepare.is_empty() && st.native.is_none());
    // This is the real constructor's unsent transition, not a fresh native admission.
    st.unsent(back);
    assert_eq!(st.phase, Phase::Announced { at_s: 7, asks: 1 });

    let (mut st, _) = pending();
    let original = st.clone();
    assert_eq!(
        recover_reply(&mut st, Reply::Decision(Decision::Unknown)),
        Err("native-unknown")
    );
    assert_eq!(
        st, original,
        "Unknown cannot erase the original id/deadline or restore an unsent mutator"
    );
}

#[test]
fn terminal_completion_keeps_the_actual_phase_but_retirement_and_cancellation_clear_it() {
    for rollback in [false, true] {
        let (mut st, _) = pending();
        st.native_rollback = rollback;
        assert_eq!(
            terminal_reply(&mut st, &Reply::Committed(ClaimOutcome::Builtin)),
            Some(Recovery::Completed)
        );
        assert_eq!(st.native_rollback, rollback);
        assert!(st.native.is_none() && st.native_prepare.is_empty());
        assert_eq!(
            terminal_reply(&mut st, &Reply::Committed(ClaimOutcome::Builtin)),
            Some(Recovery::Completed)
        );
        assert_eq!(
            st.native_rollback, rollback,
            "lost completion ACK preserves the observed phase"
        );
    }
    for decision in [
        Decision::ProvenNotCommittedRollbackSafe,
        Decision::Retired(super::super::native_act::Retirement::Exited),
    ] {
        let (mut st, _) = pending();
        st.native_rollback = true;
        terminal_reply(&mut st, &Reply::Decision(decision));
        assert!(!st.native_rollback);
    }
}

#[test]
fn completed_prepare_or_query_or_abort_receipt_moves_constructor_to_observation_only() {
    for claim in [
        ClaimOutcome::Builtin,
        ClaimOutcome::Lost,
        ClaimOutcome::Bound(9),
    ] {
        let (mut st, _) = pending();
        let recovered = recover_reply(&mut st, Reply::Committed(claim));
        assert_eq!(recovered, Ok(Recovery::Completed));
        assert!(matches!(
            super::super::relaunch::outcome(&format!("wait:{}", refusal_word(recovered))),
            super::super::relaunch::Outcome::NotYet(_)
        ));
        assert!(st.native_prepare.is_empty() && st.native.is_none());
        assert!(
            matches!(st.phase, Phase::Relaunched { .. }),
            "terminal completion never restores an unsent constructor or permits another signal"
        );
        let observed = st.clone();
        assert_eq!(
            terminal_reply(&mut st, &Reply::Committed(claim)),
            Some(Recovery::Completed)
        );
        assert_eq!(
            st, observed,
            "duplicate completion never resets the observed act time"
        );
    }
    let (mut st, _) = pending();
    assert_eq!(
        terminal_reply(
            &mut st,
            &Reply::Decision(Decision::ProvenNotCommittedRollbackSafe)
        ),
        Some(Recovery::Cancelled)
    );
    assert!(
        st.native_prepare.is_empty() && st.native.is_none(),
        "a lost Abort ACK replays its safe cancellation"
    );
}
