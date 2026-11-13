// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn expired_preparations_report_each_unanswered_address() {
    let ats = vec!["one.invalid:1".into(), "two.invalid:2".into()];
    let prepare =
        |_: &str| -> Result<Box<dyn Preparation>> { Err(Error::new("transport unavailable")) };
    let got = with_attempts(
        Attempts::Prepared {
            prepare: &prepare,
            until: std::time::Instant::now(),
        },
        &ats,
        &|| Ok(()),
        |next| {
            let mut got = Vec::new();
            while let Some((address, result)) = next() {
                assert!(result.is_err());
                got.push(address);
            }
            got
        },
    );
    assert_eq!(got, ats, "deadline must not erase failed member attempts");
}

/// One proof of this Mac at a time: a second take sees the first and sends nothing, for one
/// try and for a wait that ends while the first still holds it, and takes the lock once the
/// first let go.
///
/// NEGATIVE CONTROL: with `ProofLock::take` returning `Ok(Some(..))` without locking, the
/// second take holds a lock beside the first and the test fails at "a second proof".
#[test]
fn a_second_proof_waits_for_the_first_and_sends_nothing_while_it_runs() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let first = ProofLock::take(&paths, None).unwrap();
    assert!(first.is_some(), "the first proof takes the lock");
    assert!(
        ProofLock::take(&paths, None).unwrap().is_none(),
        "a second proof while the first runs"
    );
    let until = std::time::Instant::now() + std::time::Duration::from_millis(20);
    assert!(
        ProofLock::take(&paths, Some(until)).unwrap().is_none(),
        "a second proof whose wait ended while the first runs"
    );
    drop(first);
    assert!(
        ProofLock::take(&paths, None).unwrap().is_some(),
        "a proof after the first let go"
    );
}
