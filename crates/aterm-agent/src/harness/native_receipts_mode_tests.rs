// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::native_act::{BootStamp, ReceiptMode};

pub(super) fn complete(prepare: &mut Prepare) {
    prepare.receipt_mode = Some(ReceiptMode::Recipients5);
    prepare.boot = Some(BootStamp {
        host: format!("macos:{}", "12".repeat(16)),
        boot: "34".repeat(16),
    });
    prepare.custody = Some("56".repeat(32));
    prepare.rejection_domain = Some("78".repeat(32));
    assert!(Request::prepare(prepare.clone()).encode().is_some());
}

pub(super) fn enrolled(paths: &Paths, prepare: &mut Prepare) {
    complete(prepare);
    prepare.custody = Some(recipient_custody::enroll_at(paths).unwrap().stamp().into());
    prepare.rejection_domain = Some(rejection_domain_at(paths).unwrap());
}

#[test]
fn complete_mode_cannot_enroll_or_write_through_a_legacy_api() {
    let (_home, paths, mut prepare, binding) = fixture();
    complete(&mut prepare);
    for event in [
        None,
        Some(Event::Prepare),
        Some(Event::MayExecute),
        Some(Event::NoExec),
        Some(Event::Seal),
    ] {
        assert!(record_event_at(&paths, &prepare, &binding, event).is_err());
    }
    assert!(reject_unadmitted_at(&paths, &prepare, LIMITS).is_err());
    let mut child = prepare.clone();
    child.id = "9a".repeat(16);
    let child_binding = Binding {
        id: child.id.clone(),
        capability: binding.capability.clone(),
    };
    assert!(
        record_successor_at(&paths, &prepare, &binding, &child, &child_binding, LIMITS).is_err()
    );
    assert!(
        !paths.dir.exists(),
        "refusal creates no identity or journal"
    );
}

#[test]
fn receipt_mode_changes_the_digest_and_legacy_records_cannot_satisfy_it() {
    let (_home, paths, mut prepare, binding) = fixture();
    enrolled(&paths, &mut prepare);
    let key = write_key(&paths).unwrap();
    let strict_digest = key.prepare(&prepare).unwrap();
    let mut unmarked = prepare.clone();
    unmarked.receipt_mode = None;
    assert_ne!(strict_digest, key.prepare(&unmarked).unwrap());
    assert!(key.legacy_prepare(&prepare).is_none());

    // Negative control: bypass the guarded writers and forge a legacy-format history
    // carrying the complete recipe's digest. Its format still supplies no complete proof.
    let receipt = receipt(&key, &prepare, &binding).unwrap();
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    for event in [Event::Prepare, Event::NoExec] {
        let (mut log, view) = open(&paths, &key, LIMITS).unwrap();
        log.append(
            ISSUER_SUBJECT,
            view.encode_event(&key, &receipt, epoch, event, 1),
        )
        .unwrap();
    }
    assert_eq!(
        observe_closure_at(&paths, &prepare).unwrap(),
        Closure::Other
    );
    assert!(observe_prepare_at(&paths, &prepare).unwrap().is_none());
    assert!(
        observe_phase_at(&paths, &prepare, &binding)
            .unwrap()
            .is_none()
    );
    assert!(contains_operation_at(&paths, &prepare.id).unwrap());
}

#[test]
fn marked_complete_history_stays_readable_but_cannot_be_reinterpreted_as_unmarked() {
    let (_home, paths, mut prepare, binding) = fixture();
    enrolled(&paths, &mut prepare);
    reservation::reserve_at(&paths, &prepare, &binding, LIMITS).unwrap();
    recipients::append_fixture(
        &paths,
        &prepare,
        &binding,
        testing::RecipientAppend::Prepare,
    )
    .unwrap();
    recipients::append_fixture(&paths, &prepare, &binding, testing::RecipientAppend::NoExec)
        .unwrap();
    assert!(
        matches!(observe_closure_at(&paths, &prepare).unwrap(), Closure::Recipients(history) if history.state == Recorded::NoExec)
    );
    prepare.receipt_mode = None;
    assert_eq!(
        observe_closure_at(&paths, &prepare).unwrap(),
        Closure::Other
    );
}
