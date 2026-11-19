// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A hold's horizon is its time bound or its program backstop; a person's state and progressing
/// work have none, and each says in its words that it holds only while it is true.
#[test]
fn only_a_persons_state_and_progressing_work_have_no_horizon() {
    let minute = Duration::from_secs(60);
    assert_eq!(HoldBound::Within(minute).horizon(), Some(minute));
    assert_eq!(
        HoldBound::Program { backstop: minute }.horizon(),
        Some(minute)
    );
    for person in [
        PersonHold::DeveloperTools,
        PersonHold::SetupDraft,
        PersonHold::SetupChoice,
    ] {
        assert_eq!(HoldBound::PersonNow(person).horizon(), None);
        assert!(person.words().contains("only while"), "{person:?}");
    }
    for work in [WorkHold::Turn, WorkHold::Background] {
        assert_eq!(HoldBound::WorkNow(work).horizon(), None);
        assert!(work.words().contains("only while"), "{work:?}");
    }
}
