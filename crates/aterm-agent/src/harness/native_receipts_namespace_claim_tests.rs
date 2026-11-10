// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::{mode, namespace};
use super::*;

#[derive(Clone, Copy, Debug)]
enum Writer {
    Account,
    Fence,
    Successor,
}

const WRITERS: [Writer; 3] = [Writer::Account, Writer::Fence, Writer::Successor];

fn attempt(writer: Writer, world: &World, candidate: &Prepare, binding: &Binding) -> bool {
    match writer {
        Writer::Account => {
            account::claim_at(&world.paths, world.relation(candidate), binding, LIMITS).is_ok()
        }
        Writer::Fence => account::fence_at(&world.paths, world.relation(candidate), LIMITS).is_ok(),
        Writer::Successor => record_successor_at(
            &world.paths,
            &world.parent,
            &world.binding,
            candidate,
            binding,
            LIMITS,
        )
        .is_ok(),
    }
}

fn marked(world: &World, candidate: &Prepare) -> Prepare {
    let mut marked = candidate.clone();
    mode::complete(&mut marked);
    marked.rejection_domain = Some(rejection_domain_at(&world.paths).unwrap());
    marked
}

#[test]
fn complete_mode_alone_refuses_an_otherwise_valid_account_binding() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let mut world = World::new(phase);
        world.parent = marked(&world, &world.parent);
        let candidate = world.candidate(2);
        let key = existing_key(&world.paths).unwrap();
        assert!(world.parent.account_successor_recipe(&candidate, phase));
        assert!(valid_pair(&world.parent, &world.binding));
        assert!(account::Bound::new(&key, world.relation(&candidate)).is_err());

        // Removing only the protocol mode makes the exact same relation valid.
        // A mismatched command, stamp or account cannot explain the refusal above.
        world.parent.receipt_mode = None;
        let mut unmarked = candidate;
        unmarked.receipt_mode = None;
        assert!(account::Bound::new(&key, world.relation(&unmarked)).is_ok());
    }
}

#[test]
fn account_fence_and_successor_cannot_consume_a_quarantined_candidate() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        for writer in WRITERS {
            // Positive control: the relation is admissible without the reservation.
            let control = World::closed(phase);
            let candidate = control.candidate(2);
            assert!(
                attempt(writer, &control, &candidate, &candidate_binding(&candidate)),
                "{phase:?} {writer:?}"
            );

            for events in [
                vec![Event::Prepare],
                vec![Event::Prepare, Event::MayExecute],
                vec![Event::Prepare, Event::NoExec],
                vec![Event::Prepare, Event::Seal],
                vec![Event::Prepare, Event::MayExecute, Event::Seal],
            ] {
                let world = World::closed(phase);
                let candidate = world.candidate(2);
                let binding = candidate_binding(&candidate);
                let reservation = marked(&world, &candidate);
                for event in events {
                    namespace::append_reservation_fixture(
                        &world.paths,
                        &reservation,
                        &binding,
                        event,
                    );
                }
                let before = world.bytes();
                assert!(
                    !attempt(writer, &world, &candidate, &binding),
                    "{phase:?} {writer:?}"
                );
                assert_eq!(world.bytes(), before);

                // Refusing the reserved ID must not consume the predecessor's
                // successor slot or prevent an unrelated fresh candidate.
                let fresh = world.candidate(3);
                assert!(
                    attempt(writer, &world, &fresh, &candidate_binding(&fresh)),
                    "{phase:?} {writer:?}"
                );
            }
        }
    }
}

#[test]
fn an_existing_account_fence_or_successor_excludes_a_later_quarantine() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        for writer in WRITERS {
            let world = World::closed(phase);
            let candidate = world.candidate(2);
            let binding = candidate_binding(&candidate);
            assert!(attempt(writer, &world, &candidate, &binding));
            let reservation = marked(&world, &candidate);
            let before = world.bytes();
            assert!(
                super::super::super::reservation::reserve_at(
                    &world.paths,
                    &reservation,
                    &binding,
                    LIMITS
                )
                .is_err()
            );
            let key = existing_key(&world.paths).unwrap();
            let (_log, mut view) = open_existing(&world.paths, &key, LIMITS).unwrap();
            let body =
                namespace::reservation_body(&key, &view, &reservation, &binding, Event::Prepare);
            assert!(
                view.ingest_bounded(&key, &body, LIMITS).is_err(),
                "{phase:?} {writer:?}"
            );
            assert_eq!(world.bytes(), before);
        }
    }
}
