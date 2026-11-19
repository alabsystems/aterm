// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_fixture_hang_refuses_even_an_answer_collected_before_it() {
    let recorded = || {
        Some(Attested::Recorded {
            version: "fixture".into(),
        })
    };
    assert_eq!(answer_after_wait(Ok(()), recorded()), recorded());
    for failure in [Failure::Stalled, Failure::Guard, Failure::Output] {
        assert!(matches!(
            answer_after_wait(Err(failure), recorded()),
            Some(Attested::Failed(_))
        ));
    }
    assert!(matches!(
        answer_after_wait(Err(Failure::Stalled), None),
        Some(Attested::Failed(_))
    ));
}

#[test]
fn the_progress_policy_is_thread_scoped_and_restored_after_unwind() {
    assert!(!Wait::new().enabled);
    assert!(
        std::panic::catch_unwind(|| {
            let _scope = while_working();
            assert!(Wait::new().enabled);
            std::thread::scope(|threads| {
                threads
                    .spawn(|| assert!(!Wait::new().enabled))
                    .join()
                    .unwrap();
            });
            panic!("fixture scope unwinds");
        })
        .is_err()
    );
    assert!(!Wait::new().enabled);
}
