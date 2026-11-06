// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Physical epoch traces, including refused FILE cover, restored working origin and the
//! empty directory left after its last known member was unlinked. No process is spawned.

use super::*;
use crate::ledger::Laid;
use crate::{Ledger, Need, cohort_namespace::Namespace};
use aterm_spec::derive::Model;
use aterm_spec::interp::State;

struct World {
    _scratch: TestWorld,
    namespace: Namespace,
    ledger: Ledger,
}

impl World {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt as _;
        let scratch = TestWorld::new("history-model");
        let home = scratch.path().join("home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let namespace = Namespace::create_private(scratch.path(), &home).unwrap();
        let ledger = Ledger::at(namespace.fence());
        Self {
            _scratch: scratch,
            namespace,
            ledger,
        }
    }
}

struct Trace {
    model: Model,
    state: State,
}

impl Trace {
    fn new() -> Self {
        let model = aterm_models_capacity::capacity_history_model();
        let state = model.init_state();
        Self { model, state }
    }

    fn step(&mut self, name: &str) {
        let mut next = self.model.successors(name, &self.state);
        assert_eq!(
            next.len(),
            1,
            "actual epoch transition {name}: {:?}",
            self.state
        );
        self.state = next.pop().unwrap();
    }

    fn observe(&self, laid: &Laid, frozen: Option<&(Capture, Stamp)>, covered: bool) {
        assert_eq!(project(&self.state, laid, frozen, covered), self.state);
    }
}

fn members(directory: &File) -> Vec<String> {
    fd::Dir::read_from(directory.as_fd())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
        .filter(|name| !matches!(name.as_str(), "." | ".."))
        .collect()
}

fn base(directory: &File) -> Value {
    aterm_json::from_str(&held_immutable(directory, "base.json").unwrap().1).unwrap()
}

fn capture(value: &Value) -> Capture {
    Capture::decode(&aterm_json::to_string(value).unwrap()).unwrap()
}

/// Read actual names, original stamps and encoded generations. `covered` comes from the
/// real after-cover interruption being consumed; a refused FILE flush leaves it armed.
fn project(
    history: &State,
    laid: &Laid,
    frozen: Option<&(Capture, Stamp)>,
    covered: bool,
) -> State {
    let custody = laid.custody.as_ref().unwrap();
    let root = &custody.directory;
    let mut finals = std::collections::BTreeMap::new();
    let mut preparing = None;
    for name in members(root) {
        if let Some(number) = name
            .strip_prefix("epoch-")
            .and_then(|s| s.strip_suffix(".preparing.noindex"))
        {
            assert!(preparing.replace(number.parse::<u64>().unwrap()).is_none());
        } else if let Some(number) = name
            .strip_prefix("epoch-")
            .and_then(|s| s.strip_suffix(".noindex"))
        {
            finals.insert(
                number.parse::<u64>().unwrap(),
                open(root, &name, false, true).unwrap(),
            );
        }
    }
    let (&number, current) = finals.last_key_value().unwrap();
    let value = base(current);
    let original_base = capture(&value["capture"]);
    let mut latest = original_base.clone();
    let current_names = members(current);
    for name in &current_names {
        if name != "base.json" {
            let next = Capture::decode(&held_immutable(current, name).unwrap().1).unwrap();
            if next.ordinal > latest.ordinal {
                latest = next;
            }
        }
    }
    let mut predecessor_members = 0;
    let mut predecessor_original = true;
    if finals.len() == 2 {
        let (&old_number, old) = finals.first_key_value().unwrap();
        let declared = &value["predecessor"];
        predecessor_original = declared["epoch"].as_u64() == Some(old_number)
            && declared["directory"] == identity(old, true).unwrap();
        for name in members(old) {
            let expected = declared["members"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["name"].as_str() == Some(name.as_str()));
            if let Some(expected) = expected {
                predecessor_members += 1;
                let (_, _, stamp) = held_immutable(old, &name).unwrap();
                predecessor_original &= stamp == Stamp::decode(&expected["stamp"]).unwrap();
            } else {
                predecessor_original = false;
            }
        }
    }
    let mut actual = history.clone();
    actual.insert(
        "phase",
        if preparing.is_some() {
            if covered { 2 } else { 1 }
        } else {
            0
        },
    );
    actual.insert(
        "epochs",
        i64::try_from(finals.len() + usize::from(preparing.is_some())).unwrap(),
    );
    actual.insert("rotations", i64::try_from(number - 1).unwrap());
    actual.insert(
        "current_members",
        i64::try_from(current_names.len()).unwrap(),
    );
    actual.insert("predecessor_members", predecessor_members);
    actual.insert("predecessor_present", i64::from(finals.len() == 2));
    actual.insert("predecessor_original", i64::from(predecessor_original));
    actual.insert(
        "working_original",
        i64::from(latest.check(root, &custody.writer).is_ok()),
    );
    actual.insert(
        "base_covered",
        i64::from(if preparing.is_some() {
            covered
        } else {
            number > 1
        }),
    );
    actual.insert("published", i64::from(number > 1 && preparing.is_none()));
    if number > 1 {
        let (expected, stamp) = frozen.unwrap();
        actual.insert(
            "publication_original",
            i64::from(original_base == *expected && value["predecessor"]["tip"] == stamp.value()),
        );
    }
    actual
}

fn full_epoch() -> (World, Laid, Trace) {
    let world = World::new();
    let (_, mut laid) = world
        .ledger
        .lay(&Need::new("physical history model"), 0)
        .unwrap();
    let mut trace = Trace::new();
    trace.observe(&laid, None, false);
    for next in 1..epoch::WINDOW - 1 {
        laid.append(&crate::Record::next_line(next as u64)).unwrap();
        trace.step("Append");
        trace.observe(&laid, None, false);
    }
    (world, laid, trace)
}

fn prepare(covered: bool) -> (World, Laid, (Capture, Stamp), Trace) {
    let (world, laid, mut trace) = full_epoch();
    let mut transaction = laid.transaction().unwrap();
    let frozen = transaction.latest.clone().unwrap();
    if !covered {
        refuse_named_file_flush("base.json");
    }
    epoch::interrupt("base-covered");
    // Acquire the original authority before arming the basename hook: a new transaction
    // itself covers the current base, whereas this control targets the replacement base.
    let custody = laid.custody.as_ref().unwrap();
    assert!(
        transaction
            .ensure_room(&custody.directory, &custody.writer)
            .is_err()
    );
    let observed_cover = epoch::take_interruption().is_none();
    assert_eq!(
        observed_cover, covered,
        "FILE refusal must precede the after-cover boundary"
    );
    trace.step("BeginRotation");
    if observed_cover {
        trace.step("CoverBase");
    }
    trace.observe(&laid, Some(&frozen), observed_cover);
    drop(transaction);
    (world, laid, frozen, trace)
}

fn rotate(covered: bool) -> (World, Laid, (Capture, Stamp), Trace) {
    let (world, mut laid, frozen, mut trace) = prepare(covered);
    laid.append(&crate::Record::next_line(101)).unwrap();
    if !covered {
        trace.step("CoverBase");
    }
    trace.step("Publish");
    trace.step("Append");
    trace.observe(&laid, Some(&frozen), false);
    (world, laid, frozen, trace)
}

#[test]
fn physical_epoch_recovery_and_empty_predecessor_follow_the_model() {
    for covered in [false, true] {
        let (_world, laid, frozen, mut trace) = rotate(covered);
        let transaction = laid.transaction().unwrap();
        let epoch = transaction.epoch.as_ref().unwrap();
        while trace.state["predecessor_members"] > 0 {
            epoch::interrupt("predecessor-unlinked");
            assert!(
                epoch
                    .prune_predecessor(&transaction.anchor.directory)
                    .is_err()
            );
            assert!(epoch::take_interruption().is_none());
            trace.step("PruneOne");
            trace.observe(&laid, Some(&frozen), false);
        }
        assert_eq!(
            trace.state["epochs"], 2,
            "the empty original directory still exists"
        );
        epoch
            .prune_predecessor(&transaction.anchor.directory)
            .unwrap();
        trace.step("RemoveEmptyPredecessor");
        trace.observe(&laid, Some(&frozen), false);
    }
}

#[test]
fn unknown_data_in_an_empty_predecessor_is_preserved() {
    let (_world, laid, frozen, mut trace) = rotate(true);
    let transaction = laid.transaction().unwrap();
    let epoch = transaction.epoch.as_ref().unwrap();
    while trace.state["predecessor_members"] > 0 {
        epoch::interrupt("predecessor-unlinked");
        assert!(
            epoch
                .prune_predecessor(&transaction.anchor.directory)
                .is_err()
        );
        assert!(epoch::take_interruption().is_none());
        trace.step("PruneOne");
    }
    let old = open(
        &transaction.anchor.directory,
        "epoch-0000000000000001.noindex",
        false,
        true,
    )
    .unwrap();
    let mut unknown = open(&old, "unrelated", true, false).unwrap();
    unknown.write_all(b"preserve this unrelated data").unwrap();
    trace.step("RestorePredecessor");
    assert!(
        epoch
            .prune_predecessor(&transaction.anchor.directory)
            .is_err()
    );
    assert_eq!(
        bytes(&unknown, 128).unwrap().0,
        "preserve this unrelated data"
    );
    trace.observe(&laid, Some(&frozen), false);
    let mutant = aterm_spec::interp::with_buggy(&trace.model, 6);
    let bad = mutant
        .successors("RemoveUnknownPredecessor", &trace.state)
        .pop()
        .unwrap();
    assert!(!mutant.check_invariant("UnknownPredecessorsArePreserved", &bad));
}

#[test]
fn a_restored_working_origin_cannot_publish_a_prepared_base() {
    use std::os::unix::fs::FileExt as _;
    let (world, mut laid, frozen, mut trace) = prepare(true);
    let writer = &laid.custody.as_ref().unwrap().writer;
    let before = writer.metadata().unwrap();
    let original = std::fs::read(laid.path()).unwrap();
    past_the_clock_tick(&world.namespace.open_backend().unwrap(), writer);
    writer.write_all_at(b"!", 0).unwrap();
    writer.write_all_at(&original, 0).unwrap();
    writer.set_modified(before.modified().unwrap()).unwrap();
    assert_eq!(std::fs::read(laid.path()).unwrap(), original);
    trace.step("RestoreWorkingBeforePublication");
    assert!(laid.append(&crate::Record::next_line(102)).is_err());
    trace.observe(&laid, Some(&frozen), true);
    assert!(trace.model.successors("Publish", &trace.state).is_empty());
    let mutant = aterm_spec::interp::with_buggy(&trace.model, 2);
    let bad = mutant
        .successors("RecaptureRestoredBase", &trace.state)
        .pop()
        .unwrap();
    assert!(!mutant.check_invariant("PublicationUsesFrozenOriginal", &bad));
}

#[test]
fn each_rotation_stage_is_observed_before_the_next_checkpoint_append() {
    let (_world, mut laid, mut trace) = full_epoch();
    let mutant = aterm_spec::interp::with_buggy(&trace.model, 5);
    let bad = mutant
        .successors("ConsumeTerminalReserve", &trace.state)
        .pop()
        .unwrap();
    assert!(!mutant.check_invariant("OrdinaryWritesPreserveTerminalRoom", &bad));
    let mut transaction = laid.transaction().unwrap();
    let frozen = transaction.latest.clone().unwrap();
    let custody = laid.custody.as_ref().unwrap();
    epoch::interrupt("rotation-prepared");
    assert!(
        transaction
            .ensure_room(&custody.directory, &custody.writer)
            .is_err()
    );
    assert!(epoch::take_interruption().is_none());
    trace.step("BeginRotation");
    trace.observe(&laid, Some(&frozen), false);
    let mutant = aterm_spec::interp::with_buggy(&trace.model, 1);
    let bad = mutant
        .successors("PublishUncovered", &trace.state)
        .pop()
        .unwrap();
    assert!(!mutant.check_invariant("PublicationFollowsCover", &bad));
    drop(transaction);

    // Each interruption is reached only AFTER its physical operation. Recovery keeps
    // the already parsed base and predecessor descriptors through the next operation.
    epoch::interrupt("base-covered");
    assert!(laid.transaction().is_err());
    assert!(epoch::take_interruption().is_none());
    trace.step("CoverBase");
    trace.observe(&laid, Some(&frozen), true);
    epoch::interrupt("epoch-published");
    assert!(laid.transaction().is_err());
    assert!(epoch::take_interruption().is_none());
    trace.step("Publish");
    trace.observe(&laid, Some(&frozen), false);
    drop(laid.transaction().unwrap());
    trace.observe(&laid, Some(&frozen), false);
    laid.append(&crate::Record::next_line(103)).unwrap();
    trace.step("Append");
    trace.observe(&laid, Some(&frozen), false);
}

#[test]
fn an_unknown_predecessor_blocks_pruning_and_a_third_epoch() {
    let (_world, mut laid, frozen, mut trace) = rotate(true);
    for next in 2..epoch::WINDOW - 1 {
        laid.append(&crate::Record::next_line(200 + next as u64))
            .unwrap();
        trace.step("Append");
        trace.observe(&laid, Some(&frozen), false);
    }
    let transaction = laid.transaction().unwrap();
    let epoch = transaction.epoch.as_ref().unwrap();
    let old = open(
        &transaction.anchor.directory,
        "epoch-0000000000000001.noindex",
        false,
        true,
    )
    .unwrap();
    let mut unknown = open(&old, "unrelated", true, false).unwrap();
    unknown.write_all(b"unknown predecessor payload").unwrap();
    trace.step("RestorePredecessor");
    assert!(
        epoch
            .prune_predecessor(&transaction.anchor.directory)
            .is_err()
    );
    trace.observe(&laid, Some(&frozen), false);
    let mutant = aterm_spec::interp::with_buggy(&trace.model, 4);
    let bad = mutant
        .successors("PruneUnknown", &trace.state)
        .pop()
        .unwrap();
    assert!(!mutant.check_invariant("UnknownPredecessorsArePreserved", &bad));
    let mutant = aterm_spec::interp::with_buggy(&trace.model, 3);
    let bad = mutant
        .successors("RotatePastUnprunedPrefix", &trace.state)
        .pop()
        .unwrap();
    assert!(!mutant.check_invariant("AtMostTwoPhysicalEpochs", &bad));
    drop(transaction);
    let working = std::fs::read(laid.path()).unwrap();
    assert!(laid.append(&crate::Record::next_line(999)).is_err());
    assert_eq!(std::fs::read(laid.path()).unwrap(), working);
    assert_eq!(
        bytes(&unknown, 128).unwrap().0,
        "unknown predecessor payload"
    );
    trace.observe(&laid, Some(&frozen), false);
}

#[test]
fn a_replaced_preparation_name_is_preserved_before_any_publication_rename() {
    fn replace(root: &File, number: u64, original: &File) -> io::Result<()> {
        let name = format!("epoch-{number:016}.preparing.noindex");
        assert_eq!(
            identity(&open(root, &name, false, true)?, true)?,
            identity(original, true)?
        );
        fd::renameat_with(
            root,
            name.as_str(),
            root,
            "detached-original.noindex",
            RenameFlags::NOREPLACE,
        )?;
        fd::mkdirat(root, name.as_str(), Mode::RWXU)?;
        let replacement = open(root, &name, false, true)?;
        assert_ne!(identity(&replacement, true)?, identity(original, true)?);
        let mut unknown = open(&replacement, "owner-work", true, false)?;
        unknown.write_all(b"unknown replacement stays where it was placed")?;
        Ok(())
    }
    let (_world, laid, _trace) = full_epoch();
    let mut transaction = laid.transaction().unwrap();
    epoch::AFTER_BASE_COVER.with(|hook| hook.set(Some(replace)));
    let custody = laid.custody.as_ref().unwrap();
    assert_eq!(
        transaction
            .ensure_room(&custody.directory, &custody.writer)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        epoch::AFTER_BASE_COVER
            .with(std::cell::Cell::take)
            .is_none()
    );
    let root = &transaction.anchor.directory;
    let replacement = open(
        root,
        "epoch-0000000000000002.preparing.noindex",
        false,
        true,
    )
    .unwrap();
    let replacement_id = identity(&replacement, true).unwrap();
    let unknown = open(&replacement, "owner-work", false, false).unwrap();
    let original = open(root, "detached-original.noindex", false, true).unwrap();
    let original_id = identity(&original, true).unwrap();
    let (text, unknown_stamp) = bytes(&unknown, 128).unwrap();
    assert_eq!(text, "unknown replacement stays where it was placed");
    assert_eq!(
        open(root, "epoch-0000000000000002.noindex", false, true)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound,
        "refusal must precede moving the replacement"
    );
    // Even a fresh recovery refuses this unrelated namespace without moving either dir.
    drop(transaction);
    assert!(laid.transaction().is_err());
    let root = &laid.custody.as_ref().unwrap().directory;
    assert_eq!(
        identity(
            &open(
                root,
                "epoch-0000000000000002.preparing.noindex",
                false,
                true
            )
            .unwrap(),
            true
        )
        .unwrap(),
        replacement_id
    );
    assert_eq!(
        identity(
            &open(root, "detached-original.noindex", false, true).unwrap(),
            true
        )
        .unwrap(),
        original_id
    );
    assert_eq!(Stamp::read(&unknown).unwrap(), unknown_stamp);
    assert_eq!(bytes(&unknown, 128).unwrap().0, text);
}

#[test]
fn cold_preparation_keeps_its_directory_origin_from_before_the_first_base_cover() {
    use std::os::unix::fs::MetadataExt as _;
    fn transient(parent: &File) -> io::Result<()> {
        let before = directory_origin(parent)?;
        let clock_world = aterm_tempfile::tempdir()?;
        let clock = File::open(clock_world.path())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let probe = open(&clock, "tick", true, false)?;
            let now = probe.metadata()?;
            fd::unlinkat(&clock, "tick", fd::AtFlags::empty())?;
            if (now.ctime(), now.ctime_nsec()) > (before.3, before.4) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "filesystem clock must advance"
            );
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(
            directory_origin(parent)?,
            before,
            "clock probe leaves victim untouched"
        );
        let mut child = open(parent, "transient-child", true, false)?;
        child.write_all(b"temporary entry")?;
        fd::unlinkat(parent, "transient-child", fd::AtFlags::empty())?;
        assert_ne!(directory_origin(parent)?, before);
        Ok(())
    }
    let (_world, laid, _frozen, _trace) = prepare(true);
    let root = &laid.custody.as_ref().unwrap().directory;
    let prepared = open(
        root,
        "epoch-0000000000000002.preparing.noindex",
        false,
        true,
    )
    .unwrap();
    let original_id = identity(&prepared, true).unwrap();
    let before_cover = directory_origin(&prepared).unwrap();
    let (base, text, stamp) = held_immutable(&prepared, "base.json").unwrap();
    epoch::observe_original_base_cover(&prepared, transient);
    assert_eq!(
        laid.transaction().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        identity(
            &open(
                root,
                "epoch-0000000000000002.preparing.noindex",
                false,
                true
            )
            .unwrap(),
            true
        )
        .unwrap(),
        original_id
    );
    assert_ne!(
        directory_origin(&prepared).unwrap(),
        before_cover,
        "the actual transient mutation must have run before the refusal"
    );
    assert_eq!(members(&prepared), ["base.json"]);
    assert_eq!(bytes(&base, MAX_ENTRY).unwrap(), (text, stamp));
    assert_eq!(
        open(root, "epoch-0000000000000002.noindex", false, true)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}
