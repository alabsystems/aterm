// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::resource_admission::{CompletionCause, Outcome};

#[test]
fn account_terminal_is_bound_distinct_from_legacy_and_keeps_absent_or_observed_zero() {
    let binding = Binding {
        session: "s-abcd".into(),
        attempt: [1; 16],
    };
    let plain = EventFrame {
        binding: binding.clone(),
        event: Event::Finished {
            outcome: Outcome::Signal(9),
            cause: CompletionCause::Cancelled,
            runtime_ns: 55,
        },
    };
    for observation in [
        AccountingObservation::uncollected(),
        AccountingObservation {
            scope: AccountingScope::ChildAndWaitedDescendantsV1,
            cpu_times: Some(CpuTimes {
                user_ns: 0,
                system_ns: 0,
            }),
            peak_rss_bytes: Some(0),
        },
    ] {
        let frame = AccountedEventFrame {
            finished: plain.clone(),
            observation,
        };
        let bytes = frame.encode().unwrap();
        assert_eq!(
            AccountedEventFrame::decode(&bytes, &binding).unwrap(),
            frame
        );
        assert!(EventFrame::decode(&bytes, &binding).is_err());
        assert!(AccountedEventFrame::decode(&plain.encode().unwrap(), &binding).is_err());
        let foreign = Binding {
            attempt: [2; 16],
            ..binding.clone()
        };
        assert!(AccountedEventFrame::decode(&bytes, &foreign).is_err());
        for end in 0..bytes.len() {
            assert!(AccountedEventFrame::decode(&bytes[..end], &binding).is_err());
        }
        let mut extra = bytes;
        extra.push(0);
        assert!(AccountedEventFrame::decode(&extra, &binding).is_err());
    }
}
