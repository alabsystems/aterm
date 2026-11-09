// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mutate real original inputs at the hot Inventory -> History boundary. The
//! hook supplies no protocol data or closure; the ordinary publisher must judge
//! every input itself and preserve every changed/unknown named entry.

use super::*;
use std::cell::RefCell;

#[derive(Clone, Copy, Debug)]
enum Fault {
    Observe,
    ReplaceMember,
    RestoreMemberName,
    RestorePayload,
    Unseal,
    ReplaceDirectory,
    RestoreDirectoryName,
}
struct Plan {
    fault: Fault,
    clock: File,
}
struct Observed {
    relative: std::path::PathBuf,
    file: Arc<File>,
    body: String,
    stamp: Stamp,
}
thread_local! {
    static PLAN: RefCell<Option<Plan>> = const { RefCell::new(None) };
    static OBSERVED: RefCell<Option<Observed>> = const { RefCell::new(None) };
}

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        PLAN.with(|slot| {
            slot.borrow_mut().take();
        });
        OBSERVED.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

fn writable(parent: &File, name: &str) -> io::Result<File> {
    aterm_uds::kept_out(|| {
        fd::openat(
            parent,
            name,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    })
}

fn create_member(parent: &File, name: &str, body: &str) -> io::Result<()> {
    let mut file = open(parent, name, true, false)?;
    file.write_all(body.as_bytes())?;
    fd::fchmod(&file, Mode::RUSR)?;
    Ok(())
}

pub(super) fn before_hot_inspect(inventory: &Inventory) -> io::Result<()> {
    let Some(plan) = PLAN.with(|slot| slot.borrow_mut().take()) else {
        return Ok(());
    };
    let directory = inventory
        .directories
        .first()
        .expect("actual original epoch");
    let member = inventory
        .files
        .iter()
        .find(|entry| entry.name == "base.json" && Arc::ptr_eq(&entry.parent, &directory.file))
        .expect("actual original immutable base");
    let (file, body, stamp) = inventory.share_member(&directory.file, &member.name, true)?;
    assert_eq!(stamp, member.stamp);
    assert_eq!(body, bytes(&member.file, MAX_ENTRY)?.0);
    assert_eq!(file.metadata()?.mode() & 0o7777, 0o400);
    let relative = std::path::PathBuf::from(&directory.name).join(&member.name);
    if !matches!(plan.fault, Fault::Observe) {
        // The clock witness is outside every retained archive input. It must
        // not itself change the parent's origin and produce a false positive.
        let victim = if matches!(
            plan.fault,
            Fault::ReplaceDirectory | Fault::RestoreDirectoryName
        ) {
            &directory.file
        } else {
            &file
        };
        super::super::past_the_clock_tick(&plan.clock, victim);
    }
    match plan.fault {
        Fault::Observe => {}
        Fault::ReplaceMember | Fault::RestoreMemberName => {
            fd::renameat_with(
                directory.file.as_ref(),
                member.name.as_str(),
                directory.file.as_ref(),
                ".original-shared-member",
                RenameFlags::NOREPLACE,
            )?;
            create_member(&directory.file, &member.name, &body)?;
            if matches!(plan.fault, Fault::RestoreMemberName) {
                fd::unlinkat(
                    directory.file.as_ref(),
                    member.name.as_str(),
                    fd::AtFlags::empty(),
                )?;
                fd::renameat_with(
                    directory.file.as_ref(),
                    ".original-shared-member",
                    directory.file.as_ref(),
                    member.name.as_str(),
                    RenameFlags::NOREPLACE,
                )?;
                assert_eq!(Stamp::read(&file)?.0[..2], stamp.0[..2]);
                assert_ne!(Stamp::read(&file)?, stamp);
                assert_eq!(bytes(&file, MAX_ENTRY)?.0, body);
            } else {
                assert_ne!(
                    Stamp::read(&open(&directory.file, &member.name, false, false)?)?.0[..2],
                    stamp.0[..2]
                );
            }
            assert!(
                inventory
                    .share_member(&directory.file, &member.name, true)
                    .is_err()
            );
        }
        Fault::RestorePayload | Fault::Unseal => {
            fd::fchmod(&file, Mode::RUSR | Mode::WUSR)?;
            if matches!(plan.fault, Fault::RestorePayload) {
                let writer = writable(&directory.file, &member.name)?;
                writer.write_all_at(b"!", 0)?;
                writer.write_all_at(body.as_bytes(), 0)?;
                fd::fchmod(&file, Mode::RUSR)?;
                assert_eq!(bytes(&file, MAX_ENTRY)?.0, body);
            }
            assert_ne!(Stamp::read(&file)?, stamp);
            assert!(
                inventory
                    .share_member(&directory.file, &member.name, true)
                    .is_err()
            );
        }
        Fault::ReplaceDirectory | Fault::RestoreDirectoryName => {
            fd::renameat_with(
                inventory.root.as_ref(),
                directory.name.as_str(),
                inventory.root.as_ref(),
                ".original-shared-epoch",
                RenameFlags::NOREPLACE,
            )?;
            fd::mkdirat(inventory.root.as_ref(), directory.name.as_str(), Mode::RWXU)?;
            if matches!(plan.fault, Fault::RestoreDirectoryName) {
                fd::unlinkat(
                    inventory.root.as_ref(),
                    directory.name.as_str(),
                    fd::AtFlags::REMOVEDIR,
                )?;
                fd::renameat_with(
                    inventory.root.as_ref(),
                    ".original-shared-epoch",
                    inventory.root.as_ref(),
                    directory.name.as_str(),
                    RenameFlags::NOREPLACE,
                )?;
            }
            let named = open(&inventory.root, &directory.name, false, true)?;
            assert!(
                inventory
                    .share_directory(&inventory.root, &directory.name, &named)
                    .is_err()
            );
        }
    }
    OBSERVED.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Observed {
            relative,
            file,
            body,
            stamp,
        });
    });
    Ok(())
}

fn install(fault: Fault, namespace: &Namespace) -> Reset {
    PLAN.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Plan {
            fault,
            clock: namespace.open_backend().unwrap(),
        });
    });
    Reset
}

fn terminal(
    fixture: &crate::ledger::CohortFixture,
) -> (crate::ledger::Laid, Transaction, CohortClosureRef) {
    let (mut record, mut laid) = fixture
        .ledger
        .lay(&crate::Need::new("hot shared archive"), 0)
        .unwrap();
    fixture
        .ledger
        .admit(
            &mut laid,
            &mut record,
            crate::Held {
                admitted_ms: 1,
                cores: 1,
                alone: false,
            },
        )
        .unwrap();
    let reference = laid.cohort_closure_ref().unwrap();
    laid.finish_cohort().unwrap();
    let custody = laid.custody.as_ref().unwrap();
    assert_eq!(custody.journal.lifetime(), crate::cohort::Lifetime::Drained);
    let mut transaction = laid.transaction().unwrap();
    let name = laid.path().file_name().unwrap().to_str().unwrap();
    let text = transaction
        .observe(&custody.directory, &custody.writer, name)
        .unwrap();
    let capture = transaction
        .capture(&custody.directory, &custody.writer, name, &text, true)
        .unwrap();
    transaction
        .commit(&custody.directory, &custody.writer, &capture)
        .unwrap();
    (laid, transaction, reference)
}

#[test]
fn hot_history_keeps_original_member_bytes_through_real_compaction_unlink() {
    let fixture = crate::ledger::CohortFixture::new();
    let (laid, transaction, reference) = terminal(&fixture);
    let _reset = install(Fault::Observe, &fixture.namespace);
    transaction.archive(&fixture.namespace, &laid.lock).unwrap();
    let original = OBSERVED
        .with(|slot| slot.borrow_mut().take())
        .expect("actual hot reader entered");
    assert_eq!(original.file.metadata().unwrap().nlink(), 0);
    let mut actual = vec![0; original.body.len()];
    original.file.read_exact_at(&mut actual, 0).unwrap();
    assert_eq!(actual, original.body.as_bytes());
    assert_eq!(
        original
            .file
            .read_at(&mut [0], original.body.len() as u64)
            .unwrap(),
        0
    );
    assert!(
        !fixture
            .namespace
            .retired()
            .join(reference.key())
            .join(HISTORY)
            .exists()
    );
    assert_eq!(
        fixture.ledger.cohort_claim_state(&reference).unwrap(),
        CohortClaimState::Closed
    );
}

#[test]
fn hot_history_refuses_real_member_and_directory_replacement_restoration_or_drift() {
    for fault in [
        Fault::ReplaceMember,
        Fault::RestoreMemberName,
        Fault::RestorePayload,
        Fault::Unseal,
        Fault::ReplaceDirectory,
        Fault::RestoreDirectoryName,
    ] {
        let fixture = crate::ledger::CohortFixture::new();
        let (laid, transaction, reference) = terminal(&fixture);
        let _reset = install(fault, &fixture.namespace);
        assert!(
            transaction.archive(&fixture.namespace, &laid.lock).is_err(),
            "{fault:?}"
        );
        let original = OBSERVED
            .with(|slot| slot.borrow_mut().take())
            .expect("actual hot sharing seam entered");
        let archive = fixture.namespace.retired().join(reference.key());
        assert!(archive.join(HISTORY).is_dir());
        assert!(!archive.join(COMPACTED).exists());
        assert!(archive.join(RECEIPT).is_file());
        if matches!(fault, Fault::RestoreDirectoryName) {
            // The hot reader retained directory_origin and refused the ABA.
            // Cold receipts bind directory name/physical identity, not its
            // historical ctime; the same restored directory and unchanged
            // original member stamps/payloads may validate on a fresh read.
            assert_eq!(
                fixture.ledger.cohort_claim_state(&reference).unwrap(),
                CohortClaimState::Closed
            );
        } else {
            assert!(
                fixture.ledger.reap_cohort_claim(&reference).is_err(),
                "{fault:?}"
            );
        }
        assert!(
            archive.join(HISTORY).is_dir(),
            "hot refusal and any read-only cold observation must preserve history"
        );
        if !matches!(fault, Fault::ReplaceDirectory) {
            assert_eq!(
                std::fs::read(archive.join(HISTORY).join(&original.relative)).unwrap(),
                original.body.as_bytes()
            );
        }
        if matches!(fault, Fault::RestoreDirectoryName) {
            let named = File::open(archive.join(HISTORY).join(&original.relative)).unwrap();
            assert_eq!(Stamp::read(&named).unwrap(), original.stamp);
            assert_eq!(Stamp::read(&original.file).unwrap(), original.stamp);
            assert_eq!(
                fixture.ledger.reap_cohort_claim(&reference).unwrap(),
                CohortClaimState::Closed
            );
            assert!(
                !archive.join(HISTORY).exists(),
                "valid original cold evidence can compact"
            );
            assert_eq!(original.file.metadata().unwrap().nlink(), 0);
            let mut actual = vec![0; original.body.len()];
            original.file.read_exact_at(&mut actual, 0).unwrap();
            assert_eq!(
                actual,
                original.body.as_bytes(),
                "the actual retained original description survives cold compaction unlink"
            );
            assert_eq!(
                original
                    .file
                    .read_at(&mut [0], original.body.len() as u64)
                    .unwrap(),
                0
            );
        }
    }
}
