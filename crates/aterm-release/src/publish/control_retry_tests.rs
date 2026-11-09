// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::Mutex;

struct Fake {
    refs: Mutex<(Option<String>, Option<String>, usize)>,
    failure: usize,
    race: bool,
}

impl Origin for Fake {
    fn refresh_main(&self) -> std::result::Result<String, String> {
        unreachable!()
    }
    fn get(&self, reference: &str) -> std::result::Result<Option<String>, String> {
        let refs = self.refs.lock().unwrap();
        Ok(if reference == RELEASE_LEASE_REF {
            refs.0.clone()
        } else {
            assert_eq!(reference, PUBLISHER_FENCE_REF);
            refs.1.clone()
        })
    }
    fn import(&self, _: &str) -> std::result::Result<(), String> {
        unreachable!()
    }
    fn peel(&self, _: &str) -> std::result::Result<Option<String>, String> {
        unreachable!()
    }
    fn tag_object(&self, _: &str) -> std::result::Result<Vec<u8>, String> {
        unreachable!()
    }
    fn write(&self, _: &str, _: Option<&str>, _: &str) -> std::result::Result<RefWrite, String> {
        unreachable!()
    }
    fn delete(&self, _: &str, _: &str) -> std::result::Result<RefWrite, String> {
        unreachable!()
    }
    fn control_transaction(
        &self,
        transaction: ControlRefTransaction,
    ) -> std::result::Result<RefWrite, String> {
        let mut refs = self.refs.lock().unwrap();
        refs.2 += 1;
        if self.race {
            refs.0 = Some("c".repeat(40));
            return Ok(if self.failure == 0 {
                RefWrite::Conflict
            } else {
                RefWrite::Transient("receive failed while another owner took the lease".into())
            });
        }
        if refs.2 <= self.failure {
            return Ok(RefWrite::Transient(
                "GitHub receive side failed: [remote rejected] (transaction failed to start)"
                    .into(),
            ));
        }
        if transaction.changes().iter().any(|change| {
            let slot = if change.reference == RELEASE_LEASE_REF {
                &refs.0
            } else {
                &refs.1
            };
            *slot != change.before
        }) {
            return Ok(RefWrite::Conflict);
        }
        for change in transaction.changes() {
            let slot = if change.reference == RELEASE_LEASE_REF {
                &mut refs.0
            } else {
                &mut refs.1
            };
            *slot = change.after.clone();
        }
        Ok(RefWrite::Written)
    }
}

fn fixture(failure: usize, race: bool) -> (Fake, PublisherFenceGuard) {
    let guard = PublisherFenceGuard {
        owner: "a".repeat(40),
        token: "b".repeat(40),
    };
    let fake = Fake {
        refs: Mutex::new((Some(guard.owner.clone()), Some(guard.token.clone()), 0)),
        failure,
        race,
    };
    (fake, guard)
}

#[test]
fn transient_unlock_rechecks_then_frees_both_refs() {
    let (fake, guard) = fixture(1, false);
    assert_eq!(
        release_completed_publisher_session(&fake, &guard.owner, &guard).unwrap(),
        LeaseRelease::Released
    );
    assert_eq!(*fake.refs.lock().unwrap(), (None, None, 2));
}

#[test]
fn retry_records_cause_and_is_bounded() {
    for failures in [1, usize::MAX] {
        let (fake, guard) = fixture(failures, false);
        let transaction = ControlRefTransaction::new(
            &guard.owner,
            vec![
                ControlRefChange {
                    reference: RELEASE_LEASE_REF.into(),
                    before: Some(guard.owner.clone()),
                    after: None,
                },
                ControlRefChange {
                    reference: PUBLISHER_FENCE_REF.into(),
                    before: Some(guard.token.clone()),
                    after: None,
                },
            ],
        )
        .unwrap();
        let mut log = Vec::new();
        let result =
            write_control_transaction(&fake, transaction, &mut |line| log.push(line.to_owned()));
        assert!(log[0].contains("transaction failed to start"));
        assert!(log[0].contains("re-read confirmed expected refs"));
        if failures == 1 {
            assert_eq!(result.unwrap(), RefWrite::Written);
            assert_eq!(fake.refs.lock().unwrap().2, 2);
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("exhausted 3 attempts")
            );
            assert_eq!(fake.refs.lock().unwrap().2, 3);
        }
    }
}

#[test]
fn changed_lease_remains_a_conflict_without_retry() {
    for failure in [0, 1] {
        let (fake, guard) = fixture(failure, true);
        let error = release_completed_publisher_session(&fake, &guard.owner, &guard).unwrap_err();
        assert!(error.to_string().contains("conflicted"));
        assert_eq!(fake.refs.lock().unwrap().2, 1);
        assert_eq!(fake.refs.lock().unwrap().0, Some("c".repeat(40)));
    }
}

#[test]
fn claim_and_fence_transitions_use_the_same_recovery() {
    for reference in [RELEASE_LEASE_REF, PUBLISHER_FENCE_REF] {
        let (fake, guard) = fixture(1, false);
        let after = if reference == RELEASE_LEASE_REF {
            &guard.owner
        } else {
            &guard.token
        };
        {
            let mut refs = fake.refs.lock().unwrap();
            refs.1 = None;
            if reference == RELEASE_LEASE_REF {
                refs.0 = None;
            }
        }
        assert_eq!(
            write_control_refs(
                &fake,
                &guard.owner,
                vec![ControlRefChange {
                    reference: reference.into(),
                    before: None,
                    after: Some(after.clone()),
                }]
            )
            .unwrap(),
            RefWrite::Written
        );
        assert_eq!(fake.refs.lock().unwrap().2, 2);
    }
}
