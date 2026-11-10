// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::native_receipts::testing::RecipientAppend;

fn who(pid: u32) -> Recipient {
    Recipient {
        pid,
        birth: 80,
        group: pid,
        session: 400,
    }
}

#[test]
fn complete_mode_reopens_its_bound_history_without_touching_legacy_history() {
    let world = World::new();
    world.reserve();
    let bound = world.bind().unwrap();
    let legacy = std::fs::read(directory(&world.paths).join("noexec.astream")).unwrap();
    assert!(
        recipients::observe_at(&world.paths, &world.prepare)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        observe_closure_at(&world.paths, &world.prepare).unwrap(),
        Closure::Other
    );
    let first = append_fixture(
        &world.paths,
        &world.prepare,
        &world.binding,
        RecipientAppend::Prepare,
    )
    .unwrap();
    assert_eq!(first.sequence, 1);
    append_fixture(
        &world.paths,
        &world.prepare,
        &world.binding,
        RecipientAppend::Forward(who(701)),
    )
    .unwrap();
    // Interleaved bindings and events share one sequence and authenticated head.
    let mut other = world.prepare.clone();
    other.id = "cc".repeat(16);
    let other_binding = Binding {
        id: other.id.clone(),
        capability: "dd".repeat(32),
    };
    reservation::reserve_at(&world.paths, &other, &other_binding, LIMITS).unwrap();
    binding::bind_at(&world.paths, &other, &other_binding).unwrap();
    let legacy_after_other = std::fs::read(directory(&world.paths).join("noexec.astream")).unwrap();
    append_fixture(
        &world.paths,
        &other,
        &other_binding,
        RecipientAppend::Prepare,
    )
    .unwrap();
    append_fixture(
        &world.paths,
        &other,
        &other_binding,
        RecipientAppend::NoExec,
    )
    .unwrap();
    append_fixture(
        &world.paths,
        &world.prepare,
        &world.binding,
        RecipientAppend::Rollback(who(702)),
    )
    .unwrap();
    let last = append_fixture(
        &world.paths,
        &world.prepare,
        &world.binding,
        RecipientAppend::Seal,
    )
    .unwrap();
    let history = recipients::observe_at(&world.paths, &world.prepare)
        .unwrap()
        .unwrap();
    assert_eq!(history.forward, Some(who(701)));
    assert_eq!(history.rollback, Some(who(702)));
    assert_eq!(history.state, Recorded::Sealed { may_execute: true });
    assert_eq!(history.observed, last);
    assert_eq!(last.sequence, 7);
    assert_eq!(
        observe_closure_at(&world.paths, &world.prepare).unwrap(),
        Closure::Recipients(history)
    );
    assert_eq!(world.bind().unwrap(), bound);
    assert_eq!(
        std::fs::read(directory(&world.paths).join("noexec.astream")).unwrap(),
        legacy_after_other
    );
    assert_ne!(legacy, legacy_after_other);
    assert_eq!(read(&world.paths).unwrap().unwrap().1.sequence, 2);
    let second = recipients::observe_at(&world.paths, &other)
        .unwrap()
        .unwrap();
    assert_eq!(second.state, Recorded::NoExec);
    assert_eq!((second.forward, second.rollback), (None, None));
}

#[test]
fn complete_reads_do_not_fall_back_when_custody_or_history_is_missing() {
    for removed in [
        "enrollment",
        "recipients.astream",
        "recipients.anchor",
        "recipients.checkpoint",
    ] {
        let world = World::new();
        world.reserve();
        append_fixture(
            &world.paths,
            &world.prepare,
            &world.binding,
            RecipientAppend::Prepare,
        )
        .unwrap();
        let path = if removed == "enrollment" {
            world.paths.dir.join(ENROLLMENT)
        } else {
            dir(&world.paths).join(removed)
        };
        std::fs::remove_file(&path).unwrap();
        assert!(
            recipients::observe_at(&world.paths, &world.prepare).is_err(),
            "{removed}"
        );
        assert!(
            observe_closure_at(&world.paths, &world.prepare).is_err(),
            "{removed}"
        );
        assert!(!path.exists());
    }
}

#[test]
fn complete_reads_require_exact_recipe_and_keep_history_independent_of_legacy_loss() {
    let world = World::new();
    world.reserve();
    append_fixture(
        &world.paths,
        &world.prepare,
        &world.binding,
        RecipientAppend::Prepare,
    )
    .unwrap();
    for field in ["deadline", "boot", "epoch", "custody"] {
        let mut changed = world.prepare.clone();
        match field {
            "deadline" => changed.deadline_us += 1,
            "boot" => changed.boot.as_mut().unwrap().boot = "ab".repeat(16),
            "epoch" => changed.host_epoch = "ef".repeat(16),
            _ => changed.custody = Some("ef".repeat(32)),
        }
        let result = recipients::observe_at(&world.paths, &changed);
        assert!(matches!(result, Ok(None) | Err(_)), "{field}");
    }
    let expected = recipients::observe_at(&world.paths, &world.prepare).unwrap();
    std::fs::remove_file(directory(&world.paths).join("noexec.astream")).unwrap();
    assert_eq!(
        recipients::observe_at(&world.paths, &world.prepare).unwrap(),
        expected
    );
    assert!(world.bind().is_err());
    assert!(
        append_fixture(
            &world.paths,
            &world.prepare,
            &world.binding,
            RecipientAppend::NoExec
        )
        .is_err()
    );
}
