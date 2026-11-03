// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Physical envelope corruption is distinct from an unrelated valid guard observation.
//! All fixtures are private protocol data; no executable is created, copied or run.

use super::*;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

struct World {
    scratch: TestWorld,
    namespace: Namespace,
    ledger: crate::Ledger,
    reference: CohortClosureRef,
}
impl World {
    fn new(before_compaction: bool) -> Self {
        let scratch = TestWorld::new("archive-envelope");
        let home = scratch.path().join("home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let namespace = Namespace::create_private(scratch.path(), &home).unwrap();
        let ledger = crate::Ledger::at(namespace.fence());
        let (mut record, mut laid) = ledger
            .lay(&crate::Need::new("archive envelope"), 0)
            .unwrap();
        ledger
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
        if before_compaction {
            interrupt("archive-moved");
        }
        drop(laid);
        if before_compaction {
            assert_eq!(INTERRUPT.with(std::cell::Cell::take), None);
        }
        let world = Self {
            scratch,
            namespace,
            ledger,
            reference,
        };
        assert_eq!(
            world.ledger.cohort_claim_state(&world.reference).unwrap(),
            CohortClaimState::Closed
        );
        assert_eq!(world.slot().join(HISTORY).is_dir(), before_compaction);
        assert_eq!(world.slot().join(COMPACTED).is_file(), !before_compaction);
        world
    }
    fn slot(&self) -> std::path::PathBuf {
        self.namespace.retired().join(self.reference.key())
    }
    fn foreign(&self) -> CohortClosureRef {
        CohortClosureRef::captured(
            self.reference.namespace(),
            self.reference.marker(),
            self.reference.epoch(),
            crate::ClaimGuardIdentity {
                device: self.reference.guard().device,
                inode: self.reference.guard().inode + 1,
            },
        )
        .unwrap()
    }
    fn receipt(&self) -> Value {
        aterm_json::from_slice(&std::fs::read(self.slot().join(RECEIPT)).unwrap()).unwrap()
    }
    fn write_receipt(&self, value: &Value) {
        let path = self.slot().join(RECEIPT);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, aterm_json::to_vec(value).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    }
    fn snapshot(&self) -> Vec<(String, Stamp, Vec<u8>)> {
        [".slot.lock", RECEIPT, COMPACTED]
            .into_iter()
            .filter_map(|name| match std::fs::File::open(self.slot().join(name)) {
                Ok(file) => Some((
                    name.into(),
                    Stamp::read(&file).unwrap(),
                    std::fs::read(self.slot().join(name)).unwrap(),
                )),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => panic!("fixture metadata: {error}"),
            })
            .collect()
    }
}

fn set(value: &mut Value, name: &str, field: Value) {
    value.as_object_mut().unwrap().insert(name.into(), field);
}
fn terminal(value: &mut Value) -> &mut Value {
    value.as_object_mut().unwrap().get_mut("terminal").unwrap()
}
fn anchor(value: &mut Value, edit: impl FnOnce(&mut Value)) {
    let term = terminal(value);
    let mut parsed: Value = aterm_json::from_str(term["anchor"].as_str().unwrap()).unwrap();
    edit(&mut parsed);
    set(
        term,
        "anchor",
        aterm_json::to_string(&parsed).unwrap().into(),
    );
}
fn remove_at_same_arity(value: &mut Value, name: &str) {
    value.as_object_mut().unwrap().remove(name).unwrap();
    set(value, "unrelated-field", Value::Null);
}
fn take_refusal() -> Option<&'static str> {
    super::super::FAIL_NAMED_FILE_FLUSH.with(std::cell::Cell::take)
}

#[test]
fn valid_foreign_guards_never_cover_or_repair_the_original_archive() {
    for before_compaction in [false, true] {
        let world = World::new(before_compaction);
        let before = world.snapshot();
        let foreign = world.foreign();
        for name in [".slot.lock", RECEIPT, COMPACTED] {
            super::super::refuse_named_file_flush(name);
            assert_eq!(
                world.ledger.reap_cohort_claim(&foreign).unwrap(),
                CohortClaimState::Unknown
            );
            assert_eq!(
                take_refusal(),
                Some(name),
                "a foreign reference consumes no query-side cover"
            );
            assert_eq!(world.snapshot(), before);
            assert_eq!(world.slot().join(HISTORY).is_dir(), before_compaction);
        }
        super::super::refuse_named_file_flush(".slot.lock");
        assert!(
            world.ledger.reap_cohort_claim(&world.reference).is_err(),
            "the exact request really must cover its original gate"
        );
        assert_eq!(take_refusal(), None);
        assert_eq!(
            world.ledger.reap_cohort_claim(&world.reference).unwrap(),
            CohortClaimState::Closed
        );
        assert!(!world.slot().join(HISTORY).exists());
        assert!(world.slot().join(RECEIPT).is_file());
        assert!(world.slot().join(COMPACTED).is_file());
    }
}

#[test]
fn malformed_stored_envelopes_refuse_even_a_foreign_guard_before_cover_or_repair() {
    for before_compaction in [false, true] {
        for fault in [
            "namespace",
            "marker",
            "epoch",
            "null-claim",
            "missing-claim",
            "non-directory-claim",
            "non-numeric-claim",
            "anchor-guard",
            "null-lock",
            "null-capture",
            "not-retired",
            "origin-guard",
            "origin-directory",
            "null-terminal-origin",
            "null-tip",
            "missing-inventory",
            "malformed-inventory-file",
        ] {
            let world = World::new(before_compaction);
            let mut value = world.receipt();
            let arities = (
                value.as_object().unwrap().len(),
                value["terminal"].as_object().unwrap().len(),
            );
            match fault {
                "namespace" | "marker" | "epoch" => {
                    let mut stored = value["reference"].clone();
                    let alternate = if fault == "marker" {
                        "0000000000000-1-0".into()
                    } else if stored[fault].as_str() != Some("ffffffffffffffffffffffffffffffff") {
                        "f".repeat(32)
                    } else {
                        "e".repeat(32)
                    };
                    set(&mut stored, fault, alternate.into());
                    // The reference codec itself accepts this data. Its physical namespace/key is wrong.
                    CohortClosureRef::from_json(&stored).unwrap();
                    set(&mut value, "reference", stored.clone());
                    set(terminal(&mut value), "reference", stored);
                }
                "null-claim" | "non-directory-claim" | "non-numeric-claim" => {
                    let claim = match fault {
                        "null-claim" => Value::Null,
                        "non-numeric-claim" => aterm_json::json!(["dev", "inode", "mode", "owner"]),
                        _ => {
                            let mut claim = value["claim"].clone();
                            claim.as_array_mut().unwrap()[2] = 0o100600_u64.into();
                            claim
                        }
                    };
                    set(&mut value, "claim", claim.clone());
                    set(terminal(&mut value), "claim", claim.clone());
                    anchor(&mut value, |parsed| set(parsed, "directory", claim));
                }
                "missing-claim" => {
                    remove_at_same_arity(&mut value, "claim");
                    remove_at_same_arity(terminal(&mut value), "claim");
                    anchor(&mut value, |parsed| {
                        remove_at_same_arity(parsed, "directory")
                    });
                }
                "anchor-guard" => anchor(&mut value, |parsed| {
                    set(parsed, "guard", aterm_json::json!([1u64, 2u64]))
                }),
                "null-lock" => anchor(&mut value, |parsed| set(parsed, "lock", Value::Null)),
                "null-capture" => set(terminal(&mut value), "capture", Value::Null),
                "not-retired" => {
                    let capture = terminal(&mut value)
                        .as_object_mut()
                        .unwrap()
                        .get_mut("capture")
                        .unwrap();
                    set(capture, "retired", false.into());
                }
                "origin-guard" | "origin-directory" => {
                    let capture = terminal(&mut value)
                        .as_object_mut()
                        .unwrap()
                        .get_mut("capture")
                        .unwrap();
                    let origin = capture
                        .as_object_mut()
                        .unwrap()
                        .get_mut("origin")
                        .unwrap()
                        .as_array_mut()
                        .unwrap();
                    if fault == "origin-guard" {
                        origin[1] = (world.reference.guard().inode + 1).into();
                    } else {
                        origin[3] = 0o040700_u64.into();
                    }
                }
                "null-terminal-origin" => set(&mut value, "terminal_origin", Value::Null),
                "null-tip" => set(terminal(&mut value), "tip", Value::Null),
                "missing-inventory" => remove_at_same_arity(&mut value, "inventory"),
                "malformed-inventory-file" => {
                    let inventory = value.as_object_mut().unwrap().get_mut("inventory").unwrap();
                    set(inventory, "files", aterm_json::json!([null]));
                }
                _ => unreachable!(),
            }
            assert_eq!(
                arities,
                (
                    value.as_object().unwrap().len(),
                    value["terminal"].as_object().unwrap().len()
                ),
                "corruption keeps the original object arity"
            );
            world.write_receipt(&value);
            let before = world.snapshot();
            let foreign = world.foreign();
            for offered in [&world.reference, &foreign] {
                super::super::refuse_named_file_flush(".slot.lock");
                let error = world.ledger.reap_cohort_claim(offered).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{fault}");
                assert_eq!(
                    take_refusal(),
                    Some(".slot.lock"),
                    "malformed metadata cannot trigger cover first"
                );
                assert_eq!(
                    world.snapshot(),
                    before,
                    "{fault}: all original data remains"
                );
                assert_eq!(world.slot().join(HISTORY).is_dir(), before_compaction);
            }
            assert!(world.scratch.path().exists());
        }
    }
}

#[test]
fn an_inventory_cannot_adopt_unknown_work_as_owned_protocol_data() {
    let world = World::new(true);
    let path = world.slot().join(HISTORY).join("unrelated-work.txt");
    let contents = b"unrelated work must survive a forged cleanup inventory\n";
    std::fs::write(&path, contents).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let file = File::open(&path).unwrap();
    let stamp = Stamp::read(&file).unwrap();
    let mut value = world.receipt();
    value
        .as_object_mut()
        .unwrap()
        .get_mut("inventory")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("files")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(
            aterm_json::json!({"directory":"","name":"unrelated-work.txt","stamp":stamp.value()}),
        );
    world.write_receipt(&value);

    let metadata = world.snapshot();
    let history = history_snapshot(&world);
    let foreign = world.foreign();
    for reference in [&world.reference, &foreign] {
        super::super::refuse_named_file_flush(".slot.lock");
        let result = world.ledger.reap_cohort_claim(reference);
        assert!(
            result.is_err(),
            "a forged inventory must refuse: {result:?}; unknown_path_exists={}",
            path.exists()
        );
        assert_eq!(take_refusal(), Some(".slot.lock"));
        assert_eq!(std::fs::read(&path).unwrap(), contents);
        assert_eq!(Stamp::read(&file).unwrap(), stamp);
        assert_eq!(world.snapshot(), metadata);
        assert_eq!(history_snapshot(&world), history);
        assert!(world.slot().join(HISTORY).is_dir());
    }
}

fn history_snapshot(world: &World) -> Vec<(String, Stamp, Vec<u8>)> {
    let root = world.slot().join(HISTORY);
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            for child in std::fs::read_dir(entry.path()).unwrap() {
                let child = child.unwrap();
                let file = File::open(child.path()).unwrap();
                files.push((
                    format!(
                        "{}/{}",
                        entry.file_name().to_str().unwrap(),
                        child.file_name().to_str().unwrap()
                    ),
                    Stamp::read(&file).unwrap(),
                    std::fs::read(child.path()).unwrap(),
                ));
            }
        } else {
            let file = File::open(entry.path()).unwrap();
            files.push((
                entry.file_name().to_str().unwrap().to_owned(),
                Stamp::read(&file).unwrap(),
                std::fs::read(entry.path()).unwrap(),
            ));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn file_entry<'a>(value: &'a mut Value, directory: &str, name: &str) -> &'a mut Value {
    value
        .as_object_mut()
        .unwrap()
        .get_mut("inventory")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("files")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|v| v["directory"].as_str() == Some(directory) && v["name"].as_str() == Some(name))
        .unwrap()
}

/// The forged receipt carries the CURRENT real stamp, so stale-stamp rejection alone
/// cannot satisfy this control. Unknown contents must never become a deletion intent.
#[test]
fn a_forged_inventory_cannot_authorize_unknown_or_disguised_history_payloads() {
    for fault in [
        "unknown-executable",
        "protocol-executable",
        "anchor-payload",
        "lock-payload",
        "base-payload",
        "checkpoint-payload",
        "missing-required",
        "wrong-root-origin",
    ] {
        let world = World::new(true);
        let root = world.slot().join(HISTORY);
        let mut value = world.receipt();
        let directory = value["inventory"]["directories"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["name"]
            .as_str()
            .unwrap()
            .to_owned();
        let working = value["terminal"]["capture"]["name"]
            .as_str()
            .unwrap()
            .to_owned();
        match fault {
            "unknown-executable" => {
                let name = "unrelated-owner-work";
                let path = root.join(name);
                std::fs::write(&path, b"owner work must survive\n").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
                let stamp = Stamp::read(&File::open(path).unwrap()).unwrap();
                value
                    .as_object_mut()
                    .unwrap()
                    .get_mut("inventory")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .get_mut("files")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                    .push(aterm_json::json!({"directory":"","name":name,"stamp":stamp.value()}));
            }
            "missing-required" => {
                value
                    .as_object_mut()
                    .unwrap()
                    .get_mut("inventory")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .get_mut("files")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                    .retain(|v| v["name"].as_str() != Some(MUTATION));
            }
            "wrong-root-origin" => {
                let mut origin = file_entry(&mut value, "", &working)["stamp"].clone();
                origin.as_array_mut().unwrap()[1] = (world.reference.guard().inode + 1).into();
                set(file_entry(&mut value, "", &working), "stamp", origin);
            }
            _ => {
                let (parent, name) = match fault {
                    "protocol-executable" | "anchor-payload" => ("", HEADER.to_owned()),
                    "lock-payload" => ("", MUTATION.to_owned()),
                    "base-payload" => (directory.as_str(), "base.json".into()),
                    "checkpoint-payload" => {
                        let name = value["inventory"]["files"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|v| v["directory"].as_str() == Some(directory.as_str()))
                            .filter_map(|v| v["name"].as_str())
                            .find(|n| {
                                envelope::checkpoint_ordinal(n).is_some_and(|ordinal| {
                                    ordinal
                                        < value["terminal"]["capture"]["ordinal"].as_u64().unwrap()
                                })
                            })
                            .unwrap()
                            .to_owned();
                        (directory.as_str(), name)
                    }
                    _ => unreachable!(),
                };
                let path = root.join(parent).join(&name);
                if fault == "protocol-executable" {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                } else {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    let original = std::fs::read(&path).unwrap();
                    let mut body = original.clone();
                    if fault == "base-payload" {
                        body = br#"{"kind":"cohort-epoch-base","owner":"unrelated data"}"#.to_vec();
                    } else if fault == "checkpoint-payload" {
                        let mut capture =
                            Capture::decode(std::str::from_utf8(&body).unwrap()).unwrap();
                        capture.ordinal += 1;
                        body = capture.encode().unwrap().into_bytes();
                    } else {
                        body[0] ^= 1;
                    }
                    std::fs::write(&path, &body).unwrap();
                    std::fs::set_permissions(
                        &path,
                        std::fs::Permissions::from_mode(if name == MUTATION {
                            0o600
                        } else {
                            0o400
                        }),
                    )
                    .unwrap();
                    assert_ne!(std::fs::read(&path).unwrap(), original);
                }
                let stamp = Stamp::read(&File::open(&path).unwrap()).unwrap();
                set(
                    file_entry(&mut value, parent, &name),
                    "stamp",
                    stamp.value(),
                );
                if matches!(
                    fault,
                    "anchor-payload" | "lock-payload" | "base-payload" | "checkpoint-payload"
                ) {
                    let capture = Capture::decode(
                        &aterm_json::to_string(&value["terminal"]["capture"]).unwrap(),
                    )
                    .unwrap();
                    envelope::inventory(&value["inventory"], &capture).unwrap();
                }
            }
        }
        world.write_receipt(&value);
        let files = history_snapshot(&world);
        let metadata = world.snapshot();
        let foreign = world.foreign();
        for offered in [&world.reference, &foreign] {
            super::super::refuse_named_file_flush(".slot.lock");
            assert_eq!(
                world.ledger.reap_cohort_claim(offered).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "{fault}"
            );
            assert_eq!(
                take_refusal(),
                Some(".slot.lock"),
                "{fault}: refuse before any cover"
            );
            assert_eq!(
                history_snapshot(&world),
                files,
                "{fault}: preserve every original member"
            );
            assert_eq!(
                world.snapshot(),
                metadata,
                "{fault}: no compaction intent or metadata change"
            );
            assert!(!world.slot().join(COMPACTED).exists());
        }
    }
}

#[test]
fn missing_physical_members_cannot_start_a_new_compaction_intent() {
    for member in ["terminal", "base", "working", "anchor"] {
        let world = World::new(true);
        let value = world.receipt();
        let root = world.slot().join(HISTORY);
        let path = match member {
            "terminal" => root.join(CLOSED),
            "anchor" => root.join(HEADER),
            "working" => root.join(value["terminal"]["capture"]["name"].as_str().unwrap()),
            "base" => root
                .join(
                    value["inventory"]["directories"]
                        .as_array()
                        .unwrap()
                        .last()
                        .unwrap()["name"]
                        .as_str()
                        .unwrap(),
                )
                .join("base.json"),
            _ => unreachable!(),
        };
        std::fs::remove_file(path).unwrap();
        let metadata = world.snapshot();
        let remaining = history_snapshot(&world);
        let foreign = world.foreign();
        for reference in [&world.reference, &foreign] {
            super::super::refuse_named_file_flush(".slot.lock");
            assert_eq!(
                world
                    .ledger
                    .reap_cohort_claim(reference)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData,
                "{member}"
            );
            assert_eq!(take_refusal(), Some(".slot.lock"));
            assert_eq!(world.snapshot(), metadata);
            assert_eq!(history_snapshot(&world), remaining);
            assert!(!world.slot().join(COMPACTED).exists());
        }
    }
}

#[test]
fn only_the_retained_matching_compaction_intent_allows_an_interrupted_missing_subset() {
    let world = World::new(true);
    let all = history_snapshot(&world);
    interrupt("compaction-unlinked");
    assert!(world.ledger.reap_cohort_claim(&world.reference).is_err());
    assert_eq!(INTERRUPT.with(std::cell::Cell::take), None);
    assert!(world.slot().join(COMPACTED).is_file());
    let remaining = history_snapshot(&world);
    assert_eq!(
        remaining.len() + 1,
        all.len(),
        "the actual first unlink completed"
    );
    let path = world.slot().join(COMPACTED);
    let original = std::fs::read(&path).unwrap();
    let mut value: Value = aterm_json::from_slice(&original).unwrap();
    set(&mut value, "receipt", Value::Null);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, aterm_json::to_vec(&value).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let metadata = world.snapshot();
    let foreign = world.foreign();
    for reference in [&world.reference, &foreign] {
        super::super::refuse_named_file_flush(".slot.lock");
        assert_eq!(
            world
                .ledger
                .reap_cohort_claim(reference)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(take_refusal(), Some(".slot.lock"));
        assert_eq!(world.snapshot(), metadata);
        assert_eq!(history_snapshot(&world), remaining);
    }
    // Restore the exact known protocol bytes as an explicit fixture fault recovery. The
    // next cold read must cover this new original metadata before completing its intent.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, original).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    assert_eq!(
        world.ledger.reap_cohort_claim(&foreign).unwrap(),
        CohortClaimState::Unknown
    );
    assert_eq!(history_snapshot(&world), remaining);
    super::super::refuse_named_file_flush(COMPACTED);
    assert_eq!(
        world.ledger.reap_cohort_claim(&foreign).unwrap(),
        CohortClaimState::Unknown
    );
    assert_eq!(take_refusal(), Some(COMPACTED));
    super::super::refuse_named_file_flush(COMPACTED);
    assert!(world.ledger.reap_cohort_claim(&world.reference).is_err());
    assert_eq!(
        take_refusal(),
        None,
        "the exact intent FILE must really be covered"
    );
    assert_eq!(history_snapshot(&world), remaining);
    assert_eq!(
        world.ledger.reap_cohort_claim(&world.reference).unwrap(),
        CohortClaimState::Closed
    );
    assert!(!world.slot().join(HISTORY).exists());
}
