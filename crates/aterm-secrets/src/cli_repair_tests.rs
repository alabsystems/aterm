// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn busy_frontier_cannot_starve_service_repairs_or_claim_completion() {
    use crate::{client::HeldMend, service::Job};
    for frontier_busy in [false, true] {
        for service_busy in [false, true] {
            let calls = std::cell::RefCell::new(Vec::new());
            let (rows, complete) = super::unix::repair_plan_with(
                || {
                    calls.borrow_mut().push("frontier");
                    if frontier_busy {
                        Err(crate::Error::of_kind(crate::ErrorKind::TimedOut, "busy"))
                    } else {
                        Ok(false)
                    }
                },
                &mut |job| {
                    calls.borrow_mut().push(match job {
                        Job::Server => "server",
                        Job::Subscriber => "subscriber",
                    });
                    if service_busy && job == Job::Server {
                        Ok(HeldMend::Unread("busy".into()))
                    } else {
                        Ok(HeldMend::NotHeld)
                    }
                },
            );
            assert_eq!(*calls.borrow(), ["server", "subscriber", "frontier"]);
            assert_eq!(complete, !frontier_busy && !service_busy);
            assert_eq!(rows.len(), 3);
            assert_eq!(
                rows[0]["outcome"].as_str(),
                Some(if frontier_busy {
                    "unavailable"
                } else {
                    "unchanged"
                })
            );
        }
    }
}

#[test]
fn a_repair_without_an_installed_release_or_authority_is_incomplete() {
    use crate::client::{HeldKept, HeldMend};
    for reason in [HeldKept::Development, HeldKept::NoAdmin] {
        let (_, complete) = super::unix::repair_with(&mut |_| Ok(HeldMend::Kept(reason.clone())));
        assert!(!complete);
    }
}

#[test]
fn repair_continues_after_one_unavailable_service_and_preserves_holds() {
    use crate::{
        client::{HeldKept, HeldMend},
        service::Job,
    };
    let mut calls = Vec::new();
    let (rows, complete) = super::unix::repair_with(&mut |job| {
        calls.push(job);
        Ok(match job {
            Job::Server => HeldMend::Unread("busy".into()),
            Job::Subscriber => HeldMend::Kept(HeldKept::Pinned),
        })
    });
    assert_eq!(calls, [Job::Server, Job::Subscriber]);
    assert!(!complete);
    assert_eq!(rows[0]["outcome"].as_str(), Some("unavailable"));
    assert_eq!(rows[1]["outcome"].as_str(), Some("kept"));
    assert_eq!(
        super::parse(&["repair".into(), "--json".into()]),
        Ok(super::Command::Repair { json: true })
    );
}
