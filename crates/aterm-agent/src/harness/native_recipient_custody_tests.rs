// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::native_act::BootStamp;

#[path = "native_recipient_custody_model_tests.rs"]
mod model;

#[path = "native_recipient_history_tests.rs"]
mod history;

struct World {
    _home: aterm_tempfile::TempDir,
    paths: Paths,
    prepare: Prepare,
    binding: Binding,
}

impl World {
    fn new() -> Self {
        let (home, paths, mut prepare, binding) = super::super::tests::fixture();
        let custody = enroll_at(&paths).unwrap();
        prepare.receipt_mode = Some(ReceiptMode::Recipients5);
        prepare.boot = Some(BootStamp {
            host: format!("macos:{}", "12".repeat(16)),
            boot: "34".repeat(16),
        });
        prepare.custody = Some(custody.stamp().into());
        prepare.rejection_domain = Some(rejection_domain_at(&paths).unwrap());
        Self {
            _home: home,
            paths,
            prepare,
            binding,
        }
    }

    fn reserve(&self) {
        reservation::reserve_at(&self.paths, &self.prepare, &self.binding, LIMITS).unwrap();
    }

    fn bind(&self) -> io::Result<BoundRecipientCustody> {
        binding::bind_at(&self.paths, &self.prepare, &self.binding)
    }

    fn replace_legacy(&self) {
        std::fs::remove_dir_all(directory(&self.paths)).unwrap();
        std::fs::remove_file(self.paths.dir.join(super::super::ENROLLMENT)).unwrap();
        assert_eq!(
            rejection_domain_at(&self.paths).unwrap(),
            self.prepare.rejection_domain.as_deref().unwrap()
        );
        let mut other = self.prepare.clone();
        other.receipt_mode = None;
        other.id = "aa".repeat(16);
        let other_binding = Binding {
            id: other.id.clone(),
            capability: "bb".repeat(32),
        };
        record_event_at(&self.paths, &other, &other_binding, Some(Event::Prepare)).unwrap();
        self.reserve();
    }

    fn pending(&self, journal: bool) {
        let base = base_key(&self.paths, false).unwrap();
        let location = std::fs::canonicalize(&self.paths.dir).unwrap();
        let location = location.as_os_str().as_encoded_bytes();
        let bytes =
            private_file::read_private(&self.paths.dir, ENROLLMENT, ENROLLMENT_BYTES).unwrap();
        let mut enrollment = Enrollment::decode(&bytes, &base, location).unwrap();
        enrollment.active = false;
        aterm_journal::write_private_durable(
            &self.paths.dir,
            ENROLLMENT,
            &enrollment.encode(&base, location),
        )
        .unwrap();
        if !journal {
            std::fs::remove_dir_all(dir(&self.paths)).unwrap();
        }
    }
}

#[test]
fn enrollment_precedes_the_recipe_and_survives_exact_binding_retries() {
    let world = World::new();
    assert_eq!(read(&world.paths).unwrap().unwrap().1.sequence, 0);
    assert_eq!(
        enroll_at(&world.paths).unwrap().stamp(),
        world.prepare.custody.as_deref().unwrap()
    );
    assert!(world.bind().is_err());
    world.reserve();
    let first = world.bind().unwrap();
    assert_eq!(world.bind().unwrap(), first);
    assert_eq!(
        first.custody().stamp(),
        world.prepare.custody.as_deref().unwrap()
    );
    assert_eq!(open_at(&world.paths, false).unwrap().view.sequence, 1);
    assert_eq!(read(&world.paths).unwrap().unwrap().1.sequence, 1);
    assert_eq!(
        observe_closure_at(&world.paths, &world.prepare).unwrap(),
        Closure::Other
    );
}

#[test]
fn pending_enrollment_recovers_without_changing_its_identity() {
    for journal in [false, true] {
        let world = World::new();
        world.pending(journal);
        assert!(open_at(&world.paths, false).is_err());
        let recovered = enroll_at(&world.paths).unwrap();
        assert_eq!(Some(recovered.stamp()), world.prepare.custody.as_deref());
        assert!(
            Enrollment::decode(
                &private_file::read_private(&world.paths.dir, ENROLLMENT, ENROLLMENT_BYTES)
                    .unwrap(),
                &base_key(&world.paths, false).unwrap(),
                std::fs::canonicalize(&world.paths.dir)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes(),
            )
            .unwrap()
            .active
        );
    }
}

#[test]
fn established_partial_loss_never_reenrolls_or_rebinds() {
    for removed in [
        "recipients.astream",
        "recipients.checkpoint",
        "recipients.anchor",
        "enrollment",
        "identity",
    ] {
        let world = World::new();
        world.reserve();
        world.bind().unwrap();
        let path = match removed {
            "enrollment" => world.paths.dir.join(ENROLLMENT),
            "identity" => world.paths.dir.join("identity"),
            name => dir(&world.paths).join(name),
        };
        std::fs::remove_file(&path).unwrap();
        assert!(enroll_at(&world.paths).is_err(), "{removed}");
        assert!(world.bind().is_err(), "{removed}");
        assert!(!path.exists(), "{removed} was recreated");
    }
}

#[test]
fn new_enrollment_and_replaced_legacy_history_do_not_accept_an_old_binding() {
    let world = World::new();
    world.reserve();
    world.bind().unwrap();
    std::fs::remove_dir_all(dir(&world.paths)).unwrap();
    std::fs::remove_file(world.paths.dir.join(ENROLLMENT)).unwrap();
    assert_ne!(
        enroll_at(&world.paths).unwrap().stamp(),
        world.prepare.custody.as_deref().unwrap()
    );
    assert!(world.bind().is_err());

    let world = World::new();
    world.reserve();
    world.bind().unwrap();
    world.replace_legacy();
    assert!(world.bind().is_err());
    assert_eq!(open_at(&world.paths, false).unwrap().view.sequence, 1);
}

#[test]
fn binding_checks_custody_even_when_the_legacy_reservation_matches() {
    let mut world = World::new();
    world.prepare.custody = Some("ff".repeat(32));
    world.reserve();
    assert!(world.bind().is_err());
    assert_eq!(open_at(&world.paths, false).unwrap().view.sequence, 0);
}

#[test]
fn binding_never_initializes_missing_complete_enrollment() {
    let world = World::new();
    world.reserve();
    std::fs::remove_dir_all(dir(&world.paths)).unwrap();
    std::fs::remove_file(world.paths.dir.join(ENROLLMENT)).unwrap();
    assert!(world.bind().is_err());
    assert!(!dir(&world.paths).exists());
    assert!(!world.paths.dir.join(ENROLLMENT).exists());
}

#[test]
fn an_unauthenticated_active_to_pending_change_is_not_recovered() {
    let world = World::new();
    let mut bytes =
        private_file::read_private(&world.paths.dir, ENROLLMENT, ENROLLMENT_BYTES).unwrap();
    bytes[4] = 0;
    aterm_journal::write_private_durable(&world.paths.dir, ENROLLMENT, &bytes).unwrap();
    assert!(enroll_at(&world.paths).is_err());
    assert_eq!(
        private_file::read_private(&world.paths.dir, ENROLLMENT, ENROLLMENT_BYTES).unwrap(),
        bytes
    );
}

#[test]
fn wrong_recipe_or_binding_and_foreign_custody_leave_no_new_record() {
    let world = World::new();
    world.reserve();
    world.bind().unwrap();
    let before = std::fs::read(dir(&world.paths).join("recipients.astream")).unwrap();
    let mut wrong = world.prepare.clone();
    wrong.custody = Some("ff".repeat(32));
    assert!(binding::bind_at(&world.paths, &wrong, &world.binding).is_err());
    wrong = world.prepare.clone();
    wrong.deadline_us += 1;
    assert!(binding::bind_at(&world.paths, &wrong, &world.binding).is_err());
    let mut wrong_binding = world.binding.clone();
    wrong_binding.capability = "ee".repeat(32);
    assert!(binding::bind_at(&world.paths, &world.prepare, &wrong_binding).is_err());
    assert_eq!(
        std::fs::read(dir(&world.paths).join("recipients.astream")).unwrap(),
        before
    );

    let (_home, copied, _, _) = super::super::tests::fixture();
    aterm_secrets::client::machine_identity(&copied).unwrap();
    std::fs::copy(
        world.paths.dir.join("identity"),
        copied.dir.join("identity"),
    )
    .unwrap();
    std::fs::copy(
        world.paths.dir.join(ENROLLMENT),
        copied.dir.join(ENROLLMENT),
    )
    .unwrap();
    assert!(enroll_at(&copied).is_err());
    assert!(!dir(&copied).exists());
}
